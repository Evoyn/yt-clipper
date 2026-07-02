//! Transcript scorer for the caption-perfection loop (benchmarks/, 2026-07-02).
//!
//! Scores a candidate transcript against an operator ground-truth file with
//! token-level edit alignment, so decode/bias/correction experiments get one
//! objective yardstick (the operator's ear stays the final judge). CPU-only,
//! no models.
//!
//!   cargo run -p yt-clipper --example asr_score -- benchmarks/diskusi-biasa.groundtruth.txt <candidate.txt>
//!
//! Ground-truth format: one utterance per line; `#` comment lines and `(...)`
//! non-speech lines are ignored. The candidate file is free text (a raw
//! `language X<asr_text>...` sidecar line is handled). Normalization for both:
//! lowercase, punctuation/hyphens to spaces, whitespace collapsed.

use std::path::PathBuf;

fn normalize(text: &str) -> Vec<String> {
    let t = match text.find("<asr_text>") {
        Some(i) => &text[i + "<asr_text>".len()..],
        None => text,
    };
    t.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
}

/// Required words, plus an OPTIONAL set (`?`-prefixed lines): tokens that are
/// fine to caption but not a miss when absent — e.g. a scream ("ahh") the
/// operator marked as an acceptable reaction caption. An optional token in the
/// candidate is scored `ok?` instead of a hallucination.
fn load_ground_truth(path: &PathBuf) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let text = std::fs::read_to_string(path)?;
    let mut words = Vec::new();
    let mut optional = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with('(') {
            continue;
        }
        if let Some(rest) = l.strip_prefix('?') {
            optional.extend(normalize(rest));
            continue;
        }
        words.extend(normalize(l));
    }
    Ok((words, optional))
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Ok,
    Sub,
    Del, // ground-truth word missing from the candidate
    Ins, // candidate word not in the ground truth (hallucination)
}

/// Levenshtein alignment over tokens; returns the op sequence via backtrace.
fn align(gt: &[String], cand: &[String]) -> Vec<(Op, Option<usize>, Option<usize>)> {
    let (n, m) = (gt.len(), cand.len());
    let mut d = vec![vec![0u32; m + 1]; n + 1];
    for i in 0..=n {
        d[i][0] = i as u32;
    }
    for j in 0..=m {
        d[0][j] = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[i - 1][j - 1] + if gt[i - 1] == cand[j - 1] { 0 } else { 1 };
            d[i][j] = sub.min(d[i - 1][j] + 1).min(d[i][j - 1] + 1);
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && d[i][j] == d[i - 1][j - 1] && gt[i - 1] == cand[j - 1] {
            ops.push((Op::Ok, Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if i > 0 && j > 0 && d[i][j] == d[i - 1][j - 1] + 1 {
            ops.push((Op::Sub, Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if i > 0 && d[i][j] == d[i - 1][j] + 1 {
            ops.push((Op::Del, Some(i - 1), None));
            i -= 1;
        } else {
            ops.push((Op::Ins, None, Some(j - 1)));
            j -= 1;
        }
    }
    ops.reverse();
    ops
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let gt_path = PathBuf::from(args.next().expect("usage: asr_score <groundtruth> <candidate>"));
    let cand_path = PathBuf::from(args.next().expect("candidate file"));
    let (gt, optional) = load_ground_truth(&gt_path)?;
    let cand = normalize(&std::fs::read_to_string(&cand_path)?);

    let ops = align(&gt, &cand);
    let (mut ok, mut sub, mut del, mut ins, mut opt) = (0, 0, 0, 0, 0);
    println!("--- alignment (gt <-> candidate) ---");
    for (op, gi, ci) in &ops {
        let g = gi.map(|i| gt[i].as_str()).unwrap_or("");
        let c = ci.map(|i| cand[i].as_str()).unwrap_or("");
        match op {
            Op::Ok => {
                ok += 1;
                println!("  ok   {g}");
            }
            Op::Sub => {
                sub += 1;
                println!("  SUB  {g:<20} -> {c}");
            }
            Op::Del => {
                del += 1;
                println!("  MISS {g}");
            }
            Op::Ins => {
                if optional.iter().any(|o| o == c) {
                    opt += 1;
                    println!("  ok?  {:<20} <- {c} (optional)", "");
                } else {
                    ins += 1;
                    println!("  HALL {:<20} <- {c}", "");
                }
            }
        }
    }
    let wer = (sub + del + ins) as f64 / gt.len() as f64;
    println!("\n--- score: {} ---", cand_path.display());
    println!(
        "gt words {} | ok {} | sub {} | miss {} | hallucinated {} | optional-ok {} | WER {:.1}%",
        gt.len(),
        ok,
        sub,
        del,
        ins,
        opt,
        wer * 100.0
    );
    Ok(())
}
