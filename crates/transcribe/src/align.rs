//! wav2vec2-CTC **forced alignment** — the caption TIMING skeleton that
//! replaces whisper's DTW token times on the ensemble path (ADR 0053/0054;
//! the DEFAULT since the ADR 0055 flip, `YC_FORCED_ALIGN=0` = off-switch).
//!
//! Why: whisper's DTW is one GLOBAL path per segment; non-speech (laughter,
//! pauses) has no matching tokens, the path smears across it and the offset
//! ACCUMULATES — the operator's "slowly drifts out of sync" (ADR 0053's
//! root-cause). Forced alignment re-anchors EACH word from the audio itself:
//! a character-CTC acoustic model (`cahya/wav2vec2-large-xlsr-indonesian`,
//! exported to ONNX in `models/w2v2-align-id/`) emits per-20 ms letter
//! probabilities, and a Viterbi pass finds the best monotonic spelling of the
//! ensemble's words through them — each word's onset falls out as its first
//! letter's frame. The spike (ADR 0053, clip 3) reproduced the operator's
//! hand-pinned onsets with zero pins: SIAPA 3.66→5.08 (gt 5.0), SDC
//! 20.78→22.93 (gt 23.0), 0/111 words unaligned.
//!
//! Input contract: RAW 16 kHz mono samples, deliberately NOT the processor's
//! zero-mean/unit-var normalization — measured on clip 3 (2026-07-11):
//! normalization trades the spike's one miss (GUE +1.9 s → +0.3 s) for a new
//! one (PINGUIN grabs the earlier duplicate, −2.2 s), max onset delta 3.3 s on
//! 111 words. The operator's eye approved the RAW burn (the spike); parity
//! with that burn wins over the model card (the ADR 0050 lesson).
//!
//! Shape mirrors `detect::arousal`: the algorithm below (vocab, tokenize,
//! log-softmax, Viterbi, span grouping) is pure and always compiled/tested;
//! only the ONNX session lives behind the `align` cargo feature.

use std::collections::HashMap;

use anyhow::{Context as _, Result};

/// The character vocabulary of the CTC model, parsed from its `vocab.json`
/// (single-character tokens + `|` word delimiter + `[PAD]` blank + `[UNK]`,
/// which is never emitted as a target — OOV characters are SKIPPED, matching
/// the validated spike).
pub struct AlignVocab {
    chars: HashMap<char, u32>,
    pub blank: u32,
    pub delim: u32,
    /// Total vocabulary size (the emission's class axis).
    pub size: usize,
}

impl AlignVocab {
    /// Parse a HF `vocab.json` (token -> id map). The blank is `[PAD]`, the
    /// word delimiter `|` — the cahya model's convention (and wav2vec2-CTC's
    /// generally).
    pub fn parse(json: &str) -> Result<Self> {
        let map: HashMap<String, u32> =
            serde_json::from_str(json).context("parsing vocab.json")?;
        let blank = *map.get("[PAD]").context("vocab.json has no [PAD] blank")?;
        let delim = *map.get("|").context("vocab.json has no | word delimiter")?;
        let size = map.values().map(|&v| v as usize + 1).max().unwrap_or(0);
        let chars: HashMap<char, u32> = map
            .iter()
            .filter_map(|(k, &v)| {
                let mut it = k.chars();
                match (it.next(), it.next()) {
                    (Some(c), None) => Some((c, v)),
                    _ => None, // [PAD]/[UNK] specials; `|` re-enters via delim
                }
            })
            .collect();
        anyhow::ensure!(!chars.is_empty(), "vocab.json has no character tokens");
        Ok(Self { chars, blank, delim, size })
    }

    /// Token id for a character: exact, else lowercase, else uppercase (the
    /// vocab is lowercase; caption words may carry display case).
    pub fn char_id(&self, ch: char) -> Option<u32> {
        if let Some(&id) = self.chars.get(&ch) {
            return Some(id);
        }
        for c in ch.to_lowercase().chain(ch.to_uppercase()) {
            if let Some(&id) = self.chars.get(&c) {
                return Some(id);
            }
        }
        None
    }
}

