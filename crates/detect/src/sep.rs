//! Vocal-stem separation (htdemucs-ft **vocals** ONNX) via `ort` — the per-Clip
//! Vocal stem (CONTEXT.md): the streamer's voice split from a Segment's music /
//! game SFX, fed to transcription (captions) so whisper reads the voice above
//! the mix. It isolates *voice from music/SFX*, **not** one speaker from another
//! — an in-game / NPC voice survives it (diarization is a separate concern).
//!
//! Behind the `sep` cargo feature (pulls `ort`, the same runtime as `ser`). The
//! model does its own STFT, so the only host work is **chunk + overlap-add**.
//! Promoted from the `sep_spike` example once the in-process `ort` runtime was
//! validated; CPU execution provider (the SER precedent), CUDA EP a one-line
//! session change if a per-Clip export step ever needs it.

use anyhow::{Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;

const SR: u32 = 44_100;
const SEG: usize = 343_980; // model's fixed segment length, samples/channel
const CH: usize = 2;
const N_STEMS: usize = 4;
const VOCALS: usize = 3; // [drums, bass, other, vocals]

/// `ort::Error` holds raw pointers (not Send+Sync) — stringify it (as in arousal.rs).
fn oerr(e: ort::Error) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

/// Separate the **vocal stem** from a 44.1 kHz stereo wav into a 44.1 kHz stereo
/// vocals-only wav. Slides the model's fixed-length segment with 50% overlap and
/// a triangular window, overlap-adding the vocals stem (index 3 of
/// `[drums, bass, other, vocals]`). The host never touches the spectrogram — the
/// ONNX graph owns the STFT.
pub fn separate_vocals_wav(model: &Path, in_wav: &Path, out_wav: &Path) -> Result<()> {
    anyhow::ensure!(model.is_file(), "vocal-sep model missing: {}", model.display());

    // --- read input (44.1 kHz stereo) -> two channel vecs in [-1,1] ---
    let mut reader =
        hound::WavReader::open(in_wav).with_context(|| format!("opening {}", in_wav.display()))?;
    let spec = reader.spec();
    anyhow::ensure!(spec.channels == CH as u16, "expected stereo, got {}", spec.channels);
    anyhow::ensure!(spec.sample_rate == SR, "expected {SR} Hz, got {}", spec.sample_rate);
    let inter: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()?,
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
    };
    let n = inter.len() / CH;
    let mut left = vec![0.0f32; n];
    let mut right = vec![0.0f32; n];
    for i in 0..n {
        left[i] = inter[CH * i];
        right[i] = inter[CH * i + 1];
    }

    // --- load model on CPU (default EP, like the SER model) ---
    let t_load = std::time::Instant::now();
    let mut session = Session::builder()
        .and_then(|mut b| b.commit_from_file(model))
        .map_err(oerr)
        .with_context(|| format!("loading {}", model.display()))?;
    let input_name =
        session.inputs().first().map(|i| i.name().to_string()).context("no inputs")?;
    let output_name =
        session.outputs().first().map(|o| o.name().to_string()).context("no outputs")?;
    tracing::info!(
        "vocal-sep: loaded htdemucs in {:.1}s ({} samples/ch, {:.1}s @ {SR}Hz)",
        t_load.elapsed().as_secs_f32(),
        n,
        n as f32 / SR as f32
    );

    // --- chunk with 50% overlap, triangular weight, overlap-add ---
    let hop = SEG / 2;
    let win: Vec<f32> = (0..SEG)
        .map(|i| {
            let half = SEG as f32 / 2.0;
            1.0 - ((i as f32 - half) / half).abs()
        })
        .collect();
    let mut voc_l = vec![0.0f32; n];
    let mut voc_r = vec![0.0f32; n];
    let mut wsum = vec![0.0f32; n];

    let t_inf = std::time::Instant::now();
    let mut start = 0usize;
    while start < n {
        // [1,2,SEG], channel-major, zero-padded past the end.
        let mut data = vec![0.0f32; CH * SEG];
        for j in 0..SEG {
            let s = start + j;
            if s < n {
                data[j] = left[s];
                data[SEG + j] = right[s];
            }
        }
        let input = Tensor::from_array(([1_i64, CH as i64, SEG as i64], data)).map_err(oerr)?;
        let outputs = session.run(ort::inputs![input_name.as_str() => input]).map_err(oerr)?;
        let (_shape, out) =
            outputs[output_name.as_str()].try_extract_tensor::<f32>().map_err(oerr)?;
        anyhow::ensure!(out.len() == N_STEMS * CH * SEG, "unexpected output len {}", out.len());
        let voc0 = (VOCALS * CH) * SEG;
        let voc1 = (VOCALS * CH + 1) * SEG;
        for j in 0..SEG {
            let s = start + j;
            if s < n {
                let w = win[j];
                voc_l[s] += w * out[voc0 + j];
                voc_r[s] += w * out[voc1 + j];
                wsum[s] += w;
            }
        }
        start += hop;
    }
    let dt = t_inf.elapsed().as_secs_f32();
    tracing::info!(
        "vocal-sep: separated {:.1}s in {dt:.1}s ({:.2}x realtime)",
        n as f32 / SR as f32,
        (n as f32 / SR as f32) / dt
    );

    // --- normalize by accumulated weight, write stereo s16 ---
    let mut writer = hound::WavWriter::create(
        out_wav,
        hound::WavSpec {
            channels: CH as u16,
            sample_rate: SR,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .with_context(|| format!("creating {}", out_wav.display()))?;
    for s in 0..n {
        let w = if wsum[s] > 1e-6 { wsum[s] } else { 1.0 };
        let l = (voc_l[s] / w).clamp(-1.0, 1.0);
        let r = (voc_r[s] / w).clamp(-1.0, 1.0);
        writer.write_sample((l * 32767.0) as i16)?;
        writer.write_sample((r * 32767.0) as i16)?;
    }
    writer.finalize()?;
    Ok(())
}
