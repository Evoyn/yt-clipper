//! Speech-emotion **arousal** Signal (ADR 0008): how emotionally *activated*
//! the streamer's voice is, used in refine to demote loud-but-flat moments
//! (game explosions, music, cutscenes) that loudness alone cannot tell apart
//! from a real reaction. Arousal axis only — a laugh, a rage, and a hype-moment
//! all score high; valence is deliberately not measured.
//!
//! A small CPU ONNX model (audeering `wav2vec2-large-robust-12-ft-emotion-msp-dim`)
//! scores 16 kHz mono audio — the exact format of `analysis.wav`, so no
//! resampling. Per candidate we slide a short window and **max-pool** the
//! arousal (peak-preserving, like the max-pool loudness of ADR 0007: a 2-second
//! scream surfaces rather than being averaged away), then z-score across the
//! candidate set and weight into `combined_score` exactly like the lexicon.
//!
//! The neural inference lives behind the `ser` cargo feature so the default
//! detection tests need neither the `ort` binary nor the model. The framing /
//! normalization / pooling / ranking below are pure and always compiled.

use crate::{combined_score, score, Weights};
use yc_core::Moment;

/// Sliding-window width over a candidate's audio, seconds.
pub const WINDOW_S: f64 = 4.0;
/// Hop between sliding windows, seconds.
pub const HOP_S: f64 = 2.0;

/// Zero-mean, unit-variance normalization of one audio window — what the
/// wav2vec2 feature extractor applies before inference. A flat (silent) window
/// has ~0 variance, so we guard the divisor.
pub fn normalize(samples: &[f32]) -> Vec<f32> {
    let n = samples.len();
    if n == 0 {
        return Vec::new();
    }
    let mean = samples.iter().copied().sum::<f32>() / n as f32;
    let var = samples.iter().map(|&x| (x - mean) * (x - mean)).sum::<f32>() / n as f32;
    let inv_std = 1.0 / (var + 1e-7).sqrt();
    samples.iter().map(|&x| (x - mean) * inv_std).collect()
}

/// Start indices of full-width `win`-sample windows hopping by `hop` across
/// `len` samples. The final window is snapped to end exactly at `len` so the
/// tail is covered without ever feeding the model a short, noisy partial. A
/// range shorter than one window yields a single window over the whole range.
pub fn window_starts(len: usize, win: usize, hop: usize) -> Vec<usize> {
    if len == 0 {
        return Vec::new();
    }
    if len <= win || hop == 0 {
        return vec![0];
    }
    let mut starts = Vec::new();
    let mut s = 0;
    while s + win <= len {
        starts.push(s);
        s += hop;
    }
    let last = len - win;
    if starts.last() != Some(&last) {
        starts.push(last);
    }
    starts
}

/// Fill the `arousal` Signal on candidates from their per-candidate arousal and
/// rerank — the analogue of [`crate::lexicon::apply`]. Arousal has no VOD-wide
/// baseline (the model is only run on candidates), so values are z-scored
/// across the candidate set, then `combined_score` recomputes each rank with
/// arousal now present. `arousals[i]` must correspond to `moments[i]`.
pub fn apply(moments: &mut [Moment], arousals: &[f32], weights: &Weights) {
    debug_assert_eq!(moments.len(), arousals.len(), "one arousal per Moment");
    let z = score::robust_z(arousals);
    for (m, az) in moments.iter_mut().zip(z) {
        m.signals.arousal = Some(az);
        m.score = combined_score(&m.signals, weights);
    }
}

/// Resident speech-emotion model scoring arousal on CPU. Loaded once per detect
/// run (like the resident Transcriber), then scores every candidate.
///
/// NOTE: the `ort` 2.0.0-rc.12 binding surface below (Session input/output
/// accessors, `Tensor::from_array` shape type, `try_extract_tensor` return
/// shape) is written from the documented API and is verified on the first
/// `--features ser` build — expect minor signature fixups there, isolated to
/// this block.
#[cfg(feature = "ser")]
pub use infer::Ser;

#[cfg(feature = "ser")]
mod infer {
    use super::{normalize, window_starts};
    use anyhow::{Context, Result};
    use ort::session::Session;
    use ort::value::Tensor;
    use std::path::Path;

    /// `ort::Error` holds raw pointers, so it is not `Send + Sync` and cannot be
    /// converted into `anyhow::Error` by `?`. Stringify it.
    fn oerr(e: ort::Error) -> anyhow::Error {
        anyhow::anyhow!("{e}")
    }

    pub struct Ser {
        session: Session,
        input_name: String,
        /// Name of the V/A/D logits output; arousal is index 0.
        logits_name: String,
    }