/// Build the CTC target sequence for `words`, `|`-delimited, exactly as the
/// validated spike tokenized: per word, every in-vocab character (OOV chars
/// skipped); a word with NO alignable character contributes a lone extra
/// delimiter placeholder (it gets no span and falls to gap placement).
/// Returns `(targets, widx)` where `widx[i]` is the word index owning
/// `targets[i]`, or -1 for delimiters/placeholders.
pub fn tokenize_words(words: &[String], vocab: &AlignVocab) -> (Vec<u32>, Vec<i32>) {
    let mut targets: Vec<u32> = Vec::new();
    let mut widx: Vec<i32> = Vec::new();
    for (wi, w) in words.iter().enumerate() {
        if !targets.is_empty() {
            targets.push(vocab.delim);
            widx.push(-1);
        }
        let mut kept = 0;
        for ch in w.chars() {
            if let Some(id) = vocab.char_id(ch) {
                if id != vocab.blank {
                    targets.push(id);
                    widx.push(wi as i32);
                    kept += 1;
                }
            }
        }
        if kept == 0 {
            targets.push(vocab.delim);
            widx.push(-1);
        }
    }
    (targets, widx)
}

/// In-place log-softmax over each frame row of a row-major `(frames x vocab_n)`
/// logits buffer — the emission the Viterbi walks.
pub fn log_softmax_rows(logits: &mut [f32], frames: usize, vocab_n: usize) {
    debug_assert_eq!(logits.len(), frames * vocab_n);
    for t in 0..frames {
        let row = &mut logits[t * vocab_n..(t + 1) * vocab_n];
        let mx = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let sum: f64 = row.iter().map(|&v| f64::from(v - mx).exp()).sum();
        let ls = f64::from(mx) + sum.ln();
        for v in row.iter_mut() {
            *v = (f64::from(*v) - ls) as f32;
        }
    }
}

/// CTC Viterbi forced alignment (torchaudio `forced_align`'s algorithm):
/// find the maximum-probability monotonic frame path that spells `targets`
/// through a `(frames x vocab_n)` row-major LOG-probability emission, blanks
/// interleaved per the CTC topology (a blank is REQUIRED between repeated
/// identical targets, optional elsewhere). Returns one `(start_frame,
/// end_frame_exclusive)` span per target, in target order.
///
/// Errors when the alignment is infeasible (more forced steps than frames) —
/// the caller falls back to the DTW skeleton, never panics.
pub fn forced_align_spans(
    emission: &[f32],
    frames: usize,
    vocab_n: usize,
    targets: &[u32],
    blank: u32,
) -> Result<Vec<(usize, usize)>> {
    anyhow::ensure!(!targets.is_empty(), "no targets to align");
    anyhow::ensure!(frames > 0 && emission.len() == frames * vocab_n, "emission shape mismatch");
    anyhow::ensure!((blank as usize) < vocab_n, "blank id outside vocab");
    anyhow::ensure!(
        targets.iter().all(|&t| (t as usize) < vocab_n && t != blank),
        "target id outside vocab (or blank)"
    );
    let min_frames =
        targets.len() + targets.windows(2).filter(|w| w[0] == w[1]).count();
    anyhow::ensure!(
        frames >= min_frames,
        "audio too short to align: {} frames < {} forced steps",
        frames,
        min_frames
    );

    // CTC state lattice: states 0..2N+1, even = blank, odd s = targets[(s-1)/2].
    let n = targets.len();
    let s_count = 2 * n + 1;
    let ext = |s: usize| -> u32 { if s % 2 == 1 { targets[(s - 1) / 2] } else { blank } };
    let em = |t: usize, v: u32| f64::from(emission[t * vocab_n + v as usize]);

    // Two-row alphas in f64 (a 61 s clip sums ~3000 log-probs; f32 granularity
    // at that magnitude could flip near-tie argmaxes) + a full back-pointer
    // plane (u8: how state s at time t was reached).
    let mut prev = vec![f64::NEG_INFINITY; s_count];
    let mut cur = vec![f64::NEG_INFINITY; s_count];
    let mut back = vec![0u8; frames * s_count];
    prev[0] = em(0, blank);
    if s_count > 1 {
        prev[1] = em(0, ext(1));
    }
    for t in 1..frames {
        for s in 0..s_count {
            let mut best = prev[s];
            let mut who = 0u8;
            if s >= 1 && prev[s - 1] > best {
                best = prev[s - 1];
                who = 1;
            }
            // The skip (s-2 -> s) enters a token state over one blank, legal
            // only when the two tokens differ (CTC's repeat rule).
            if s >= 2 && s % 2 == 1 && ext(s) != ext(s - 2) && prev[s - 2] > best {
                best = prev[s - 2];
                who = 2;
            }
            cur[s] = if best == f64::NEG_INFINITY { best } else { best + em(t, ext(s)) };
            back[t * s_count + s] = who;
        }
        std::mem::swap(&mut prev, &mut cur);
    }

    // The path may end on the final token or the trailing blank.
    let mut s = s_count - 1;
    if s_count >= 2 && prev[s_count - 2] > prev[s_count - 1] {
        s = s_count - 2;
    }
    anyhow::ensure!(prev[s] > f64::NEG_INFINITY, "forced alignment found no feasible path");

    // Backtrack, accumulating each odd (token) state's frame run as its span.
    let mut spans = vec![(usize::MAX, 0usize); n];
    for t in (0..frames).rev() {
        if s % 2 == 1 {
            let ti = (s - 1) / 2;
            spans[ti].0 = spans[ti].0.min(t);
            spans[ti].1 = spans[ti].1.max(t + 1);
        }
        if t > 0 {
            s -= back[t * s_count + s] as usize;
        }
    }
    debug_assert!(spans.iter().all(|&(a, b)| a < b), "every target visited by the forced path");
    Ok(spans)
}

