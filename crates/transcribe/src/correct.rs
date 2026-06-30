//! LLM caption-correction pass (ADR 0030): a confidence-aware, dialect-store-fed
//! repair of whisper's caption units, run **after** the deterministic dialect dict
//! and **before** caption timing. It targets the *linguistic* errors no audio
//! processing fixes — garbles, local slang, and viewer/streamer names — that the
//! operator reported as "too much error".
//!
//! The model **cannot hear the audio**, so the whole design guardrails against
//! over-correction (ADR 0027/0030): the model could "fix" a garble to a confident-
//! but-wrong word and make captions worse. Hence the hybrid:
//!
//! - A word whisper was **unsure** of (confidence below [`CORRECT_UNSURE_P`]) the
//!   model may rewrite to coherent text, or drop if clearly spurious. These are
//!   already wrong/garbled, so a coherent guess rarely makes them worse.
//! - A word whisper was **confident** about is kept verbatim **unless** it matches
//!   a curated context override the operator confirmed in the dialect store
//!   (`cowok -> cok`, `tidur -> tur`): a real word the streamer meant as slang or a
//!   name. The model applies these *in context* — what a blind global dict cannot
//!   do safely (it would corrupt every real `cowok`). The model never free-invents
//!   over a confident word.
//! - **No words are added** (inventing a whisper-missed word *is* the hallucination
//!   risk); a spurious word is dropped, not replaced with filler.
//!
//! The transport is **index-anchored**: each unit is sent as `N: word`, the model
//! replies `N: word` / `N: DELETE`, and corrections map back by index — so a unit's
//! DTW timing is preserved exactly and the model rephrasing the list cannot
//! desynchronise the mapping. The model inference itself runs out-of-process in
//! `yc-llm-judge --correct` (whisper.cpp and llama.cpp can't co-link, ADR 0010);
//! this module owns only the pure pieces — building the request and applying the
//! reply — so the guardrails are unit-tested without a model or a GPU.

use std::collections::{HashMap, HashSet};

use yc_core::{CaptionUnit, Language};

use crate::DialectLexicon;

/// A unit's confidence at/above this is treated as **confident** (protected: only a
/// curated context override may change it); below it, **unsure** (the model may
/// rewrite or drop it). The minimum token probability, the same signal the harvest
/// flags on — so a word the harvest would queue is also one the model may repair.
/// Tune-from-use against the operator's ground-truth.
pub const CORRECT_UNSURE_P: f32 = 0.50;

/// The system instruction for the correction model. Kept language-neutral (the
/// language and topic ride in the user turn) and tightly scoped to *repair*, never
/// rewrite — the load-bearing over-correction guard ([`tests`] assert its clauses).
/// Directive (a timid "only where it fits" wording made the model echo the input
/// unchanged) and few-shot, so the 7B reliably applies the curated terms, fixes the
/// obvious garbles, and collapses filler runs while leaving coherent words alone.
pub const SYSTEM: &str = "\
You repair automatic speech-to-text captions of a live game-streamer, word by word. The streamer's words are listed one per line as \"N: word\". A word marked [?] was low-confidence.

Make these repairs and no others:
1. A [?] word may be mis-heard. If it looks garbled or makes no sense beside its neighbours, replace it with the word the streamer most likely said (SAME language, never translated). If it already reads as a sensible word in context, leave it.
2. Apply every listed slang/name correction to its word - they are confirmed for this streamer.
3. Reply \"N: DELETE\" only for a word that is plainly a stray sound and does not belong; when in doubt, keep it.
4. Leave every other word exactly as written.

Never add, invent, or split words. Output exactly one line per input line - \"N: word\" or \"N: DELETE\" - with the same numbers and nothing else.

Example
slang/name: \"maen\" -> \"main\"
Input:
1: harus [?]
2: maen [?]
3: skarank [?]
4: kita
Output:
1: harus
2: main
3: sekarang
4: kita";

/// Topic / language context for the correction model, woven into the user turn so
/// the model knows the domain (slang, names) without it ever reaching whisper —
/// unlike whisper's `initial_prompt`, a chat model takes a descriptive topic line
/// without hallucinating boilerplate.
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

