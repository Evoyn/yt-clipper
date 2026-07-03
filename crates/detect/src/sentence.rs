//! Sentence-boundary clip bounds (ADR 0040): detected Moments start at a
//! sentence START and end at a sentence END, so no clip opens or closes on a
//! half-spoken thought. ADR 0037 deliberately deferred this ("revisit if
//! trailing words get clipped in practice") — the operator filed exactly that,
//! so the snap now runs at the REFINE stage (ADR 0007), which already
//! transcribes each candidate and therefore has the words for free.
//!
//! The contract (grilled 2026-07-03):
//! - Sentences come from whisper punctuation (`. ? !` + id/en/ja variants),
//!   falling back to inter-word pauses when punctuation is absent.
//! - Bounds snap OUTWARD: a mid-sentence edge extends to that sentence's
//!   start/end. A trailing edge in dead air retreats to the last complete
//!   sentence (the tail pad the signal walk appended is not content).
//! - A short Moment grows to `min_dur_s` (45 s) by pulling in ADJACENT
//!   sentences — context fill through real speech only. When speech runs dry,
//!   the shorter clip ships: context over duration, never dead-air padding.
//! - `max_dur_s` is a hard cap: a sentence that would cross it is dropped
//!   (end at the last complete sentence under the cap); a single run-on that
//!   spans the cap region falls back to a pause boundary, then a word-boundary
//!   hard cut.
//! - Manually-marked Moments never pass through here (Transcript-override
//!   philosophy, ADR 0039): verbatim range, no snapping, no floor.

use yc_core::{CaptionUnit, Language, TimeRange};

/// An inter-word gap this long closes a sentence even without punctuation —
/// the "segment-gap pause" fallback for unpunctuated speech. Refine timestamps
/// are heuristic (no DTW), so this stays generous.
pub const PAUSE_SPLIT_S: f64 = 1.25;
/// A gap this long marks a usable in-sentence cut point for the cap's run-on
/// fallback (a breath, not a full stop).
pub const PAUSE_CUT_S: f64 = 0.40;
/// How far below the cap the run-on fallback may retreat to land on a breath
/// pause. Giving up at most this much content to avoid a mid-flow cut is the
/// trade; past it, a plain word boundary right under the cap wins.
pub const RUN_ON_PAUSE_BAND_S: f64 = 10.0;
/// Growth never bridges a silence longer than this to reach the next sentence:
/// past it the speech is a different beat, not context — shipping the shorter
/// clip beats padding with dead air (the operator's call).
pub const MAX_GROW_GAP_S: f64 = 4.0;

/// One sentence recovered from refine's word units: a contiguous run closed by
/// terminal punctuation or a pause. `closed` is false only for the window's
/// trailing run when the transcription cut it off mid-sentence — growth never
/// pulls such a run in (its true end is unknown).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sentence {
    pub start_s: f64,
    pub end_s: f64,
    pub closed: bool,
}

/// Whether this unit's text ends a sentence. Trailing closers (quotes,
/// brackets) are stripped first so `word."` still closes. For JA the terminal
/// may sit *inside* the unit: units there are fixed-size character chunks
/// (ADR 0003), so `だ。そ` carries the boundary mid-chunk — off by a couple of
/// characters of time, which is noise at clip scale.
fn ends_sentence(text: &str, language: Language) -> bool {
    const TERMINALS: &[char] = &['.', '?', '!', '…', '。', '？', '！', '｡', '‼', '⁇', '⁈', '⁉'];
    const CLOSERS: &[char] = &['"', '\'', ')', ']', '}', '」', '』', '）', '»'];
    let trimmed = text.trim_end().trim_end_matches(CLOSERS);
    if trimmed.ends_with(TERMINALS) {
        return true;
    }
    // JA chunks: a terminal anywhere in the chunk closes the sentence.
    language == Language::Ja && trimmed.contains(&['。', '？', '！', '｡'][..])
}

