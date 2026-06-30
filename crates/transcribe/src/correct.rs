//! LLM caption-correction pass (ADR 0030): a **curated-only**, context-aware
//! repair of whisper's caption units, run after the deterministic dialect dict and
//! before caption timing. It targets the one *linguistic* error class the global
//! dict cannot fix safely: a word whisper transcribes that is a **real word** but
//! the streamer meant as **slang or a name** (`cowok -> cok`, `tidur -> tur`,
//! `teh -> eh`). A blind global rule would corrupt every genuine `cowok`/`teh`; the
//! LLM applies the operator's confirmed correction *only where the context fits*.
//!
//! It deliberately does **nothing else**. An earlier version also let the model
//! *guess* a fix for any low-confidence garble and collapse repeated fillers — but
//! the operator's real-render A/B showed both hurt: the model can't hear the audio,
//! so on this streamer's names/slang it guessed confident-but-wrong (`buntur`, the
//! guest "Guntur", became "buntut"; `dakenyang` -> "dakanya"), and collapsing ate
//! real repeated shock-reactions ("eh eh eh"). So garbles are left for the
//! **harvest -> operator-curate -> dict** loop (accurate, the operator is ground
//! truth), repeated fillers are left for the timing pass (which already hides the
//! zero-width hallucination pile), and the LLM only applies confirmed overrides.
//!
//! Transport is **index-anchored**: each unit is sent as `N: word`, the model
//! replies `N: word`, and a change maps back by index — so a unit's DTW timing is
//! preserved and a rephrased reply can't desync the mapping. The model never adds,
//! splits, or deletes words; a confirmed multi-word expansion (`dakenyang ->
//! "dah kenyang"`) is the **dict's** job (applied before this pass), never the LLM's.
//! The inference runs out-of-process in `yc-llm-judge --correct` (whisper.cpp and
//! llama.cpp can't co-link, ADR 0010); this module owns only the pure pieces —
//! building the request and applying the reply — unit-tested without a model/GPU.

use std::collections::HashMap;

use yc_core::{CaptionUnit, Language};

use crate::DialectLexicon;

/// The system instruction for the correction model: apply the confirmed list, touch
/// nothing else. Directive + few-shot — a timid wording made the model echo the
/// input, and an open-ended one made it invent (the over-correction the operator
/// caught). [`tests`] assert the load-bearing clauses survive.
pub const SYSTEM: &str = "\
You fix automatic speech-to-text captions of a live game-streamer by applying a short list of confirmed corrections. The streamer's words are listed one per line as \"N: word\".

Rules:
- The list below gives confirmed corrections for this streamer as wrong -> right. For each numbered word that matches a \"wrong\" form and fits that meaning in context, output its \"right\" form instead.
- Apply a correction ONLY where the context fits: if a word matches a \"wrong\" form but clearly means the ordinary thing here, leave it.
- Leave EVERY other word exactly as written.
- Never add, invent, translate, split, merge, or delete words.
- Output exactly one \"N: word\" line per input line, with the same numbers and nothing else.

Example
Confirmed corrections: \"cowok\" -> \"cok\"; \"tidur\" -> \"tur\"
Input:
1: semua
2: cowok
3: pergi
4: tidur
Output:
1: semua
2: cok
3: pergi
4: tur";

/// Topic / language context for the correction model, woven into the user turn so
/// it knows the domain (which slang/names to expect) without it ever reaching
/// whisper — unlike whisper's `initial_prompt`, a chat model takes a descriptive
/// topic line without hallucinating boilerplate.
#[derive(Debug, Clone)]
pub struct CorrectionContext {
    pub language: Language,
    /// Free text: the clip's generated title and/or the store's `note`. May be empty.
    pub topic: String,
}

/// The two prompt strings handed to the `yc-llm-judge --correct` sidecar. The app
/// shells out with these (the prompt rides in the request, ADR 0030); the model's
/// raw reply comes back to [`apply_correction`].
#[derive(Debug, Clone, PartialEq)]
pub struct CorrectionRequest {
    pub system: String,
    pub user: String,
}

