//! LLM judgment Signal (ADR 0010, ADR 0002): a local GGUF language model reads
//! each refine candidate's transcript and scores how clip-worthy the *streamer's*
//! speech is. It is the semantic counterpart to the lexicon (which only counts
//! excitement words) - and, critically, it is told to discount scripted in-game
//! dialogue / cutscene narration, so a dramatic game line (ADR 0009's #11
//! anti-signal: arousal 1.107, streamer silent) cannot inflate its score the way
//! it inflates the non-semantic signals.
//!
//! Per candidate the model runs one inference (greedy, temp 0, GBNF-constrained
//! to a tiny JSON object), yielding a 0-10 score; the scores are z-scored across
//! the candidate set and weighted into `combined_score` exactly like the lexicon
//! and arousal. The model reads only the transcript the refine pass already built
//! - no audio - plus the corroborating per-candidate signals as context.
//!
//! **The inference does not run here.** whisper.cpp and llama.cpp each vendor
//! their own `ggml`, which cannot co-link into one binary (duplicate symbols), so
//! the actual llama.cpp call lives in the separate `yc-llm-judge` binary, which
//! links llama only (ADR 0010). The app shells out to it, passing a
//! [`JudgeRequest`] over stdin and reading back a `Vec<`[`JudgeVerdict`]`>`. This
//! module owns the pieces both sides share: the prompt + the load-bearing
//! mitigation, the lenient output parser, the z-score `apply`, and the IPC
//! structs - all pure and unit-tested, so the mitigation is verifiable without a
//! model or a GPU.

use crate::{combined_score, score, Weights};
use serde::{Deserialize, Serialize};
use yc_core::{Language, Moment};

/// Clip-worthiness rubric ceiling - the model emits an integer in `[0, SCORE_MAX]`.
pub const SCORE_MAX: u32 = 10;

/// The system instruction: the rubric **and** the load-bearing game-narration
/// mitigation (ADR 0009/0010). A regression here silently un-protects the #11
/// scripted-cutscene anti-signal, so [`tests`] assert its key clauses survive.
pub const SYSTEM: &str = "\
You rate moments from a live game-streaming VOD for short-form clip potential, one moment at a time, from a transcript of the streamer's audio.

Score 0-10 how clip-worthy the STREAMER'S OWN reaction is:
- 10: a peak genuine reaction - a big laugh, shock, hype, rage, a clutch play or a funny line.
- 5: a mild reaction or moderately interesting talk.
- 0: mundane talk, menu/UI reading, or nothing notable.

CRITICAL: the transcript is the loudest voice in a MIXED game+microphone recording, so it may be scripted in-game dialogue or cutscene narration rather than the streamer. Score the STREAMER, not the game. A dramatic, emotional or shocking line that is clearly scripted game/cutscene narration is NOT clip-worthy on its own - score it low unless the streamer is audibly reacting to it. Use the provided signals to corroborate: high audience-chat and high vocal-arousal alongside reaction-like words point to a real streamer moment; dramatic words with flat arousal and no chat point to scripted game audio.

