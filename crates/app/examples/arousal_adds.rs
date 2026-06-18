//! Ground-truth the whole-VOD arousal "adds" (ADR 0008 discovery question):
//!   scripts\cargo-cuda.bat run -p yt-clipper --example arousal_adds -- workspace\ZSegfmsrYmE <start_s[,start_s..]> [dur_s]
//!
//! `arousal_scan`'s off-diagonal report lists the candidate ranges that
//! whole-VOD arousal would surface but loudness+chat never nominate. Whether a
//! discovery-arousal pass *helps* (real quiet reactions) or *hurts* (the model
//! firing on quiet clear speech) hinges on the PRECISION of those adds, which a
//! score alone can't reveal. This transcribes each range and excitement-lexicon
//! scores it so we can read whether the streamer is reacting or just talking.
//! Lives in the app crate (not yc-detect) so the fast no-GPU `test -p yc-detect`
//! path stays free of whisper/CUDA.

use std::path::PathBuf;
use yc_core::{Language, Project, TimeRange};

fn mmss(t: f64) -> String {
    let s = t.round() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(
        args.next().expect("usage: arousal_adds <workdir> <start_s[,start_s..]> [dur_s]"),
    );
    let starts: Vec<f64> = args
        .next()
        .expect("need start_s list")
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let dur_s: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30.0);

    let wav = dir.join("analysis.wav");
    let model = PathBuf::from("models/ggml-large-v3.bin");
    anyhow::ensure!(model.is_file(), "whisper model missing: {}", model.display());
    let lang = Project::load(&dir.join("project.json")).map(|p| p.vod.language).unwrap_or(Language::Id);

    let transcriber = yc_transcribe::Transcriber::load_text_only(&model)?;
    println!(
        "transcribing {} ranges (dur {}s, lang {:?}); '[d]' = excitement-lexicon density:\n",
        starts.len(),
        dur_s,
        lang
    );
    for start in starts {
        let range = TimeRange { start_s: start, end_s: start + dur_s };
        let samples = yc_ingest::read_range_samples(&wav, range)?;
        let t = transcriber.transcribe(&samples, lang, || false)?;
        let density = yc_detect::lexicon::density(&t, lang);
        let text = t.units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>().join(" ");
        println!("{:>6} [{:.3}]  {}", mmss(start), density, text.trim());
    }
    Ok(())
}
