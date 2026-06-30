//! LLM caption-correction spike + inspector (ADR 0030). Replays the REAL caption
//! transcription path over a wav range (whisper large-v3 + DTW + dialect dict,
//! exactly as `do_render`), prints each unit with its whisper confidence, then
//! either **builds** the correction request for the `yc-llm-judge --correct`
//! sidecar or **applies** the sidecar's reply and shows the before/after.
//!
//! Two-phase, because the corrector links llama (can't co-link whisper, ADR 0010):
//!
//!   # 1) build the request (writes <reqfile>):
//!   scripts\cargo-cuda.bat run -p yt-clipper --example correct_diag -- <wav> <start> <end> id <reqfile>
//!   # 2) run the sidecar (CUDA on PATH; run the exe directly - stdin via cargo was lossy):
//!   target\release\yc-llm-judge.exe --correct < <reqfile> > <respfile>
//!   # 3) apply the reply and inspect (re-transcribes; beam+temp0 is deterministic):
//!   scripts\cargo-cuda.bat run -p yt-clipper --example correct_diag -- <wav> <start> <end> id <reqfile> <respfile>
//!
//! The point is to measure the correction against the operator's ground-truth on
//! the production audio source BEFORE shipping (the enh-overclaim lesson) — off by
//! default until the operator A/Bs the real render.

use std::path::PathBuf;
use yc_core::{Language, TimeRange};
use yc_ingest::read_range_samples;
use yc_transcribe::{
    apply_correction, build_correction_request, transcribe_range_full, CorrectionContext,
    DialectLexicon,
};

const MODEL_GGUF: &str = "models/qwen2.5-7b-instruct-q5_k_m.gguf";

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let wav = PathBuf::from(a.next().expect("usage: correct_diag <wav> <start_s> <end_s> [lang] [reqfile] [respfile]"));
    let start_s: f64 = a.next().expect("start_s").parse()?;
    let end_s: f64 = a.next().expect("end_s").parse()?;
    let lang = match a.next().as_deref() {
        Some("en") => Language::En,
        Some("ja") => Language::Ja,
        _ => Language::Id,
    };
    let reqfile = a.next().map(PathBuf::from);
    let respfile = a.next().map(PathBuf::from);

    let model = PathBuf::from("models/ggml-large-v3.bin");
    anyhow::ensure!(model.is_file(), "whisper model missing: {}", model.display());
    let dialect_dir = PathBuf::from("assets/dialect");
    let lexicon = DialectLexicon::load(&dialect_dir, lang);

    let samples = read_range_samples(&wav, TimeRange { start_s, end_s })?;
    eprintln!("[correct_diag] transcribing {:.1}-{:.1}s ({:.1}s) on GPU...", start_s, end_s, (end_s - start_s));
    let (transcript, conf, _harvest) =
        transcribe_range_full(&model, &samples, lang, &lexicon, || false)?;
    let units = transcript.units;

    println!("=== correct_diag: {} [{:.1}-{:.1}s] {:?} — {} units ===", wav.display(), start_s, end_s, lang, units.len());
    let n_overrides = lexicon.context_overrides().len();
    let n_corr = lexicon.corrections.iter().filter(|c| !c.right.is_empty() && !c.context).count();
    println!("dialect: {n_corr} global corrections, {n_overrides} context overrides\n");
    println!("--- units (conf shown for diagnostics; the pass applies only curated context overrides) ---");
    for (i, u) in units.iter().enumerate() {
        let c = conf.get(i).copied().unwrap_or(0.0);
        let mark = if c < 0.50 { "[?]" } else { "   " };
        println!("{:>3} {:.2} {} {}", i + 1, c, mark, u.text);
    }
    println!();

    let ctx = CorrectionContext {
        language: lang,
        topic: "Indonesian horror-game live stream; streamer Ino with guest Guntur (@guntur69)".into(),
    };

    // APPLY mode: a response file is present -> apply it and show the diff.
    if let Some(resp) = respfile.as_ref().filter(|p| p.is_file()) {
        let raw = std::fs::read_to_string(resp)?;
        println!("--- raw model reply ---\n{}\n", raw.trim());
        let mut corrected = units.clone();
        let stats = apply_correction(&mut corrected, &lexicon, &raw);
        println!("--- correction stats ---\n{}\n", stats.summary());
        println!("--- diff (only changed) ---");
        // The pass preserves unit count + timing, so units and corrected align 1:1.
        for (i, (u, c)) in units.iter().zip(corrected.iter()).enumerate() {
            if u.text != c.text {
                println!("{:>3} CHANGE  {:>18}  ->  {}", i + 1, u.text, c.text);
            }
        }
        println!("\nbefore: {}", units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>().join(" "));
        println!("after:  {}", corrected.iter().map(|u| u.text.as_str()).collect::<Vec<_>>().join(" "));
        return Ok(());
    }

    // BUILD mode: write the sidecar request JSON.
    match build_correction_request(&units, &lexicon, &ctx) {
        None => println!("build_correction_request: None (no curated context overrides)"),
        Some(req) => {
            println!("--- system ---\n{}\n", req.system);
            println!("--- user ---\n{}\n", req.user);
            if let Some(path) = reqfile {
                let json = serde_json::json!({
                    "model_path": MODEL_GGUF,
                    "system": req.system,
                    "user": req.user,
                });
                // BOM-less UTF-8; serde handles the escaping the manual rig couldn't.
                std::fs::write(&path, serde_json::to_string(&json)?)?;
                println!("wrote request -> {}", path.display());
                println!("next: target\\release\\yc-llm-judge.exe --correct < {} > <respfile>", path.display());
            }
        }
    }
    Ok(())
}