/// Collapse per-target spans to per-word `(start_frame, end_frame)` (min/max
/// over the word's characters; `None` = the word had no alignable target).
pub fn word_frame_spans(
    spans: &[(usize, usize)],
    widx: &[i32],
    n_words: usize,
) -> Vec<Option<(usize, usize)>> {
    debug_assert_eq!(spans.len(), widx.len());
    let mut out: Vec<Option<(usize, usize)>> = vec![None; n_words];
    for (sp, &wi) in spans.iter().zip(widx) {
        if wi < 0 {
            continue;
        }
        let e = &mut out[wi as usize];
        *e = Some(match *e {
            None => *sp,
            Some((a, b)) => (a.min(sp.0), b.max(sp.1)),
        });
    }
    out
}

/// End-to-end word alignment over a pre-computed emission: tokenize + Viterbi
/// + per-word spans in SECONDS (`frame * dur / frames`, the spike's clock).
/// Pure — the ONNX session (behind the `align` feature) only supplies the
/// emission; everything after it is this function, so the parity harness and
/// the unit tests exercise the exact production math.
pub fn align_words_on_emission(
    words: &[String],
    vocab: &AlignVocab,
    emission: &[f32],
    frames: usize,
    vocab_n: usize,
    dur_s: f64,
) -> Result<Vec<Option<(f64, f64)>>> {
    let (targets, widx) = tokenize_words(words, vocab);
    anyhow::ensure!(!targets.is_empty(), "no alignable words");
    let spans = forced_align_spans(emission, frames, vocab_n, &targets, vocab.blank)?;
    let spf = dur_s / frames as f64;
    Ok(word_frame_spans(&spans, &widx, words.len())
        .into_iter()
        .map(|o| o.map(|(a, b)| (a as f64 * spf, b as f64 * spf)))
        .collect())
}

/// Resident forced-alignment model (ONNX via `ort`, CPU execution provider —
/// deterministic, and the emission is the cheap half next to the ensemble's
/// five sidecar decodes). Loaded per clip by `ensemble::apply` — by default
/// since ADR 0055 (`YC_FORCED_ALIGN=0` disables).
#[cfg(feature = "align")]
pub use infer::Aligner;

