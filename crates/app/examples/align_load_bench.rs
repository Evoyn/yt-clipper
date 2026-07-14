//! Measure what a per-clip `Aligner::load` actually costs (perf slice,
//! 2026-07-14). `forced_align_fusion` rebuilds the wav2vec2-CTC ort session on
//! EVERY clip (`ensemble.rs`), and the model is 1.2 GB on disk — but the
//! residency fix is only worth its machinery if the load is a real slice of the
//! per-clip budget. Times three cold-ish loads plus one alignment so the
//! load-vs-inference split is evidence, not a guess.
//!
//!   scripts\cargo-cuda.bat run -p yt-clipper --example align_load_bench --features align

use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let dir = PathBuf::from("models/w2v2-align-id");
    anyhow::ensure!(
        dir.join("model.onnx").is_file(),
        "run from the repo root (models/w2v2-align-id/model.onnx not found)"
    );
    let mb = dir.join("model.onnx").metadata()?.len() as f64 / (1024.0 * 1024.0);
    println!("model: {} ({mb:.0} MB)", dir.display());

    let mut loads = Vec::new();
    for i in 1..=3 {
        let t = Instant::now();
        let aligner = yc_transcribe::align::Aligner::load(&dir)?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("load #{i}: {ms:.0} ms");
        loads.push(ms);
        // Drop between loads: this is exactly what today's per-clip path does.
        drop(aligner);
    }
    let mean = loads.iter().sum::<f64>() / loads.len() as f64;

    // One alignment on a short synthetic clip, to size the load against the
    // work it precedes (2 s of 16 kHz silence + a couple of words is enough —
    // we are timing the session, not judging the alignment).
    let mut aligner = yc_transcribe::align::Aligner::load(&dir)?;
    let samples = vec![0.0f32; 16_000 * 2];
    let words: Vec<String> = ["halo", "dunia"].iter().map(|s| s.to_string()).collect();
    let t = Instant::now();
    let _ = aligner.align_words(&words, &samples, 16_000);
    let infer_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("align_words on 2 s audio: {infer_ms:.0} ms");

    println!(
        "\nmean load {mean:.0} ms — a 10-clip batch pays {:.1} s on reloads alone",
        mean * 10.0 / 1000.0
    );
    Ok(())
}
