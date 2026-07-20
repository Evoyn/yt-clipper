//! Transcription: caption POLICY + the whisper wire client, model `large-v3`
//! (ADR 0003; f16 weights). Whisper emits token-level timestamps; the
//! language-aware grouping layer in this crate converts tokens into animatable
//! caption units — space-delimited words for EN/ID, fixed-size character
//! chunks for JA (which whisper emits without inter-word spaces).
//!
//! Since ADR 0072 the whisper.cpp decode itself runs in the **`yc-whisper`
//! sidecar child** ([`wire`]): this crate links NO whisper/ggml, so a CUDA
//! abort under GPU contention kills a child and fails one job instead of the
//! app. [`Transcriber`] keeps its exact pre-move API — it now spawns and owns
//! the child (residency = the child's lifetime), sends raw f32 samples, and
//! runs the unchanged grouping/correction policy over the raw tokens that
//! come back. The raw token is the seam; the fixture identity bar (A3) pins
//! it.
//!
//! M1 transcribes only the manually-picked range's samples (transcribe-range-
//! only), so timestamps are already 0-based to the clip and line up with the
//! render timeline.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;
use yc_core::{CaptionUnit, Language, Transcript};

pub mod align;
pub mod ensemble;
pub mod wire;

pub use wire::is_special;

mod correct;
pub use correct::{
    apply_correction, build_correction_request, correction_topic, CorrectionContext,
    CorrectionRequest, CorrectionStats,
};

fn lang_code(l: Language) -> &'static str {
    match l {
        Language::En => "en",
        Language::Id => "id",
        Language::Ja => "ja",
    }
}

/// Group raw whisper tokens into animatable caption units for the given
/// `language` — the language-aware layer (ADR 0003). EN/ID group into
/// space-delimited words ([`group_into_words`]); JA, which whisper emits without
/// leading spaces, groups into small character chunks ([`group_into_chars`]).
/// Returns the units plus a parallel per-unit confidence (min token probability)
/// for the auto-harvest. Pure, so it is unit-tested without a model.
fn group_tokens<I>(tokens: I, language: Language) -> (Vec<CaptionUnit>, Vec<f32>)
where
    I: IntoIterator<Item = (String, f64, f64, f32)>,
{
    match language {
        Language::Ja => group_into_chars(tokens),
        Language::En | Language::Id => group_into_words(tokens),
    }
}

/// Group raw whisper tokens — each carrying whisper's leading-space word
/// marking, start/end seconds, and decode probability — into **word** caption
/// units (EN/ID). A unit begins at every whitespace-led token; a token with no
/// leading space (subword piece or trailing punctuation) extends the current
/// unit. Returns the units plus a parallel per-unit **confidence** (the minimum
/// token probability over the unit's tokens), the signal the auto-harvest uses
/// to flag words whisper was unsure about. Pure, so it is unit-tested without a
/// model.
fn group_into_words<I>(tokens: I) -> (Vec<CaptionUnit>, Vec<f32>)
where
    I: IntoIterator<Item = (String, f64, f64, f32)>,
{
    let mut units: Vec<CaptionUnit> = Vec::new();
    let mut conf: Vec<f32> = Vec::new();
    for (raw, t0, t1, p) in tokens {
        let clean = raw.trim_start();
        if clean.trim().is_empty() {
            continue;
        }
        let starts_word = raw.starts_with(' ') || raw.starts_with('\u{2581}'); // ' ' or ▁
        if starts_word || units.is_empty() {
            units.push(CaptionUnit { text: clean.to_string(), start_s: t0, end_s: t1 });
            conf.push(p);
        } else {
            let last = units.last_mut().expect("non-empty by branch");
            last.text.push_str(clean);
            last.end_s = t1;
            let c = conf.last_mut().expect("non-empty by branch");
            *c = c.min(p); // a word is only as confident as its least-sure token
        }
    }
    (units, conf)
}

/// Characters per JA caption chunk (tune-from-use, like the caption-timing
/// constants). Japanese has no inter-word spaces, so whisper emits JA tokens with
/// no leading-space marking — [`group_into_words`] would collapse a whole segment
/// into one unit. JA instead chunks into small fixed-size character runs (ADR
/// 0003: "character chunks at kanji/kana boundaries"), each an animatable unit.
/// ~4 reads cleanly one-chunk-at-a-time on a phone without flashing single glyphs.
const JA_CHUNK_CHARS: usize = 4;

/// Sentence-ending punctuation that closes a JA chunk early (so a chunk never
/// straddles a sentence boundary), full- and half-width.
fn is_ja_break_punct(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '．' | '…' | '!' | '?')
}

/// Group raw whisper tokens into **JA character chunks**. Whole tokens are
/// accumulated into a chunk until adding the next would exceed [`JA_CHUNK_CHARS`]
/// (a token is never split — it carries a single DTW time span), and a token
/// ending in sentence punctuation closes the chunk. Each chunk's span runs from
/// its first token's start to its last token's end; the per-chunk confidence is
/// the minimum token probability, exactly as for words. Pure, so it is unit-tested
/// without a model.
fn group_into_chars<I>(tokens: I) -> (Vec<CaptionUnit>, Vec<f32>)
where
    I: IntoIterator<Item = (String, f64, f64, f32)>,
{
    let mut units: Vec<CaptionUnit> = Vec::new();
    let mut conf: Vec<f32> = Vec::new();
    let mut open = false; // is the last chunk still accepting tokens?
    for (raw, t0, t1, p) in tokens {
        let clean = raw.trim();
        if clean.is_empty() {
            continue;
        }
        let n = clean.chars().count();
        let cur_n = if open {
            units.last().map_or(0, |u| u.text.chars().count())
        } else {
            0
        };
        if !open || cur_n + n > JA_CHUNK_CHARS {
            units.push(CaptionUnit { text: clean.to_string(), start_s: t0, end_s: t1 });
            conf.push(p);
        } else {
            let last = units.last_mut().expect("non-empty by branch");
            last.text.push_str(clean);
            last.end_s = t1;
            let c = conf.last_mut().expect("non-empty by branch");
            *c = c.min(p);
        }
        // The chunk stays open only while under budget and not sentence-ended.
        let chunk_chars = units.last().expect("just pushed/extended").text.chars().count();
        let ends_sentence = clean.chars().next_back().is_some_and(is_ja_break_punct);
        open = chunk_chars < JA_CHUNK_CHARS && !ends_sentence;
    }
    (units, conf)
}

/// A curatable per-language (later per-Creator) store of dialect / slang
/// mishears and domain vocabulary, loaded from `<dir>/<lang>.json`. It drives
/// two cheap caption fixes that lift the transcription ceiling without a heavier
/// model, because the real caption pain here is *linguistic* (a streamer's
/// accent / local slang / viewer names), not acoustic:
///
///   1. **Priming** — the vocabulary seeds whisper's `initial_prompt`, so the
///      decoder mishears the streamer's real words *less at the source*.
///   2. **Correction** — confirmed `wrong -> right` pairs patch whole-word
///      mishears *after* transcription (captions and the detection lexicon both
///      read the corrected text).
///
/// The operator curates the JSON over time — filling blank `right`s, adding
/// entries — so detection of that Creator's dialect keeps improving. A missing
/// or unparseable file yields an empty lexicon (both fixes no-op), so a Creator
/// without a store still transcribes, just without the fix-ups.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct DialectLexicon {
    /// Who this store is for (free text, e.g. the Creator name). Informational.
    #[serde(default)]
    pub creator: String,
    /// Language code this store is for ("id"/"en"/"ja"); preserved on round-trip.
    #[serde(default)]
    pub language: String,
    /// Operator documentation only (e.g. "Streamer logat Medan, main game"). It
    /// is NEVER fed to whisper — a descriptive sentence in the prompt makes the
    /// decoder hallucinate boilerplate (see `initial_prompt`).
    #[serde(default)]
    pub note: String,
    /// Opt-in whisper priming (#1). Off by default because `initial_prompt`
    /// biases the *whole* transcription, not just the target words — measured to
    /// drift / shorten an otherwise-complete transcript (min3: 108 -> 77 units).
    /// The correction dict (#2) is always applied and is risk-free; set this true
    /// per-Creator only to experiment with source-level priming.
    #[serde(default)]
    pub prime: bool,
    /// Auto-harvest (default on): append words whisper was unsure about to this
    /// store as `unverified` to-dos on each caption run, so the review queue
    /// fills itself instead of the operator hunting garbles. Set false to freeze.
    #[serde(default = "default_true")]
    pub harvest: bool,
    /// Language codes whose bundled wordlists (`<code>.words.txt`) feed the
    /// auto-harvest filter — e.g. `["id","en"]` for a streamer who code-switches
    /// to English, so their English words aren't flagged as garbles. Empty = just
    /// this store's own language.
    #[serde(default)]
    pub dictionaries: Vec<String>,
    /// Domain words to bias whisper toward when `prime` is on (game terms, names,
    /// catchphrases). Joined into the `initial_prompt`.
    #[serde(default)]
    pub vocabulary: Vec<String>,
    /// Known mishears. Only entries with a non-empty `right` are applied; a
    /// blank `right` is an operator to-do that records the unsolved garble for
    /// later review without changing any output.
    #[serde(default)]
    pub corrections: Vec<Correction>,
    /// Real-word set for the auto-harvest filter, merged at load from the
    /// `dictionaries` wordlists (`<code>.words.txt`) plus the `names.json` roster
    /// (not part of this JSON). A flagged word that IS a real word / known name is
    /// skipped, so only out-of-dictionary tokens — the garbles, slang, and unknown
    /// names worth recording — reach the review queue. Empty when no wordlist is
    /// present (harvest then falls back to confidence alone).
    #[serde(skip)]
    pub dictionary: HashSet<String>,
}

