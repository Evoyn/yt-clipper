//! Moment detection: the heuristic ensemble (chat-replay rate spikes, audio
//! loudness, transcript excitement lexicon) plus the local-LLM rerank stage
//! (ADR 0002). Every signal is stored per-Moment unblended (`yc_core::Signals`)
//! so ranking can be retuned without re-analysis.
//!
//! M3 (ADR 0007) runs two phases. **Discover** (here) scans the whole VOD with
//! only the cheap, no-whisper signals - chat-rate + loudness - and emits ranked
//! candidate Moments. **Refine** (later in M3) transcribes each candidate's
//! range and applies the excitement lexicon. **Rerank** (M4) is the LLM pass.
//!
//! GPU discipline: the refine/LLM stages must never run while transcription
//! holds VRAM - staging is strictly sequential on the single 8 GB GPU.

pub mod arousal;
pub mod chat;
pub mod lexicon;
pub mod llm;
pub mod loudness;
pub mod score;
pub mod sentence;
/// Vocal-stem separation (htdemucs vocals ONNX). Behind `sep` so the default
/// detection build needs neither the `ort` binary nor the htdemucs model.
#[cfg(feature = "sep")]
pub mod sep;

use anyhow::Result;
use std::path::Path;
use yc_core::{Moment, Signals, TimeRange};

/// Tunable detection parameters. Defaults are scaffolding to be retuned against
/// real VODs - none of them are load-bearing decisions (ADR 0007).
#[derive(Debug, Clone, Copy)]
pub struct DetectParams {
    /// Width of the analysis grid bins, seconds.
    pub bin_s: f64,
    /// Moving-average window for finding *sustained* spikes, seconds.
    pub smooth_s: f64,
    /// How far a Moment starts before its peak (chat lags the event), seconds -
    /// the base/floor lead, used for a chat-driven peak.
    pub lead_s: f64,
    /// Max pre-roll lead for a strongly loud-driven peak (a jumpscare / loud
    /// mediashare donation needs its build-up captured), seconds. The per-peak
    /// lead scales from `lead_s` up to this as the peak's loudness z rises
    /// (ADR 0020).
    pub loud_lead_s: f64,
    /// Loudness z-score at which the pre-roll reaches `loud_lead_s` (ADR 0020); at
    /// or below `min_z` (the peak threshold) it stays at the base `lead_s`. Linear
    /// between.
    pub loud_lead_full_z: f32,
    /// Shortest Moment detection aims for, seconds (ADR 0040). The adaptive
    /// window ([`score::adaptive_range`]) floors its *seed* here, and refine's
    /// sentence snap ([`sentence::sentence_bounds`]) grows a trimmed clip back
    /// toward it through adjacent real sentences — context fill, never dead-air
    /// padding, so a clip ships shorter when speech runs dry. Manual Moments
    /// are exempt (they never pass through detection).
    pub min_dur_s: f64,
    /// Longest Moment the ranker emits, seconds — the YouTube-Shorts ceiling is
    /// 180 s; the operator can tune below that. A sustained hype moment grows
    /// toward this instead of being cut at a fixed ~30 s.
    pub max_dur_s: f64,
    /// Minimum combined (z-scored) score for a bin to be a peak.
    pub min_z: f32,
    /// Cap on how many Moments discovery returns.
    pub top_n: usize,
    /// Ranking weights over the unblended signals.
    pub weights: Weights,
}

/// Weights applied to the unblended signals to form a Moment's rank. The score
/// renormalizes over whichever signals are present (ADR 0002: retunable).
#[derive(Debug, Clone, Copy)]
pub struct Weights {
    pub chat: f32,
    pub loudness: f32,
    pub lexicon: f32,
    pub arousal: f32,
    pub llm: f32,
}

