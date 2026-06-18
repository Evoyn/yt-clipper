//! Eyeball + tune discovery on a real imported VOD (no GPU):
//!   scripts\cargo-cuda.bat run -p yc-detect --example detect_vod -- workspace\ZSegfmsrYmE
//!
//! Reads the chat + audio once, then sweeps (smooth_s x min_z) so the discovery
//! defaults can be tuned against a real chat+audio VOD before the refine/UI
//! stages exist, then dumps the ranked Moments for a chosen combo.

use std::path::PathBuf;
use yc_detect::{chat, loudness, rank_moments, score, DetectParams};

fn mmss(t: f64) -> String {
    let s = t.round() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn main() -> anyhow::Result<()> {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: detect_vod <vod_workdir>"));
    let wav = dir.join("analysis.wav");
    let chat_path = dir.join("chat.live_chat.json");

    let bin_s = 1.0;
    let t0 = std::time::Instant::now();
    let loud_raw = loudness::read_rms_bins(&wav, bin_s)?;
    let chat_counts = if chat_path.exists() {
        let offsets = chat::message_offsets(&chat_path)?;
        eprintln!("chat: {} viewer messages", offsets.len());
        Some(score::bin_counts(&offsets, bin_s, loud_raw.len()))
    } else {
        None
    };
    eprintln!(
        "read {} bins ({}) in {:.1}s\n",
        loud_raw.len(),
        mmss(loud_raw.len() as f64 * bin_s),
        t0.elapsed().as_secs_f64(),
    );

    // --- sweep: candidate count per (smooth_s, min_z) ---
    let smooths = [5.0, 10.0, 15.0];
    let min_zs = [0.75, 1.0, 1.25, 1.5];
    println!("candidate counts (rows=smooth_s, cols=min_z):");
    print!("        ");
    for z in min_zs {
        print!("z>={z:<6}");
    }
    println!();
    for smooth_s in smooths {
        print!("  {smooth_s:>3}s  ");
        for min_z in min_zs {
            let p = DetectParams { smooth_s, min_z, top_n: 9999, ..Default::default() };
            let n = rank_moments(&loud_raw, chat_counts.as_deref(), &p).len();
            print!("{n:<7}");
        }
        println!();
    }

    // --- dump the ranked Moments and flag which window covers a target event
    //     time (2nd arg, default 39:38 = 2378 s - the operator's jumpscare). The
    //     dump is loudness-only to match the GUI local-file (no-chat) test. ---
    let target_s: f64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(2378.0);
    let chosen = DetectParams { smooth_s: 10.0, min_z: 1.0, ..Default::default() };
    let moments = rank_moments(&loud_raw, None, &chosen);
    println!(
        "\ntop {} moments (loudness-only) at smooth_s={} min_z={}; '*' covers {}:",
        moments.len(),
        chosen.smooth_s,
        chosen.min_z,
        mmss(target_s),
    );
    println!("    #   start    end     score    loud");
    for m in &moments {
        let covers = if m.range.start_s <= target_s && target_s < m.range.end_s { "*" } else { " " };
        let f = |o: Option<f32>| o.map(|v| format!("{v:6.2}")).unwrap_or_else(|| "     -".into());
        println!(
            "  {} {:>2}  {:>6}  {:>6}  {:>6.2}  {}",
            covers,
            m.id,
            mmss(m.range.start_s),
            mmss(m.range.end_s),
            m.score,
            f(m.signals.loudness),
        );
    }
    Ok(())
}
