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
        "\nmean load {mean:.0} ms — a 10-clip batch USED to pay {:.1} s on reloads alone",
        mean * 10.0 / 1000.0
    );

    // The residency fix, on the production path: `forced_align_retime` is the
    // exact function the pipeline wires, and it now goes through the worker's
    // resident aligner. Clip 1 pays the load; clips 2..N must not.
    println!("\n--- residency (yc_transcribe::ensemble::forced_align_retime) ---");
    let units = vec![
        yc_core::CaptionUnit { text: "halo".into(), start_s: 0.0, end_s: 0.4 },
        yc_core::CaptionUnit { text: "dunia".into(), start_s: 0.5, end_s: 1.0 },
    ];
    let mut clip_ms = Vec::new();
    for clip in 1..=3 {
        let t = Instant::now();
        let out = yc_transcribe::ensemble::forced_align_retime(
            Some(&dir),
            yc_core::Language::Id,
            &units,
            &samples,
            16_000,
            2.0,
        );
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("clip #{clip}: {ms:.0} ms ({} units)", out.map(|u| u.len()).unwrap_or(0));
        clip_ms.push(ms);
    }
    // Now the worker goes idle — the app frees the 1.2 GB session.
    yc_transcribe::ensemble::release_resident_models();
    let t = Instant::now();
    let _ = yc_transcribe::ensemble::forced_align_retime(
        Some(&dir),
        yc_core::Language::Id,
        &units,
        &samples,
        16_000,
        2.0,
    );
    let after_release_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("after release_resident_models(): {after_release_ms:.0} ms (reloads, as intended)");

    let saved = clip_ms[0] - clip_ms[1];
    println!(
        "\nclip 1 {:.0} ms -> clip 2 {:.0} ms: the {saved:.0} ms load is paid ONCE per batch",
        clip_ms[0], clip_ms[1]
    );
    anyhow::ensure!(
        clip_ms[1] < clip_ms[0] / 2.0 && clip_ms[2] < clip_ms[0] / 2.0,
        "FAIL: clip 2/3 still paying the load — the aligner is not resident"
    );
    anyhow::ensure!(
        after_release_ms > clip_ms[1] * 2.0,
        "FAIL: release_resident_models() did not free the session"
    );
    println!("PASS: resident across a batch, freed on idle");
    Ok(())
}
