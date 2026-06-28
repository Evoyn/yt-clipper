//! Caption-timing inspector (ADR 0013). Replays the REAL caption path over a wav
//! range - whisper large-v3 + DTW (`transcribe_range`, exactly as `do_render`),
//! then the real `refine_caption_timing` - and reports raw whisper units (flagging
//! non-monotonic DTW onsets), the RMS envelope distribution, and a per-word verdict
//! (dropped vs kept, each kept word's on-screen duration + what bounded it). The
//! feedback loop ADR 0013's gap-fill decision was made and verified against; kept
//! for future caption-timing work.
//!
//!   scripts\cargo-cuda.bat run -p yt-clipper --example caption_diag -- workspace\9-X80Ozwo1I\analysis.wav 2049.5 2079.5 id
//!
//! Everything comes from the real functions - the only mirrored numbers are the
//! MIN_READ/MAX_HOLD labels used to *describe* each kept word's limiter (not to
//! recompute anything), so they cannot silently change the verdict.

use std::path::PathBuf;
use yc_core::Language;
use yc_ingest::{read_range_samples, WHISPER_SR};

// Labels only (mirror ass.rs for the limiter column; not load-bearing).
const MIN_READ_S: f64 = 0.40;
const MAX_HOLD_S: f64 = 1.2;
const ENV_HOP_S: f64 = 0.02;
const ENV_WIN_S: f64 = 0.04;

