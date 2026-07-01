//! Caption-timing inspector (ADR 0013). Replays the REAL caption path over a wav
//! range - whisper large-v3 + DTW (`transcribe_range`, exactly as `do_render`),
//! then the real `refine_caption_timing_traced` - and reports raw whisper units
//! (flagging non-monotonic DTW onsets), the RMS envelope distribution, and a
//! per-word verdict (dropped vs kept, each kept word's on-screen duration + what
//! bounded it). The feedback loop ADR 0013's gap-fill decision was made and verified
//! against; kept for future caption-timing work.
//!
//!   scripts\cargo-cuda.bat run -p yt-clipper --example caption_diag -- workspace\9-X80Ozwo1I\analysis.wav 2049.5 2079.5 id
//!
//! Everything comes from the real functions: the per-word verdict is read straight
//! off `refine_caption_timing_traced` (the render's own keep/drop + re-timing
//! decision, one entry per unit), NOT reconstructed from the output timing. The
//! onset clamp (ADR 0019) can push a kept word's start onto the next word's onset,
//! which made the old output-matching heuristic misreport it as DROP and desync
//! every word after it (that was this tool's bug). The only mirrored numbers now are
//! the MIN_READ/MAX_HOLD labels that *describe* each kept word's limiter - they
//! cannot change the kept/dropped verdict, which is the trace's.

use std::path::PathBuf;
use yc_core::Language;
use yc_ingest::{read_range_samples, WHISPER_SR};

// Labels only (mirror ass.rs for the limiter / silence-drop columns; not
// load-bearing - they describe each verdict, they don't recompute it).
const MIN_READ_S: f64 = 0.40;
const MAX_HOLD_S: f64 = 1.2;
const ENV_HOP_S: f64 = 0.02;
const ENV_WIN_S: f64 = 0.04;
const SILENCE_DROP_FRAC: f32 = 0.10;
const SILENCE_DROP_ABS: f32 = 0.006; // ADR 0021 absolute floor

fn ts(t: f64) -> String {
    format!("{:6.2}", t)
}

