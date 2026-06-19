//! Excitement-lexicon signal (ADR 0002, ADR 0007).
//!
//! Per-language starter word lists, matched against a candidate's transcript to
//! gauge how excited the streamer's *own* speech was. The lists are compiled-in
//! data - easy to edit, no runtime file resolution - and deliberately small;
//! expand them against real VODs. Per ADR 0007 the lexicon runs only on
//! candidates (Design B), so there is no VOD-wide baseline to normalize against:
//! [`apply`] z-scores the per-candidate densities across the candidate set so
//! the signal lands on the same scale as the chat/loudness z-scores.
//!
//! Matching is case-insensitive substring over the joined transcript, so
//! variants ("ggwp" hits "gg", "wkwkwk" hits "wkwk") still register. The
//! mixed-audio limitation (ADR 0007) applies: when the game's speech dominates
//! the transcript, this reads the wrong speaker - a known, bounded weakness.

use crate::{combined_score, score, Weights};
use yc_core::{Language, Moment, Transcript};

/// English excitement markers (lowercased).
const EN: &[&str] = &[
    "oh my", "no way", "let's go", "lets go", "gg", "insane", "clutch", "holy",
    "wow", "omg", "pog", "crazy", "huge", "what the", "no shot", "actually",
];
/// Bahasa Indonesia excitement markers (lowercased).
const ID: &[&str] = &[
    "anjay", "anjir", "anjg", "buset", "gila", "parah", "mantap", "mantul",
    "wkwk", "njir", "astaga", "ya ampun", "kocak", "ngakak", "gg", "sadis", "edan",
];
/// Japanese excitement markers.
const JA: &[&str] = &[
    "やばい", "やば", "すごい", "すご", "うまい", "やった", "まじ", "マジ",
    "草", "うわ", "えぐ", "神", "www", "わら",
];

fn list(language: Language) -> &'static [&'static str] {
    match language {
        Language::En => EN,
        Language::Id => ID,
        Language::Ja => JA,
    }
}

/// Excitement density of a transcript: total lexicon-term occurrences divided by
/// the number of caption units, so it is duration-independent. Empty transcript
/// scores 0.
pub fn density(transcript: &Transcript, language: Language) -> f32 {
    let units = transcript.units.len();
    if units == 0 {
        return 0.0;
    }
    let text = transcript
        .units
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let hits: usize = list(language).iter().map(|t| count(&text, t)).sum();
    hits as f32 / units as f32
}

/// Non-overlapping occurrences of `needle` in `haystack`.
fn count(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut n = 0;
    let mut start = 0;
    while let Some(i) = haystack[start..].find(needle) {
        n += 1;
        start += i + needle.len();
    }
    n
}

/// Fill the lexicon signal on candidates from their per-candidate densities and
/// rerank. Densities are z-scored across the candidate set (lexicon has no
/// VOD-wide baseline), then `combined_score` recomputes each Moment's rank with
/// the lexicon now present. `densities[i]` must correspond to `moments[i]`.
pub fn apply(moments: &mut [Moment], densities: &[f32], weights: &Weights) {
    debug_assert_eq!(moments.len(), densities.len(), "one density per Moment");
    let z = score::robust_z(densities);
    for (m, lz) in moments.iter_mut().zip(z) {
        m.signals.lexicon = Some(lz);
        m.score = combined_score(&m.signals, weights);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yc_core::{CaptionUnit, Signals, TimeRange};

    fn transcript(language: Language, words: &[&str]) -> Transcript {
        let units = words
            .iter()
            .enumerate()
            .map(|(i, w)| CaptionUnit { text: w.to_string(), start_s: i as f64, end_s: i as f64 + 0.5 })
            .collect();
        Transcript { language, units }
    }

    #[test]
    fn density_is_hits_over_units_case_insensitive() {
        // "anjay" + "wkwk" (from WKWKWK) = 2 hits over 4 units = 0.5.
        let t = transcript(Language::Id, &["Anjay", "keren", "WKWKWK", "main"]);
        assert!((density(&t, Language::Id) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn density_of_calm_speech_is_zero() {
        let t = transcript(Language::En, &["i", "will", "place", "this", "block"]);
        assert_eq!(density(&t, Language::En), 0.0);
    }

    #[test]
    fn empty_transcript_scores_zero() {
        assert_eq!(density(&Transcript { language: Language::En, units: vec![] }, Language::En), 0.0);
    }

    #[test]
    fn apply_sets_lexicon_and_reranks_by_density() {
        let w = Weights { chat: 0.5, loudness: 0.3, lexicon: 0.2, arousal: 0.0, llm: 0.0 };
        let mk = |id, lex_neutral_signals: Signals| Moment {
            id,
            range: TimeRange { start_s: 0.0, end_s: 30.0 },
            signals: lex_neutral_signals,
            score: 0.0,
        };
        let base =
            Signals { chat_rate: Some(1.0), loudness: Some(1.0), lexicon: None, arousal: None, llm: None };
        let mut moments = vec![mk(1, base), mk(2, base)];
        // Moment 2 is lexicon-hot, Moment 1 is not.
        apply(&mut moments, &[0.0, 2.0], &w);
        assert!(moments[0].signals.lexicon.unwrap() < moments[1].signals.lexicon.unwrap());
        assert!(moments[1].score > moments[0].score);
    }
}