/// What [`apply_correction`] changed — for the render log and the diag, and to
/// assert the guardrails in tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CorrectionStats {
    /// Unsure words the model rewrote (the auto-fix).
    pub replaced_unsure: usize,
    /// Confident words a curated context override changed (`cowok -> cok`).
    pub replaced_curated: usize,
    /// Spurious unsure words dropped.
    pub deleted: usize,
    /// Model edits refused by a guardrail (a confident word it tried to change or
    /// drop without a curated override) — kept verbatim.
    pub rejected: usize,
}

impl CorrectionStats {
    /// Total edits actually applied (replacements + deletions).
    pub fn applied(&self) -> usize {
        self.replaced_unsure + self.replaced_curated + self.deleted
    }

    /// One-line summary for the render log.
    pub fn summary(&self) -> String {
        format!(
            "{} applied ({} unsure-fix, {} curated, {} dropped), {} rejected",
            self.applied(),
            self.replaced_unsure,
            self.replaced_curated,
            self.deleted,
            self.rejected,
        )
    }
}

/// The full language name for the prompt (whisper's two-letter code is too terse
/// for the chat model).
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

/// Whether unit `i` is unsure (confidence below the bar, or no confidence known).
fn is_unsure(conf: &[f32], i: usize) -> bool {
    conf.get(i).map_or(true, |&c| c < CORRECT_UNSURE_P)
}

/// Longest word (in chars) a repeated-run collapse will merge. whisper repetition-
/// hallucinations are short fillers ("eh", "ah", "oh", "ya"); a longer repeated word
/// is more likely real emphasis, so it is left alone.
const COLLAPSE_MAX_LEN: usize = 3;

/// Collapse each run of the same short adjacent filler word into one unit spanning
/// the run (its confidence shrinks to the run's least-sure). whisper repetition-
/// hallucinates short fillers on noisy audio — a 22-long "eh eh eh ..." run appeared
/// on clip-7 — and beyond looking bad, a long run makes the correction model miscount
/// the per-line index protocol. Collapsing in code is deterministic, removes the
/// spam, and keeps the list short so the model's line numbers stay aligned. Only
/// short words (<= [`COLLAPSE_MAX_LEN`] chars) merge, so word-level emphasis
/// ("asli asli asli") is preserved; reduplication is a single token (`kanan-kanan`),
/// never an adjacent pair, so it is untouched. `conf` shrinks in lockstep with
/// `units`. Run before the request is built (and, in the pipeline, before apply, on
/// the same units). Returns how many units were removed.
pub fn collapse_adjacent_duplicates(units: &mut Vec<CaptionUnit>, conf: &mut Vec<f32>) -> usize {
    if units.len() < 2 {
        return 0;
    }
    let mut out_u: Vec<CaptionUnit> = Vec::with_capacity(units.len());
    let mut out_c: Vec<f32> = Vec::with_capacity(units.len());
    let mut removed = 0;
    for (i, u) in units.iter().enumerate() {
        let ci = core(&u.text).to_lowercase();
        let short = !ci.is_empty() && ci.chars().count() <= COLLAPSE_MAX_LEN;
        if short {
            if let Some(last) = out_u.last_mut() {
                if core(&last.text).to_lowercase() == ci {
                    last.end_s = u.end_s; // absorb the repeat's span
                    if let Some(c) = out_c.last_mut() {
                        *c = c.min(conf.get(i).copied().unwrap_or(*c));
                    }
                    removed += 1;
                    continue;
                }
            }
        }
        out_u.push(u.clone());
        out_c.push(conf.get(i).copied().unwrap_or(1.0));
    }
    *units = out_u;
    *conf = out_c;
    removed
}