fn default_true() -> bool {
    true
}

/// One mishear record in a [`DialectLexicon`].
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Correction {
    /// The garbled text whisper produces (matched whole-word, case-insensitive).
    pub wrong: String,
    /// What it should be. Blank = unverified, awaiting operator review.
    #[serde(default)]
    pub right: String,
    /// Free-text operator note (meaning, source clip, "viewer name?", ...).
    #[serde(default)]
    pub note: String,
    /// "confirmed" | "unverified" | whatever the operator writes. Informational —
    /// application keys off whether `right` is filled, not this.
    #[serde(default)]
    pub status: String,
    /// Context-sensitive override (ADR 0030): when true this `wrong -> right` is
    /// **not** applied by the always-on global dict — that would corrupt every
    /// real occurrence of `wrong` — but offered to the LLM correction pass, which
    /// applies it only where the surrounding context fits. For real words whisper
    /// transcribes *confidently* yet the streamer meant as slang or a name (e.g.
    /// `cowok -> cok`, `tidur -> tur`). Inert unless the correction pass runs.
    #[serde(default)]
    pub context: bool,
    /// Clip-scoped (2026-07-02): when true this correction applies to **this clip's**
    /// render (via the layered merge) but is **not** auto-promoted to the per-Creator
    /// store by [`DialectLexicon::promote_confirmed`] (ADR 0031). For a fix that is right
    /// for one clip yet would be wrong globalized — a real word the streamer means
    /// literally in most clips but as slang/an insult in this one (e.g. `pancingan ->
    /// bajingan` on a single clip). Curated in the per-clip store, it stays there.
    #[serde(default)]
    pub clip_only: bool,
    /// Time anchor (VOD-absolute seconds; the ROADMAP "time-anchored curation"
    /// seed, 2026-07-02). When set, the ENSEMBLE caption path (ADR 0034)
    /// applies this correction **positionally**: only to the token occurrence
    /// nearest this moment (±3 s guard) — finally safe for words that appear
    /// several times with only one occurrence needing the fix — and PINS the
    /// corrected word's caption to the speech onset there (`wrong == right` is
    /// a pure timing pin). The whisper dict path ignores it (no per-unit VOD
    /// times at dict-application depth); harvest notes already record the same
    /// timestamp (ADR 0022), so future curation can fill it from the queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_s: Option<f64>,
}

/// `names.json` — the roster of viewer names the streamer reads aloud, loaded
/// alongside the store (language-agnostic, per-Creator's community). A correctly
/// transcribed name is a real word, not a garble, so its parts join the harvest
/// dictionary; a *garbled* name still harvests and maps to the right name via a
/// `corrections` entry.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct NamesFile {
    #[serde(default)]
    names: Vec<String>,
}