impl Default for DetectParams {
    fn default() -> Self {
        // Tuned against a real 66-min chat+audio VOD (smooth_s/min_z swept):
        // 10 s smoothing + min_z 1.0 surfaced ~18 plausible candidates; 5 s was
        // noisy (37+), 15 s too sparse. Retune per ADR 0002 as more VODs land.
        Self {
            bin_s: 1.0,
            smooth_s: 10.0,
            lead_s: 5.0,
            // A loud-driven peak gets up to a ~10 s pre-roll (the operator wants
            // the build-up before a jumpscare / loud donation); reached by
            // loudness z 3.0, the base 5 s at/below the peak threshold (ADR 0020).
            loud_lead_s: 10.0,
            loud_lead_full_z: 3.0,
            // Natural clip length (the "~30 s everywhere" fix): the window
            // follows the combined signal's elevated span, floored/capped here.
            // The floor rose 15 -> 45 with ADR 0040 (sentence-boundary bounds):
            // a Short under ~45 s rarely carries a full thought, and refine
            // reaches the floor through *real adjacent sentences* (shipping
            // shorter when speech runs dry) rather than dead air. 90 s default
            // cap, operator-tunable up to the 180 s Shorts ceiling.
            min_dur_s: 45.0,
            max_dur_s: 90.0,
            min_z: 1.0,
            top_n: 25,
            // Rebalanced for M5 (ADR 0010): the LLM judgment Signal gets a
            // strong-but-not-dominant voice (0.25); the lexicon drops to 0.10
            // since it overlaps the LLM (both read the transcript). Arousal 0.15
            // is gate-validated (ADR 0008). All retunable per ADR 0002.
            weights: Weights { chat: 0.30, loudness: 0.20, lexicon: 0.10, arousal: 0.15, llm: 0.25 },
        }
    }
}