/// What [`apply_correction`] changed — for the render log and the diag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CorrectionStats {
    /// Confirmed context overrides the model applied (`cowok -> cok`).
    pub applied: usize,
    /// Model edits refused by the guardrail (a change that isn't a confirmed
    /// override, or a delete) — the word was kept verbatim.
    pub rejected: usize,
}

impl CorrectionStats {
    /// One-line summary for the render log.
    pub fn summary(&self) -> String {
        format!("{} applied, {} rejected", self.applied, self.rejected)
    }
}

/// The full language name for the prompt (whisper's two-letter code is too terse).
fn lang_full(l: Language) -> &'static str {
    match l {
        Language::En => "English",
        Language::Id => "Indonesian (casual, with local slang)",
        Language::Ja => "Japanese",
    }
}

/// The alphanumeric core of a unit's text — its leading/trailing punctuation
/// trimmed (internal hyphens, e.g. `kanan-kanan`, are kept). Empty for a
/// pure-punctuation unit.
fn core(text: &str) -> &str {
    text.trim_matches(|c: char| !c.is_alphanumeric())
}

/// Lowercased, alphanumeric-only form for a separator/case-insensitive comparison —
/// so the model reformatting a word's separators (`kanan-kanan` -> `kanan_kanan`) or
/// case reads as *no change* rather than a corrupting edit.
fn alnum_lower(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Build the correction request, or `None` when there is nothing to do — i.e. the
/// store has no confirmed **context** overrides (the only thing this pass applies),
/// so the LLM round-trip would be a pure echo. A clean clip therefore skips the GPU.
pub fn build_correction_request(
    units: &[CaptionUnit],
    lexicon: &DialectLexicon,
    ctx: &CorrectionContext,
) -> Option<CorrectionRequest> {
    if units.is_empty() {
        return None;
    }
    let overrides = lexicon.context_overrides();
    if overrides.is_empty() {
        return None;
    }

    let mut user = String::new();
    let topic = ctx.topic.trim();
    if !topic.is_empty() {
        user.push_str(&format!("Stream: {topic}\n"));
    }
    user.push_str(&format!("Language: {}.\n", lang_full(ctx.language)));
    user.push_str("Confirmed corrections (apply ONLY where the context fits):\n");
    for (w, r) in &overrides {
        user.push_str(&format!("- \"{w}\" -> \"{r}\"\n"));
    }
    user.push_str("\nWords:\n");
    for (i, u) in units.iter().enumerate() {
        let c = core(&u.text);
        let word = if c.is_empty() { u.text.as_str() } else { c };
        user.push_str(&format!("{}: {word}\n", i + 1));
    }
    user.push_str("\nReply with the corrected list, one \"N: word\" per line, same numbers, nothing else.");

    Some(CorrectionRequest { system: SYSTEM.to_string(), user })
}

/// One per-index instruction parsed from the model's reply (absent index = keep).
enum Decision {
    Replace(String),
    Delete,
}

/// Parse the model's reply into per-index (0-based) decisions. Each acted-on line
/// starts with the unit number; `N: word` is a replacement, `N: DELETE` a drop
/// (which [`apply_correction`] refuses, since this pass never deletes). Lines
/// without a leading number (prose, code fences, blanks) are ignored, and only the
/// first whitespace-token after the number is taken — so the model cannot smuggle in
/// extra words ("no adding").
fn parse_decisions(raw: &str) -> HashMap<usize, Decision> {
    let mut out = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        let digits = line.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            continue;
        }
        let Ok(n) = line[..digits].parse::<usize>() else { continue };
        if n == 0 {
            continue;
        }
        let rest = line[digits..]
            .trim_start_matches(|c: char| c == ':' || c == '.' || c == ')' || c == '-' || c.is_whitespace())
            .trim();
        let Some(tok) = rest.split_whitespace().next() else { continue };
        if tok.eq_ignore_ascii_case("DELETE") {
            out.insert(n - 1, Decision::Delete);
            continue;
        }
        let word = core(tok);
        if !word.is_empty() {
            out.insert(n - 1, Decision::Replace(word.to_string()));
        }
    }
    out
}