/// Group refine's word units (absolute VOD times) into [`Sentence`]s: a run
/// closes at terminal punctuation or at an inter-word pause > [`PAUSE_SPLIT_S`]
/// (the fallback for speech whisper leaves unpunctuated). The window's trailing
/// unclosed run is kept but marked `closed: false`.
pub fn split_sentences(units: &[CaptionUnit], language: Language) -> Vec<Sentence> {
    let mut out = Vec::new();
    let mut first: Option<usize> = None;
    for i in 0..units.len() {
        if first.is_none() {
            first = Some(i);
        }
        let gap_after =
            units.get(i + 1).map(|n| n.start_s - units[i].end_s).unwrap_or(f64::INFINITY);
        if ends_sentence(&units[i].text, language) || gap_after > PAUSE_SPLIT_S {
            out.push(Sentence {
                start_s: units[first.take().unwrap()].start_s,
                end_s: units[i].end_s,
                // The last run only counts as closed when punctuation says so —
                // a pause "closes" it too (gap_after is INFINITY at the end),
                // but past the window edge we can't tell pause from cut-off.
                closed: i + 1 < units.len() || ends_sentence(&units[i].text, language),
            });
        }
    }
    out
}

/// Snap a detected Moment's signal window (`seed`) to sentence bounds, grow it
/// toward `min_dur_s` through adjacent sentences, and enforce `max_dur_s`.
/// `units` must carry absolute VOD times covering the seed plus the transcribe
/// pad. Pure and total: with no usable speech the seed returns unchanged (a
/// speech-free moment — music, a raid — has no word edges to protect).
pub fn sentence_bounds(
    units: &[CaptionUnit],
    language: Language,
    seed: TimeRange,
    min_dur_s: f64,
    max_dur_s: f64,
) -> TimeRange {
    let sents = split_sentences(units, language);
    if sents.is_empty() {
        return seed;
    }

    // --- Snap outward -------------------------------------------------------
    // Start: mid-sentence -> that sentence's start. In a gap -> keep the seed
    // start: the ADR 0020 pre-roll lead is deliberate (the event before the
    // reaction), and a start in silence clips no words.
    let (mut lo, mut start_s) = match sents.iter().position(|s| contains(s, seed.start_s)) {
        Some(i) => (i, sents[i].start_s.min(seed.start_s)),
        None => match sents.iter().position(|s| s.start_s >= seed.start_s) {
            Some(i) => (i, seed.start_s),
            // All speech lies before the seed — nothing to snap to.
            None => return seed,
        },
    };
    // End: mid-sentence -> that sentence's end (the fix for clipped trailing
    // words). In a gap -> retreat to the last complete sentence: the trailing
    // silence is the signal walk's pad, not content.
    let (mut hi, mut end_s) = match sents.iter().position(|s| contains(s, seed.end_s)) {
        Some(i) => (i, sents[i].end_s.max(seed.end_s)),
        None => match sents.iter().rposition(|s| s.end_s <= seed.end_s) {
            Some(i) => (i, sents[i].end_s),
            // All speech lies after the seed — nothing to snap to.
            None => return seed,
        },
    };
    if lo > hi || end_s <= start_s {
        // The seed straddles a silence with no complete sentence inside it.
        return seed;
    }

    // --- Grow to the floor through real speech ------------------------------
    // Pull in the adjacent sentence with the smaller silence to bridge (ties
    // forward: the reaction usually continues). A side is eligible while the
    // result fits the cap and the bridged gap stays conversational
    // ([`MAX_GROW_GAP_S`]); an unclosed trailing run is never pulled (its true
    // end is unknown). When both sides run dry the shorter clip ships.
    while end_s - start_s < min_dur_s {
        let prev = (lo > 0).then(|| sents[lo - 1]);
        let next = sents.get(hi + 1).copied().filter(|s| s.closed);
        let prev_gap = prev.map(|p| start_s - p.end_s);
        let next_gap = next.map(|n| n.start_s - end_s);
        let prev_ok = prev.is_some_and(|p| {
            end_s - p.start_s <= max_dur_s && prev_gap.unwrap() <= MAX_GROW_GAP_S
        });
        let next_ok = next.is_some_and(|n| {
            n.end_s - start_s <= max_dur_s && next_gap.unwrap() <= MAX_GROW_GAP_S
        });
        let take_prev = match (prev_ok, next_ok) {
            (true, true) => prev_gap.unwrap() < next_gap.unwrap(),
            (true, false) => true,
            (false, true) => false,
            (false, false) => break, // speech ran dry — ship the shorter clip
        };
        if take_prev {
            lo -= 1;
            start_s = sents[lo].start_s;
        } else {
            hi += 1;
            end_s = sents[hi].end_s;
        }
    }

    // --- Enforce the cap -----------------------------------------------------
    // Drop trailing sentences (end at the last complete one under the cap),
    // but never cut back into the signal window's own content: `end_floor`
    // protects min(seed end, cap). When the boundary sentence itself spans the
    // cap region (a run-on), fall back to a pause inside it, then to a word
    // boundary — never mid-word.
    if end_s - start_s > max_dur_s {
        let cap_t = start_s + max_dur_s;
        let end_floor = seed.end_s.min(cap_t);
        while hi > lo && end_s - start_s > max_dur_s && sents[hi - 1].end_s >= end_floor {
            hi -= 1;
            end_s = sents[hi].end_s;
        }
        if end_s - start_s > max_dur_s {
            end_s = run_on_cut(units, cap_t);
        }
    }

    if end_s > start_s {
        TimeRange { start_s, end_s }
    } else {
        seed
    }
}

