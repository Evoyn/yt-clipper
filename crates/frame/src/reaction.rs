//! Shared-reaction (laughter-class) acoustic evidence — the ADR 0045 SPIKE:
//! an AudioSet-class audio-event tagger scores every [`TAG_STEP_S`] step of
//! the analysis audio for the laughter family, so the split grammar can one
//! day read "the group reacts together" as positive evidence (CONTEXT.md:
//! **Shared reaction**). ADR 0044 measured the speech-domain instruments
//! dead on this class (contested share, absence share, and voice-cluster
//! composition all fail to discriminate the Deddy laughter stretch); this
//! module is the named replacement path: a model trained to tag laughter
//! directly.
//!
//! Nothing in the production analysis path calls this module. The
//! `speaker_diag` harness drives it against pre-declared discrimination bars
//! on the Deddy fixture (laughter mass >= 50% on the known stretch,
//! <= 10% on every monologue segment at the same tau from the declared grid,
//! gap >= 5x), and production wiring waits on that gate plus the operator's
//! ruling — the ADR 0042/0043 spike discipline.
//!
//! Split mirrors [`crate::voice`]: the tag-step window grid, the AudioSet
//! label-CSV parse, the laughter-family lookup, and the mass math are pure
//! and unit-tested here; only [`TagSession`] (the ONNX session) sits behind
//! the `voice` cargo feature (the same `ort` runtime as CAM++). The tagger
//! reads the SAME 16 kHz analysis.wav samples the voice lane embeds, and it
//! scores every step regardless of the VAD — breathy laughter the VAD
//! misses is exactly what an acoustic instrument adds.

#[cfg(feature = "voice")]
use anyhow::Result;

/// Analysis window per tag step (seconds): enough acoustic context for an
/// AudioSet-class model (trained on 10 s clips, usable well below that)
/// without smearing much mass across segment boundaries.
pub const TAG_WIN_S: f64 = 2.0;
/// The scoring grid the discrimination bars are declared on: one laughter
/// score per 0.25 s step, each step scored by the [`TAG_WIN_S`] window
/// centered on it.
pub const TAG_STEP_S: f64 = 0.25;

/// The AudioSet laughter family — the "Laughter" class and its ontology
/// children, matched exactly against the release's class_labels_indices.csv
/// display names. A step's laughter score is the MAX over these classes.
pub const LAUGHTER_FAMILY: [&str; 6] = [
    "Laughter",
    "Baby laughter",
    "Giggle",
    "Snicker",
    "Belly laugh",
    "Chuckle, chortle",
];

/// Parse an AudioSet `class_labels_indices.csv` (header `index,mid,display_name`,
/// display names quoted and possibly containing commas) into display names
/// ordered by class index — the model's output order.
pub fn parse_class_labels(csv: &str) -> Vec<String> {
    let mut rows: Vec<(usize, String)> = Vec::new();
    for line in csv.lines().skip(1) {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let Some((idx, rest)) = line.split_once(',') else {
            continue;
        };
        let Ok(idx) = idx.trim().parse::<usize>() else {
            continue;
        };
        let Some((_mid, name)) = rest.split_once(',') else {
            continue;
        };
        let name = name.trim().trim_matches('"').to_string();
        rows.push((idx, name));
    }
    rows.sort_by_key(|&(i, _)| i);
    rows.into_iter().map(|(_, n)| n).collect()
}

/// Class indices of the [`LAUGHTER_FAMILY`] present in `labels` (exact
/// display-name match). The spike prints how many were found — all six on a
/// genuine AudioSet label file.
pub fn laughter_family(labels: &[String]) -> Vec<usize> {
    labels
        .iter()
        .enumerate()
        .filter(|(_, l)| LAUGHTER_FAMILY.contains(&l.as_str()))
        .map(|(i, _)| i)
        .collect()
}

/// Number of [`TAG_STEP_S`] steps covering `duration_s`.
pub fn n_steps(duration_s: f64) -> usize {
    (duration_s / TAG_STEP_S).ceil().max(1.0) as usize
}