/// Apply the model's raw reply onto `units` in place, accepting a change **only**
/// when it is a confirmed context override the operator curated (ADR 0030). A
/// replacement that isn't in the override set is refused (the model never
/// free-invents); a deletion is refused (this pass never drops words); a
/// separator/case-only rewrite is a no-op. A replacement keeps the unit's original
/// surrounding punctuation and its DTW timing. Call **before** caption timing.
pub fn apply_correction(
    units: &mut [CaptionUnit],
    lexicon: &DialectLexicon,
    raw: &str,
) -> CorrectionStats {
    let decisions = parse_decisions(raw);
    if decisions.is_empty() {
        return CorrectionStats::default();
    }
    // The confirmed (wrong_lower -> right_lower) override set — the only edits allowed.
    let overrides: std::collections::HashSet<(String, String)> = lexicon
        .context_overrides()
        .into_iter()
        .map(|(w, r)| (w, r.to_lowercase()))
        .collect();

    let mut stats = CorrectionStats::default();
    for (i, u) in units.iter_mut().enumerate() {
        match decisions.get(&i) {
            None => {}
            Some(Decision::Delete) => stats.rejected += 1, // this pass never deletes
            Some(Decision::Replace(new)) => {
                let c = core(&u.text).to_string();
                if c.is_empty() || alnum_lower(new) == alnum_lower(&c) {
                    continue; // echo / punctuation-only / separator rewrite -> no change
                }
                if overrides.contains(&(c.to_lowercase(), new.to_lowercase())) {
                    u.text = u.text.replacen(&c, new, 1);
                    stats.applied += 1;
                } else {
                    stats.rejected += 1; // not a curated override -> protect the word
                }
            }
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Correction;

    fn unit(text: &str) -> CaptionUnit {
        CaptionUnit { text: text.into(), start_s: 0.0, end_s: 0.4 }
    }

    /// A lexicon with `cowok -> cok` and `tidur -> tur` as **context** overrides.
    fn slang_lex() -> DialectLexicon {
        DialectLexicon {
            corrections: vec![
                Correction { wrong: "cowok".into(), right: "cok".into(), context: true, ..Default::default() },
                Correction { wrong: "tidur".into(), right: "tur".into(), context: true, ..Default::default() },
            ],
            ..Default::default()
        }
    }

    fn ctx() -> CorrectionContext {
        CorrectionContext { language: Language::Id, topic: "Horror game w/ Guntur".into() }
    }

    #[test]
    fn system_prompt_keeps_the_guards() {
        let s = SYSTEM.to_lowercase();
        assert!(s.contains("never add") || s.contains("never invent"));
        assert!(s.contains("never") && s.contains("translate"));
        assert!(s.contains("only where the context fits"), "must gate on context");
        assert!(s.contains("exactly as written"), "must protect other words");
    }

    #[test]
    fn request_lists_overrides_and_plain_numbered_words() {
        let units = vec![unit("semua"), unit("cowok"), unit("tidur")];
        let req = build_correction_request(&units, &slang_lex(), &ctx()).expect("some request");
        assert!(req.user.contains("Horror game w/ Guntur"));
        assert!(req.user.contains("Indonesian"));
        assert!(req.user.contains("\"cowok\" -> \"cok\""));
        assert!(req.user.contains("2: cowok\n") && req.user.contains("3: tidur\n"));
        assert!(!req.user.contains("[?]"), "no confidence marks in the curated-only request");
    }

    #[test]
    fn request_is_none_without_context_overrides() {
        // No context overrides -> the LLM would be a pure echo; skip it. (A blank
        // store, or one with only global/garble corrections.)
        let units = vec![unit("halo"), unit("dunia")];
        assert!(build_correction_request(&units, &DialectLexicon::default(), &ctx()).is_none());
        let global_only = DialectLexicon {
            corrections: vec![Correction { wrong: "dimalai-malai".into(), right: "dimarahin".into(), ..Default::default() }],
            ..Default::default()
        };
        assert!(build_correction_request(&units, &global_only, &ctx()).is_none());
    }

    #[test]
    fn applies_a_curated_override_in_context() {
        let mut units = vec![unit("semua"), unit("cowok"), unit("tidur")];
        let raw = "1: semua\n2: cok\n3: tur\n";
        let stats = apply_correction(&mut units, &slang_lex(), raw);
        assert_eq!(units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["semua", "cok", "tur"]);
        assert_eq!(stats.applied, 2);
        assert_eq!(stats.rejected, 0);
    }

    #[test]
    fn leaves_a_word_the_model_keeps() {
        // The model judges context and leaves a genuine "cowok" (means "boy" here).
        let mut units = vec![unit("dia"), unit("cowok")];
        let stats = apply_correction(&mut units, &slang_lex(), "1: dia\n2: cowok\n");
        assert_eq!(units[1].text, "cowok");
        assert_eq!(stats.applied, 0);
    }

    #[test]
    fn refuses_a_non_curated_change() {
        // The model proposing a change that is NOT a confirmed override is refused —
        // the load-bearing guard against guessing (buntur->buntut, the operator's bug).
        let mut units = vec![unit("buntur"), unit("dakenyang")];
        let stats = apply_correction(&mut units, &slang_lex(), "1: buntut\n2: dakanya\n");
        assert_eq!(units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["buntur", "dakenyang"]);
        assert_eq!(stats.applied, 0);
        assert_eq!(stats.rejected, 2);
    }

    #[test]
    fn refuses_a_deletion() {
        // This pass never drops a word (collapsing real repeated reactions was wrong).
        let mut units = vec![unit("eh"), unit("eh"), unit("eh")];
        let stats = apply_correction(&mut units, &slang_lex(), "1: eh\n2: DELETE\n3: DELETE\n");
        assert_eq!(units.len(), 3, "no unit dropped");
        assert_eq!(stats.rejected, 2);
    }

    #[test]
    fn a_separator_only_rewrite_is_a_no_op() {
        // Even if "kanan-kanan" were a curated wrong, a separator rewrite isn't a real
        // change; and here it isn't curated at all, so doubly a no-op.
        let mut units = vec![unit("kanan-kanan")];
        let stats = apply_correction(&mut units, &slang_lex(), "1: kanan_kanan\n");
        assert_eq!(units[0].text, "kanan-kanan");
        assert_eq!(stats.applied, 0);
    }

    #[test]
    fn replacement_preserves_punctuation_and_timing() {
        let mut units = vec![CaptionUnit { text: "cowok,".into(), start_s: 10.1, end_s: 10.9 }];
        apply_correction(&mut units, &slang_lex(), "1: cok\n");
        assert_eq!(units[0].text, "cok,", "trailing punctuation kept");
        assert!((units[0].start_s - 10.1).abs() < 1e-9 && (units[0].end_s - 10.9).abs() < 1e-9);
    }

    #[test]
    fn a_missing_or_unparseable_line_keeps_the_word() {
        let mut units = vec![unit("cowok"), unit("tidur"), unit("semua")];
        let raw = "1: cok\n(model rambles here)\n3: semua\n"; // index 2 (tidur) omitted
        let stats = apply_correction(&mut units, &slang_lex(), raw);
        assert_eq!(units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["cok", "tidur", "semua"]);
        assert_eq!(stats.applied, 1);
    }

    #[test]
    fn extra_words_in_a_reply_line_are_clipped_to_one() {
        let mut units = vec![unit("cowok")];
        apply_correction(&mut units, &slang_lex(), "1: cok banget lah\n");
        assert_eq!(units[0].text, "cok"); // only the first token is taken; "never add"
    }
}