    impl Ser {
        /// Load the ONNX SER model on the CPU execution provider (default).
        pub fn load(onnx: &Path) -> Result<Self> {
            let session = Session::builder()
                .and_then(|mut b| b.commit_from_file(onnx))
                .map_err(oerr)
                .with_context(|| format!("loading SER model {}", onnx.display()))?;

            let input_name = session
                .inputs()
                .first()
                .map(|i| i.name().to_string())
                .ok_or_else(|| anyhow::anyhow!("SER model has no inputs"))?;

            // audonnx exports `hidden_states` (dim 1024) and `logits` (dim 3 =
            // arousal/dominance/valence). Prefer the name; else the 2nd output
            // (the model card reads predictions at index 1); else the first.
            let outputs_meta = session.outputs();
            let logits_name = outputs_meta
                .iter()
                .find(|o| o.name().to_lowercase().contains("logit"))
                .or_else(|| outputs_meta.get(1))
                .or_else(|| outputs_meta.first())
                .map(|o| o.name().to_string())
                .ok_or_else(|| anyhow::anyhow!("SER model has no outputs"))?;

            tracing::info!(
                input = %input_name,
                logits = %logits_name,
                outputs = ?outputs_meta.iter().map(|o| o.name().to_string()).collect::<Vec<_>>(),
                "SER model loaded"
            );
            Ok(Self { session, input_name, logits_name })
        }

        /// Arousal in ~[0,1] for one 16 kHz mono window.
        fn arousal_window(&mut self, win: &[f32]) -> Result<f32> {
            let norm = normalize(win);
            let len = norm.len() as i64;
            let input = Tensor::from_array(([1_i64, len], norm)).map_err(oerr)?;
            let outputs =
                self.session.run(ort::inputs![self.input_name.as_str() => input]).map_err(oerr)?;
            let (_shape, data) =
                outputs[self.logits_name.as_str()].try_extract_tensor::<f32>().map_err(oerr)?;
            // logits = [arousal, dominance, valence]
            data.first().copied().context("SER logits output was empty")
        }

        /// Per-window arousal across `samples` (16 kHz mono f32) — the shared
        /// primitive (ADR 0008): refine max-pools it per candidate, the
        /// whole-VOD discovery pass peak-detects it. Window starts come from
        /// [`window_starts`], so callers can recompute the same grid for times.
        pub fn arousal_series(
            &mut self,
            samples: &[f32],
            win: usize,
            hop: usize,
        ) -> Result<Vec<f32>> {
            let starts = window_starts(samples.len(), win, hop);
            let mut out = Vec::with_capacity(starts.len());
            for s in starts {
                let end = (s + win).min(samples.len());
                out.push(self.arousal_window(&samples[s..end])?);
            }
            Ok(out)
        }

        /// Max-pooled arousal over sliding windows (ADR 0008: peak-preserving).
        /// `samples` is 16 kHz mono f32. Empty range -> 0.
        pub fn arousal_max(&mut self, samples: &[f32], win: usize, hop: usize) -> Result<f32> {
            // Arousal is >= 0, so fold from 0.0 also yields 0 for an empty range.
            Ok(self.arousal_series(samples, win, hop)?.into_iter().fold(0.0_f32, f32::max))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yc_core::{Signals, TimeRange};

    #[test]
    fn normalize_gives_zero_mean_unit_variance() {
        let out = normalize(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let n = out.len() as f32;
        let mean = out.iter().sum::<f32>() / n;
        let var = out.iter().map(|&x| (x - mean) * (x - mean)).sum::<f32>() / n;
        assert!(mean.abs() < 1e-5, "mean {mean}");
        assert!((var - 1.0).abs() < 1e-4, "var {var}");
    }

    #[test]
    fn normalize_handles_empty_and_silent() {
        assert!(normalize(&[]).is_empty());
        // A flat window must not divide-by-zero; it maps to all zeros.
        assert_eq!(normalize(&[0.7, 0.7, 0.7]), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn window_starts_uses_full_windows_and_covers_the_tail() {
        // Exact fit: 0,2,4,6 with 6+4 == 10.
        assert_eq!(window_starts(10, 4, 2), vec![0, 2, 4, 6]);
        // Ragged tail: snap a final full window to end at len (7+4 == 11).
        assert_eq!(window_starts(11, 4, 2), vec![0, 2, 4, 6, 7]);
        // Shorter than a window -> one window over the whole range.
        assert_eq!(window_starts(3, 4, 2), vec![0]);
        assert_eq!(window_starts(0, 4, 2), Vec::<usize>::new());
    }

    #[test]
    fn apply_sets_arousal_and_reranks_by_activation() {
        let w = Weights { chat: 0.0, loudness: 0.0, lexicon: 0.0, arousal: 1.0, llm: 0.0 };
        let mk = |id, signals| Moment {
            id,
            range: TimeRange { start_s: 0.0, end_s: 30.0 },
            signals,
            score: 0.0,
            title: None,
        };
        let base = Signals { chat_rate: Some(1.0), loudness: Some(1.0), ..Default::default() };
        let mut moments = vec![mk(1, base), mk(2, base)];
        // Moment 2 is far more emotionally activated than Moment 1.
        apply(&mut moments, &[0.1, 0.9], &w);
        assert!(moments[0].signals.arousal.unwrap() < moments[1].signals.arousal.unwrap());
        assert!(moments[1].score > moments[0].score);
    }
}
