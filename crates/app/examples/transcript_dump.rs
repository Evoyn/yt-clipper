//! Full-precision transcript dump — the ADR 0072 bar A3 identity instrument.
//!
//! Runs the EXACT production `Transcriber` surface (caption DTW+beam load and
//! the detect refine's text-only greedy load) over a range of a real 16 kHz
//! analysis wav and dumps units + per-unit confidence + harvest candidates
//! with shortest-roundtrip float formatting. Byte-diffing this output between
//! the pre-sidecar build and the sidecar build IS the caption-identity proof:
//! the wire seam sits above `group_tokens`, so any lossy field in the wire
//! token shows up here as a diff.
//!
//! Usage: transcript_dump <analysis.wav> <caption|text> <start_s> <end_s> [en|id|ja]
//! Output: JSON-ish dump on stdout (deterministic; diff-friendly).

use anyhow::{Context, Result};
use yc_core::{Language, TimeRange};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let argv: Vec<String> = std::env::args().collect();
    let usage = "usage: transcript_dump <analysis.wav> <caption|text> <start_s> <end_s> [en|id|ja]";
    let wav = std::path::PathBuf::from(argv.get(1).context(usage)?);
    let mode = argv.get(2).context(usage)?.as_str();
    let start_s: f64 = argv.get(3).context(usage)?.parse().context("start_s")?;
    let end_s: f64 = argv.get(4).context(usage)?.parse().context("end_s")?;
    let language = match argv.get(5).map(|s| s.as_str()) {
        Some("en") => Language::En,
        Some("ja") => Language::Ja,
        _ => Language::Id,
    };

    // The model resolves like the app does: walk up from the exe to the repo
    // root (examples live in target/<profile>/examples).
    let exe = std::env::current_exe().context("current_exe")?;
    let root = exe
        .ancestors()
        .find(|p| p.join("models").is_dir())
        .context("no models/ dir above the example exe")?
        .to_path_buf();
    let model = root.join("models").join("ggml-large-v3.bin");

    let samples = yc_ingest::read_range_samples(&wav, TimeRange { start_s, end_s })?;
    eprintln!("transcript_dump: {} samples, mode={mode}", samples.len());

    let lexicon = yc_transcribe::DialectLexicon::default();
    let transcriber = match mode {
        "caption" => yc_transcribe::Transcriber::load(&model)?,
        "text" => yc_transcribe::Transcriber::load_text_only(&model)?,
        other => anyhow::bail!("mode must be caption|text, got {other}"),
    };
    let (transcript, conf, harvest) =
        transcriber.transcribe_full(&samples, language, &lexicon, || false)?;

    // serde_json + Debug both print shortest-roundtrip floats — byte-stable.
    println!("units = {}", serde_json::to_string_pretty(&transcript)?);
    println!("conf = {conf:?}");
    println!("harvest = {harvest:#?}");
    Ok(())
}
