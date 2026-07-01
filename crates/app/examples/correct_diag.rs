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
use yc_core::{Language, Project, TimeRange};
use yc_ingest::read_range_samples;
use yc_transcribe::{
    apply_correction, build_correction_request, correction_topic, transcribe_range_full,
    CorrectionContext, DialectLexicon,
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

    // Load the LAYERED dialect store the way do_render does (ADR 0031): base <
    // per-Creator < per-clip, derived from the wav's location
    // (`<workspace>/<creator>/<stream>/data/analysis.wav`). The bundled base is now
    // generic (0 corrections since ADR 0031), so a plain `load` would see NO context
    // overrides and always print `build_correction_request: None` - the same
    // stale-loader lie caption_diag had (fixed 863af86), which would make this
    // inspector useless for validating a per-Creator curated override. (Was `load`.)
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
    let lexicon = DialectLexicon::load_layered(&base_dir, &overlays, lang);

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

    // Mirror do_render's topic EXACTLY (pipeline.rs `correct_captions`): the shared
    // `correction_topic` over the clip's generated title (absent for this inspector,
    // as in a headless render) + the REAL Creator + VOD title, read from the
    // project.json beside the wav. A topic that diverges from do_render's makes the
    // preview UNFAITHFUL: the topic string flips Qwen's decision on whisper's
    // doubled tokens (an earlier hardcoded topic applied `pancingan pancingan`
    // 2-of-2 vs the render's 1-of-2) - the preview must send the request the render
    // sends. A missing project.json (bare wav outside a stream folder) degrades to
    // an empty topic, printed below so the divergence is visible, never silent.
    let vod = wav
        .parent()
        .map(|d| d.join("project.json"))
        .and_then(|p| Project::load(&p).ok())
        .map(|p| p.vod);
    let topic = match &vod {
        Some(v) => correction_topic(None, &v.creator, &v.title),
        None => {
            eprintln!("[correct_diag] WARNING: no project.json beside the wav - empty topic (do_render would send the real Creator/stream)");
            String::new()
        }
    };
    println!("topic: {topic:?}");
    let ctx = CorrectionContext { language: lang, topic };

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
