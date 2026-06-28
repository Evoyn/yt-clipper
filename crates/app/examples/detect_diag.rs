//! Detection pre-roll inspector (ADR 0020). Runs the REAL `discover` (chat +
//! loudness, no GPU/whisper) over a VOD's analysis wav + chat replay and reports
//! each candidate Moment's window, its loudness/chat z-scores, and the
//! signal-aware pre-roll lead applied — so the loud-driven (longer-lead) vs
//! chat-driven (base-lead) split is visible on real data. Cheap: seconds, no model.
//!
//!   scripts\cargo-cuda.bat run -p yt-clipper --example detect_diag -- "workspace\<creator>\<stream>\data\analysis.wav" "workspace\...\data\chat.live_chat.json"
//!
//! The lead column is recomputed with the real `score::peak_lead_s` from each
//! Moment's stored loudness z — the exact value `rank_moments` used for its range.

use std::path::PathBuf;
use yc_detect::{score, DetectParams};

fn clock(t: f64) -> String {
    let s = t.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let wav = PathBuf::from(a.next().expect("usage: detect_diag <analysis.wav> [chat.json]"));
    let chat = a.next().map(PathBuf::from);
    anyhow::ensure!(wav.is_file(), "analysis wav missing: {}", wav.display());

    let params = DetectParams::default();
    println!(
        "=== detect_diag: {} ===\nlead_s {} -> loud_lead_s {} (full at loudness z {}), dur_s {}, min_z {}",
        wav.display(),
        params.lead_s,
        params.loud_lead_s,
        params.loud_lead_full_z,
        params.dur_s,
        params.min_z,
    );
    let moments = yc_detect::discover(chat.as_deref(), &wav, &params)?;
    println!("\n  id  window           dur   loud_z  chat_z   lead  driver");
    let mut loud_driven = 0;
    let mut max_lead = 0.0_f64;
    for m in &moments {
        let loud_z = m.signals.loudness.unwrap_or(0.0);
        let chat_z = m.signals.chat_rate.unwrap_or(0.0);
        let lead = score::peak_lead_s(
            loud_z,
            params.min_z,
            params.loud_lead_full_z,
            params.lead_s,
            params.loud_lead_s,
        );
        max_lead = max_lead.max(lead);
        let driver = if lead > params.lead_s + 0.25 {
            loud_driven += 1;
            "LOUD"
        } else {
            "chat/base"
        };
        println!(
            "  #{:<3} {:>6}-{:<6} {:>4.0}s  {:>6.2}  {:>6.2}  {:>4.1}  {}",
            m.id,
            clock(m.range.start_s),
            clock(m.range.end_s),
            m.range.end_s - m.range.start_s,
            loud_z,
            chat_z,
            lead,
            driver
        );
    }
    println!(
        "\nloud-driven (lead > base {}s): {} / {};  longest pre-roll {:.1}s",
        params.lead_s,
        loud_driven,
        moments.len(),
        max_lead
    );
    Ok(())
}