/// One [`TAG_WIN_S`] analysis window per step, centered on the step's center
/// and shifted (not shrunk) to stay inside `[0, duration_s]`; a clip shorter
/// than one window yields the whole clip each step. Returns `(start_s, end_s)`
/// per step — the samples [`TagSession::tag`] scores for that step.
pub fn plan_tag_windows(duration_s: f64) -> Vec<(f64, f64)> {
    let n = n_steps(duration_s);
    (0..n)
        .map(|k| {
            if duration_s <= TAG_WIN_S {
                return (0.0, duration_s);
            }
            let center = (k as f64 + 0.5) * TAG_STEP_S;
            let start = (center - TAG_WIN_S * 0.5).clamp(0.0, duration_s - TAG_WIN_S);
            (start, start + TAG_WIN_S)
        })
        .collect()
}

/// Project per-step scores onto the analysis bin grid: each bin (at `bin_s`)
/// takes the score of the step containing its center — the piecewise-constant
/// view the evidence table and any future lane consumer read.
pub fn project_to_bins(steps: &[f32], n_bins: usize, bin_s: f64) -> Vec<f32> {
    (0..n_bins)
        .map(|b| {
            let t = (b as f64 + 0.5) * bin_s;
            let k = ((t / TAG_STEP_S) as usize).min(steps.len().saturating_sub(1));
            steps.get(k).copied().unwrap_or(0.0)
        })
        .collect()
}

/// `(hits, total)` — steps whose center lies in `[span.0, span.1)` and whose
/// score clears `tau`, over the steps considered. Mass = hits / total.
pub fn mass_in_span(steps: &[f32], span: (f64, f64), tau: f32) -> (usize, usize) {
    let mut hits = 0;
    let mut total = 0;
    for (k, &s) in steps.iter().enumerate() {
        let t = (k as f64 + 0.5) * TAG_STEP_S;
        if t >= span.0 && t < span.1 {
            total += 1;
            if s >= tau {
                hits += 1;
            }
        }
    }
    (hits, total)
}

/// Contiguous runs of steps scoring `>= tau`, as `(start_s, end_s)` spans —
/// the printable shared-reaction mask.
pub fn mask_runs(steps: &[f32], tau: f32) -> Vec<(f64, f64)> {
    let mut runs: Vec<(f64, f64)> = Vec::new();
    for (k, &s) in steps.iter().enumerate() {
        if s < tau {
            continue;
        }
        let (t0, t1) = (k as f64 * TAG_STEP_S, (k + 1) as f64 * TAG_STEP_S);
        match runs.last_mut() {
            Some((_, e)) if (*e - t0).abs() < 1e-9 => *e = t1,
            _ => runs.push((t0, t1)),
        }
    }
    runs
}

/// What an audio-tagging export's raw output means — a per-model convention
/// like [`crate::voice::SampleScale`], pinned by the `tagselftest` (which
/// prints the raw range: a sigmoid-terminated export lives in [0, 1] with
/// hard zeros; a linear-classifier export goes deeply negative on absent
/// classes).
#[cfg(feature = "voice")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagOutput {
    /// Raw logits — [`TagSession::tag`] applies the sigmoid.
    Logits,
    /// Already probabilities — [`TagSession::tag`] returns them as-is.
    Probs,
}

/// Resident audio-event tagging session (`voice` feature): 16 kHz samples →
/// fbank (no CMN — event taggers keep absolute level) → `[1, T, 80]` →
/// per-class scores. Handles both export shapes seen in the sherpa-onnx
/// audio-tagging release: a single fbank input, or (x, x_lens). Mirrors
/// [`crate::voice::VoiceEmbedder`]'s loading shape.
#[cfg(feature = "voice")]
pub struct TagSession {
    session: ort::session::Session,
    input_name: String,
    /// Second input (frame lengths, i64), when the export takes one.
    lens_name: Option<String>,
    output_name: String,
    scale: crate::voice::SampleScale,
    output: TagOutput,
    fbank: crate::voice::Fbank,
}

