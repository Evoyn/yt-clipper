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

/// Per-language fix-ups for words whisper reliably mishears on the mixed game+mic
/// track (e.g. Indonesian streamer slang). A cheap, deterministic patch over the
/// transcription ceiling - the real fix is vocal separation. Expand against real
/// VODs, like the excitement lexicon. (wrong, right), lowercase.
fn corrections(language: Language) -> &'static [(&'static str, &'static str)] {
    match language {
        Language::Id => &[("bocal", "bocil")],
        Language::En => &[],
        Language::Ja => &[],
    }
}

/// Replace whole words whisper commonly mishears (case-insensitive, surrounding
/// punctuation preserved). Applied to every transcript so both captions and the
/// detection lexicon read the corrected text.
fn correct_known_mishears(units: &mut [CaptionUnit], language: Language) {
    let map = corrections(language);
    if map.is_empty() {
        return;
    }
    for u in units.iter_mut() {
        let core: String = u.text.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
        if core.is_empty() {
            continue;
        }
        let lower = core.to_lowercase();
        for &(wrong, right) in map {
            if wrong == lower.as_str() {
                u.text = u.text.replacen(&core, right, 1);
                break;
            }
        }
    }
}

/// A whisper model kept resident on the GPU so a *batch* of ranges transcribes
/// with one model load instead of reloading per range (ADR 0002/0007: detection
/// refine transcribes ~N candidate Moments). Loading the model is the expensive
/// step; each [`Transcriber::transcribe`] creates a fresh, cheap state.
///
/// GPU discipline: hold one of these only while transcription owns the VRAM -
/// drop it before the LLM stage loads (M4), since the 8 GB card stages stages
/// strictly sequentially.
pub struct Transcriber {
    ctx: WhisperContext,
}

impl Transcriber {
    /// Load `large-v3` on the GPU with DTW alignment heads (ADR 0003). The
    /// default heuristic token times drift by a few hundred ms — exactly the
    /// word-sync wobble — so we align each token to the audio via DTW. Use this
    /// for caption work (the promote path).
    pub fn load(model: &Path) -> Result<Self> {
        Self::load_inner(model, true)
    }

    /// Load without DTW alignment — for bulk *text-only* passes (detection
    /// refine reads the excitement lexicon, not word timing). Besides being
    /// faster, this avoids whisper's DTW median-filter assertion
    /// (`filter_width < a->ne[2]`), which aborts the process on a sparse,
    /// few-token window — common when scanning many arbitrary candidate ranges,
    /// some of which are music/SFX with almost no speech.
    pub fn load_text_only(model: &Path) -> Result<Self> {
        Self::load_inner(model, false)
    }

    fn load_inner(model: &Path, dtw: bool) -> Result<Self> {
        // Route whisper.cpp/ggml logging through `tracing` so its verbose
        // per-token DEBUG dump is dropped by the app's `info` filter rather than
        // flooding stderr (that flood also slowed transcription badly). Once-
        // guarded inside whisper-rs, so calling it on every load is free.
        whisper_rs::install_logging_hooks();
        let mut cparams = WhisperContextParameters::default();
        cparams.use_gpu(true);
        if dtw {
            cparams.dtw_parameters(DtwParameters {
                mode: DtwMode::ModelPreset { model_preset: DtwModelPreset::LargeV3 },
                ..Default::default()
            });
        }
        let ctx = WhisperContext::new_with_params(model, cparams)
            .with_context(|| format!("loading whisper model {}", model.display()))?;
        Ok(Self { ctx })
    }

    /// Transcribe one range's 16 kHz mono f32 samples into animatable caption
    /// units, reusing the resident model. Timestamps are 0-based to the range.
    ///
    /// `should_abort` is currently **inert**: whisper's abort hook collapses GPU
    /// throughput (see the note in the body), so it is not installed. It is kept
    /// in the signature so a graph-safe cancel can be re-wired without touching
    /// callers; for now cancellation happens between candidates in the detect loop.
    pub fn transcribe(
        &self,
        samples: &[f32],
        language: Language,
        should_abort: impl FnMut() -> bool + 'static,
    ) -> Result<Transcript> {
        let mut state = self.ctx.create_state().context("creating whisper state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(lang_code(language)));
        params.set_token_timestamps(true); // populate per-token t0/t1 for word timing
        params.set_translate(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_print_special(false);
        // NOTE: we deliberately do NOT install whisper's abort callback here.
        // Installing it collapsed whisper's GPU throughput to a crawl (a
        // hung-looking ~5% util detect) under concurrent desktop GPU load -
        // almost certainly because the abort hook forces per-op synchronization /
        // disables ggml-cuda graph batching. The LLM judge (no abort hook, keeps
        // CUDA graphs) stays fast under the same load, which isolates the hook as
        // the cause. Cancellation therefore falls back to the detect loop's
        // per-candidate `cancel.is_cancelled()` check (between candidates, not
        // mid-transcription). `should_abort` stays in the signature so a
        // graph-safe abort can be re-wired later without touching callers.
        let _ = should_abort;

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
                // Prefer the DTW-aligned time; fall back to the heuristic t0/t1
                // when DTW produced no value for this token (t_dtw == -1). DTW
                // gives a single aligned point per token, so a word's span runs
                // from its first token's time to its last — the caption builders
                // handle the (zero-width) single-token case.
                let (t0, t1) = if data.t_dtw >= 0 {
                    (data.t_dtw, data.t_dtw)
                } else {
                    (data.t0, data.t1)
                };
                raw_tokens.push((text, t0 as f64 / 100.0, t1 as f64 / 100.0));
            }
        }

        let mut units = group_into_words(raw_tokens);
        correct_known_mishears(&mut units, language);
        Ok(Transcript { language, units })
    }
}

/// Load the model, transcribe one range, and drop the model — the one-shot path
/// for the Promote pipeline (M1/M2). Detection refine loads a [`Transcriber`]
/// once and reuses it across the candidate batch instead (ADR 0007).
pub fn transcribe_range(
    model: &Path,
    samples: &[f32],
    language: Language,
    should_abort: impl FnMut() -> bool + 'static,
) -> Result<Transcript> {
    Transcriber::load(model)?.transcribe(samples, language, should_abort)
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

    #[test]
    fn corrects_known_mishears_whole_word_case_insensitively() {
        let mk = |t: &str| CaptionUnit { text: t.into(), start_s: 0.0, end_s: 0.4 };
        let mut units = vec![mk("Bocal"), mk("bocal,"), mk("lokal")];
        correct_known_mishears(&mut units, Language::Id);
        assert_eq!(units[0].text, "bocil"); // case-insensitive match
        assert_eq!(units[1].text, "bocil,"); // trailing punctuation preserved
        assert_eq!(units[2].text, "lokal"); // not a key - untouched
        // Other languages have no ID corrections.
        let mut en = vec![mk("bocal")];
        correct_known_mishears(&mut en, Language::En);
        assert_eq!(en[0].text, "bocal");
    }
}
