//! A/B the voice-clustering rewrite (perf slice, 2026-07-14): the old
//! recompute-every-merge agglomeration vs the cached one now in
//! `yc_frame::voice`, at the sizes the camera plan actually sees.
//!
//! Two sizes matter. The gate fixtures (~70 s clips) embed ~64 windows — the
//! clustering was never their bottleneck. A fully-voiced ~180 s clip (the
//! Shorts ceiling) embeds ~240 windows, and the cost is O(n³·d): that is the
//! "billions of multiplies per plan" the code review measured.
//!
//! Both are timed for ONE camera plan, i.e. the 8 clusterings `build_lane`
//! runs (7 sweep thresholds + the final pick) — which the rewrite also collapses
//! into a single agglomeration, since a threshold only decides where the merge
//! trail stops.
//!
//!   cargo run --release -p yt-clipper --example cluster_bench

use std::time::Instant;

const DIM: usize = 192; // CAM++ speaker-embedding dimension
const SWEEP: [f32; 7] = [0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60];

/// The pre-2026-07-14 implementation, copied verbatim for the A/B: every merge
/// recomputes every cluster pair's member-by-member distances from scratch.
fn cluster_cosine_naive(embs: &[Vec<f32>], threshold: f32) -> usize {
    let n = embs.len();
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let dist = |a: usize, b: usize| -> f32 {
        1.0 - embs[a].iter().zip(embs[b].iter()).map(|(x, y)| x * y).sum::<f32>()
    };
    loop {
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..members.len() {
            for j in i + 1..members.len() {
                let mut sum = 0f32;
                for &a in &members[i] {
                    for &b in &members[j] {
                        sum += dist(a, b);
                    }
                }
                let d = sum / (members[i].len() * members[j].len()) as f32;
                if best.map(|(_, _, bd)| d < bd).unwrap_or(true) {
                    best = Some((i, j, d));
                }
            }
        }
        match best {
            Some((i, j, d)) if d < threshold => {
                let b = members.remove(j);
                members[i].extend(b);
            }
            _ => break,
        }
    }
    members.len()
}

/// Unit-norm embeddings around `k` voices — the shape real windows have.
fn synth(n: usize, k: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut s = seed | 1;
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / 8_388_608.0 - 1.0
    };
    let centers: Vec<Vec<f32>> = (0..k).map(|_| (0..DIM).map(|_| next()).collect()).collect();
    (0..n)
        .map(|i| {
            let c = &centers[i % k];
            let v: Vec<f32> = c.iter().map(|x| x + 0.35 * next()).collect();
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
            v.into_iter().map(|x| x / norm).collect()
        })
        .collect()
}

fn main() {
    println!("{:>8}  {:>12}  {:>12}  {:>9}  {}", "windows", "old (8 runs)", "new (1 trail)", "speedup", "k agrees");
    for (n, label) in [(64usize, "gate fixtures (~70 s clip)"), (240, "fully-voiced 180 s clip")] {
        let embs = synth(n, 3, 2024);

        // OLD: one full agglomeration per sweep threshold + the final pick.
        let t = Instant::now();
        let mut old_ks = Vec::new();
        for &thr in &SWEEP {
            old_ks.push(cluster_cosine_naive(&embs, thr));
        }
        old_ks.push(cluster_cosine_naive(&embs, 0.45)); // the final pick
        let old_ms = t.elapsed().as_secs_f64() * 1000.0;

        // NEW: one agglomeration, cut eight times.
        let t = Instant::now();
        let trail = yc_frame::voice::agglomerate(&embs);
        let mut new_ks: Vec<usize> = SWEEP.iter().map(|&thr| trail.cut(thr).k).collect();
        new_ks.push(trail.cut(0.45).k);
        let new_ms = t.elapsed().as_secs_f64() * 1000.0;

        println!(
            "{n:>8}  {old_ms:>10.1}ms  {new_ms:>10.1}ms  {:>8.0}x  {}   <- {label}",
            old_ms / new_ms.max(1e-6),
            if old_ks == new_ks { "yes" } else { "NO — DIVERGED" },
        );
    }
}
