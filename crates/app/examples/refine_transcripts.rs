//! M5 wrong-speaker gate (ADR 0009): dump the REAL refine candidate set with
//! each candidate's transcript, so we can classify whether whisper's per-candidate
//! text is the streamer's speech or the game's *before* committing to the LLM
//! judgment Signal. The LLM never hears audio - it only ever reads this text - so
//! its whole value rides on these transcripts representing the streamer.
//!
//!   scripts\cargo-cuda.bat run -p yt-clipper --example refine_transcripts --features ser -- workspace\ZSegfmsrYmE
//!
//! Mirrors `pipeline.rs::do_detect`'s discovery exactly (chat + loudness,
//! DetectParams::default, top_n=25) so the dumped set IS the population the LLM
//! would see. Lives in the app crate (like `arousal_adds`) so it has whisper;
//! the no-GPU `test -p yc-detect` path stays free of whisper/CUDA.

use std::path::PathBuf;
use yc_core::{Language, Project};
use yc_detect::{chat, lexicon, loudness, rank_moments, score, DetectParams};

fn mmss(t: f64) -> String {
    let s = t.round() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn fsig(o: Option<f32>) -> String {
    o.map(|v| format!("{v:5.2}")).unwrap_or_else(|| "    -".into())
}

fn main() -> anyhow::Result<()> {
    let dir = PathBuf::from(
        std::env::args().nth(1).expect("usage: refine_transcripts <vod_workdir>"),
    );
    let wav = dir.join("analysis.wav");
    let chat_path = dir.join("chat.live_chat.json");
    let model = PathBuf::from("models/ggml-large-v3.bin");
    anyhow::ensure!(model.is_file(), "whisper model missing: {}", model.display());
    let lang =
        Project::load(&dir.join("project.json")).map(|p| p.vod.language).unwrap_or(Language::Id);

    // The REAL discovery candidate set: chat + loudness, default params (top_n=25)
    // - identical to do_detect, so this is the exact population the LLM would judge.
    let params = DetectParams::default();
    let loud_raw = loudness::read_rms_bins(&wav, params.bin_s)?;
    let chat_counts = if chat_path.exists() {
        let offsets = chat::message_offsets(&chat_path)?;
        eprintln!("chat: {} viewer messages", offsets.len());
        Some(score::bin_counts(&offsets, params.bin_s, loud_raw.len()))
    } else {
        eprintln!("WARNING: no chat.live_chat.json - candidate set is loudness-only, NOT the real refine population");
        None
    };
    let moments = rank_moments(&loud_raw, chat_counts.as_deref(), &params);
    eprintln!("discovered {} candidates (lang {:?})\n", moments.len(), lang);

    // Transcribe each candidate once, text-only (exactly as refine does).
    let transcriber = yc_transcribe::Transcriber::load_text_only(&model)?;
    let mut texts = Vec::with_capacity(moments.len());
    let mut densities = Vec::with_capacity(moments.len());
    for (i, m) in moments.iter().enumerate() {
        eprintln!("transcribing {}/{} ({})", i + 1, moments.len(), mmss(m.range.start_s));
        let samples = yc_ingest::read_range_samples(&wav, m.range)?;
        let t = transcriber.transcribe(&samples, lang, || false)?;
        densities.push(lexicon::density(&t, lang));
        texts.push(t.units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>().join(" "));
    }
    drop(transcriber); // free VRAM before SER (mirrors the pipeline)

    // Arousal context (ser only): the dangerous anti-signal is HIGH-arousal +
    // game-dramatic text (a scripted cutscene line the LLM would rate clip-worthy).
    // Raw max-pooled arousal here (not z-scored) - we just want to spot the highs.
    #[cfg(feature = "ser")]
    let arousals: Vec<f32> = {
        use yc_detect::arousal;
        let smodel = PathBuf::from("models/w2v2-emotion/model.onnx");
        if smodel.is_file() {
            let sr = yc_ingest::WHISPER_SR as f64;
            let win = (arousal::WINDOW_S * sr) as usize;
            let hop = (arousal::HOP_S * sr) as usize;
            let mut ser = arousal::Ser::load(&smodel)?;
            let mut v = Vec::with_capacity(moments.len());
            for m in &moments {
                let samples = yc_ingest::read_range_samples(&wav, m.range)?;
                v.push(ser.arousal_max(&samples, win, hop)?);
            }
            v
        } else {
            eprintln!("[ser] {} missing - arousal column blank", smodel.display());
            Vec::new()
        }
    };
    #[cfg(not(feature = "ser"))]
    let arousals: Vec<f32> = Vec::new();

    // One block per candidate: signals, then the transcript to classify.
    println!(
        "=== {} refine candidates (discovery rank). Classify each transcript:",
        moments.len()
    );
    println!("=== Streamer / Game-dramatic / Game-mundane / Mixed / Empty-nonlexical");
    println!("=== gate: streamer >=70% AND game-dramatic <=15% -> build M5; streamer <=50% OR game-dramatic >=30% -> vocal separation first\n");
    for (i, m) in moments.iter().enumerate() {
        let arou = arousals.get(i).map(|a| format!("{a:.3}")).unwrap_or_else(|| "-".into());
        println!(
            "#{:<2} {:>6}-{:<6}  chat z {} | loud z {} | lex {:.3} | arousal_raw {}",
            m.id,
            mmss(m.range.start_s),
            mmss(m.range.end_s),
            fsig(m.signals.chat_rate),
            fsig(m.signals.loudness),
            densities[i],
            arou,
        );
        let text = texts[i].trim();
        println!("    {}\n", if text.is_empty() { "(no speech transcribed)" } else { text });
    }
    Ok(())
}