/// A Moment's combined rank: weighted mean of its present signals, renormalized
/// so a missing signal (e.g. no chat, or pre-refine lexicon) rescales the rest
/// rather than dragging the score toward zero. The single place ranking is
/// computed, so discovery, refine, and any UI retune stay consistent.
pub fn combined_score(s: &Signals, w: &Weights) -> f32 {
    let mut num = 0.0;
    let mut den = 0.0;
    for (val, weight) in [
        (s.chat_rate, w.chat),
        (s.loudness, w.loudness),
        (s.lexicon, w.lexicon),
        (s.arousal, w.arousal),
        (s.llm, w.llm),
    ] {
        if let Some(v) = val {
            num += weight * v;
            den += weight;
        }
    }
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

/// Discover candidate Moments from the cheap signals (ADR 0007): chat-rate (if
/// chat was captured) + loudness, both over the whole VOD with no whisper.
/// Returns Moments ranked by descending score, ids assigned 1.. in that order.
/// `lexicon` is left `None` for the refine pass to fill.
pub fn discover(
    chat_json: Option<&Path>,
    analysis_wav: &Path,
    params: &DetectParams,
) -> Result<Vec<Moment>> {
    // Loudness defines the analysis grid: one bin per `bin_s` of audio.
    let loud_raw = loudness::read_rms_bins(analysis_wav, params.bin_s)?;
    let chat_counts = match chat_json {
        Some(path) if path.exists() => {
            let offsets = chat::message_offsets(path)?;
            Some(score::bin_counts(&offsets, params.bin_s, loud_raw.len()))
        }
        _ => {
            tracing::info!("no chat replay; discovering on loudness alone");
            None
        }
    };
    Ok(rank_moments(&loud_raw, chat_counts.as_deref(), params))
}

/// Score pre-binned signals into ranked Moments (the I/O-free core of
/// [`discover`]). Split out so tuning can sweep parameters over in-memory bins
/// without re-reading the multi-hundred-MB wav each pass. `loud_raw` is per-bin
/// RMS; `chat_counts`, if present, is per-bin viewer-message count on the same
/// grid (same length as `loud_raw`).
pub fn rank_moments(
    loud_raw: &[f32],
    chat_counts: Option<&[f32]>,
    params: &DetectParams,
) -> Vec<Moment> {
    let n_bins = loud_raw.len();
    if n_bins == 0 {
        return Vec::new();
    }
    let grid_dur_s = n_bins as f64 * params.bin_s;
    let win = (params.smooth_s / params.bin_s).round() as usize;

    // Normalize each signal over the VOD (a spike is relative; constant game
    // loudness sits near z=0). Loudness uses a peak-preserving max-pool so a
    // brief scream/hype spike surfaces and the window lands on it - a clip is
    // judged by the peak it contains, not its sustained level. Chat uses a mean,
    // because sustained reaction volume is the signal there.
    let loud = score::smooth_max(&score::robust_z(loud_raw), win);
    let chat = chat_counts.map(|c| score::smooth(&score::robust_z(c), win));

    // Combined series for peak-finding, using renormalized weights over the
    // signals actually present (matches `combined_score` per-Moment).
    let w = &params.weights;
    let mut weighted: Vec<(&[f32], f32)> = vec![(loud.as_slice(), w.loudness)];
    if let Some(c) = &chat {
        weighted.push((c.as_slice(), w.chat));
    }
    let wsum: f32 = weighted.iter().map(|(_, x)| x).sum();
    if wsum > 0.0 {
        for (_, x) in weighted.iter_mut() {
            *x /= wsum;
        }
    }
    let combined = score::combine(&weighted, n_bins);

    // Peaks -> naturally-sized candidates -> ranked Moments. A light peak-space
    // NMS (one peak per min_dur span) dedupes shoulder maxima; each survivor
    // then grows an *adaptive* window over the signal's elevated span (the
    // "every clip is ~30 s" fix), and overlap suppression trims/drops weaker
    // windows against stronger ones since lengths now vary. Both deliberately
    // inherit the 45 s min_dur (ADR 0040 retune): at the new duration scale two
    // peaks within one minimum-clip span cover the same content, so spacing
    // them at 45 s and dropping sub-45 s trim survivors is the same semantics
    // the 15 s world had — one Moment per clip-length of VOD.
    let peaks = score::find_peaks(&combined, params.min_z);
    let min_gap = (params.min_dur_s / params.bin_s).round().max(1.0) as usize;
    let kept = score::nms(peaks, &combined, min_gap);

    let candidates: Vec<(usize, TimeRange)> = kept
        .into_iter()
        .map(|bin| {
            // Signal-aware pre-roll (ADR 0020): a loud-driven peak (high loudness z)
            // leads by up to loud_lead_s, a chat-driven one keeps the base lead_s.
            // The lead stays the adaptive window's *minimum* pre-roll.
            let lead = score::peak_lead_s(
                loud[bin],
                params.min_z,
                params.loud_lead_full_z,
                params.lead_s,
                params.loud_lead_s,
            );
            let range = score::adaptive_range(
                &combined,
                bin,
                params.bin_s,
                lead,
                params.min_dur_s,
                params.max_dur_s,
                grid_dur_s,
            );
            (bin, range)
        })
        .collect();

    score::suppress_overlaps(candidates, params.bin_s, params.min_dur_s)
        .into_iter()
        .take(params.top_n)
        .enumerate()
        .map(|(i, (bin, range))| {
            let signals = Signals {
                chat_rate: chat.as_ref().map(|c| c[bin]),
                loudness: Some(loud[bin]),
                lexicon: None,
                arousal: None,
                llm: None,
            };
            Moment {
                id: (i + 1) as u64,
                range,
                score: combined_score(&signals, w),
                signals,
                title: None, // generated by the LLM judge in refine (ADR 0015)
            }
        })
        .collect()
}

/// Whether a Moment's range contains an absolute VOD time (helper for review).
pub fn range_contains(range: &TimeRange, t_s: f64) -> bool {
    t_s >= range.start_s && t_s < range.end_s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, sr: u32, samples: &[f32]) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sr,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            w.write_sample((s * 32767.0) as i16).unwrap();
        }
        w.finalize().unwrap();
    }

    fn chat_line(off_ms: u64) -> String {
        format!(
            r#"{{"replayChatItemAction":{{"actions":[{{"addChatItemAction":{{"item":{{"liveChatTextMessageRenderer":{{"message":{{"runs":[{{"text":"hype"}}]}}}}}}}}}}],"videoOffsetTimeMsec":"{off_ms}"}}}}"#
        )
    }

    #[test]
    fn discover_surfaces_a_moment_around_a_loud_chatty_burst() {
        let dir = std::env::temp_dir();
        let wav = dir.join("yc_detect_discover.wav");
        let chat = dir.join("yc_detect_discover.json");

        // 60 s of mono 16 kHz silence with a loud 6 s burst at t=30..36.
        let sr = 16_000u32;
        let mut samples = vec![0.0f32; 60 * sr as usize];
        for s in samples.iter_mut().skip(30 * sr as usize).take(6 * sr as usize) {
            *s = 0.8;
        }
        write_wav(&wav, sr, &samples);

        // A chat burst that lags the audio slightly (t=31..37).
        let mut lines = String::new();
        for k in 0..40u64 {
            lines.push_str(&chat_line(31_000 + k * 150));
            lines.push('\n');
        }
        std::fs::write(&chat, &lines).unwrap();

        // Modest smoothing so the 6 s burst isn't diluted by the 20 s default.
        let params = DetectParams { smooth_s: 3.0, min_z: 1.0, ..Default::default() };
        let moments = discover(Some(&chat), &wav, &params).unwrap();

        assert!(!moments.is_empty(), "expected at least one Moment");
        let top = &moments[0];
        // The top Moment's window should straddle the burst (~t=33).
        assert!(range_contains(&top.range, 33.0), "range = {:?}", top.range);
        // Both cheap signals populated; lexicon waits for refine.
        assert!(top.signals.chat_rate.is_some());
        assert!(top.signals.loudness.is_some());
        assert!(top.signals.lexicon.is_none());
        assert!(top.score > 0.0);
        // Ids are assigned in rank order.
        assert_eq!(top.id, 1);

        let _ = std::fs::remove_file(&wav);
        let _ = std::fs::remove_file(&chat);
    }

    #[test]
    fn loud_driven_peak_gets_a_longer_pre_roll_than_a_chat_driven_one() {
        // ADR 0020: two non-overlapping peaks on a 300 s grid - a sharp LOUD spike
        // at bin 60 (no chat) and a CHAT spike at bin 200 (baseline loudness). No
        // smoothing (smooth_s = bin_s) so the peaks land on the spike bins. The
        // grid leaves each peak room for the 45 s floor (ADR 0040) so the
        // VOD-tail pull-back never distorts the leads this test pins.
        let n = 300;
        let mut loud = vec![0.1f32; n];
        loud[60] = 10.0; // a scream / jumpscare -> high loudness z
        let mut chat = vec![0.0f32; n];
        chat[200] = 20.0; // a chat burst with no loud reaction
        let params = DetectParams { smooth_s: 1.0, min_z: 0.5, ..Default::default() };
        let moments = rank_moments(&loud, Some(&chat), &params);

        let loud_m = moments.iter().find(|m| range_contains(&m.range, 60.0)).expect("loud Moment");
        let chat_m =
            moments.iter().find(|m| range_contains(&m.range, 200.0)).expect("chat Moment");
        // The loud peak (bin 60, peak_t 60.5) leads by ~loud_lead_s (10 s); the
        // chat peak (bin 200, peak_t 200.5) keeps the base lead (~5 s).
        let loud_lead = 60.5 - loud_m.range.start_s;
        let chat_lead = 200.5 - chat_m.range.start_s;
        assert!((loud_lead - 10.0).abs() < 0.75, "loud lead {loud_lead}");
        assert!((chat_lead - 5.0).abs() < 0.75, "chat lead {chat_lead}");
        assert!(loud_lead > chat_lead + 2.0, "loud must lead more: {loud_lead} vs {chat_lead}");
        // The widened min_gap keeps the two candidate windows from overlapping.
        assert!(loud_m.range.end_s <= chat_m.range.start_s, "candidate windows overlap");
    }

    #[test]
    fn combined_score_renormalizes_over_present_signals() {
        let w = Weights { chat: 0.5, loudness: 0.3, lexicon: 0.2, arousal: 0.0, llm: 0.0 };
        // Only loudness present -> score is just the loudness value.
        let only_loud =
            Signals { chat_rate: None, loudness: Some(2.0), lexicon: None, arousal: None, llm: None };
        assert!((combined_score(&only_loud, &w) - 2.0).abs() < 1e-6);
        // Chat + loud present -> renormalized over 0.5/0.3.
        let both =
            Signals { chat_rate: Some(4.0), loudness: Some(2.0), lexicon: None, arousal: None, llm: None };
        let expect = (0.5 * 4.0 + 0.3 * 2.0) / 0.8;
        assert!((combined_score(&both, &w) - expect).abs() < 1e-6);
    }
}