fn ts(t: f64) -> String {
    format!("{:6.2}", t)
}

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let wav = PathBuf::from(a.next().expect("usage: caption_diag <wav> <start_s> <end_s> [lang]"));
    let start_s: f64 = a.next().expect("start_s").parse()?;
    let end_s: f64 = a.next().expect("end_s").parse()?;
    let lang = match a.next().as_deref() {
        Some("en") => Language::En,
        Some("ja") => Language::Ja,
        _ => Language::Id,
    };
    let model = PathBuf::from("models/ggml-large-v3.bin");
    anyhow::ensure!(model.is_file(), "whisper model missing: {}", model.display());

    let samples = read_range_samples(&wav, yc_core::TimeRange { start_s, end_s })?;
    let sr = WHISPER_SR;
    println!(
        "=== caption_diag: {} [{:.1}-{:.1}s] {:?}  ({} samples, {:.1}s) ===",
        wav.display(), start_s, end_s, lang, samples.len(), samples.len() as f64 / sr as f64
    );

    // Dialect store (priming + post-correction) - the same path do_render takes.
    let lexicon = yc_transcribe::DialectLexicon::load(&PathBuf::from("assets/dialect"), lang);
    let prompt = lexicon.initial_prompt();
    let n_corr = lexicon.corrections.iter().filter(|c| !c.right.is_empty()).count();
    println!(
        "\ndialect: {} confirmed corrections | {} dict words | prime: {}",
        n_corr,
        lexicon.dictionary.len(),
        if !lexicon.prime {
            "(off)".to_string()
        } else if prompt.is_empty() {
            "(on, no vocab)".to_string()
        } else {
            format!("on \"{prompt}\"")
        }
    );

    // 1) RAW whisper (WITH DTW - exactly the render path: transcribe_range).
    eprintln!("[caption_diag] loading whisper + transcribing (GPU)...");
    let (raw, harvest) =
        yc_transcribe::transcribe_range_harvesting(&model, &samples, lang, &lexicon, || false)?;
    println!(
        "auto-harvest candidates ({}): {}",
        harvest.len(),
        if harvest.is_empty() {
            "(none)".to_string()
        } else {
            harvest.iter().map(|(w, c)| format!("{w}({c:.2})")).collect::<Vec<_>>().join(", ")
        }
    );
    println!("\n--- RAW whisper units ({}) - '!' = DTW start < previous (non-monotonic) ---", raw.units.len());
    let mut prev = -1.0_f64;
    let mut nonmono = 0;
    for (i, u) in raw.units.iter().enumerate() {
        let bang = if u.start_s < prev { nonmono += 1; "!" } else { " " };
        println!("{bang}{:>3} [{}-{}] dur {:>5.2}  {}", i, ts(u.start_s), ts(u.end_s), u.end_s - u.start_s, u.text);
        prev = u.start_s;
    }
    println!("(non-monotonic starts: {nonmono})");

    // 2) RMS envelope distribution (context for the silence-drop).
    let hop = ((sr as f64) * ENV_HOP_S) as usize;
    let win = ((sr as f64) * ENV_WIN_S) as usize;
    let mut env = Vec::new();
    let mut i = 0;
    while i < samples.len() {
        let e = (i + win).min(samples.len());
        let w = &samples[i..e];
        env.push((w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt());
        i += hop;
    }
    env.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| env[((env.len() as f64 * p) as usize).min(env.len() - 1)];
    println!("\n--- RMS envelope ({} frames) ---", env.len());
    println!(
        "p10 {:.4}  p25 {:.4}  p50 {:.4}  p75 {:.4}  p90 {:.4}  p95(loud_ref) {:.4}  max {:.4}",
        pct(0.10), pct(0.25), pct(0.50), pct(0.75), pct(0.90), pct(0.95), pct(1.0)
    );

    // 3) Per-word verdict from the REAL refine. refined is the kept subsequence of
    // raw (silence-drops removed) with each kept word's start possibly clamped
    // FORWARD to its acoustic onset (ADR 0019) - so align by walking refined in
    // order, matching a kept word when its (clamped) start sits in [raw onset, next
    // raw onset). `lead` is how far the clamp pushed the start past the DTW onset.
    let refined = yc_render::refine_caption_timing(raw.clone(), &samples, sr);
    let clip_end = samples.len() as f64 / sr as f64;
    println!("\n--- refine verdict (real refine_caption_timing) ---");
    println!("  (show = clamped start; lead = forward clamp applied to the DTW onset, ADR 0019)");
    let (mut kept, mut dropped, mut flash_with_room, mut held) = (0, 0, 0, 0);
    let (mut clamped, mut total_lead, mut max_lead) = (0, 0.0_f64, 0.0_f64);
    let mut max_dur = 0.0_f64;
    let mut j = 0usize; // pointer into refined.units (the kept subsequence)
    for (idx, u) in raw.units.iter().enumerate() {
        let next = raw.units.get(idx + 1).map(|n| n.start_s).unwrap_or(clip_end);
        let room = next - u.start_s;
        let matched = refined.units.get(j).filter(|r| r.start_s + 1e-6 >= u.start_s && r.start_s < next);
        match matched {
            None => {
                dropped += 1;
                println!(" DROP {:>3} [{}]  {}", idx, ts(u.start_s), u.text);
            }
            Some(r) => {
                j += 1;
                kept += 1;
                let lead = r.start_s - u.start_s;
                if lead > 1e-3 {
                    clamped += 1;
                    total_lead += lead;
                    max_lead = max_lead.max(lead);
                }
                let dur = r.end_s - r.start_s;
                max_dur = max_dur.max(dur);
                let limiter = if (r.end_s - next).abs() < 1e-3 {
                    "next"
                } else if (dur - MAX_HOLD_S).abs() < 1e-3 {
                    held += 1;
                    "MAX_HOLD"
                } else {
                    "min/other"
                };
                // A flash (<MIN_READ) is only a bug when there was room to show longer.
                if dur + 1e-3 < MIN_READ_S && room + 1e-3 >= MIN_READ_S {
                    flash_with_room += 1;
                }
                println!(
                    " keep {:>3} dtw[{}] show[{}-{}] lead +{:>4.2} dur {:>5.2} room {:>5.2} via {:<9} {}",
                    idx, ts(u.start_s), ts(r.start_s), ts(r.end_s), lead, dur, room, limiter, u.text
                );
            }
        }
    }

    println!("\n--- summary ---");
    println!("raw units:                 {}", raw.units.len());
    println!("kept / dropped:            {} / {}", kept, dropped);
    println!("flashes WITH room (<MIN_READ but room to show longer - should be 0): {}", flash_with_room);
    println!("held at MAX_HOLD:          {}", held);
    println!("longest on-screen (s):     {:.2}", max_dur);
    println!("onset-clamped (ADR 0019):  {} word(s); total lead corrected {:.2}s, max {:.2}s", clamped, total_lead, max_lead);
    println!("non-monotonic DTW starts:  {}", nonmono);
    Ok(())
}