impl DialectLexicon {
    /// Load `<dir>/<lang>.json`, falling back to an empty lexicon when the file
    /// is absent or unparseable (so transcription never fails on a bad store).
    pub fn load(dir: &Path, language: Language) -> Self {
        let path = dir.join(format!("{}.json", lang_code(language)));
        let mut lex: DialectLexicon = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("dialect: ignoring {} ({e})", path.display());
                    DialectLexicon::default()
                }
            },
            Err(_) => DialectLexicon::default(),
        };
        // Harvest dictionary: merge the bundled wordlist(s) for every language in
        // `dictionaries` (default: this store's own language), so a code-switching
        // streamer's other-language words aren't flagged as garbles. Read as bytes
        // + lossy UTF-8 — community wordlists carry the odd non-UTF-8 byte that
        // `read_to_string` would reject wholesale; the valid (ASCII) words matter.
        let codes: Vec<String> = if lex.dictionaries.is_empty() {
            vec![lang_code(language).to_string()]
        } else {
            lex.dictionaries.clone()
        };
        for code in &codes {
            load_wordlist(&dir.join(format!("{code}.words.txt")), &mut lex.dictionary);
        }
        // Common given names (`names.words.txt`, ADR 0023): always loaded, like
        // `names.json` - a correctly-read common name (Budi, Siti, John, ...) is a
        // real word, not a garble, so it must not flag the harvest. Language-
        // agnostic (a streamer reads viewer names from anywhere), so it loads
        // regardless of `dictionaries`. An *unusual* handle is absent here and still
        // harvests (the one to curate, now with the title+timestamp of ADR 0022).
        let n_name_words = load_wordlist(&dir.join("names.words.txt"), &mut lex.dictionary);
        // Viewer names (`names.json`, language-agnostic): a correctly-read name is
        // a real word, not a garble. Split multi-word names so the whole-word
        // harvest skips each part.
        let mut n_names = 0;
        if let Ok(text) = std::fs::read_to_string(dir.join("names.json")) {
            if let Ok(nf) = serde_json::from_str::<NamesFile>(&text) {
                for name in &nf.names {
                    for word in name.split_whitespace() {
                        let w = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
                        if !w.is_empty() {
                            lex.dictionary.insert(w);
                        }
                    }
                }
                n_names = nf.names.len();
            }
        }
        tracing::info!(
            "dialect: {} ({} corrections, {} vocab, {} dict words [{}], {} name words, {} roster names)",
            if lex.creator.is_empty() { "lexicon" } else { lex.creator.as_str() },
            lex.corrections.iter().filter(|c| !c.right.is_empty()).count(),
            lex.vocabulary.len(),
            lex.dictionary.len(),
            codes.join("+"),
            n_name_words,
            n_names,
        );
        lex
    }

    /// Load the bundled **base** store for `language` (dictionary + config + base
    /// corrections via [`load`](Self::load)), then layer **overlay** corrections
    /// from each path in `overlays` on top — later overlays win on a duplicate
    /// `wrong` (ADR 0031). Used to stack the per-Creator and per-clip dialect stores
    /// over the bundled per-language base: base < per-Creator < per-clip, so a
    /// clip-specific fix overrides a Creator one overrides the bundled default. The
    /// dictionary, wordlists, and config (prime/harvest/dictionaries) come from the
    /// base; overlays contribute only `corrections` (a missing/unparseable overlay
    /// is skipped, so a fresh Creator/clip with no store still transcribes).
    pub fn load_layered(base_dir: &Path, overlays: &[std::path::PathBuf], language: Language) -> Self {
        let mut lex = Self::load(base_dir, language);
        let mut n_overlay = 0;
        for path in overlays {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Ok(overlay) = serde_json::from_str::<DialectLexicon>(&text) {
                    merge_corrections(&mut lex.corrections, overlay.corrections);
                    n_overlay += 1;
                }
            }
        }
        if n_overlay > 0 {
            tracing::info!(
                "dialect: layered {n_overlay} overlay store(s) -> {} corrections total",
                lex.corrections.iter().filter(|c| !c.right.is_empty()).count(),
            );
        }
        lex
    }

    /// whisper `initial_prompt` (#1): a **bare comma-separated term list** — the
    /// vocabulary plus every confirmed correct word (deduped, order-preserving).
    ///
    /// Deliberately NOT a sentence. A natural-language primer that *describes the
    /// content* ("Streamer Indonesia main game live...") makes whisper-large-v3
    /// abandon the audio and hallucinate YouTube boilerplate ("Jangan lupa like,
    /// share, dan subscribe") on repeat — measured on min3, 108 real units ->
    /// 14 hallucinated. A bare word list biases spelling toward the streamer's
    /// vocabulary without that collapse. The `note` field stays documentation
    /// only; it is never primed. Empty when the store has no vocabulary.
    pub fn initial_prompt(&self) -> String {
        let mut seen = HashSet::new();
        let mut list: Vec<&str> = Vec::new();
        for v in &self.vocabulary {
            if !v.is_empty() && seen.insert(v.to_lowercase()) {
                list.push(v.as_str());
            }
        }
        for c in &self.corrections {
            if !c.right.is_empty() && seen.insert(c.right.to_lowercase()) {
                list.push(c.right.as_str());
            }
        }
        list.join(", ")
    }

    /// Confirmed `(wrong_lowercased, right)` pairs for the post-transcription
    /// whole-word fix-up. Blank-`right` entries (operator to-dos) and
    /// **context-sensitive** entries (ADR 0030, applied by the LLM pass instead)
    /// are skipped — the global dict only carries the unambiguous garble fixes.
    fn pairs(&self) -> Vec<(String, &str)> {
        self.corrections
            .iter()
            // `at_s` entries are positional (one occurrence at one moment,
            // ensemble path only) — a global dict application would hit every
            // occurrence, exactly what the time anchor exists to prevent.
            .filter(|c| !c.wrong.is_empty() && !c.right.is_empty() && !c.context && c.at_s.is_none())
            .map(|c| (c.wrong.to_lowercase(), c.right.as_str()))
            .collect()
    }

    /// Confirmed context-sensitive `(wrong_lowercased, right)` overrides (ADR
    /// 0030): the curated corrections the LLM correction pass may apply over a word
    /// whisper transcribed *confidently*, but only where the surrounding context
    /// fits. The always-on global dict ([`pairs`](Self::pairs)) deliberately skips
    /// these — it can't tell a real `cowok` from the slang `cok` — so they are
    /// inert unless the correction pass runs. Blank-`right` entries are skipped.
    pub fn context_overrides(&self) -> Vec<(String, String)> {
        self.corrections
            .iter()
            .filter(|c| c.context && !c.wrong.is_empty() && !c.right.is_empty())
            .map(|c| (c.wrong.to_lowercase(), c.right.clone()))
            .collect()
    }

    /// Append `candidates` to the bundled base store `<dir>/<lang>.json` (the
    /// pre-layering target). Thin wrapper over [`harvest_to_file`](Self::harvest_to_file);
    /// the layered caption path harvests to the **per-clip** store instead (ADR 0031).
    pub fn harvest_to_store(
        dir: &Path,
        language: Language,
        candidates: &[HarvestCandidate],
        clip_start_s: f64,
        title: Option<&str>,
    ) -> usize {
        Self::harvest_to_file(
            &dir.join(format!("{}.json", lang_code(language))),
            language,
            candidates,
            clip_start_s,
            title,
        )
    }

    /// Append `candidates` to the dialect store **file** at `path` as `unverified`
    /// to-dos, skipping any the file already knows (wrong / right / vocabulary).
    /// Re-reads the file first so concurrent operator edits to the `right` fields are
    /// preserved, and writes back pretty-printed. Returns how many new entries were
    /// added. Best-effort: a read/parse/write failure logs and adds nothing (never
    /// fails a render). The layered caption path points this at the **per-clip** store
    /// so each export gets its own small, easy-to-curate review queue (ADR 0031).
    ///
    /// Each new entry's note records **where the word came from** (ADR 0022) so the
    /// operator can curate it: the generated Short `title` (if any) and the
    /// **absolute VOD timestamp** `clip_start_s + candidate.start_s` (the clip's VOD
    /// start plus the word's clip-relative onset), as `from "Title" at h:mm:ss`. A
    /// word is recorded once (first run it is flagged in); a later run skips it.
    pub fn harvest_to_file(
        path: &Path,
        language: Language,
        candidates: &[HarvestCandidate],
        clip_start_s: f64,
        title: Option<&str>,
    ) -> usize {
        if candidates.is_empty() {
            return 0;
        }
        // Re-read (not the in-memory copy) so any hand-edits since load survive.
        let mut lex: DialectLexicon = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if lex.language.is_empty() {
            lex.language = lang_code(language).to_string();
        }
        let mut known: HashSet<String> = HashSet::new();
        for c in &lex.corrections {
            known.insert(c.wrong.to_lowercase());
            if !c.right.is_empty() {
                known.insert(c.right.to_lowercase());
            }
        }
        for v in &lex.vocabulary {
            known.insert(v.to_lowercase());
        }
        let mut added = 0;
        for cand in candidates {
            if known.insert(cand.word.to_lowercase()) {
                // VOD-absolute source location (ADR 0022): the clip's VOD start plus
                // the word's clip-relative onset, so the operator can jump to it.
                let at = vod_clock(clip_start_s + cand.start_s);
                let note = match title {
                    Some(t) if !t.is_empty() => format!(
                        "auto-harvested (conf {:.2}) from \"{t}\" at {at} - operator verify",
                        cand.confidence
                    ),
                    _ => format!(
                        "auto-harvested (conf {:.2}) at {at} - operator verify",
                        cand.confidence
                    ),
                };
                lex.corrections.push(Correction {
                    wrong: cand.word.clone(),
                    right: String::new(),
                    note,
                    status: "unverified".into(),
                    context: false, // a harvested garble is a global fix-up, not a context override
                    clip_only: false, // and it promotes to the Creator once confirmed (ADR 0031)
                    at_s: None, // the note carries the timestamp; the operator sets at_s when curating
                });
                added += 1;
            }
        }
        if added > 0 {
            match serde_json::to_string_pretty(&lex) {
                // Atomic (temp + rename): a kill mid-write must never tear the
                // operator's curated store — the lossy loader would then read it
                // as empty and the curation would be silently gone.
                Ok(s) => {
                    if let Err(e) = yc_core::write_atomic(&path, &(s + "\n")) {
                        tracing::warn!("dialect: harvest write to {} failed ({e})", path.display());
                        return 0;
                    }
                }
                Err(e) => {
                    tracing::warn!("dialect: harvest serialize failed ({e})");
                    return 0;
                }
            }
        }
        added
    }

    /// Copy every confirmed correction (a filled `right`) from the per-clip store at
    /// `clip_path` up into the per-Creator store at `creator_path`, skipping any the
    /// Creator store already knows (by `wrong`). Creates/updates the Creator store
    /// (pretty-printed). So a fix curated once in a clip's store **sticks for the
    /// Creator** — it applies to every future clip of theirs (ADR 0031, the operator's
    /// auto-promote choice). A **`clip_only`** correction is deliberately NOT promoted —
    /// it stays scoped to its clip (2026-07-02). Best-effort: a read/serialize/write
    /// failure logs and promotes nothing (never fails a render). Returns how many were
    /// promoted.
    pub fn promote_confirmed(clip_path: &Path, creator_path: &Path, language: Language) -> usize {
        let clip: DialectLexicon = std::fs::read_to_string(clip_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let confirmed: Vec<&Correction> = clip
            .corrections
            .iter()
            // Skip `clip_only` corrections: they are curated to fix THIS clip and would
            // be wrong globalized to the Creator (2026-07-02).
            .filter(|c| !c.wrong.is_empty() && !c.right.is_empty() && !c.clip_only)
            .collect();
        if confirmed.is_empty() {
            return 0;
        }
        let mut creator: DialectLexicon = std::fs::read_to_string(creator_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if creator.language.is_empty() {
            creator.language = lang_code(language).to_string();
        }
        let known: HashSet<String> =
            creator.corrections.iter().map(|c| c.wrong.to_lowercase()).collect();
        let mut added = 0;
        for c in confirmed {
            if !known.contains(&c.wrong.to_lowercase()) {
                creator.corrections.push((*c).clone());
                added += 1;
            }
        }
        if added > 0 {
            match serde_json::to_string_pretty(&creator) {
                // Atomic for the same reason as the harvest write: the
                // per-Creator store is the operator's cross-VOD curation.
                Ok(s) => {
                    if let Err(e) = yc_core::write_atomic(creator_path, &(s + "\n")) {
                        tracing::warn!(
                            "dialect: promote write to {} failed ({e})",
                            creator_path.display()
                        );
                        return 0;
                    }
                }
                Err(e) => {
                    tracing::warn!("dialect: promote serialize failed ({e})");
                    return 0;
                }
            }
        }
        added
    }
}

/// Merge `overlay` corrections into `into`, **overlay wins** on a duplicate `wrong`
/// (case-insensitive) *at the same moment*: a time-anchored (`at_s`) entry is a
/// per-moment correction, so entries for the same `wrong` at different moments
/// coexist (the measured case: three "blok on" pairs in one clip, one of them a
/// different word), and an anchored entry never replaces a global one (they serve
/// different passes). Else appended. The layering primitive for
/// [`DialectLexicon::load_layered`] — per-clip over per-Creator over base
/// (ADR 0031). Pure, so it is unit-tested.
fn merge_corrections(into: &mut Vec<Correction>, overlay: Vec<Correction>) {
    for c in overlay {
        if c.wrong.is_empty() {
            continue;
        }
        let key = c.wrong.to_lowercase();
        if let Some(existing) =
            into.iter_mut().find(|e| e.wrong.to_lowercase() == key && e.at_s == c.at_s)
        {
            // A blank-`right` overlay entry (an operator to-do) must NOT shadow a
            // confirmed fix from a lower layer: a stale per-clip to-do would otherwise
            // erase the per-Creator correction of the same word, which then `pairs()`
            // filters out as empty (ADR 0031/0032). A filled overlay still wins.
            if c.right.is_empty() && !existing.right.is_empty() {
                continue;
            }
            *existing = c;
        } else {
            into.push(c);
        }
    }
}

/// Replace whole **single** words the lexicon maps (case-insensitive, surrounding
/// punctuation preserved). A multi-word `wrong` never matches here (a unit is one
/// word, so its core has no space) — [`apply_multiword_corrections`] handles those
/// first. Applied to every transcript so both captions and the detection lexicon
/// read the corrected text.
fn correct_known_mishears(units: &mut [CaptionUnit], lexicon: &DialectLexicon) {
    let map = lexicon.pairs();
    if map.is_empty() {
        return;
    }
    for u in units.iter_mut() {
        let core: String = u.text.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
        if core.is_empty() {
            continue;
        }
        let lower = core.to_lowercase();
        for (wrong, right) in &map {
            if wrong == &lower {
                u.text = u.text.replacen(&core, right, 1);
                break;
            }
        }
    }
}

/// Apply **multi-word** `wrong -> right` corrections (ADR 0014): a streamer's
/// two-plus-word catchphrase or a multi-word viewer handle that mis-transcribes
/// across several caption units (e.g. `"point blank" -> "Point Blank"`). The
/// single-word [`correct_known_mishears`] can't reach these because each unit is
/// one word. For each multi-word correction, slide a K-unit window over the units;
/// where K consecutive units' cores (case-insensitive, punctuation-trimmed) equal
/// the `wrong` words in order, **collapse** them into one unit holding `right`
/// (spanning the first unit's start to the last unit's end, with the last unit's
/// trailing punctuation kept), and shrink `conf` in lockstep (the collapsed unit
/// keeps the least-sure confidence). Runs **before** the single-word pass so a
/// phrase wins over a word, and so the collapsed unit (whose core now contains a
/// space) is untouched by it. Pure, so it is unit-tested without a model.
fn apply_multiword_corrections(
    units: &mut Vec<CaptionUnit>,
    conf: &mut Vec<f32>,
    lexicon: &DialectLexicon,
) {
    for c in &lexicon.corrections {
        // Context-sensitive overrides (ADR 0030) are LLM-applied, never global;
        // time-anchored entries (at_s) are ensemble-positional, never global.
        if c.wrong.is_empty() || c.right.is_empty() || c.context || c.at_s.is_some() {
            continue;
        }
        let words: Vec<String> =
            c.wrong.to_lowercase().split_whitespace().map(|w| w.to_string()).collect();
        if words.len() < 2 {
            continue; // single-word corrections are handled by correct_known_mishears
        }
        let k = words.len();
        let mut i = 0;
        while i + k <= units.len() {
            let matches = (0..k).all(|j| {
                let core =
                    units[i + j].text.trim_matches(|ch: char| !ch.is_alphanumeric()).to_lowercase();
                core == words[j]
            });
            if matches {
                let start = units[i].start_s;
                let end = units[i + k - 1].end_s;
                // Keep the last unit's trailing punctuation (e.g. "blank!" -> right + "!").
                let last = &units[i + k - 1].text;
                let trail = last[last.trim_end_matches(|ch: char| !ch.is_alphanumeric()).len()..]
                    .to_string();
                let new_conf =
                    (i..i + k).map(|j| conf[j]).fold(f32::INFINITY, f32::min);
                units[i] =
                    CaptionUnit { text: format!("{}{}", c.right, trail), start_s: start, end_s: end };
                conf[i] = new_conf;
                units.drain(i + 1..i + k);
                conf.drain(i + 1..i + k);
                i += 1; // past the collapsed unit
            } else {
                i += 1;
            }
        }
    }
}

/// A word whose least-sure token fell below this probability is *eligible* for
/// the review queue. Whisper is uncertain about many genuinely-correct words on
/// accented audio, so this alone is noisy — [`HARVEST_MAX_PER_CLIP`] then keeps
/// only the least-confident few. Tune-from-use. (A dictionary filter would cut
/// the noise further; deferred.)
const HARVEST_MAX_P: f32 = 0.50;
/// Don't harvest fragments shorter than this (punctuation, "ya", "ke") — too
/// noisy and rarely the dialect/name garbles worth recording.
const HARVEST_MIN_LEN: usize = 4;
/// Cap on words harvested per clip — only the N least-confident survive, so the
/// operator's review queue stays small even when whisper is broadly unsure.
const HARVEST_MAX_PER_CLIP: usize = 8;

/// Beam width for the caption path's beam-search decode (ADR 0027). whisper.cpp's
/// quality decoder; 5 is its conventional default. Slower than greedy, but the
/// render is not time-bound and accuracy is the goal. Tune-from-use.
const BEAM_SIZE: i32 = 5;

// --- caption decode trial knobs (2026-07-02, ADR 0033) -----------------------
// OPT-IN anti-hallucination decoder settings for the CAPTION path only (the
// beam/DTW load; detect refine is untouched). ALL DEFAULT OFF: an A/B on the
// guntur69 "Diskusi" clip showed ANY decode perturbation reshuffles whisper's
// garbles on a masked clip — suppress_nst alone recovered the missed 23-26 s
// speech AND the mistimed "bangke" (what the rejected enh recovered), but it
// also re-garbled every word the operator's curated corrections were keyed to
// (dijekat->dijegat, pancingan->Pacingan...), silently breaking the curation.
// The dialect store is coupled to the exact decoder config, so a decode change
// is a curation-breaking event the operator must opt into per render — never a
// silent default flip. Env knobs (the YC_ENH_ATTEN precedent), read here inside
// the transcriber so every caption consumer — do_render AND the diag
// inspectors — always decodes identically (the inspector-fidelity lesson,
// ADR 0030).

/// `YC_CAPTION_NOCTX=1` (trial, default off) decodes each 30 s window from a
/// fresh slate (`no_context=true`). whisper seeds each window with the previous
/// window's text, so on masked audio a window-1 hallucination propagates into
/// window 2 — and promoted clips (30 s + up to 10 s loud pre-roll) cross that
/// seam exactly at the loud Moment. Inert on a <=30 s (single-window) clip.
fn parse_caption_noctx(v: Option<&str>) -> bool {
    matches!(v, Some("1"))
}

/// `YC_SUPPRESS_NST=1` (trial, default off) suppresses whisper's non-speech
/// tokens (music notes, bracketed sound-effect marks). Measured on the masked
/// "Diskusi" clip to recover real missed speech (23 -> 40 units, the 23-26 s
/// hole, "bangke") by redistributing probability off the junk tokens — and to
/// re-garble the curated anchor words in the same stroke (see the module note).
/// Worth trying on a NEW noisy clip before curating it; wrong for an
/// already-curated one.
fn parse_suppress_nst(v: Option<&str>) -> bool {
    matches!(v, Some("1"))
}

/// `YC_VAD=1` (trial, default off) turns on whisper.cpp's built-in Silero VAD
/// pre-segmentation: only voiced spans are decoded, so whisper never free-runs
/// over a pure music/SFX window. Needs [`VAD_MODEL_FILE`] beside the whisper
/// model. Measured a NO-OP on two masked gaming clips (Silero reads loud game
/// audio as speech, so nothing was trimmed — byte-identical output); its value,
/// if any, is on clips with real silent/music-only stretches. Timestamps are
/// remapped through the VAD segment map; validate with `caption_diag` first.
fn parse_vad_requested(v: Option<&str>) -> bool {
    matches!(v, Some("1"))
}

/// The Silero VAD model (ggml), expected beside the whisper model:
/// `models/ggml-silero-v5.1.2.bin` (whisper.cpp's pinned VAD release).
const VAD_MODEL_FILE: &str = "ggml-silero-v5.1.2.bin";

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Words whisper was least sure about (confidence below [`HARVEST_MAX_P`]) that
/// the store doesn't already know — the review queue the auto-harvest appends,
/// each paired with its confidence so the operator can prioritise. Skips short /
/// non-alphabetic tokens and anything already a `wrong`, `right`, or vocabulary
/// entry (including the word a correction just fixed). Deduped; returned
/// least-confident first and capped at [`HARVEST_MAX_PER_CLIP`]. Pure (testable
/// without a model).
/// Merge a newline-delimited wordlist into `dict` (trimmed, lowercased), skipping
/// blank and `#`-comment lines. Read as bytes + lossy UTF-8 — community wordlists
/// carry the odd non-UTF-8 byte that `read_to_string` would reject wholesale; the
/// valid (ASCII) words matter. Returns how many new words it added. Shared by the
/// `dictionaries` codes and the always-loaded `names.words.txt` (ADR 0023).
fn load_wordlist(path: &Path, dict: &mut HashSet<String>) -> usize {
    let mut added = 0;
    if let Ok(bytes) = std::fs::read(path) {
        for line in String::from_utf8_lossy(&bytes).lines() {
            let w = line.trim().to_lowercase();
            if !w.is_empty() && !w.starts_with('#') && dict.insert(w) {
                added += 1;
            }
        }
    }
    added
}

/// `h:mm:ss` (or `m:ss` under an hour) for a VOD-absolute timestamp — the source
/// location written into a harvested word's review note (ADR 0022). Filesystem
/// concerns don't apply (it goes in a note, not a filename), so the natural `:` is
/// used. Pure, so the format is unit-tested.
fn vod_clock(t_s: f64) -> String {
    let s = t_s.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// The parsed contents of a harvested to-do's provenance note (ADR 0022), for the
/// caption review-queue UI (ADR 0032). The note is the single source of provenance
/// (ADR 0022 deferred structured fields), so this parses it back: the UI groups by
/// `title`, shows `confidence`, and builds a VOD jump link from `at_s`. Lenient — any
/// field not present comes back `None` (a hand-written note still lists as a to-do).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HarvestNote {
    pub confidence: Option<f32>,
    pub title: Option<String>,
    pub at_s: Option<f64>,
}

/// Parse a note written by [`DialectLexicon::harvest_to_file`] — e.g. `auto-harvested
/// (conf 0.42) from "He LOST it on the boss" at 1:23:45 - operator verify` (or without
/// the `from "..."` for a manual clip). Inverse of that `format!` + [`vod_clock`];
/// pure, unit-tested. Hand-edited/unknown notes yield whatever fields are present.
pub fn parse_harvest_note(note: &str) -> HarvestNote {
    // confidence: `conf 0.42)` -> 0.42
    let confidence = note
        .split("conf ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|c| c.trim().parse::<f32>().ok());
    // title: the text between `from "` and `" at ` (empty title -> None)
    let title = note
        .split("from \"")
        .nth(1)
        .and_then(|rest| rest.split("\" at ").next())
        .map(|t| t.to_string())
        .filter(|t| !t.is_empty());
    // timestamp: the `h:mm:ss`/`m:ss` after the last ` at `, before ` - operator
    // verify`. Anchoring on the trailer keeps a title containing " at " from fooling us.
    let head = note.split(" - operator verify").next().unwrap_or(note);
    let at_s = head
        .rsplit(" at ")
        .next()
        .filter(|seg| *seg != head) // only if an " at " was actually present
        .and_then(parse_vod_clock);
    HarvestNote { confidence, title, at_s }
}

/// Inverse of [`vod_clock`]: `h:mm:ss` or `m:ss` -> seconds. `None` unless the string
/// is a colon-separated clock of 2 or 3 numeric parts.
fn parse_vod_clock(s: &str) -> Option<f64> {
    let nums: Option<Vec<u64>> =
        s.trim().split(':').map(|p| p.trim().parse::<u64>().ok()).collect();
    match nums?.as_slice() {
        [m, s] => Some((m * 60 + s) as f64),
        [h, m, s] => Some((h * 3600 + m * 60 + s) as f64),
        _ => None,
    }
}

/// A word the harvest flags for review: the garbled text, the whisper confidence
/// that flagged it, and its **clip-relative onset**. The onset lets the render
/// record an absolute VOD timestamp (clip start + this) in the review note, so the
/// operator can jump to the source and check the word (ADR 0022).
#[derive(Debug, Clone)]
pub struct HarvestCandidate {
    pub word: String,
    pub confidence: f32,
    pub start_s: f64,
    /// Index of the source unit in the transcript this candidate was harvested
    /// from (pre-timing-refine order). Lets the render skip candidates whose unit
    /// the timing pass then **drops** as near-silence — whisper hallucinating into
    /// a silent/music window — so they never pollute the review queue with words
    /// that aren't in the caption (ADR 0032).
    pub unit_index: usize,
}

fn harvest_candidates(
    units: &[CaptionUnit],
    conf: &[f32],
    lexicon: &DialectLexicon,
) -> Vec<HarvestCandidate> {
    let mut known: HashSet<String> = HashSet::new();
    for c in &lexicon.corrections {
        known.insert(c.wrong.to_lowercase());
        if !c.right.is_empty() {
            known.insert(c.right.to_lowercase());
        }
    }
    for v in &lexicon.vocabulary {
        known.insert(v.to_lowercase());
    }
    let mut out: Vec<HarvestCandidate> = Vec::new();
    let mut seen = HashSet::new();
    for (i, (u, &c)) in units.iter().zip(conf.iter()).enumerate() {
        if c >= HARVEST_MAX_P {
            continue;
        }
        let core: String = u.text.trim_matches(|ch: char| !ch.is_alphanumeric()).to_string();
        if core.chars().count() < HARVEST_MIN_LEN || !core.chars().any(|ch| ch.is_alphabetic()) {
            continue;
        }
        let lc = core.to_lowercase();
        // A real Indonesian word whisper was merely unsure of - not a garble.
        if lexicon.dictionary.contains(&lc) {
            continue;
        }
        if !known.contains(&lc) && seen.insert(lc) {
            out.push(HarvestCandidate {
                word: core,
                confidence: c,
                start_s: u.start_s,
                unit_index: i,
            });
        }
    }
    out.sort_by(|a, b| a.confidence.partial_cmp(&b.confidence).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(HARVEST_MAX_PER_CLIP);
    out
}

/// A whisper model kept resident on the GPU so a *batch* of ranges transcribes
/// with one model load instead of reloading per range (ADR 0002/0007: detection
/// refine transcribes ~N candidate Moments). Loading the model is the expensive
/// step; each [`Transcriber::transcribe`] runs against the resident model.
///
/// Since ADR 0072 "resident" means a live **`yc-whisper` child** holding the
/// CUDA context: this struct owns the child, and dropping it kills the child —
/// the same synchronous VRAM release the in-process drop gave, so the staging
/// discipline below is unchanged. A CUDA abort under GPU contention (another
/// app exhausting VRAM) now kills the child and fails the JOB with a
/// "GPU busy — retry when free" error; the app and UI stay alive.
///
/// GPU discipline: hold one of these only while transcription owns the VRAM -
/// drop it before the LLM stage loads (M4), since the 8 GB card stages
/// strictly sequentially.
pub struct Transcriber {
    /// The live `yc-whisper` child (the resident model). Mutex only for
    /// interior mutability — the API stays `&self` so no caller changes; the
    /// pipeline worker is the single production caller and calls serially.
    conn: std::sync::Mutex<wire::SidecarConn>,
    /// Beam-search decoding (ADR 0027) for accuracy on the caption path; greedy
    /// for the bulk text-only detect refine (speed). Tied to the DTW load.
    beam: bool,
    /// Silero VAD model path when the caption path opted in (`YC_VAD=1` + the
    /// model beside the whisper model) — pre-segments so whisper never decodes a
    /// pure-noise window. `None` = VAD off (the default).
    vad_model: Option<String>,
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
    /// (`filter_width < a->ne[2]`) on a sparse, few-token window — common when
    /// scanning many arbitrary candidate ranges, some of which are music/SFX
    /// with almost no speech. (Since ADR 0072 that assert would abort the
    /// yc-whisper CHILD, not the app — still a failed detect, so the DTW-less
    /// load remains the right call here.)
    pub fn load_text_only(model: &Path) -> Result<Self> {
        Self::load_inner(model, false)
    }

    fn load_inner(model: &Path, dtw: bool) -> Result<Self> {
        // Opt-in VAD (caption load only): resolve the Silero model beside the
        // whisper model. Requested-but-missing warns and runs without, so a
        // stray YC_VAD=1 never sinks a render. Resolved HERE (app-side), like
        // every env-driven knob — the sidecar reads no env (ADR 0072).
        let vad_model = if dtw && parse_vad_requested(env_opt("YC_VAD").as_deref()) {
            let path = model.with_file_name(VAD_MODEL_FILE);
            if path.is_file() {
                Some(path.to_string_lossy().into_owned())
            } else {
                tracing::warn!(
                    "caption: YC_VAD=1 but {} is missing - VAD off",
                    path.display()
                );
                None
            }
        } else {
            None
        };
        // Spawn the yc-whisper child: it loads the CUDA context (DTW heads on
        // the caption load) before answering its ready line, so a load failure
        // — missing model, or the GPU already saturated by another app —
        // surfaces right here, exactly where the in-process load used to fail.
        let conn = wire::SidecarConn::spawn(model, dtw)
            .with_context(|| format!("loading whisper model {} via yc-whisper", model.display()))?;
        // The DTW (caption) load decodes with beam search for accuracy; the
        // text-only detect load stays greedy for speed (ADR 0027).
        Ok(Self { conn: std::sync::Mutex::new(conn), beam: dtw, vad_model })
    }

    /// Transcribe one range's 16 kHz mono f32 samples into animatable caption
    /// units, reusing the resident model. Timestamps are 0-based to the range.
    ///
    /// `should_abort` is **live** (ADR 0072 — the signature waited for exactly
    /// this): polled ~every 50 ms while the decode runs in the yc-whisper
    /// child; returning `true` kills the child mid-decode (VRAM freed by
    /// process death) and fails with "cancelled". The detect loop's
    /// between-candidate check still stands on top.
    pub fn transcribe(
        &self,
        samples: &[f32],
        language: Language,
        lexicon: &DialectLexicon,
        should_abort: impl FnMut() -> bool + 'static,
    ) -> Result<Transcript> {
        Ok(self.run(samples, language, lexicon, should_abort)?.0)
    }

    /// Like [`Self::transcribe`], but also returns the **auto-harvest candidates**
    /// — words whisper was unsure about that the store doesn't yet know. The
    /// caption path appends these to the dialect store for the operator to review
    /// (so the dict keeps improving without manual garble-hunting).
    pub fn transcribe_with_harvest(
        &self,
        samples: &[f32],
        language: Language,
        lexicon: &DialectLexicon,
        should_abort: impl FnMut() -> bool + 'static,
    ) -> Result<(Transcript, Vec<HarvestCandidate>)> {
        let (transcript, _conf, harvest) =
            self.transcribe_full(samples, language, lexicon, should_abort)?;
        Ok((transcript, harvest))
    }

    /// Like [`Self::transcribe_with_harvest`], but also returns the **per-unit
    /// whisper confidence** (the minimum token probability over each unit's
    /// tokens), aligned 1:1 with the returned units. The LLM caption-correction
    /// pass (ADR 0030) gates on it: a word whisper was *unsure* of it may auto-fix
    /// or drop, a *confident* one it may change only via a curated context
    /// override. Confidence survives the dialect dict's multi-word collapse (it
    /// shrinks in lockstep), so it still lines up with the corrected units.
    pub fn transcribe_full(
        &self,
        samples: &[f32],
        language: Language,
        lexicon: &DialectLexicon,
        should_abort: impl FnMut() -> bool + 'static,
    ) -> Result<(Transcript, Vec<f32>, Vec<HarvestCandidate>)> {
        let (transcript, conf) = self.run(samples, language, lexicon, should_abort)?;
        let harvest = harvest_candidates(&transcript.units, &conf, lexicon);
        Ok((transcript, conf, harvest))
    }

    fn run(
        &self,
        samples: &[f32],
        language: Language,
        lexicon: &DialectLexicon,
        should_abort: impl FnMut() -> bool + 'static,
    ) -> Result<(Transcript, Vec<f32>)> {
        // Opt-in decoder priming (#1): only when the store sets `prime`, since
        // it biases the whole transcription (drift risk). The dict (#2, applied
        // after grouping) is the always-on, risk-free fix.
        let prompt = if lexicon.prime { lexicon.initial_prompt() } else { String::new() };

        // Caption-path decode trial knobs (2026-07-02, ADR 0033; see the parse_*
        // docs). ALL OFF by default — a decode change re-garbles the words the
        // operator's curated corrections are keyed to, so each is an explicit
        // per-render opt-in. The detect load keeps whisper defaults regardless.
        // Resolved HERE from env — the request carries them resolved, so the
        // render path's scoped YC_SUPPRESS_NST write keeps working even though
        // the child inherited its env at spawn (ADR 0072).
        let (no_context, suppress_nst) = if self.beam {
            // Fresh slate per 30 s window, UNLESS the store primes: whisper.cpp
            // feeds `initial_prompt` through the same cross-window context
            // (`prompt_past`) that `no_context` disables, so forcing no_context
            // would silently kill opt-in priming.
            let no_context =
                prompt.is_empty() && parse_caption_noctx(env_opt("YC_CAPTION_NOCTX").as_deref());
            let suppress_nst = parse_suppress_nst(env_opt("YC_SUPPRESS_NST").as_deref());
            tracing::info!(
                "caption decode: beam={BEAM_SIZE} no_context={no_context} \
                 suppress_nst={suppress_nst} vad={}",
                if self.vad_model.is_some() { "on" } else { "off" },
            );
            (no_context, suppress_nst)
        } else {
            (false, false)
        };

        // Caption path: beam search (whisper.cpp's quality decoder) — render
        // time is no object and accuracy is the goal (ADR 0027). Detect refine:
        // greedy, to keep the many-candidate scan fast. The old in-process
        // abort-callback story (it collapsed CUDA-graph batching; see
        // yc-whisper's main.rs) is moot here: `should_abort` is REAL now — the
        // client polls it while awaiting the response and kills the child on
        // cancel, freeing the VRAM by process death (ADR 0072).
        let req = wire::DecodeRequest {
            op: "decode".into(),
            language: lang_code(language).into(),
            prompt,
            beam_size: if self.beam { Some(BEAM_SIZE) } else { None },
            no_context,
            suppress_nst,
            vad_model: self.vad_model.clone(),
            n_samples: samples.len(),
        };
        let raw = self
            .conn
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .decode(&req, samples, should_abort)?;
        let raw_tokens: Vec<(String, f64, f64, f32)> =
            raw.into_iter().map(|wire::WireToken(text, t0, t1, p)| (text, t0, t1, p)).collect();

        let (mut units, mut conf) = group_tokens(raw_tokens, language);
        // Multi-word phrase corrections first (they collapse units + shrink conf in
        // lockstep), then the single-word fix-ups over the result (ADR 0014).
        apply_multiword_corrections(&mut units, &mut conf, lexicon);
        correct_known_mishears(&mut units, lexicon);
        Ok((Transcript { language, units }, conf))
    }
}

/// Load the model, transcribe one range, and drop the model — the one-shot path
/// for the Promote pipeline (M1/M2). Detection refine loads a [`Transcriber`]
/// once and reuses it across the candidate batch instead (ADR 0007).
pub fn transcribe_range(
    model: &Path,
    samples: &[f32],
    language: Language,
    lexicon: &DialectLexicon,
    should_abort: impl FnMut() -> bool + 'static,
) -> Result<Transcript> {
    Transcriber::load(model)?.transcribe(samples, language, lexicon, should_abort)
}

/// Like [`transcribe_range`], but also returns the auto-harvest candidates so the
/// caption path can self-populate the dialect store's review queue.
pub fn transcribe_range_harvesting(
    model: &Path,
    samples: &[f32],
    language: Language,
    lexicon: &DialectLexicon,
    should_abort: impl FnMut() -> bool + 'static,
) -> Result<(Transcript, Vec<HarvestCandidate>)> {
    Transcriber::load(model)?.transcribe_with_harvest(samples, language, lexicon, should_abort)
}

/// Like [`transcribe_range_harvesting`], but also returns the per-unit whisper
/// confidence the LLM caption-correction pass gates on (ADR 0030). One-shot model
/// load for the Promote path; the resident [`Transcriber::transcribe_full`] is the
/// reusable form.
pub fn transcribe_range_full(
    model: &Path,
    samples: &[f32],
    language: Language,
    lexicon: &DialectLexicon,
    should_abort: impl FnMut() -> bool + 'static,
) -> Result<(Transcript, Vec<f32>, Vec<HarvestCandidate>)> {
    Transcriber::load(model)?.transcribe_full(samples, language, lexicon, should_abort)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_decode_trial_knobs_are_strictly_opt_in() {
        // ALL default off (ADR 0033): a decode change re-garbles the words the
        // curated corrections are keyed to, so it must never be a silent default.
        assert!(!parse_caption_noctx(None));
        assert!(parse_caption_noctx(Some("1")));
        assert!(!parse_caption_noctx(Some("0")));
        assert!(!parse_suppress_nst(None));
        assert!(parse_suppress_nst(Some("1")));
        assert!(!parse_suppress_nst(Some("0")));
        assert!(!parse_vad_requested(None));
        assert!(parse_vad_requested(Some("1")));
        assert!(!parse_vad_requested(Some("true")));
    }

    #[test]
    fn groups_leading_space_tokens_into_words_with_min_confidence() {
        let toks = vec![
            (" Hello".to_string(), 0.0, 0.4, 0.95),
            (" world".to_string(), 0.4, 0.8, 0.90),
            ("!".to_string(), 0.8, 0.9, 0.30), // extends "world", drags conf to the min
            (" GG".to_string(), 1.0, 1.2, 0.80),
        ];
        let (units, conf) = group_into_words(toks);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0].text, "Hello");
        assert_eq!(units[1].text, "world!");
        assert_eq!(units[2].text, "GG");
        assert_eq!(units[0].start_s, 0.0);
        assert_eq!(units[1].end_s, 0.9); // end advanced by the "!" token
        // a unit's confidence is its least-sure token: min(0.90, 0.30)
        assert_eq!(conf.len(), 3);
        assert!((conf[1] - 0.30).abs() < 1e-6);
    }

    #[test]
    fn first_token_without_leading_space_still_starts_a_word() {
        let toks =
            vec![("Yo".to_string(), 0.0, 0.3, 0.9), ("urs".to_string(), 0.3, 0.5, 0.8)];
        let (units, conf) = group_into_words(toks);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].text, "Yours");
        assert_eq!(units[0].end_s, 0.5);
        assert_eq!(conf, vec![0.8]); // min(0.9, 0.8)
    }

    // --- JA character chunking (ADR 0003) -----------------------------------

    /// One JA token (no leading space), `chars` long, spanning [t0,t1] at prob p.
    fn jtok(text: &str, t0: f64, t1: f64, p: f32) -> (String, f64, f64, f32) {
        (text.to_string(), t0, t1, p)
    }

    #[test]
    fn ja_chunks_at_the_char_budget_with_min_confidence() {
        // Five single-char JA tokens, no leading spaces -> chunks of <=4 chars.
        // Word grouping would collapse all five into one unit; chunking splits them.
        let toks = vec![
            jtok("こ", 0.0, 0.1, 0.9),
            jtok("ん", 0.1, 0.2, 0.8),
            jtok("に", 0.2, 0.3, 0.95),
            jtok("ち", 0.3, 0.4, 0.7),
            jtok("は", 0.4, 0.5, 0.6),
        ];
        let (units, conf) = group_into_chars(toks);
        assert_eq!(units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["こんにち", "は"]);
        // First chunk spans its first..last token; conf is its least-sure token.
        assert_eq!(units[0].start_s, 0.0);
        assert!((units[0].end_s - 0.4).abs() < 1e-9);
        assert!((conf[0] - 0.7).abs() < 1e-6); // min(0.9,0.8,0.95,0.7)
        assert!((conf[1] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn ja_sentence_punctuation_closes_a_chunk() {
        // A token ending in sentence punctuation keeps the punctuation with the
        // preceding text but closes the chunk, so the next chars start fresh.
        let toks = vec![
            jtok("あ", 0.0, 0.1, 0.9),
            jtok("い", 0.1, 0.2, 0.9),
            jtok("。", 0.2, 0.3, 0.9),
            jtok("う", 0.3, 0.4, 0.9),
            jtok("え", 0.4, 0.5, 0.9),
        ];
        let units = group_into_chars(toks).0;
        assert_eq!(units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["あい。", "うえ"]);
    }

    #[test]
    fn ja_never_splits_a_multi_char_token() {
        // A 3-char token then a 2-char token: 3+2 > 4, so they land in separate
        // chunks rather than the token being cut to fill the budget exactly.
        let toks = vec![jtok("あいう", 0.0, 0.3, 0.9), jtok("えお", 0.3, 0.5, 0.8)];
        let units = group_into_chars(toks).0;
        assert_eq!(units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["あいう", "えお"]);
        // An oversized single token is its own chunk, never dropped.
        let big = group_into_chars(vec![jtok("あいうえお", 0.0, 0.5, 0.9)]).0;
        assert_eq!(big.len(), 1);
        assert_eq!(big[0].text, "あいうえお");
    }

    #[test]
    fn group_tokens_dispatches_by_language() {
        // JA -> character chunks (one segment, no spaces, splits).
        let ja = vec![jtok("か", 0.0, 0.1, 0.9), jtok("き", 0.1, 0.2, 0.9), jtok("く", 0.2, 0.3, 0.9),
                      jtok("け", 0.3, 0.4, 0.9), jtok("こ", 0.4, 0.5, 0.9)];
        let (ja_units, _) = group_tokens(ja, Language::Ja);
        assert_eq!(ja_units.len(), 2); // "かきくけ" (4) + "こ" (1), not one blob
        // EN -> word grouping (leading-space tokens start words), unchanged.
        let en = vec![jtok(" hello", 0.0, 0.4, 0.9), jtok(" world", 0.4, 0.8, 0.9)];
        let (en_units, _) = group_tokens(en, Language::En);
        assert_eq!(en_units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["hello", "world"]);
    }

    #[test]
    fn harvest_flags_only_unsure_unknown_real_words() {
        let mk = |t: &str| CaptionUnit { text: t.into(), start_s: 0.0, end_s: 0.4 };
        let units = vec![
            mk("mendoakan"),   // low conf but in vocabulary -> skip
            mk("lenjakgawa"),  // low conf, unknown, long -> HARVEST
            mk("ya"),          // low conf but too short -> skip
            mk("profesi"),     // a known `right` (and confident) -> skip
            mk("kusursekali"), // low conf, unknown -> HARVEST
            mk("jelas"),       // unknown but confident -> skip
        ];
        let conf = vec![0.20, 0.10, 0.05, 0.95, 0.30, 0.99];
        let lex = DialectLexicon {
            vocabulary: vec!["mendoakan".into()],
            corrections: vec![Correction {
                wrong: "protesi".into(),
                right: "profesi".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let got = harvest_candidates(&units, &conf, &lex);
        let words: Vec<&str> = got.iter().map(|c| c.word.as_str()).collect();
        // least-confident first: lenjakgawa(0.10) before kusursekali(0.30)
        assert_eq!(words, vec!["lenjakgawa", "kusursekali"]);
        // Each candidate addresses its source unit (pre-refine order), so the
        // render can skip candidates whose unit the timing pass drops — the
        // index survives the least-confident-first sort.
        assert_eq!(got[1].unit_index, 4, "kusursekali is the 5th unit");
        assert_eq!(units[got[0].unit_index].text, "lenjakgawa");
    }

    #[test]
    fn harvest_skips_real_dictionary_words() {
        let mk = |t: &str| CaptionUnit { text: t.into(), start_s: 0.0, end_s: 0.4 };
        let units = vec![mk("berapa"), mk("sempurxyz")]; // both flagged unsure
        let conf = vec![0.10, 0.10];
        let mut dictionary = HashSet::new();
        dictionary.insert("berapa".to_string()); // a real word whisper merely doubted
        let lex = DialectLexicon { dictionary, ..Default::default() };
        let got = harvest_candidates(&units, &conf, &lex);
        let words: Vec<&str> = got.iter().map(|c| c.word.as_str()).collect();
        assert_eq!(words, vec!["sempurxyz"]); // only the out-of-dictionary garble survives
    }

    #[test]
    fn harvest_candidates_carry_their_clip_relative_onset() {
        // ADR 0022: each candidate keeps its unit's onset so the render can record
        // an absolute VOD timestamp.
        let units = vec![
            CaptionUnit { text: "garblexyz".into(), start_s: 12.5, end_s: 13.0 },
        ];
        let got = harvest_candidates(&units, &[0.10], &DialectLexicon::default());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].word, "garblexyz");
        assert!((got[0].start_s - 12.5).abs() < 1e-9);
    }

    #[test]
    fn names_words_filters_common_names_but_not_unusual_handles() {
        // ADR 0023: a bundled common given name is loaded into the harvest
        // dictionary (always, skipping `#` comments), so a correctly-read name is
        // not flagged; an unusual viewer handle is absent and still harvests.
        let dir = std::env::temp_dir().join("yc_names_words_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("names.words.txt"), "# a comment\nbudi\nsiti\njohn\n").unwrap();

        let lex = DialectLexicon::load(&dir, Language::Id);
        assert!(lex.dictionary.contains("budi"), "name loaded into the dictionary");
        assert!(!lex.dictionary.contains("# a comment"), "comment line skipped");

        let mk = |t: &str| CaptionUnit { text: t.into(), start_s: 0.0, end_s: 0.4 };
        let units = vec![mk("Budi"), mk("zxqwerty")]; // common name + unusual handle
        let got = harvest_candidates(&units, &[0.10, 0.10], &lex);
        let words: Vec<&str> = got.iter().map(|c| c.word.as_str()).collect();
        assert_eq!(words, vec!["zxqwerty"], "common name filtered, unusual handle harvested");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vod_clock_formats_minutes_and_hours() {
        assert_eq!(vod_clock(0.0), "0:00");
        assert_eq!(vod_clock(83.0), "1:23");
        assert_eq!(vod_clock(3723.0), "1:02:03"); // past an hour -> h:mm:ss
    }

    #[test]
    fn parse_harvest_note_full_entry() {
        // The exact shape harvest_to_file writes with a title (ADR 0022).
        let n = "auto-harvested (conf 0.42) from \"He LOST it on the boss\" at 1:23:45 - operator verify";
        let p = parse_harvest_note(n);
        assert_eq!(p.confidence, Some(0.42));
        assert_eq!(p.title.as_deref(), Some("He LOST it on the boss"));
        assert_eq!(p.at_s, Some((3600 + 23 * 60 + 45) as f64));
    }

    #[test]
    fn parse_harvest_note_no_title() {
        // A manual clip (no generated title) degrades to `at h:mm:ss`.
        let p = parse_harvest_note("auto-harvested (conf 0.13) at 36:49 - operator verify");
        assert_eq!(p.confidence, Some(0.13));
        assert_eq!(p.title, None);
        assert_eq!(p.at_s, Some((36 * 60 + 49) as f64));
    }

    #[test]
    fn parse_harvest_note_real_store_entries() {
        // Verbatim from workspace/Ino Gemink Live Streaming/id.json.
        let p = parse_harvest_note(
            "auto-harvested (conf 0.21) from \"Nyoli Setan, Kaki Tiket!\" at 1:11:11 - operator verify",
        );
        assert_eq!(p.confidence, Some(0.21));
        assert_eq!(p.title.as_deref(), Some("Nyoli Setan, Kaki Tiket!")); // comma + `!` survive
        assert_eq!(p.at_s, Some((3600 + 11 * 60 + 11) as f64));
    }

    #[test]
    fn parse_harvest_note_roundtrips_vod_clock() {
        for t in [0.0, 59.0, 60.0, 3599.0, 3600.0, 4271.0] {
            assert_eq!(parse_vod_clock(&vod_clock(t)), Some(t), "roundtrip {t}");
        }
    }

    #[test]
    fn parse_harvest_note_tolerates_junk() {
        // A hand-written note with none of the machine fields -> all None, still a to-do.
        assert_eq!(parse_harvest_note("some hand-written note"), HarvestNote::default());
        assert_eq!(parse_vod_clock("not:a:clock"), None);
        assert_eq!(parse_vod_clock("1:2:3:4"), None);
    }

    #[test]
    fn merge_corrections_a_todo_does_not_shadow_a_confirmed_fix() {
        // ADR 0031/0032: a stale per-clip to-do (blank right) must not erase a
        // confirmed per-Creator fix of the same word when layered on top.
        let mut base = vec![Correction {
            wrong: "dijekat".into(),
            right: "dicegat".into(),
            status: "confirmed".into(),
            ..Default::default()
        }];
        merge_corrections(
            &mut base,
            vec![Correction {
                wrong: "dijekat".into(),
                right: String::new(),
                status: "unverified".into(),
                ..Default::default()
            }],
        );
        assert_eq!(base.len(), 1);
        assert_eq!(base[0].right, "dicegat", "the confirmed fix survives the stale to-do");
        // A filled overlay still wins (clip > creator) — real layering preserved.
        merge_corrections(
            &mut base,
            vec![Correction { wrong: "dijekat".into(), right: "digondol".into(), ..Default::default() }],
        );
        assert_eq!(base[0].right, "digondol");
    }

    #[test]
    fn harvest_to_store_writes_title_and_absolute_timestamp_in_the_note() {
        // ADR 0022: the note records the Short title + the VOD-absolute time
        // (clip_start_s + the word's clip-relative onset), so id.json is curatable.
        let dir = std::env::temp_dir().join("yc_harvest_note_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let cands =
            vec![HarvestCandidate {
                word: "garblexyz".into(),
                confidence: 0.42,
                start_s: 12.0,
                unit_index: 0,
            }];
        // Clip starts at 1:00:00 in the VOD; the word at +12 s -> 1:00:12.
        let n = DialectLexicon::harvest_to_store(
            &dir,
            Language::Id,
            &cands,
            3600.0,
            Some("He LOST it on the boss"),
        );
        assert_eq!(n, 1);
        let saved = std::fs::read_to_string(dir.join("id.json")).unwrap();
        let lex: DialectLexicon = serde_json::from_str(&saved).unwrap();
        let note = &lex.corrections[0].note;
        assert!(note.contains("He LOST it on the boss"), "title in note: {note}");
        assert!(note.contains("1:00:12"), "absolute timestamp in note: {note}");
        assert!(note.contains("0.42"), "confidence in note: {note}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_corrections_at_s_entries_coexist_per_moment() {
        // The Deddy gate case: three "blok on" pairs in one clip, one a
        // different word. Keyed by wrong alone they collapse to the last;
        // the moment is part of the identity.
        let mut into = vec![Correction {
            wrong: "blok on".into(),
            right: "blo'on".into(),
            at_s: Some(1846.8),
            ..Default::default()
        }];
        merge_corrections(
            &mut into,
            vec![
                // same wrong, different moment -> coexists
                Correction {
                    wrong: "blok on".into(),
                    right: "goblok".into(),
                    at_s: Some(1848.0),
                    ..Default::default()
                },
                // same wrong + same moment -> overlay replaces
                Correction {
                    wrong: "blok on".into(),
                    right: "blo'on".into(),
                    note: "updated".into(),
                    at_s: Some(1846.8),
                    ..Default::default()
                },
                // same wrong, global (no anchor) -> coexists with anchored ones
                Correction { wrong: "blok on".into(), right: "global".into(), ..Default::default() },
            ],
        );
        assert_eq!(into.len(), 3);
        assert_eq!(into[0].note, "updated");
        assert_eq!(into[1].right, "goblok");
        assert_eq!(into[2].right, "global");
    }

    #[test]
    fn merge_corrections_overlay_wins_on_duplicate_else_appends() {
        let mut base = vec![
            Correction { wrong: "a".into(), right: "base-a".into(), ..Default::default() },
            Correction { wrong: "b".into(), right: "base-b".into(), ..Default::default() },
        ];
        let overlay = vec![
            Correction { wrong: "A".into(), right: "over-a".into(), ..Default::default() }, // dup (ci) -> replace
            Correction { wrong: "c".into(), right: "over-c".into(), ..Default::default() }, // new -> append
        ];
        merge_corrections(&mut base, overlay);
        assert_eq!(base.len(), 3);
        assert_eq!(base.iter().find(|c| c.wrong.eq_ignore_ascii_case("a")).unwrap().right, "over-a");
        assert_eq!(base.iter().find(|c| c.wrong == "b").unwrap().right, "base-b");
        assert!(base.iter().any(|c| c.wrong == "c"));
    }

    #[test]
    fn load_layered_stacks_clip_over_creator_over_base() {
        // ADR 0031: base < per-Creator < per-clip on a duplicate `wrong`.
        let dir = std::env::temp_dir().join("yc_layered_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("id.json"),
            r#"{"language":"id","corrections":[{"wrong":"x","right":"base-x"},{"wrong":"shared","right":"base"}]}"#,
        )
        .unwrap();
        let creator = dir.join("creator.json");
        std::fs::write(&creator, r#"{"corrections":[{"wrong":"shared","right":"creator"},{"wrong":"y","right":"creator-y"}]}"#).unwrap();
        let clip = dir.join("clip.json");
        std::fs::write(&clip, r#"{"corrections":[{"wrong":"shared","right":"clip"}]}"#).unwrap();
        let lex = DialectLexicon::load_layered(&dir, &[creator, clip], Language::Id);
        let r = |w: &str| lex.corrections.iter().find(|c| c.wrong == w).map(|c| c.right.clone());
        assert_eq!(r("x"), Some("base-x".into()));
        assert_eq!(r("y"), Some("creator-y".into()));
        assert_eq!(r("shared"), Some("clip".into())); // clip wins
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn promote_confirmed_copies_filled_rights_to_creator_skipping_known() {
        let dir = std::env::temp_dir().join("yc_promote_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.json");
        std::fs::write(&clip, r#"{"corrections":[
            {"wrong":"buntur","right":"Guntur"},
            {"wrong":"todo","right":""},
            {"wrong":"cowok","right":"cok","context":true}]}"#).unwrap();
        let creator = dir.join("creator.json");
        std::fs::write(&creator, r#"{"language":"id","corrections":[{"wrong":"buntur","right":"Guntur"}]}"#).unwrap();
        // buntur already known -> skip; blank `todo` -> skip; cowok -> promoted (keeps context).
        let n = DialectLexicon::promote_confirmed(&clip, &creator, Language::Id);
        assert_eq!(n, 1);
        let saved: DialectLexicon =
            serde_json::from_str(&std::fs::read_to_string(&creator).unwrap()).unwrap();
        assert!(saved.corrections.iter().any(|c| c.wrong == "cowok" && c.right == "cok" && c.context));
        assert_eq!(saved.corrections.iter().filter(|c| c.wrong == "buntur").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn promote_confirmed_skips_clip_only() {
        // A clip_only correction fixes THIS clip but must NOT rise to the Creator store
        // (it would corrupt other clips where the word is literal) - 2026-07-02.
        let dir = std::env::temp_dir().join("yc_promote_cliponly_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.json");
        std::fs::write(
            &clip,
            r#"{"corrections":[
            {"wrong":"buntur","right":"Guntur"},
            {"wrong":"pancingan","right":"bajingan","clip_only":true}]}"#,
        )
        .unwrap();
        let creator = dir.join("creator.json");
        std::fs::write(&creator, r#"{"language":"id","corrections":[]}"#).unwrap();
        let n = DialectLexicon::promote_confirmed(&clip, &creator, Language::Id);
        assert_eq!(n, 1, "only the non-clip_only correction promotes");
        let saved: DialectLexicon =
            serde_json::from_str(&std::fs::read_to_string(&creator).unwrap()).unwrap();
        assert!(saved.corrections.iter().any(|c| c.wrong == "buntur"));
        assert!(
            !saved.corrections.iter().any(|c| c.wrong == "pancingan"),
            "clip_only stays scoped to its clip"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
        let lex = DialectLexicon {
            corrections: vec![
                Correction { wrong: "bocal".into(), right: "bocil".into(), ..Default::default() },
                // blank `right` = unverified operator to-do: must be skipped, not
                // used to blank the word.
                Correction { wrong: "lenjak".into(), right: String::new(), ..Default::default() },
            ],
            ..Default::default()
        };
        let mut units = vec![mk("Bocal"), mk("bocal,"), mk("lokal"), mk("lenjak")];
        correct_known_mishears(&mut units, &lex);
        assert_eq!(units[0].text, "bocil"); // case-insensitive match
        assert_eq!(units[1].text, "bocil,"); // trailing punctuation preserved
        assert_eq!(units[2].text, "lokal"); // not a key - untouched
        assert_eq!(units[3].text, "lenjak"); // unverified (blank right) - untouched

        // An empty lexicon is a no-op (the pre-store behavior).
        let mut u2 = vec![mk("bocal")];
        correct_known_mishears(&mut u2, &DialectLexicon::default());
        assert_eq!(u2[0].text, "bocal");
    }

    // --- multi-word phrase corrections (ADR 0014) ---------------------------

    fn phrase_lex() -> DialectLexicon {
        DialectLexicon {
            corrections: vec![Correction {
                wrong: "point blank".into(),
                right: "Point Blank".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn multiword_correction_collapses_units_and_keeps_timing_and_conf() {
        let mk = |t: &str, s: f64, e: f64| CaptionUnit { text: t.into(), start_s: s, end_s: e };
        // "main point blank!" -> "main" then the collapsed "Point Blank!".
        let mut units =
            vec![mk("main", 0.0, 0.3), mk("point", 0.3, 0.6), mk("blank!", 0.6, 1.0)];
        let mut conf = vec![0.9, 0.4, 0.2];
        apply_multiword_corrections(&mut units, &mut conf, &phrase_lex());
        assert_eq!(
            units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(),
            vec!["main", "Point Blank!"] // collapsed, last unit's "!" kept
        );
        assert_eq!(units.len(), 2);
        // The collapsed unit spans the first matched start to the last matched end.
        assert!((units[1].start_s - 0.3).abs() < 1e-9);
        assert!((units[1].end_s - 1.0).abs() < 1e-9);
        // conf shrinks in lockstep; the collapsed unit keeps the least-sure (0.2).
        assert_eq!(conf.len(), 2);
        assert!((conf[1] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn multiword_correction_is_case_insensitive_and_skips_non_matches() {
        let mk = |t: &str| CaptionUnit { text: t.into(), start_s: 0.0, end_s: 0.4 };
        // Case-insensitive whole-phrase match.
        let mut hit = vec![mk("POINT"), mk("Blank")];
        let mut hc = vec![0.5, 0.5];
        apply_multiword_corrections(&mut hit, &mut hc, &phrase_lex());
        assert_eq!(hit.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(), vec!["Point Blank"]);
        // A partial / wrong phrase is left untouched (no collapse).
        let mut miss = vec![mk("point"), mk("guard")];
        let mut mc = vec![0.5, 0.5];
        apply_multiword_corrections(&mut miss, &mut mc, &phrase_lex());
        assert_eq!(miss.len(), 2);
        assert_eq!(miss[0].text, "point");
    }

    #[test]
    fn multiword_then_singleword_corrections_compose() {
        // The phrase pass runs first, then the single-word pass over the result —
        // both fire, and the collapsed phrase isn't re-touched by the word pass.
        let mk = |t: &str| CaptionUnit { text: t.into(), start_s: 0.0, end_s: 0.4 };
        let lex = DialectLexicon {
            corrections: vec![
                Correction { wrong: "point blank".into(), right: "Point Blank".into(), ..Default::default() },
                Correction { wrong: "bocal".into(), right: "bocil".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        let mut units = vec![mk("bocal"), mk("point"), mk("blank")];
        let mut conf = vec![0.9, 0.5, 0.5];
        apply_multiword_corrections(&mut units, &mut conf, &lex);
        correct_known_mishears(&mut units, &lex);
        assert_eq!(
            units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(),
            vec!["bocil", "Point Blank"]
        );
        assert_eq!(conf.len(), 2); // bocal(kept) + collapsed phrase
    }

    #[test]
    fn initial_prompt_is_a_bare_deduped_term_list_never_the_note() {
        let lex = DialectLexicon {
            // The note must NEVER reach the prompt: a descriptive sentence makes
            // whisper hallucinate YouTube boilerplate (measured on min3).
            note: "Streamer logat Medan main game live".into(),
            vocabulary: vec!["mendoakan".into(), "profesi".into()],
            corrections: vec![
                // confirmed right already in vocab -> deduped, primed once
                Correction { wrong: "mendokong".into(), right: "mendoakan".into(), ..Default::default() },
                Correction { wrong: "bocal".into(), right: "bocil".into(), ..Default::default() },
                // blank right -> not primed
                Correction { wrong: "wasi".into(), right: String::new(), ..Default::default() },
            ],
            ..Default::default()
        };
        let p = lex.initial_prompt();
        assert!(!p.to_lowercase().contains("streamer") && !p.contains("game")); // note not primed
        assert!(p.starts_with("mendoakan")); // first vocabulary term, no lead sentence
        assert!(p.contains("profesi") && p.contains("bocil"));
        assert!(!p.contains("wasi")); // blank-right entry is not primed
        assert_eq!(p.matches("mendoakan").count(), 1); // vocab + correction => once
    }

    #[test]
    fn empty_lexicon_has_no_prompt() {
        assert!(DialectLexicon::default().initial_prompt().is_empty());
    }
}