#[cfg(feature = "voice")]
impl TagSession {
    /// Load an audio-tagging ONNX on the CPU execution provider.
    pub fn load(
        onnx: &std::path::Path,
        scale: crate::voice::SampleScale,
        output: TagOutput,
    ) -> Result<Self> {
        use anyhow::Context;
        let oerr = |e: ort::Error| anyhow::anyhow!("{e}");
        let session = ort::session::Session::builder()
            .and_then(|mut b| b.commit_from_file(onnx))
            .map_err(oerr)
            .with_context(|| format!("loading tag model {}", onnx.display()))?;
        let mut names = session.inputs().iter().map(|i| i.name().to_string());
        let input_name = names
            .next()
            .ok_or_else(|| anyhow::anyhow!("tag model has no inputs"))?;
        let lens_name = names.next();
        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| anyhow::anyhow!("tag model has no outputs"))?;
        tracing::info!(input = %input_name, lens = ?lens_name, output = %output_name, "tag model loaded");
        Ok(Self {
            session,
            input_name,
            lens_name,
            output_name,
            scale,
            output,
            fbank: crate::voice::Fbank::new(),
        })
    }

    /// Raw per-class model outputs for one window of 16 kHz mono samples —
    /// meaning per the pinned [`TagOutput`]; the selftest prints these so the
    /// convention is seen, not assumed. No CMN: an event tagger's evidence
    /// includes absolute level, which CMN would erase.
    pub fn tag_raw(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        use crate::voice::{SampleScale, N_MELS};
        let oerr = |e: ort::Error| anyhow::anyhow!("{e}");
        anyhow::ensure!(!samples.is_empty(), "empty window");
        let feats = match self.scale {
            SampleScale::Unit => self.fbank.compute(samples),
            SampleScale::Int16 => {
                let scaled: Vec<f32> = samples.iter().map(|s| s * 32768.0).collect();
                self.fbank.compute(&scaled)
            }
        };
        let t = feats.len() / N_MELS;
        let x = ort::value::Tensor::from_array(([1_i64, t as i64, N_MELS as i64], feats))
            .map_err(oerr)?;
        let outputs = match &self.lens_name {
            Some(lens) => {
                let l = ort::value::Tensor::from_array(([1_i64], vec![t as i64])).map_err(oerr)?;
                self.session
                    .run(ort::inputs![self.input_name.as_str() => x, lens.as_str() => l])
                    .map_err(oerr)?
            }
            None => self
                .session
                .run(ort::inputs![self.input_name.as_str() => x])
                .map_err(oerr)?,
        };
        let (_shape, out) = outputs[self.output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(oerr)?;
        Ok(out.to_vec())
    }

    /// Per-class probabilities: [`Self::tag_raw`] mapped through the pinned
    /// [`TagOutput`] convention (sigmoid for a logits export, identity for a
    /// sigmoid-terminated one — the zipformer AT release export is the
    /// latter, caught by the selftest's raw-range printout).
    pub fn tag(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let raw = self.tag_raw(samples)?;
        Ok(match self.output {
            TagOutput::Logits => raw.into_iter().map(|v| 1.0 / (1.0 + (-v).exp())).collect(),
            TagOutput::Probs => raw,
        })
    }
}