#[cfg(feature = "align")]
mod infer {
    use super::{align_words_on_emission, log_softmax_rows, AlignVocab};
    use anyhow::{Context, Result};
    use std::path::Path;

    /// `ort::Error` holds raw pointers (not `Send + Sync`); stringify it.
    fn oerr(e: ort::Error) -> anyhow::Error {
        anyhow::anyhow!("{e}")
    }

    pub struct Aligner {
        session: ort::session::Session,
        input_name: String,
        logits_name: String,
        vocab: AlignVocab,
    }

    impl Aligner {
        /// Load `model.onnx` + `vocab.json` from the model directory
        /// (`models/w2v2-align-id/`, pinned by fetch-models.ps1).
        pub fn load(dir: &Path) -> Result<Self> {
            let onnx = dir.join("model.onnx");
            let vocab_path = dir.join("vocab.json");
            let vocab = AlignVocab::parse(
                &std::fs::read_to_string(&vocab_path)
                    .with_context(|| format!("reading {}", vocab_path.display()))?,
            )?;
            let session = ort::session::Session::builder()
                .and_then(|mut b| b.commit_from_file(&onnx))
                .map_err(oerr)
                .with_context(|| format!("loading align model {}", onnx.display()))?;
            let input_name = session
                .inputs()
                .first()
                .map(|i| i.name().to_string())
                .ok_or_else(|| anyhow::anyhow!("align model has no inputs"))?;
            let logits_name = session
                .outputs()
                .iter()
                .find(|o| o.name().to_lowercase().contains("logit"))
                .or_else(|| session.outputs().first())
                .map(|o| o.name().to_string())
                .ok_or_else(|| anyhow::anyhow!("align model has no outputs"))?;
            tracing::info!(
                input = %input_name,
                logits = %logits_name,
                vocab = vocab.size,
                "align model loaded"
            );
            Ok(Self { session, input_name, logits_name, vocab })
        }

