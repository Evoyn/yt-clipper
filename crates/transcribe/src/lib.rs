//! Transcription: whisper.cpp via whisper-rs (CUDA build), model `large-v3`
//! (ADR 0003; M1 uses the f16 weights, the ADR's quantized ship-default is
//! revisited at M6). Whisper emits token-level timestamps; the language-aware
//! grouping layer in this crate converts tokens into animatable caption
//! units — space-delimited words for EN/ID, fixed-size character chunks for JA
//! (which whisper emits without inter-word spaces).
//!
//! M1 transcribes only the manually-picked range's samples (transcribe-range-
//! only), so timestamps are already 0-based to the clip and line up with the
//! render timeline. whisper-rs needs libclang at build time on Windows (its
//! bundled bindings are Linux-only).

use anyhow::{anyhow, Context, Result};
use std::collections::HashSet;
use std::path::Path;
use whisper_rs::{
    DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters,
};
use yc_core::{CaptionUnit, Language, Transcript};

mod correct;
pub use correct::{
    apply_correction, build_correction_request, CorrectionContext, CorrectionRequest,
    CorrectionStats,
};

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
            .filter(|c| !c.wrong.is_empty() && !c.right.is_empty() && !c.context)
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

    /// Append `candidates` to `<dir>/<lang>.json` as `unverified` to-dos, skipping
    /// any the store already knows (wrong / right / vocabulary). Re-reads the file
    /// first so concurrent operator edits to the `right` fields are preserved, and
    /// writes back pretty-printed. Returns how many new entries were added. Best-
    /// effort: a read/parse/write failure logs and adds nothing (never fails a
    /// render). The caption path calls this so the review queue self-populates.
    ///
    /// Each new entry's note records **where the word came from** (ADR 0022) so the
    /// operator can curate it: the generated Short `title` (if any) and the
    /// **absolute VOD timestamp** `clip_start_s + candidate.start_s` (the clip's VOD
    /// start plus the word's clip-relative onset), as `from "Title" at h:mm:ss`. A
    /// word is recorded once (first clip it is flagged in); a later clip skips it.
    pub fn harvest_to_store(
        dir: &Path,
        language: Language,
        candidates: &[HarvestCandidate],
        clip_start_s: f64,
        title: Option<&str>,
    ) -> usize {
        if candidates.is_empty() {
            return 0;
        }
        let path = dir.join(format!("{}.json", lang_code(language)));
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
                });
                added += 1;
            }
        }
        if added > 0 {
            match serde_json::to_string_pretty(&lex) {
                Ok(s) => {
                    if let Err(e) = std::fs::write(&path, s + "\n") {
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
        // Context-sensitive overrides (ADR 0030) are LLM-applied, never global.
        if c.wrong.is_empty() || c.right.is_empty() || c.context {
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

/// A word the harvest flags for review: the garbled text, the whisper confidence
/// that flagged it, and its **clip-relative onset**. The onset lets the render
/// record an absolute VOD timestamp (clip start + this) in the review note, so the
/// operator can jump to the source and check the word (ADR 0022).
#[derive(Debug, Clone)]
pub struct HarvestCandidate {
    pub word: String,
    pub confidence: f32,
    pub start_s: f64,
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
    for (u, &c) in units.iter().zip(conf.iter()) {
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
            out.push(HarvestCandidate { word: core, confidence: c, start_s: u.start_s });
        }
    }
    out.sort_by(|a, b| a.confidence.partial_cmp(&b.confidence).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(HARVEST_MAX_PER_CLIP);
    out
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
    /// Beam-search decoding (ADR 0027) for accuracy on the caption path; greedy
    /// for the bulk text-only detect refine (speed). Tied to the DTW load.
    beam: bool,
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
        // The DTW (caption) load decodes with beam search for accuracy; the
        // text-only detect load stays greedy for speed (ADR 0027).
        Ok(Self { ctx, beam: dtw })
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
        let mut state = self.ctx.create_state().context("creating whisper state")?;

        // Opt-in decoder priming (#1): only when the store sets `prime`, since
        // it biases the whole transcription (drift risk). The dict (#2, applied
        // after grouping) is the always-on, risk-free fix. Declared before
        // `params` so the prompt outlives the borrow.
        let prompt = if lexicon.prime { lexicon.initial_prompt() } else { String::new() };

        // Caption path: beam search (whisper.cpp's quality decoder) — render time is
        // no object and accuracy is the goal (ADR 0027). Detect refine: greedy, to
        // keep the many-candidate scan fast (it reads the excitement lexicon, not
        // exact words).
        let strategy = if self.beam {
            SamplingStrategy::BeamSearch { beam_size: BEAM_SIZE, patience: -1.0 }
        } else {
            SamplingStrategy::Greedy { best_of: 1 }
        };
        let mut params = FullParams::new(strategy);
        params.set_language(Some(lang_code(language)));
        if !prompt.is_empty() {
            params.set_initial_prompt(&prompt);
        }
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

        // Collect (text, t0_s, t1_s, prob) for every real token, then group.
        let mut raw_tokens: Vec<(String, f64, f64, f32)> = Vec::new();
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
                raw_tokens.push((text, t0 as f64 / 100.0, t1 as f64 / 100.0, data.p));
            }
        }

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
    fn harvest_to_store_writes_title_and_absolute_timestamp_in_the_note() {
        // ADR 0022: the note records the Short title + the VOD-absolute time
        // (clip_start_s + the word's clip-relative onset), so id.json is curatable.
        let dir = std::env::temp_dir().join("yc_harvest_note_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let cands =
            vec![HarvestCandidate { word: "garblexyz".into(), confidence: 0.42, start_s: 12.0 }];
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