/// Score every [`TAG_STEP_S`] step of a clip: laughter-family max probability
/// per step, over the same 16 kHz samples the voice lane embeds. The spike's
/// per-step evidence, VAD-independent by design.
#[cfg(feature = "voice")]
pub fn tag_steps(
    session: &mut TagSession,
    samples: &[f32],
    duration_s: f64,
    family: &[usize],
) -> Result<Vec<f32>> {
    let sr = crate::voice::VOICE_SR as f64;
    let mut steps = Vec::new();
    for (s, e) in plan_tag_windows(duration_s) {
        let (i0, i1) = (
            (s * sr).round() as usize,
            ((e * sr).round() as usize).min(samples.len()),
        );
        if i1 <= i0 {
            steps.push(0.0);
            continue;
        }
        let probs = session.tag(&samples[i0..i1])?;
        let score = family
            .iter()
            .filter_map(|&c| probs.get(c))
            .fold(0.0f32, |a, &b| a.max(b));
        steps.push(score);
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "index,mid,display_name\n\
        0,/m/09x0r,\"Speech\"\n\
        16,/m/01j3sz,\"Laughter\"\n\
        17,/t/dd00001,\"Baby laughter\"\n\
        18,/m/07r660_,\"Giggle\"\n\
        19,/m/07s04w4,\"Snicker\"\n\
        20,/m/07sq110,\"Belly laugh\"\n\
        21,/m/07rgt08,\"Chuckle, chortle\"\n\
        22,/m/0463cq4,\"Crying, sobbing\"\n";

    #[test]
    fn labels_parse_in_index_order_with_quoted_commas() {
        let labels = parse_class_labels(CSV);
        assert_eq!(labels.len(), 8);
        assert_eq!(labels[0], "Speech");
        assert_eq!(labels[6], "Chuckle, chortle"); // comma inside quotes survives
        assert_eq!(labels[7], "Crying, sobbing");
    }

    #[test]
    fn laughter_family_finds_all_six_and_only_them() {
        let labels = parse_class_labels(CSV);
        let fam = laughter_family(&labels);
        assert_eq!(fam, vec![1, 2, 3, 4, 5, 6]); // positions in the parsed list
        assert!(!fam.contains(&0), "Speech is not laughter");
        assert!(!fam.contains(&7), "Crying is not laughter");
    }

    #[test]
    fn tag_windows_cover_every_step_full_size_and_in_bounds() {
        let dur = 10.0;
        let wins = plan_tag_windows(dur);
        assert_eq!(wins.len(), 40);
        for (k, &(s, e)) in wins.iter().enumerate() {
            assert!((e - s - TAG_WIN_S).abs() < 1e-9, "window {k} is full size");
            assert!(s >= -1e-9 && e <= dur + 1e-9, "window {k} in bounds");
            let center = (k as f64 + 0.5) * TAG_STEP_S;
            assert!(center >= s && center <= e, "step {k} center covered");
        }
        // Interior steps are exactly centered; edge steps shift, never shrink.
        assert_eq!(wins[0], (0.0, 2.0));
        assert_eq!(wins[39], (8.0, 10.0));
        let mid = wins[20];
        assert!((mid.0 - (20.5 * 0.25 - 1.0)).abs() < 1e-9);
    }

    #[test]
    fn short_clip_scores_whole_clip_every_step() {
        let wins = plan_tag_windows(1.2);
        assert_eq!(wins.len(), 5);
        assert!(wins.iter().all(|&w| w == (0.0, 1.2)));
    }

    #[test]
    fn projection_gives_each_bin_its_covering_step() {
        // Two steps (0.25 s each) onto a 24 Hz bin grid: bins 0..6 take step 0,
        // bins 6..12 take step 1.
        let steps = vec![0.9f32, 0.1];
        let bins = project_to_bins(&steps, 12, 1.0 / 24.0);
        assert!(bins[..6].iter().all(|&v| (v - 0.9).abs() < 1e-6));
        assert!(bins[6..].iter().all(|&v| (v - 0.1).abs() < 1e-6));
    }

    #[test]
    fn mass_counts_step_centers_inside_the_span_only() {
        let steps = vec![0.9, 0.9, 0.1, 0.9]; // centers 0.125, 0.375, 0.625, 0.875
        let (hits, total) = mass_in_span(&steps, (0.25, 1.0), 0.5);
        assert_eq!(total, 3);
        assert_eq!(hits, 2);
        let (h0, t0) = mass_in_span(&steps, (5.0, 6.0), 0.5);
        assert_eq!((h0, t0), (0, 0), "empty span");
    }

    #[test]
    fn mask_runs_merge_adjacent_steps_and_split_on_gaps() {
        let steps = vec![0.9, 0.9, 0.1, 0.9, 0.1];
        let runs = mask_runs(&steps, 0.5);
        assert_eq!(runs.len(), 2);
        assert!((runs[0].0 - 0.0).abs() < 1e-9 && (runs[0].1 - 0.5).abs() < 1e-9);
        assert!((runs[1].0 - 0.75).abs() < 1e-9 && (runs[1].1 - 1.0).abs() < 1e-9);
    }
}
