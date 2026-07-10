//! Forced-alignment PARITY harness (ADR 0054): proves the Rust port reproduces
//! the spike-validated Python pipeline on the spike's exact inputs (clip 3's
//! mixed 16 kHz audio + the operator-approved 111-word sequence), before the
//! production wiring is trusted. Reference artifacts come from
//! `export_align_onnx.py` (the ONNX export + reference dump).
//!
//!   cargo run -p yt-clipper --features align --example align_parity -- \
//!     emission <align_ref.json> <emission_ort.f32>     # Viterbi-only, no ort
//!   cargo run -p yt-clipper --features align --example align_parity -- \
//!     full <align_ref.json> <clip3_16k.wav> [models/w2v2-align-id]
//!
//! `emission` isolates the Viterbi/tokenize port on the exact emission Python
//! saw (span-for-span equality expected). `full` runs the shipped chain (ort
//! session -> log-softmax -> Viterbi) and compares per-word onsets to Python's
//! (<= one 20 ms frame expected).

use std::path::PathBuf;

use yc_core::TimeRange;
use yc_transcribe::align;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut a = std::env::args().skip(1);
    let mode = a.next().expect("mode: emission | full");
    let ref_path = PathBuf::from(a.next().expect("align_ref.json path"));
    let refj: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&ref_path)?)?;

    let words: Vec<String> = refj["words"]
        .as_array()
        .expect("words")
        .iter()
        .map(|v| v.as_str().expect("word").to_string())
        .collect();
    let py_onsets: Vec<Option<f64>> =
        refj["onsets_ort"].as_array().expect("onsets_ort").iter().map(|v| v.as_f64()).collect();
    let frames = refj["T"].as_u64().expect("T") as usize;
    let vocab_n = refj["V"].as_u64().expect("V") as usize;
    let dur_s = refj["dur_s"].as_f64().expect("dur_s");
    println!("ref: {} words, {frames} frames x {vocab_n} vocab, {dur_s:.2}s", words.len());

    // The vocab the model ships with (models/w2v2-align-id/vocab.json).
    let vocab_path = PathBuf::from("models/w2v2-align-id/vocab.json");
    let vocab = align::AlignVocab::parse(&std::fs::read_to_string(&vocab_path)?)?;
    anyhow::ensure!(
        vocab.blank as u64 == refj["blank_id"].as_u64().unwrap()
            && vocab.delim as u64 == refj["delim_id"].as_u64().unwrap(),
        "vocab blank/delim disagree with the reference dump"
    );

    // Tokenization must reproduce the spike's targets exactly.
    let (targets, widx) = align::tokenize_words(&words, &vocab);
    let py_targets: Vec<u32> = refj["targets"]
        .as_array()
        .expect("targets")
        .iter()
        .map(|v| v.as_u64().expect("target id") as u32)
        .collect();
    let py_widx: Vec<i32> = refj["widx"]
        .as_array()
        .expect("widx")
        .iter()
        .map(|v| v.as_i64().expect("widx") as i32)
        .collect();
    anyhow::ensure!(targets == py_targets, "targets diverge from the spike tokenization");
    anyhow::ensure!(widx == py_widx, "widx diverges from the spike tokenization");
    println!("tokenize: {} targets — matches the spike exactly", targets.len());

    let rust_spans_s: Vec<Option<(f64, f64)>> = match mode.as_str() {
        "emission" => {
            let em_path = PathBuf::from(a.next().expect("emission_ort.f32 path"));
            let bytes = std::fs::read(&em_path)?;
            anyhow::ensure!(bytes.len() == frames * vocab_n * 4, "emission size mismatch");
            let emission: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            // Span-for-span check against torchaudio's merge_tokens output.
            let spans =
                align::forced_align_spans(&emission, frames, vocab_n, &targets, vocab.blank)?;
            let py_spans: Vec<(usize, usize)> = refj["spans_on_ort_emission"]
                .as_array()
                .expect("spans")
                .iter()
                .map(|v| {
                    let p = v.as_array().expect("span pair");
                    (p[0].as_u64().unwrap() as usize, p[1].as_u64().unwrap() as usize)
                })
                .collect();
            anyhow::ensure!(spans.len() == py_spans.len(), "span count differs");
            let mut off = 0usize;
            let mut worst = 0usize;
            for (i, (r, p)) in spans.iter().zip(&py_spans).enumerate() {
                if r != p {
                    off += 1;
                    let d = r.0.abs_diff(p.0).max(r.1.abs_diff(p.1));
                    worst = worst.max(d);
                    if off <= 10 {
                        println!("  span {i} target {}: rust {:?} vs py {:?}", targets[i], r, p);
                    }
                }
            }
            println!(
                "viterbi spans: {}/{} identical to torchaudio, {off} differ (worst {worst} frames)",
                spans.len() - off,
                spans.len()
            );
            let spf = dur_s / frames as f64;
            align::word_frame_spans(&spans, &widx, words.len())
                .into_iter()
                .map(|o| o.map(|(x, y)| (x as f64 * spf, y as f64 * spf)))
                .collect()
        }
        "full" => {
            let wav = PathBuf::from(a.next().expect("wav path"));
            let model_dir =
                a.next().map(PathBuf::from).unwrap_or_else(|| "models/w2v2-align-id".into());
            let samples = yc_ingest::read_range_samples(
                &wav,
                TimeRange { start_s: 0.0, end_s: f64::MAX },
            )?;
            println!("wav: {} samples ({:.2}s)", samples.len(), samples.len() as f64 / 16000.0);
            let mut aligner = align::Aligner::load(&model_dir)?;
            let t0 = std::time::Instant::now();
            let out = aligner.align_words(&words, &samples, yc_ingest::WHISPER_SR)?;
            println!("aligner: emission + viterbi in {:.1}s", t0.elapsed().as_secs_f64());
            out
        }
        other => anyhow::bail!("unknown mode {other:?} (emission | full)"),
    };

    // Per-word onset comparison vs the Python reference.
    let mut worst = 0.0f64;
    let mut worst_i = 0usize;
    let mut n_diff = 0usize;
    let mut missing_mismatch = 0usize;
    for (i, (r, p)) in rust_spans_s.iter().zip(&py_onsets).enumerate() {
        match (r, p) {
            (Some((rs, _)), Some(ps)) => {
                let d = (rs - ps).abs();
                if d > worst {
                    worst = d;
                    worst_i = i;
                }
                if d > 1e-9 {
                    n_diff += 1;
                }
            }
            (None, None) => {}
            _ => missing_mismatch += 1,
        }
    }
    println!(
        "onsets vs python: worst |delta| {:.4}s (word {:?}), {} words differ at all, {} aligned/unaligned mismatches",
        worst, words.get(worst_i).map(String::as_str).unwrap_or("?"), n_diff, missing_mismatch
    );

    // The 8 ground-truth words the ADR 0053 spike was judged on.
    const GT: &[(&str, f64)] = &[
        ("siapa", 5.0), ("otot", 6.5), ("kreatin", 8.5), ("sdc", 23.0),
        ("gue", 28.0), ("fadil", 32.0), ("pinguin", 52.0), ("jalanannya", 54.0),
    ];
    println!("\nword         gt      rust-onset");
    for (word, gt) in GT {
        let best = words
            .iter()
            .zip(&rust_spans_s)
            .filter(|(w, s)| w.as_str() == *word && s.is_some())
            .map(|(_, s)| s.unwrap().0)
            .min_by(|a, b| (a - gt).abs().total_cmp(&(b - gt).abs()));
        match best {
            Some(o) => println!("{word:<12} {gt:5.1}   {o:6.2} ({:+.2})", o - gt),
            None => println!("{word:<12} {gt:5.1}   MISSING"),
        }
    }
    let tol = dur_s / frames as f64 + 1e-9; // one frame
    anyhow::ensure!(
        worst <= tol && missing_mismatch == 0,
        "PARITY FAIL: worst onset delta {worst:.4}s exceeds one frame ({tol:.4}s)"
    );
    println!("\nPARITY OK (every onset within one frame of the Python reference)");
    Ok(())
}