/// Build the correction request for these units, or `None` when the model would
/// have nothing to safely do — no units, or every word is confident **and** the
/// store has no context overrides (so the confident-word path is empty too). That
/// skip avoids a GPU round-trip on a clean clip.
pub fn build_correction_request(
    units: &[CaptionUnit],
    conf: &[f32],
    lexicon: &DialectLexicon,
    ctx: &CorrectionContext,
) -> Option<CorrectionRequest> {
    if units.is_empty() {
        return None;
    }
    let overrides = lexicon.context_overrides();
    let any_unsure = (0..units.len()).any(|i| is_unsure(conf, i));
    if overrides.is_empty() && !any_unsure {
        return None;
    }

    let mut user = String::new();
    let topic = ctx.topic.trim();
    if topic.is_empty() {
        user.push_str(&format!("Language: {}.\n", lang_full(ctx.language)));
    } else {
        user.push_str(&format!("Stream: {topic}\nLanguage: {}.\n", lang_full(ctx.language)));
    }
    if overrides.is_empty() {
        user.push_str("Slang/name corrections: none.\n\n");
    } else {
        user.push_str("Slang/name corrections (confirmed for this streamer):\n");
        for (w, r) in &overrides {
            user.push_str(&format!("- \"{w}\" -> \"{r}\"\n"));
        }
        user.push('\n');
    }
    user.push_str("Words:\n");
    for (i, u) in units.iter().enumerate() {
        let c = core(&u.text);
        let word = if c.is_empty() { u.text.as_str() } else { c };
        let mark = if is_unsure(conf, i) { " [?]" } else { "" };
        user.push_str(&format!("{}: {word}{mark}\n", i + 1));
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
/// starts with the unit number; `N: word` is a replacement, `N: DELETE` a drop.
/// Lines without a leading number (prose, code fences, blanks) are ignored, and
/// only the first whitespace-token after the number is taken as the word — so the
/// model cannot smuggle in extra words ("no adding").
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
            continue; // 1-based protocol; 0 is never a valid index
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

/// Apply the model's raw reply onto `units` in place, enforcing the confidence +
/// curated-override guardrails (ADR 0030). `conf` is the per-unit confidence
/// aligned 1:1 with `units` as passed (call this **before** caption timing, which
/// drops/clamps units). A replacement keeps the unit's original surrounding
/// punctuation and its DTW timing; a deletion removes the unit (its time absorbs
/// into neighbours when the timing pass gap-fills). Returns what changed.
pub fn apply_correction(
    units: &mut Vec<CaptionUnit>,
    conf: &[f32],
    lexicon: &DialectLexicon,
    raw: &str,
) -> CorrectionStats {
    let decisions = parse_decisions(raw);
    if decisions.is_empty() {
        return CorrectionStats::default();
    }
    // Curated context overrides as a (wrong_lower, right_lower) allow-set: the only
    // edits permitted over a *confident* word.
    let overrides: HashSet<(String, String)> = lexicon
        .context_overrides()
        .into_iter()
        .map(|(w, r)| (w, r.to_lowercase()))
        .collect();

    let mut stats = CorrectionStats::default();
    let mut out: Vec<CaptionUnit> = Vec::with_capacity(units.len());
    for (i, u) in units.iter().enumerate() {
        match decisions.get(&i) {
            None => out.push(u.clone()),
            Some(Decision::Replace(new)) => {
                let c = core(&u.text);
                let new_lc = new.to_lowercase();
                // A no-op echo, a punctuation-only unit, or a change only in
                // separators/case (the model rewriting "kanan-kanan" as
                // "kanan_kanan"): keep the original word untouched.
                if c.is_empty() || alnum_lower(new) == alnum_lower(c) {
                    out.push(u.clone());
                    continue;
                }
                let curated = overrides.contains(&(c.to_lowercase(), new_lc));
                if curated {
                    out.push(replaced(u, c, new));
                    stats.replaced_curated += 1;
                } else if is_unsure(conf, i) {
                    out.push(replaced(u, c, new));
                    stats.replaced_unsure += 1;
                } else {
                    out.push(u.clone()); // confident, non-curated -> protect
                    stats.rejected += 1;
                }
            }
            Some(Decision::Delete) => {
                // Drop an unsure word the model flags, or a back-to-back repeat
                // (the "eh eh eh" hallucination run — confident yet clearly filler).
                // A confident, non-repeated word is protected (it might be real).
                if is_unsure(conf, i) || adjacent_duplicate(units, i) {
                    stats.deleted += 1; // drop (skip pushing)
                } else {
                    out.push(u.clone());
                    stats.rejected += 1;
                }
            }
        }
    }
    *units = out;
    stats
}

/// A copy of `u` with its core word replaced by `new`, preserving the original
/// surrounding punctuation and timing (same splice as the dialect dict's fix-up).
fn replaced(u: &CaptionUnit, c: &str, new: &str) -> CaptionUnit {
    CaptionUnit { text: u.text.replacen(c, new, 1), start_s: u.start_s, end_s: u.end_s }
}

/// Whether unit `i` is a back-to-back repeat of the unit immediately before or
/// after it (same core word, case-insensitive) — the signature of a recognizer
/// repetition-hallucination ("eh eh eh"), which the model may drop even when whisper
/// was confident. A lone confident word is never an adjacent duplicate, so it stays
/// protected. Real reduplication (`kanan-kanan`) is one unit, not two, so it is not
/// flagged.
fn adjacent_duplicate(units: &[CaptionUnit], i: usize) -> bool {
    let ci = core(&units[i].text).to_lowercase();
    if ci.is_empty() {
        return false;
    }
    let prev = i.checked_sub(1).is_some_and(|p| core(&units[p].text).to_lowercase() == ci);
    let next = units.get(i + 1).is_some_and(|n| core(&n.text).to_lowercase() == ci);
    prev || next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Correction;

    fn unit(text: &str) -> CaptionUnit {
        CaptionUnit { text: text.into(), start_s: 0.0, end_s: 0.4 }
    }

    fn ctx_unit(text: &str, s: f64, e: f64) -> CaptionUnit {
        CaptionUnit { text: text.into(), start_s: s, end_s: e }
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

    #[test]
    fn system_prompt_keeps_the_over_correction_guards() {
        let s = SYSTEM.to_lowercase();
        assert!(s.contains("never translate"));
        assert!(s.contains("never add") || s.contains("never invent"));
        assert!(s.contains("exactly as written"), "must protect unmarked words");
        assert!(s.contains("delete"), "must offer the drop instruction");
    }

    #[test]
    fn request_marks_unsure_words_and_lists_overrides() {
        let units = vec![unit("dimalai-malai"), unit("semua"), unit("cowok")];
        let conf = vec![0.20, 0.95, 0.92]; // only the first is unsure
        let ctx = CorrectionContext { language: Language::Id, topic: "Horror game w/ Guntur".into() };
        let req = build_correction_request(&units, &conf, &slang_lex(), &ctx).expect("some request");
        assert!(req.user.contains("Horror game w/ Guntur"));
        assert!(req.user.contains("Indonesian"));
        assert!(req.user.contains("\"cowok\" -> \"cok\""));
        assert!(req.user.contains("1: dimalai-malai [?]"), "unsure word marked");
        assert!(req.user.contains("2: semua\n"), "confident word unmarked");
        assert!(req.user.contains("3: cowok\n"), "confident word unmarked");
    }

    #[test]
    fn request_is_none_when_nothing_to_do() {
        // All confident and no context overrides -> the model would be a no-op.
        let units = vec![unit("halo"), unit("dunia")];
        let conf = vec![0.99, 0.99];
        let ctx = CorrectionContext { language: Language::Id, topic: String::new() };
        assert!(build_correction_request(&units, &conf, &DialectLexicon::default(), &ctx).is_none());
        // But an override alone is enough reason to run (a confident word may match).
        assert!(build_correction_request(&units, &conf, &slang_lex(), &ctx).is_some());
        // And an unsure word alone is enough, with no overrides.
        let conf2 = vec![0.10, 0.99];
        assert!(build_correction_request(&units, &conf2, &DialectLexicon::default(), &ctx).is_some());
    }

    #[test]
    fn auto_fixes_an_unsure_word_but_protects_a_confident_one() {
        let mut units = vec![unit("dimalai-malai"), unit("interaksi")];
        let conf = vec![0.20, 0.95]; // garble unsure, real word confident
        // The model tries to change BOTH; only the unsure one is allowed.
        let raw = "1: dimarahin\n2: interaksinya\n";
        let stats = apply_correction(&mut units, &conf, &DialectLexicon::default(), raw);
        assert_eq!(units[0].text, "dimarahin");
        assert_eq!(units[1].text, "interaksi", "confident word protected from a non-curated change");
        assert_eq!(stats.replaced_unsure, 1);
        assert_eq!(stats.rejected, 1);
    }

    #[test]
    fn curated_override_changes_a_confident_word_in_context() {
        // cowok/tidur are CONFIDENT real words; only the curated override lets the
        // model change them — and only to exactly the curated `right`.
        let mut units = vec![unit("semua"), unit("cowok"), unit("tidur")];
        let conf = vec![0.95, 0.92, 0.90];
        let raw = "1: semua\n2: cok\n3: tur\n";
        let stats = apply_correction(&mut units, &conf, &slang_lex(), raw);
        assert_eq!(units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["semua", "cok", "tur"]);
        assert_eq!(stats.replaced_curated, 2);
        assert_eq!(stats.rejected, 0);
    }

    #[test]
    fn a_confident_word_cannot_be_changed_to_a_non_curated_value() {
        // The override is cowok->cok; the model proposing cowok->cewek must be refused.
        let mut units = vec![unit("cowok")];
        let conf = vec![0.95];
        let stats = apply_correction(&mut units, &conf, &slang_lex(), "1: cewek\n");
        assert_eq!(units[0].text, "cowok", "non-curated change to a confident word refused");
        assert_eq!(stats.rejected, 1);
        assert_eq!(stats.replaced_curated, 0);
    }

    #[test]
    fn deletes_a_spurious_unsure_word_keeps_a_confident_one() {
        // The "eh eh" hallucination spam: unsure -> droppable. A confident "itu"
        // the model wants gone is kept (v1 never drops a confident word).
        let mut units = vec![unit("horor"), unit("eh"), unit("itu")];
        let conf = vec![0.95, 0.10, 0.93];
        let raw = "1: horor\n2: DELETE\n3: DELETE\n";
        let stats = apply_correction(&mut units, &conf, &DialectLexicon::default(), raw);
        assert_eq!(units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["horor", "itu"]);
        assert_eq!(stats.deleted, 1);
        assert_eq!(stats.rejected, 1);
    }

    #[test]
    fn collapses_a_confident_repeated_filler_run() {
        // The "eh eh eh" hallucination: whisper is CONFIDENT, so the unsure gate
        // alone would keep them — the adjacent-duplicate rule lets the model drop
        // the repeats while keeping the first.
        let mut units = vec![unit("eh"), unit("eh"), unit("eh"), unit("horor")];
        let conf = vec![0.80, 0.85, 0.88, 0.95]; // all confident
        let raw = "1: eh\n2: DELETE\n3: DELETE\n4: horor\n";
        let stats = apply_correction(&mut units, &conf, &DialectLexicon::default(), raw);
        assert_eq!(units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["eh", "horor"]);
        assert_eq!(stats.deleted, 2);
        assert_eq!(stats.rejected, 0);
    }

    #[test]
    fn a_confident_non_repeated_word_is_never_dropped() {
        // A confident word that is NOT an adjacent duplicate stays even if flagged —
        // protects a real word from an over-eager DELETE.
        let mut units = vec![unit("horor"), unit("itu"), unit("yang")];
        let conf = vec![0.95, 0.93, 0.90];
        let stats = apply_correction(&mut units, &conf, &DialectLexicon::default(), "2: DELETE\n");
        assert_eq!(units.len(), 3, "confident non-duplicate protected from deletion");
        assert_eq!(stats.rejected, 1);
    }

    #[test]
    fn a_separator_only_rewrite_is_treated_as_no_change() {
        // The model rewriting "kanan-kanan" as "kanan_kanan" must not corrupt the
        // word, even though it is unsure (a real change would otherwise be allowed).
        let mut units = vec![unit("kanan-kanan")];
        let conf = vec![0.29];
        let stats = apply_correction(&mut units, &conf, &DialectLexicon::default(), "1: kanan_kanan\n");
        assert_eq!(units[0].text, "kanan-kanan", "separator-only rewrite ignored");
        assert_eq!(stats.applied(), 0);
    }

    #[test]
    fn collapse_merges_short_filler_runs_keeps_emphasis_and_conf() {
        // "eh eh eh" -> one "eh" spanning the run, with the run's least-sure conf; a
        // word between runs is untouched, and word-length emphasis is preserved.
        let mut units = vec![
            ctx_unit("eh", 0.0, 0.1),
            ctx_unit("eh", 0.2, 0.3),
            ctx_unit("eh", 0.4, 0.5),
            ctx_unit("harus", 1.0, 1.4),
            ctx_unit("asli", 2.0, 2.2),
            ctx_unit("asli", 2.3, 2.5),
        ];
        let mut conf = vec![0.8, 0.5, 0.3, 0.9, 0.9, 0.9];
        let removed = collapse_adjacent_duplicates(&mut units, &mut conf);
        assert_eq!(removed, 2, "three eh collapse to one");
        assert_eq!(
            units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(),
            vec!["eh", "harus", "asli", "asli"], // "asli asli" (len 4) is emphasis, kept
        );
        assert!((units[0].end_s - 0.5).abs() < 1e-9, "merged eh spans the whole run");
        assert!((conf[0] - 0.3).abs() < 1e-6, "merged conf is the run's least-sure");
        assert_eq!(conf.len(), units.len(), "conf stays aligned with units");
    }

    #[test]
    fn replacement_preserves_punctuation_and_timing() {
        let mut units = vec![ctx_unit("dimalai-malai,", 10.10, 10.96)];
        let conf = vec![0.20];
        apply_correction(&mut units, &conf, &DialectLexicon::default(), "1: dimarahin\n");
        assert_eq!(units[0].text, "dimarahin,", "trailing punctuation kept");
        assert!((units[0].start_s - 10.10).abs() < 1e-9, "DTW timing preserved");
        assert!((units[0].end_s - 10.96).abs() < 1e-9);
    }

    #[test]
    fn a_missing_or_unparseable_line_keeps_the_word() {
        // The model omits index 2 and rambles on a line with no number; both keep.
        let mut units = vec![unit("alpha"), unit("beta"), unit("gamma")];
        let conf = vec![0.10, 0.10, 0.10];
        let raw = "1: alpha\n(sorry, unsure about the rest)\n3: gamma\n";
        let stats = apply_correction(&mut units, &conf, &DialectLexicon::default(), raw);
        assert_eq!(units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["alpha", "beta", "gamma"]);
        assert_eq!(stats.applied(), 0);
    }

    #[test]
    fn extra_words_in_a_reply_line_are_clipped_to_one() {
        // "no adding": a multi-word reply for one index takes only the first token.
        let mut units = vec![unit("garblexyz")];
        let conf = vec![0.10];
        apply_correction(&mut units, &conf, &DialectLexicon::default(), "1: dimarahin lagi banget\n");
        assert_eq!(units[0].text, "dimarahin");
    }

    #[test]
    fn merge_is_replace_plus_delete_on_adjacent_unsure_units() {
        // "dimalai malai" -> "dimarahin": unit 1 becomes the word, unit 2 is dropped;
        // unit 1 keeps its onset and (via the later gap-fill) spans the freed time.
        let mut units = vec![ctx_unit("dimalai", 10.10, 10.40), ctx_unit("malai", 10.50, 10.96)];
        let conf = vec![0.20, 0.18];
        let stats = apply_correction(&mut units, &conf, &DialectLexicon::default(), "1: dimarahin\n2: DELETE\n");
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].text, "dimarahin");
        assert!((units[0].start_s - 10.10).abs() < 1e-9);
        assert_eq!(stats.replaced_unsure, 1);
        assert_eq!(stats.deleted, 1);
    }
}