        /// Force-align `words` to 16 kHz mono samples; per-word `(start_s,
        /// end_s)` spans (`None` = unalignable word, e.g. all-digit). RAW
        /// samples by contract (see the module doc — measured decision).
        pub fn align_words(
            &mut self,
            words: &[String],
            samples: &[f32],
            sample_rate: u32,
        ) -> Result<Vec<Option<(f64, f64)>>> {
            anyhow::ensure!(sample_rate == 16_000, "aligner expects 16 kHz mono");
            anyhow::ensure!(!samples.is_empty(), "no samples to align");
            let dur_s = samples.len() as f64 / f64::from(sample_rate);
            let input = ort::value::Tensor::from_array((
                [1_i64, samples.len() as i64],
                samples.to_vec(),
            ))
            .map_err(oerr)?;
            let outputs = self
                .session
                .run(ort::inputs![self.input_name.as_str() => input])
                .map_err(oerr)?;
            let (shape, data) =
                outputs[self.logits_name.as_str()].try_extract_tensor::<f32>().map_err(oerr)?;
            anyhow::ensure!(shape.len() == 3, "unexpected logits rank {}", shape.len());
            let (frames, vocab_n) = (shape[1] as usize, shape[2] as usize);
            anyhow::ensure!(
                vocab_n == self.vocab.size,
                "vocab.json ({}) does not match the model's class axis ({vocab_n})",
                self.vocab.size
            );
            let mut emission = data.to_vec();
            log_softmax_rows(&mut emission, frames, vocab_n);
            align_words_on_emission(words, &self.vocab, &emission, frames, vocab_n, dur_s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cahya vocab, abbreviated to the shape that matters.
    fn vocab() -> AlignVocab {
        AlignVocab::parse(
            r#"{"a": 0, "b": 1, "n": 2, "|": 3, "[UNK]": 4, "[PAD]": 5}"#,
        )
        .unwrap()
    }

    #[test]
    fn vocab_parses_chars_blank_delim() {
        let v = vocab();
        assert_eq!((v.blank, v.delim, v.size), (5, 3, 6));
        assert_eq!(v.char_id('a'), Some(0));
        assert_eq!(v.char_id('A'), Some(0), "display-case falls to lowercase");
        assert_eq!(v.char_id('9'), None, "digits are OOV");
    }

    #[test]
    fn tokenize_matches_the_spike_shape() {
        let v = vocab();
        let words: Vec<String> = ["ban", "77", "ab"].iter().map(|s| s.to_string()).collect();
        let (targets, widx) = tokenize_words(&words, &v);
        // ban | <placeholder for 77> | ab — the all-OOV word keeps a lone
        // delimiter (spike parity) and owns no target.
        assert_eq!(targets, vec![1, 0, 2, 3, 3, 3, 0, 1]);
        assert_eq!(widx, vec![0, 0, 0, -1, -1, -1, 2, 2]);
    }

    /// A tiny emission where the best path is unambiguous: vocab {a=0, b=1,
    /// blank=2}, 6 frames spelling "a a b b _ b" -> targets [a, b, b].
    #[test]
    fn viterbi_spells_targets_with_repeat_blank() {
        let lo = -20.0f32;
        let hi = -0.1f32;
        // frames x vocab (3): favor a,a,b,b,blank,b
        #[rustfmt::skip]
        let emission = vec![
            hi, lo, lo,
            hi, lo, lo,
            lo, hi, lo,
            lo, hi, lo,
            lo, lo, hi,
            lo, hi, lo,
        ];
        let spans = forced_align_spans(&emission, 6, 3, &[0, 1, 1], 2).unwrap();
        // "a" spans frames 0-2, first "b" 2-4, forced blank at 4, second "b" 5-6.
        assert_eq!(spans, vec![(0, 2), (2, 4), (5, 6)]);
    }

    #[test]
    fn viterbi_rejects_too_short_audio() {
        // 3 targets with one repeat need >= 4 frames; give 3.
        let emission = vec![-1.0f32; 3 * 3];
        assert!(forced_align_spans(&emission, 3, 3, &[0, 1, 1], 2).is_err());
    }

    #[test]
    fn word_spans_group_by_word_and_skip_delims() {
        let spans = vec![(0, 2), (2, 3), (3, 5), (6, 8), (8, 9)];
        let widx = vec![0, 0, -1, 1, 1];
        let ws = word_frame_spans(&spans, &widx, 3);
        assert_eq!(ws, vec![Some((0, 3)), Some((6, 9)), None]);
    }

    #[test]
    // Row 0 / column 0 spelled out (`0 * 6 + 0`) for symmetry with the frames
    // below — the grid layout is the point.
    #[allow(clippy::erasing_op, clippy::identity_op)]
    fn emission_to_seconds_uses_dur_over_frames() {
        let v = vocab();
        let lo = -20.0f32;
        let hi = -0.1f32;
        // 4 frames x 6 vocab; word "a" then delim then "b":
        // frame0 a, frame1 delim, frame2 b, frame3 blank
        let mut e = vec![lo; 4 * 6];
        e[0 * 6 + 0] = hi; // a
        e[1 * 6 + 3] = hi; // |
        e[2 * 6 + 1] = hi; // b
        e[3 * 6 + 5] = hi; // blank
        let words: Vec<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let out = align_words_on_emission(&words, &v, &e, 4, 6, 2.0).unwrap();
        // 0.5 s per frame: "a" = [0, 0.5), "b" = [1.0, 1.5)
        assert_eq!(out[0], Some((0.0, 0.5)));
        assert_eq!(out[1], Some((1.0, 1.5)));
    }

    #[test]
    fn log_softmax_rows_normalizes_each_frame() {
        let mut x = vec![0.0f32, 0.0, 1.0, 1.0];
        log_softmax_rows(&mut x, 2, 2);
        for t in 0..2 {
            let s: f64 = x[t * 2..(t + 1) * 2].iter().map(|&v| f64::from(v).exp()).sum();
            assert!((s - 1.0).abs() < 1e-6, "row {t} sums to {s}");
        }
        assert!((x[0] - (-(2.0f32).ln())).abs() < 1e-6);
    }
}