fn main() -> anyhow::Result<()> {
    // Same subscriber as the app: without one, every tracing event — the dialect
    // store line, the caption decode config, whisper.cpp's own hooked logs — is
    // silently dropped, and the inspector hides exactly the context it exists to
    // show. Logs go to stderr; the report stays clean on stdout.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
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

    // Dialect store (priming + post-correction) - the LAYERED path do_render takes
    // (ADR 0031): base < per-Creator < per-clip. Derived from the wav's location
    // (`<workspace>/<creator>/<stream>/data/analysis.wav`) so the units reflect the
    // real render's corrections; the bundled base is now generic (0 corrections), so
    // plain `load` would show none. (Was `load` — drifted from do_render since ADR 0031.)
    let base_dir = PathBuf::from("assets/dialect");
    let lc = match lang {
        Language::En => "en",
        Language::Ja => "ja",
        Language::Id => "id",
    };
    let mut overlays: Vec<PathBuf> = Vec::new();
    if let Some(stream_dir) = wav.parent().and_then(|d| d.parent()) {
        if let Some(creator_dir) = stream_dir.parent() {
            overlays.push(creator_dir.join(format!("{lc}.json"))); // per-Creator
        }
        if let Ok(rd) = std::fs::read_dir(stream_dir) {
            let suffix = format!(".{lc}.json");
            for e in rd.flatten() {
                let p = e.path();
                if p.file_name().and_then(|n| n.to_str()).map(|n| n.ends_with(&suffix)).unwrap_or(false)
                {
                    overlays.push(p); // per-clip
                }
            }
        }
    }
    let lexicon = yc_transcribe::DialectLexicon::load_layered(&base_dir, &overlays, lang);
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
            harvest
                .iter()
                .map(|c| format!("{}({:.2}@{:.1}s)", c.word, c.confidence, c.start_s))
                .collect::<Vec<_>>()
                .join(", ")
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

    // 2) RMS envelope distribution (context for calibrating SILENCE_DROP_ABS; the
    // actual drop bar the render applies is reported with the verdict below, straight
    // from the trace - not recomputed here).
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

    // 3) Per-word verdict, read straight off the REAL decision.
    // `refine_caption_timing_traced` returns one outcome per raw unit, IN ORDER - the
    // render's own keep/drop + re-timing - so the verdict is the trace itself, never a
    // reconstruction from output timing. (The old loop matched the kept *subsequence*
    // back onto the raw units by an "output start < next onset" window; the onset
    // clamp can push a kept word's start onto the next onset, so that window failed on
    // closely-spaced / clamped units - a false DROP, then a desync of every word
    // after. Reading the 1:1 trace removes the whole class of error.)
    let trace = yc_render::refine_caption_timing_traced(&raw, &samples, sr);
    let clip_end = samples.len() as f64 / sr as f64;
    // The trace's own thresholds (the render's, not mirrored): the effective drop bar
    // and its relative component, used only to annotate the drop lines below.
    let trace_rel_drop = SILENCE_DROP_FRAC * trace.loud_ref;
    println!("\n--- refine verdict (real refine_caption_timing_traced) ---");
    println!(
        "  (drop bar {:.4} = min(rel {:.4} = {:.2}*loud_ref {:.4}, abs {:.4}); show = clamped start; lead = forward clamp, ADR 0019)",
        trace.silence_drop, trace_rel_drop, SILENCE_DROP_FRAC, trace.loud_ref, SILENCE_DROP_ABS
    );
    let (mut kept, mut dropped, mut flash_with_room, mut held) = (0, 0, 0, 0);
    let (mut clamped, mut total_lead, mut max_lead) = (0, 0.0_f64, 0.0_f64);
    let mut max_dur = 0.0_f64;
    for (idx, (u, outcome)) in raw.units.iter().zip(&trace.outcomes).enumerate() {
        let next = raw.units.get(idx + 1).map(|n| n.start_s).unwrap_or(clip_end);
        let room = next - u.start_s;
        match *outcome {
            yc_render::UnitOutcome::Dropped { peak } => {
                dropped += 1;
                // Flag a word the relative bar drops but the abs floor would keep -
                // quiet-but-present speech. (Only possible if SILENCE_DROP_ABS is ever
                // raised above the relative bar; the render's bar is the min of the two,
                // so this stays empty today - it is a tripwire, not a live case.)
                let rescue = if peak >= SILENCE_DROP_ABS && peak < trace_rel_drop {
                    "  <- abs floor KEEPS (quiet speech)"
                } else {
                    ""
                };
                println!(
                    " DROP {:>3} [{}] peak {:.4} (bar {:.4}){}  {}",
                    idx, ts(u.start_s), peak, trace.silence_drop, rescue, u.text
                );
            }
            yc_render::UnitOutcome::Kept { start_s, end_s } => {
                kept += 1;
                let lead = start_s - u.start_s;
                if lead > 1e-3 {
                    clamped += 1;
                    total_lead += lead;
                    max_lead = max_lead.max(lead);
                }
                let dur = end_s - start_s;
                max_dur = max_dur.max(dur);
                let limiter = if (end_s - next).abs() < 1e-3 {
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
                    idx, ts(u.start_s), ts(start_s), ts(end_s), lead, dur, room, limiter, u.text
                );
            }
        }
    }

    println!("\n--- summary ---");
    println!("raw units:                 {}", raw.units.len());
    // kept + dropped must equal raw units: the verdict is 1:1 with the input (one
    // trace outcome per unit). The old subsequence-matching loop could violate this
    // by desyncing on a clamped unit - printing the reconciliation makes it visible.
    println!(
        "kept / dropped:            {} / {}  (sum {} == raw {})",
        kept, dropped, kept + dropped, raw.units.len()
    );
    println!("flashes WITH room (<MIN_READ but room to show longer - should be 0): {}", flash_with_room);
    println!("held at MAX_HOLD:          {}", held);
    println!("longest on-screen (s):     {:.2}", max_dur);
    println!("onset-clamped (ADR 0019):  {} word(s); total lead corrected {:.2}s, max {:.2}s", clamped, total_lead, max_lead);
    println!("non-monotonic DTW starts:  {}", nonmono);
    Ok(())
}