fn contains(s: &Sentence, t: f64) -> bool {
    s.start_s <= t && t < s.end_s
}

/// The cap's run-on fallback, best cut first: the latest word end followed by
/// a breath-length pause ([`PAUSE_CUT_S`]) within [`RUN_ON_PAUSE_BAND_S`] of
/// the cap; else the latest word end under the cap (never mid-word); else the
/// start of the word crossing the cap; else the cap itself (silence there).
fn run_on_cut(units: &[CaptionUnit], cap_t: f64) -> f64 {
    let pause_floor = cap_t - RUN_ON_PAUSE_BAND_S;
    let mut pause_end: Option<f64> = None;
    let mut any_end: Option<f64> = None;
    let mut any_start: Option<f64> = None;
    for (i, u) in units.iter().enumerate() {
        if u.end_s <= cap_t {
            any_end = Some(any_end.map_or(u.end_s, |b: f64| b.max(u.end_s)));
            let gap_after =
                units.get(i + 1).map(|n| n.start_s - u.end_s).unwrap_or(f64::INFINITY);
            if u.end_s >= pause_floor && gap_after >= PAUSE_CUT_S {
                pause_end = Some(pause_end.map_or(u.end_s, |b: f64| b.max(u.end_s)));
            }
        }
        if u.start_s <= cap_t {
            any_start = Some(any_start.map_or(u.start_s, |b: f64| b.max(u.start_s)));
        }
    }
    pause_end.or(any_end).or(any_start).unwrap_or(cap_t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build word units from `(text, start, end)` triples (absolute seconds).
    fn units(words: &[(&str, f64, f64)]) -> Vec<CaptionUnit> {
        words
            .iter()
            .map(|&(t, s, e)| CaptionUnit { text: t.into(), start_s: s, end_s: e })
            .collect()
    }

    /// A run of one-second words `t0..t0+n`, the last carrying `punct`.
    fn spoken(t0: f64, n: usize, punct: &str) -> Vec<(String, f64, f64)> {
        (0..n)
            .map(|i| {
                let text =
                    if i + 1 == n { format!("w{i}{punct}") } else { format!("w{i}") };
                (text, t0 + i as f64, t0 + i as f64 + 1.0)
            })
            .collect()
    }

    fn to_units(runs: &[Vec<(String, f64, f64)>]) -> Vec<CaptionUnit> {
        runs.iter()
            .flatten()
            .map(|(t, s, e)| CaptionUnit { text: t.clone(), start_s: *s, end_s: *e })
            .collect()
    }

    #[test]
    fn splits_on_terminal_punctuation_and_pauses() {
        // "w0 w1 w2." then (0.2 s gap, same breath) "w0 w1?" then a 2 s pause
        // before an unpunctuated run — the pause closes it.
        let u = to_units(&[spoken(0.0, 3, "."), spoken(3.2, 2, "?"), spoken(7.2, 3, "")]);
        let s = split_sentences(&u, Language::En);
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].start_s, s[0].end_s, s[0].closed), (0.0, 3.0, true));
        assert_eq!((s[1].start_s, s[1].end_s, s[1].closed), (3.2, 5.2, true));
        // The trailing unpunctuated run ends the window: kept, but not closed.
        assert_eq!((s[2].start_s, s[2].end_s, s[2].closed), (7.2, 10.2, false));
    }

    #[test]
    fn splits_ja_terminals_inside_chunks_and_strips_closers() {
        let u = units(&[("だ。そ", 0.0, 1.0), ("れで", 1.0, 2.0)]);
        let s = split_sentences(&u, Language::Ja);
        assert_eq!(s.len(), 2, "mid-chunk 。 closes the first sentence");
        assert!(s[0].closed);

        let u = units(&[("said", 0.0, 1.0), ("\"go.\"", 1.0, 2.0)]);
        let s = split_sentences(&u, Language::En);
        assert_eq!(s.len(), 1);
        assert!(s[0].closed, "a terminal inside trailing quotes still closes");
    }

    #[test]
    fn snaps_both_edges_outward_to_sentence_bounds() {
        // Sentences: [0,10.] [10.3,20.3!] [20.6,30.6.] — seed cuts into the
        // first and last: both edges snap OUT.
        let u = to_units(&[spoken(0.0, 10, "."), spoken(10.3, 10, "!"), spoken(20.6, 10, ".")]);
        let seed = TimeRange { start_s: 4.0, end_s: 25.0 };
        let r = sentence_bounds(&u, Language::En, seed, 5.0, 180.0);
        assert_eq!((r.start_s, r.end_s), (0.0, 30.6));
    }

    #[test]
    fn trailing_dead_air_is_trimmed_to_the_last_complete_sentence() {
        // Speech ends at 20.3; the seed's tail (signal pad) reaches 40.
        let u = to_units(&[spoken(0.0, 10, "."), spoken(10.3, 10, ".")]);
        let seed = TimeRange { start_s: 0.0, end_s: 40.0 };
        let r = sentence_bounds(&u, Language::En, seed, 5.0, 180.0);
        assert_eq!(r.end_s, 20.3, "tail retreats to the last sentence end");
    }

    #[test]
    fn a_start_in_silence_keeps_the_lead_and_clips_no_words() {
        // Speech starts at 8; the seed leads from 3 (ADR 0020 pre-roll).
        let u = to_units(&[spoken(8.0, 10, ".")]);
        let seed = TimeRange { start_s: 3.0, end_s: 15.0 };
        let r = sentence_bounds(&u, Language::En, seed, 5.0, 180.0);
        assert_eq!(r.start_s, 3.0, "the event lead survives");
        assert_eq!(r.end_s, 18.0, "the clipped tail word snaps out");
    }

    #[test]
    fn grows_through_adjacent_sentences_to_the_floor() {
        // Four 10 s sentences back-to-back; the seed covers only the second.
        // Floor 35 pulls in neighbours (smaller gap first, ties forward).
        let u = to_units(&[
            spoken(0.0, 10, "."),
            spoken(10.4, 10, "."),
            spoken(20.8, 10, "."),
            spoken(31.2, 10, "."),
        ]);
        let seed = TimeRange { start_s: 10.4, end_s: 20.4 };
        let r = sentence_bounds(&u, Language::En, seed, 35.0, 180.0);
        assert!(r.duration_s() >= 35.0, "grew to the floor: {:?}", r);
        assert_eq!(r.start_s, 0.0);
        assert_eq!(r.end_s, 41.2);
    }

    #[test]
    fn ships_shorter_when_speech_runs_dry() {
        // One lone 12 s sentence in silence: no material to grow with — the
        // clip ships at 12 s (context over duration, no dead-air padding).
        let u = to_units(&[spoken(30.0, 12, ".")]);
        let seed = TimeRange { start_s: 28.0, end_s: 45.0 };
        let r = sentence_bounds(&u, Language::En, seed, 45.0, 180.0);
        assert_eq!((r.start_s, r.end_s), (28.0, 42.0));
    }

    #[test]
    fn growth_never_bridges_a_scene_break() {
        // The next sentence sits past a 10 s silence (> MAX_GROW_GAP_S): the
        // clip ships short instead of padding across dead air.
        let u = to_units(&[spoken(0.0, 10, "."), spoken(20.0, 10, ".")]);
        let seed = TimeRange { start_s: 0.0, end_s: 10.0 };
        let r = sentence_bounds(&u, Language::En, seed, 30.0, 180.0);
        assert_eq!((r.start_s, r.end_s), (0.0, 10.0));
    }

    #[test]
    fn cap_drops_the_sentence_that_would_cross_it() {
        // Growth toward a 45 s floor under a 25 s cap: the sentence that would
        // cross the cap is dropped — end at the last complete one under it.
        let u = to_units(&[spoken(0.0, 10, "."), spoken(10.4, 10, "."), spoken(20.8, 10, ".")]);
        let seed = TimeRange { start_s: 0.0, end_s: 20.4 };
        let r = sentence_bounds(&u, Language::En, seed, 45.0, 25.0);
        assert_eq!((r.start_s, r.end_s), (0.0, 20.4));
        assert!(r.duration_s() <= 25.0);
    }

    #[test]
    fn a_run_on_falls_back_to_a_pause_then_a_word_boundary() {
        // One unpunctuated 40 s run with a single 0.6 s breath at t=30: a 35 s
        // cap cuts at the breath (t=30), never mid-word.
        let mut words: Vec<(String, f64, f64)> = (0..30)
            .map(|i| (format!("w{i}"), i as f64, i as f64 + 1.0))
            .collect();
        words.extend((0..10).map(|i| {
            (format!("v{i}"), 30.6 + i as f64, 30.6 + i as f64 + 1.0)
        }));
        let u = to_units(&[words]);
        let seed = TimeRange { start_s: 0.0, end_s: 40.6 };
        let r = sentence_bounds(&u, Language::En, seed, 15.0, 35.0);
        assert_eq!(r.end_s, 30.0, "cut at the breath under the cap");

        // No pause at all: the cut lands on the last word boundary under the cap.
        let words: Vec<(String, f64, f64)> =
            (0..40).map(|i| (format!("w{i}"), i as f64, i as f64 + 1.0)).collect();
        let u = to_units(&[words]);
        let seed = TimeRange { start_s: 0.0, end_s: 40.0 };
        let r = sentence_bounds(&u, Language::En, seed, 15.0, 35.5);
        assert_eq!(r.end_s, 35.0, "word boundary, never mid-word");
    }

    #[test]
    fn speechless_seed_returns_unchanged() {
        let seed = TimeRange { start_s: 100.0, end_s: 145.0 };
        assert_eq!(sentence_bounds(&[], Language::En, seed, 45.0, 180.0), seed);
        // Speech entirely outside the seed: nothing to snap to either.
        let u = to_units(&[spoken(300.0, 5, ".")]);
        assert_eq!(sentence_bounds(&u, Language::En, seed, 45.0, 180.0), seed);
    }

    #[test]
    fn growth_skips_the_windows_unclosed_trailing_run() {
        // The trailing run has no punctuation and no pause after (window cut it
        // off): growth must not pull it in — its true end is unknown.
        let u = to_units(&[spoken(0.0, 10, "."), spoken(10.4, 8, "")]);
        let seed = TimeRange { start_s: 0.0, end_s: 10.0 };
        let r = sentence_bounds(&u, Language::En, seed, 30.0, 180.0);
        assert_eq!(r.end_s, 10.0, "unclosed run stays out");
    }
}