Reply with ONLY a JSON object: {\"score\": <integer 0-10>, \"reason\": \"<at most 12 words>\"}.";

/// GBNF grammar constraining generation to `{"score": <0-10>, "reason": "<text>"}`
/// so a local 7B emits a parseable object instead of free prose (ADR 0010). The
/// reason forbids `"`/`\` to dodge JSON-escape edge cases; length is bounded by
/// the inference's token cap, not the grammar. [`parse_output`] is lenient enough
/// to also handle output produced without this grammar (the documented fallback).
pub const GRAMMAR: &str = r#"root   ::= "{\"score\": " score ", \"reason\": \"" reason "\"}"
score  ::= "10" | [0-9]
reason ::= [^"\\]*"#;

/// The corroborating per-candidate context fed to the model so it judges by more
/// than the words alone (the ADR 0010 mitigation). These are the z-scored signals
/// already written onto the Moment by the time the LLM stage runs (`>0` means
/// above this VOD's average); any may be absent (no chat, or `ser` not built).
#[derive(Debug, Clone, Copy, Default)]
pub struct Context {
    pub chat_z: Option<f32>,
    pub loudness_z: Option<f32>,
    pub arousal_z: Option<f32>,
}

fn fz(o: Option<f32>) -> String {
    o.map(|v| format!("{v:+.2}")).unwrap_or_else(|| "n/a".into())
}

fn lang_name(l: Language) -> &'static str {
    match l {
        Language::En => "English",
        Language::Id => "Bahasa Indonesia",
        Language::Ja => "Japanese",
    }
}

/// Build the user-turn prompt for one candidate: the corroborating signals plus
/// the transcript. Pure + unit-tested so the mitigation context is verifiable
/// without the model. The system instruction ([`SYSTEM`]) carries the rubric.
pub fn build_prompt(transcript: &str, ctx: &Context, language: Language) -> String {
    let body = transcript.trim();
    let body = if body.is_empty() { "(no speech transcribed)" } else { body };
    format!(
        "Transcript language: {lang}.\n\
         Corroborating signals (z-scored across this VOD's candidates; >0 is above average):\n\
         - audience chat rate: {chat}\n\
         - loudness: {loud}\n\
         - streamer vocal arousal: {arou}\n\n\
         Transcript:\n\"\"\"\n{body}\n\"\"\"\n\n\
         Score this moment.",
        lang = lang_name(language),
        chat = fz(ctx.chat_z),
        loud = fz(ctx.loudness_z),
        arou = fz(ctx.arousal_z),
        body = body,
    )
}

/// First integer in `[0, SCORE_MAX]` appearing in `s` (the lenient fallback when
/// JSON parsing fails - e.g. the grammar was unavailable and the model rambled).
fn first_score(s: &str) -> Option<f32> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if let Ok(n) = s[start..i].parse::<u32>() {
                if n <= SCORE_MAX {
                    return Some(n as f32);
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Parse the model's output into `(score in [0, SCORE_MAX], reason)`. Prefers the
/// grammar-constrained JSON object; falls back to the first 0-10 integer in the
/// text (reason = the whole trimmed text) so a malformed generation still yields
/// a usable score rather than aborting the whole detect run.
pub fn parse_output(raw: &str) -> (f32, String) {
    let clamp = |s: f32| s.clamp(0.0, SCORE_MAX as f32);
    if let Some(start) = raw.find('{') {
        if let Some(rel_end) = raw[start..].rfind('}') {
            let obj = &raw[start..=start + rel_end];
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(obj) {
                let score = v
                    .get("score")
                    .and_then(|s| s.as_f64().map(|f| f as f32))
                    .or_else(|| v.get("score").and_then(|s| s.as_str()).and_then(|s| s.parse().ok()));
                if let Some(score) = score {
                    let reason =
                        v.get("reason").and_then(|r| r.as_str()).unwrap_or("").trim().to_string();
                    return (clamp(score), reason);
                }
            }
        }
    }
    (clamp(first_score(raw).unwrap_or(0.0)), raw.trim().to_string())
}

/// Fill the `llm` Signal on candidates from their per-candidate raw scores and
/// rerank - the analogue of [`crate::lexicon::apply`] and [`crate::arousal::apply`].
/// The LLM has no VOD-wide baseline (it is only run on candidates), so raw scores
/// are z-scored across the candidate set, then `combined_score` recomputes each
/// rank with the LLM judgment now present. `scores[i]` must correspond to
/// `moments[i]`.
pub fn apply(moments: &mut [Moment], scores: &[f32], weights: &Weights) {
    debug_assert_eq!(moments.len(), scores.len(), "one llm score per Moment");
    let z = score::robust_z(scores);
    for (m, lz) in moments.iter_mut().zip(z) {
        m.signals.llm = Some(lz);
        m.score = combined_score(&m.signals, weights);
    }
}

// --- IPC protocol with the `yc-llm-judge` sidecar binary (ADR 0010) ----------
// whisper.cpp and llama.cpp can't co-link, so the inference runs out-of-process.
// The app serializes a `JudgeRequest` to the child's stdin and reads back a
// `Vec<JudgeVerdict>` (one per candidate, in order) from its stdout.

/// One candidate to judge: the transcript plus its corroborating z-scored signals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeCandidate {
    pub transcript: String,
    pub chat_z: Option<f32>,
    pub loudness_z: Option<f32>,
    pub arousal_z: Option<f32>,
}

impl JudgeCandidate {
    /// The corroborating [`Context`] for this candidate's prompt.
    pub fn context(&self) -> Context {
        Context { chat_z: self.chat_z, loudness_z: self.loudness_z, arousal_z: self.arousal_z }
    }
}

/// The whole candidate batch for one detect run, sent to the judge over stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeRequest {
    /// Absolute path to the GGUF the judge should load (operator-selectable per
    /// ADR 0002; the app passes its configured default).
    pub model_path: String,
    pub language: Language,
    pub candidates: Vec<JudgeCandidate>,
}

/// One candidate's verdict, returned by the judge in request order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeVerdict {
    /// Raw clip-worthiness in `[0, SCORE_MAX]` (z-scored by [`apply`] afterward).
    pub score: f32,
    /// One-line rationale for the review UI.
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use yc_core::{Signals, TimeRange};

    #[test]
    fn system_prompt_keeps_the_load_bearing_mitigation() {
        // If any of these clauses is edited away, the #11 scripted-cutscene
        // anti-signal (ADR 0009) is silently un-protected. Guard them.
        let s = SYSTEM.to_lowercase();
        assert!(s.contains("streamer"), "must anchor on the streamer");
        assert!(s.contains("scripted") && s.contains("cutscene"), "must name scripted game audio");
        assert!(s.contains("not clip-worthy"), "must say scripted drama is not clip-worthy");
        assert!(s.contains("corroborate") || s.contains("signals"), "must use the signals");
        assert!(SYSTEM.contains("\"score\""), "must request the JSON score field");
    }

    #[test]
    fn build_prompt_embeds_transcript_and_signals() {
        let ctx = Context { chat_z: Some(1.8), loudness_z: Some(-0.3), arousal_z: None };
        let p = build_prompt("Kaget mampus", &ctx, Language::Id);
        assert!(p.contains("Kaget mampus"));
        assert!(p.contains("Bahasa Indonesia"));
        assert!(p.contains("+1.80")); // chat z, signed
        assert!(p.contains("-0.30")); // loudness z, signed
        assert!(p.contains("n/a")); // arousal absent
    }

    #[test]
    fn build_prompt_handles_empty_transcript() {
        let p = build_prompt("   ", &Context::default(), Language::En);
        assert!(p.contains("(no speech transcribed)"));
    }

    #[test]
    fn parse_output_reads_clean_json() {
        let (s, r) = parse_output(r#"{"score": 8, "reason": "big laugh"}"#);
        assert_eq!(s, 8.0);
        assert_eq!(r, "big laugh");
    }

    #[test]
    fn parse_output_accepts_string_score_and_surrounding_prose() {
        let (s, r) = parse_output("Sure! {\"score\": \"3\", \"reason\": \"menu reading\"} done");
        assert_eq!(s, 3.0);
        assert_eq!(r, "menu reading");
    }

    #[test]
    fn parse_output_falls_back_to_first_integer() {
        let (s, r) = parse_output("I'd say 7 out of 10, lots of hype");
        assert_eq!(s, 7.0); // first 0-10 integer (the 7, not the 10)
        assert!(r.contains("hype"));
    }

    #[test]
    fn parse_output_clamps_and_defaults() {
        assert_eq!(parse_output(r#"{"score": 99, "reason": "x"}"#).0, SCORE_MAX as f32);
        assert_eq!(parse_output("no number here").0, 0.0);
    }

    #[test]
    fn apply_sets_llm_and_reranks_by_judgment() {
        let w = Weights { chat: 0.0, loudness: 0.0, lexicon: 0.0, arousal: 0.0, llm: 1.0 };
        let mk = |id, signals| Moment {
            id,
            range: TimeRange { start_s: 0.0, end_s: 30.0 },
            signals,
            score: 0.0,
        };
        let base = Signals { chat_rate: Some(1.0), loudness: Some(1.0), ..Default::default() };
        let mut moments = vec![mk(1, base), mk(2, base)];
        // Moment 2 is judged far more clip-worthy than Moment 1.
        apply(&mut moments, &[2.0, 8.0], &w);
        assert!(moments[0].signals.llm.unwrap() < moments[1].signals.llm.unwrap());
        assert!(moments[1].score > moments[0].score);
    }

    #[test]
    fn judge_request_roundtrips_through_json() {
        let req = JudgeRequest {
            model_path: "models/x.gguf".into(),
            language: Language::Id,
            candidates: vec![JudgeCandidate {
                transcript: "Gila".into(),
                chat_z: Some(1.0),
                loudness_z: None,
                arousal_z: Some(0.5),
            }],
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: JudgeRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.candidates[0].transcript, "Gila");
        assert_eq!(back.candidates[0].context().arousal_z, Some(0.5));
    }
}
