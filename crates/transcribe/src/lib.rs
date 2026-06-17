//! Transcription: whisper.cpp via whisper-rs (CUDA build), model `large-v3`
//! (ADR 0003; M1 uses the f16 weights, the ADR's quantized ship-default is
//! revisited at M6). Whisper emits token-level timestamps; the language-aware
//! grouping layer in this crate converts tokens into animatable caption
//! units — space-delimited words for EN/ID, character chunks for JA (M6).
//!
//! M1 transcribes only the manually-picked range's samples (transcribe-range-
//! only), so timestamps are already 0-based to the clip and line up with the
//! render timeline. whisper-rs needs libclang at build time on Windows (its
//! bundled bindings are Linux-only).

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use whisper_rs::{
    DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters,
};
use yc_core::{CaptionUnit, Language, Transcript};

fn lang_code(l: Language) -> &'static str {
    match l {
        Language::En => "en",
        Language::Id => "id",
        Language::Ja => "ja",
    }
}

/// whisper's non-text special tokens ([_BEG_], `<|...|>` markers, timestamp
/// tokens) carry no caption text and must be dropped before grouping.
fn is_special(text: &str) -> bool {
    text.starts_with("[_") || text.starts_with("<|")
}

/// Group raw whisper tokens — each carrying whisper's leading-space word
/// marking and start/end seconds — into caption units. A unit begins at every
/// whitespace-led token (a word for EN/ID); a token with no leading space
/// (subword piece or trailing punctuation) extends the current unit. Pure, so
/// it is unit-tested without a model. JA character chunking arrives at M6.
fn group_into_words<I>(tokens: I) -> Vec<CaptionUnit>
where
    I: IntoIterator<Item = (String, f64, f64)>,
{
    let mut units: Vec<CaptionUnit> = Vec::new();
    for (raw, t0, t1) in tokens {
        let clean = raw.trim_start();
        if clean.trim().is_empty() {
            continue;
        }
        let starts_word = raw.starts_with(' ') || raw.starts_with('\u{2581}'); // ' ' or ▁
        if starts_word || units.is_empty() {
            units.push(CaptionUnit { text: clean.to_string(), start_s: t0, end_s: t1 });
        } else {
            let last = units.last_mut().expect("non-empty by branch");
            last.text.push_str(clean);
            last.end_s = t1;
        }
    }
    units
}

/// Load the whisper model on the GPU, transcribe one range's 16 kHz mono f32
/// samples, and group the tokens into animatable caption units (ADR 0003).
/// The model is loaded and dropped per call — M1 has one clip; sequential
/// GPU staging that keeps a context resident is an M3/M4 concern (ADR 0002).
pub fn transcribe_range(model: &Path, samples: &[f32], language: Language) -> Result<Transcript> {
    let mut cparams = WhisperContextParameters::default();
    cparams.use_gpu(true);
    // DTW token-level timestamps with large-v3's alignment heads. The default
    // heuristic token times drift by a few hundred ms — exactly the word-sync
    // wobble — so we align each token to the audio via DTW and read `t_dtw`.
    cparams.dtw_parameters(DtwParameters {
        mode: DtwMode::ModelPreset { model_preset: DtwModelPreset::LargeV3 },
        ..Default::default()
    });
    let ctx = WhisperContext::new_with_params(model, cparams)
        .with_context(|| format!("loading whisper model {}", model.display()))?;
    let mut state = ctx.create_state().context("creating whisper state")?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(lang_code(language)));
    params.set_token_timestamps(true); // populate per-token t0/t1 for word timing
    params.set_translate(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_print_special(false);

    state
        .full(params, samples)
        .context("whisper transcription failed")?;

    // Collect (text, t0_s, t1_s) for every real token, then group into words.
    let mut raw_tokens: Vec<(String, f64, f64)> = Vec::new();
    for s in 0..state.full_n_segments() {
        let segment = state
            .get_segment(s)
            .ok_or_else(|| anyhow!("segment {s} out of bounds mid-read"))?;
        for t in 0..segment.n_tokens() {
            let Some(token) = segment.get_token(t) else {
                continue;
            };
            let text = token.to_str_lossy().context("reading token text")?.into_owned();
            if is_special(&text) {
                continue;
            }
            let data = token.token_data();
            // Prefer the DTW-aligned time; fall back to the heuristic t0/t1 when
            // DTW produced no value for this token (t_dtw == -1). DTW gives a
            // single aligned point per token, so a word's span runs from its
            // first token's time to its last — the caption builders handle the
            // (zero-width) single-token case.
            let (t0, t1) = if data.t_dtw >= 0 {
                (data.t_dtw, data.t_dtw)
            } else {
                (data.t0, data.t1)
            };
            raw_tokens.push((text, t0 as f64 / 100.0, t1 as f64 / 100.0));
        }
    }

    Ok(Transcript { language, units: group_into_words(raw_tokens) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_leading_space_tokens_into_words() {
        let toks = vec![
            (" Hello".to_string(), 0.0, 0.4),
            (" world".to_string(), 0.4, 0.8),
            ("!".to_string(), 0.8, 0.9), // punctuation extends "world"
            (" GG".to_string(), 1.0, 1.2),
        ];
        let units = group_into_words(toks);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0].text, "Hello");
        assert_eq!(units[1].text, "world!");
        assert_eq!(units[2].text, "GG");
        assert_eq!(units[0].start_s, 0.0);
        assert_eq!(units[1].end_s, 0.9); // end advanced by the "!" token
    }

    #[test]
    fn first_token_without_leading_space_still_starts_a_word() {
        let toks = vec![("Yo".to_string(), 0.0, 0.3), ("urs".to_string(), 0.3, 0.5)];
        let units = group_into_words(toks);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].text, "Yours");
        assert_eq!(units[0].end_s, 0.5);
    }

    #[test]
    fn special_tokens_are_recognised() {
        assert!(is_special("[_BEG_]"));
        assert!(is_special("<|endoftext|>"));
        assert!(!is_special(" hello"));
    }
}
