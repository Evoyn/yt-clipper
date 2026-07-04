//! Segment-anchor measurement (the caption/audio desync root fix, 2026-07-04).
//!
//! The promote path assumed a fetched Segment's timeline anchors at the
//! requested section start (`in_segment_offset`, ADR 0006). Measured on a real
//! podcast VOD, yt-dlp's HLS section download can instead snap to a stream
//! fragment boundary **before** the request (observed: 5.84 s early, and the
//! section then also *ends* that much short of the request). Every consumer of
//! the in-segment seek — the export cut, face detect, the speaker analysis
//! frames, preview playback — then reads media shifted by the snap, while
//! captions are cut from `analysis.wav` at the true VOD range: a constant
//! caption-vs-audio offset in the export, and mouth-vs-voice misattribution in
//! the speaker pass. Other VODs anchor exactly as requested (measured
//! sample-exact on a second creator), which is why the bug looked per-video.
//!
//! The cure is measurement, not a better assumption: cross-correlate the
//! Segment's audio envelope against the whole-VOD `analysis.wav` around the
//! requested start and read off where the Segment actually sits. Two windows
//! must independently agree, so a false lock on repetitive audio is rejected
//! and callers can trust a `Some` to a few milliseconds.

use anyhow::{Context, Result};
use std::path::Path;
use yc_core::TimeRange;

use crate::youtube::CancelToken;

/// Envelope hop in samples at 16 kHz: 4 ms bins — fine enough that the
/// parabolic peak refine lands within ~2 ms, coarse enough that the search
/// stays trivial next to a whisper decode.
const HOP: usize = 64;
/// Seconds of Segment audio decoded for the measurement. Long enough to span
/// two well-separated correlation windows; short next to any real Segment.
const MEASURE_TAKE_S: f64 = 36.0;
/// Truth margin read around the requested start on each side. Fragment snaps
/// are seconds; a lock needing more than this is treated as no lock.
const SEARCH_PAD_S: f64 = 30.0;
/// Largest believable |anchor - requested start|. Beyond this the "lock" is
/// more likely envelope self-similarity than a real fragment snap.
const MAX_SNAP_S: f64 = 25.0;
/// Both correlation windows must clear this normalized-correlation floor.
const MIN_WINDOW_CORR: f32 = 0.55;
/// ...and agree on the offset to within this (seconds), or there is no lock.
const WINDOW_AGREE_S: f64 = 0.08;
/// Media the clip range must be covered by, beyond `range.end`, before the
/// export/caption/speaker window is considered safe (`-t` rounding + fades).
const END_COVER_MARGIN_S: f64 = 0.5;

/// A confident measurement of where a Segment sits on the VOD timeline.
#[derive(Debug, Clone, Copy)]
pub struct SegmentAnchor {
    /// VOD seconds of the Segment's **container t=0** — the timeline `-ss`
    /// addresses. Seek to VOD time `v` with `v - vod_t0_s`.
    pub vod_t0_s: f64,
    /// The weaker of the two window correlations (0..1), for logging.
    pub corr: f32,
}

/// How much media is missing past the clip's end: `> 0` means the Segment ends
/// before `range_end_s` (+ margin) and a wider refetch is needed to cover the
/// clip. A non-positive `media_dur_s` (ffprobe "N/A") reports 0 — coverage
/// unknown is not coverage missing.
pub fn tail_shortfall_s(anchor_vod_t0_s: f64, media_dur_s: f64, range_end_s: f64) -> f64 {
    if media_dur_s <= 0.0 {
        return 0.0;
    }
    ((range_end_s + END_COVER_MARGIN_S) - (anchor_vod_t0_s + media_dur_s)).max(0.0)
}

/// Measure where `segment`'s timeline actually sits on the VOD: decode its
/// first [`MEASURE_TAKE_S`] of audio, cross-correlate against `analysis_wav`
/// around `padded_start_s`, and return the VOD time of container t=0.
///
/// `Ok(None)` = no confident lock (silent/repetitive audio, or a snap beyond
/// [`MAX_SNAP_S`]) — the caller falls back to the requested-start assumption.
/// `Err` = IO/decode failure (including cancellation), for the caller to
/// surface or swallow as it sees fit.
pub fn measure_segment_anchor(
    ffmpeg: &Path,
    ffprobe: &Path,
    segment: &Path,
    analysis_wav: &Path,
    padded_start_s: f64,
    cancel: &CancelToken,
) -> Result<Option<SegmentAnchor>> {
    // Segment audio, 16 kHz mono f32, bounded to the measurement take.
    let args: Vec<String> = vec![
        "-v".into(),
        "error".into(),
        "-i".into(),
        segment.display().to_string(),
        "-map".into(),
        "0:a:0".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        crate::WHISPER_SR.to_string(),
        "-t".into(),
        format!("{MEASURE_TAKE_S:.3}"),
        "-f".into(),
        "f32le".into(),
        "-".into(),
    ];
    let bytes = crate::youtube::run_capture_bytes(ffmpeg, &args, None, cancel)
        .context("decoding segment audio for anchor measurement")?;
    let seg: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    if (seg.len() as f64) < 3.0 * crate::WHISPER_SR as f64 {
        tracing::warn!("segment audio too short to measure an anchor ({} samples)", seg.len());
        return Ok(None);
    }

    // The truth window from the whole-VOD analysis wav, wide enough to see a
    // snap of up to SEARCH_PAD_S on either side of the requested start.
    let truth_lo = (padded_start_s - SEARCH_PAD_S).max(0.0);
    let truth_hi = padded_start_s + MEASURE_TAKE_S + SEARCH_PAD_S;
    let truth = crate::read_range_samples(
        analysis_wav,
        TimeRange { start_s: truth_lo, end_s: truth_hi },
    )
    .context("reading analysis wav for anchor measurement")?;

    let Some((offset_s, corr)) = measure_offset(&seg, &truth, crate::WHISPER_SR) else {
        return Ok(None);
    };
    // VOD time of the segment's first decoded audio sample.
    let anchor_decode0 = truth_lo + offset_s;
    if (anchor_decode0 - padded_start_s).abs() > MAX_SNAP_S {
        tracing::warn!(
            anchor = anchor_decode0,
            requested = padded_start_s,
            "anchor lock {:.2}s from the requested start exceeds the believable snap; ignoring",
            anchor_decode0 - padded_start_s
        );
        return Ok(None);
    }

    // The first decoded audio sample sits at the audio stream's start_time on
    // the container timeline (-ss addresses container time), so shift by it.
    let audio_start = probe_audio_start_time(ffprobe, segment, cancel)?;
    Ok(Some(SegmentAnchor { vod_t0_s: anchor_decode0 - audio_start, corr }))
}

/// The audio stream's container `start_time` (seconds); 0.0 when absent/N-A.
fn probe_audio_start_time(ffprobe: &Path, media: &Path, cancel: &CancelToken) -> Result<f64> {
    let args: Vec<String> = vec![
        "-v".into(),
        "error".into(),
        "-select_streams".into(),
        "a:0".into(),
        "-show_entries".into(),
        "stream=start_time".into(),
        "-of".into(),
        "default=noprint_wrappers=1:nokey=1".into(),
        media.display().to_string(),
    ];
    let out = crate::youtube::run_capture(ffprobe, &args, None, cancel)
        .context("probing segment audio start_time")?;
    Ok(out.trim().parse::<f64>().unwrap_or(0.0))
}

/// Where does `seg` start inside `truth` (seconds, `>= 0`)? Two well-separated
/// windows of `seg` are matched independently; they must both correlate and
/// agree, so repetitive audio cannot fake a lock. Returns the mean offset and
/// the weaker window correlation.
fn measure_offset(seg: &[f32], truth: &[f32], sr: u32) -> Option<(f64, f32)> {
    let bins_per_s = sr as f64 / HOP as f64;
    let es = envelope(seg);
    let et = envelope(truth);
    let dur = seg.len() as f64 / sr as f64;

    // Two windows when the take allows, else one long-enough window.
    let windows: Vec<(f64, f64)> = if dur >= 26.0 {
        vec![(2.0, 12.0), (dur - 12.0, dur - 2.0)]
    } else if dur >= 8.0 {
        vec![(1.0, (dur - 1.0).min(11.0)), ((dur - 8.0).max(1.0), dur - 1.0)]
    } else {
        vec![(0.2, dur - 0.2)]
    };

    let mut offsets = Vec::new();
    let mut min_corr = 1.0f32;
    for (w0, w1) in windows {
        let a0 = (w0 * bins_per_s) as usize;
        let a1 = ((w1 * bins_per_s) as usize).min(es.len());
        if a1 <= a0 + (bins_per_s as usize) {
            return None; // degenerate window: nothing to match
        }
        let (lag, corr) = best_lag(&es[a0..a1], &et)?;
        if corr < MIN_WINDOW_CORR {
            return None;
        }
        min_corr = min_corr.min(corr);
        offsets.push(lag / bins_per_s - w0);
    }
    let (first, last) = (offsets[0], *offsets.last().expect("nonempty"));
    if (first - last).abs() > WINDOW_AGREE_S {
        tracing::warn!(first, last, "anchor windows disagree; no lock");
        return None;
    }
    Some((offsets.iter().sum::<f64>() / offsets.len() as f64, min_corr))
}

/// Log-compressed RMS envelope at [`HOP`]-sample bins: robust to the AAC
/// re-encode between the Segment and the analysis wav, and to level shifts.
fn envelope(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks(HOP)
        .map(|c| {
            let e = c.iter().map(|s| s * s).sum::<f32>() / c.len().max(1) as f32;
            (1.0 + 1e4 * e).ln()
        })
        .collect()
}

/// Best normalized-cross-correlation alignment of `a` inside `b`, over every
/// feasible integer envelope lag, with a parabolic sub-bin refine. Returns
/// `(lag_bins, corr)`; `None` when `a` cannot fit inside `b`.
fn best_lag(a: &[f32], b: &[f32]) -> Option<(f64, f32)> {
    if a.is_empty() || b.len() < a.len() {
        return None;
    }
    let am = a.iter().sum::<f32>() / a.len() as f32;
    let av: Vec<f32> = a.iter().map(|x| x - am).collect();
    let an = av.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);

    // Rolling sums over b windows of |a| length keep the scan O(len(b)).
    let n = a.len();
    let mut scores: Vec<f32> = Vec::with_capacity(b.len() - n + 1);
    let mut sum: f64 = b[..n].iter().map(|x| *x as f64).sum();
    let mut sumsq: f64 = b[..n].iter().map(|x| (*x as f64) * (*x as f64)).sum();
    for lag in 0..=(b.len() - n) {
        if lag > 0 {
            let out = b[lag - 1] as f64;
            let inn = b[lag + n - 1] as f64;
            sum += inn - out;
            sumsq += inn * inn - out * out;
        }
        let bm = (sum / n as f64) as f32;
        let bvar = (sumsq - sum * sum / n as f64).max(0.0) as f32;
        let mut dot = 0.0f32;
        for (x, y) in av.iter().zip(&b[lag..lag + n]) {
            dot += x * (y - bm);
        }
        scores.push(dot / (an * bvar.sqrt().max(1e-9)));
    }
    let (best_i, best_c) =
        scores.iter().enumerate().max_by(|x, y| x.1.total_cmp(y.1)).map(|(i, c)| (i, *c))?;
    // Parabolic refine on the discrete peak.
    let cm = if best_i > 0 { scores[best_i - 1] } else { best_c };
    let cp = if best_i + 1 < scores.len() { scores[best_i + 1] } else { best_c };
    let denom = cm - 2.0 * best_c + cp;
    let frac = if denom.abs() > 1e-9 { 0.5 * (cm - cp) / denom } else { 0.0 };
    Some((best_i as f64 + frac as f64, best_c))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random speech-like test signal: noise bursts of
    /// varying length/level separated by near-silence, so envelopes carry
    /// alignment structure the way real speech does.
    fn synth(seed: u64, dur_s: f64, sr: u32) -> Vec<f32> {
        let n = (dur_s * sr as f64) as usize;
        let mut x = seed | 1;
        let mut rand = move || {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((x >> 33) as u32 as f64 / u32::MAX as f64) as f32
        };
        let mut out = vec![0.0f32; n];
        let mut i = 0usize;
        while i < n {
            let burst = (rand() * 0.6 * sr as f32) as usize + sr as usize / 10;
            let gap = (rand() * 0.4 * sr as f32) as usize;
            let level = 0.05 + rand() * 0.6;
            for j in i..(i + burst).min(n) {
                out[j] = (rand() * 2.0 - 1.0) * level;
            }
            i += burst + gap;
        }
        out
    }

    #[test]
    fn recovers_a_known_offset_to_milliseconds() {
        let sr = 16_000u32;
        let truth = synth(7, 90.0, sr);
        // The "segment": truth from 12.34s, 36s long — the snapped-fetch shape.
        let a = (12.34 * sr as f64) as usize;
        let seg = truth[a..a + (36.0 * sr as f64) as usize].to_vec();
        let (offset, corr) = measure_offset(&seg, &truth, sr).expect("lock");
        assert!((offset - 12.34).abs() < 0.01, "offset {offset} != 12.34");
        assert!(corr > 0.9, "corr {corr}");
    }

    #[test]
    fn unrelated_audio_yields_no_lock() {
        let sr = 16_000u32;
        let truth = synth(7, 60.0, sr);
        let other = synth(999, 36.0, sr);
        assert!(measure_offset(&other, &truth, sr).is_none(), "no false lock");
    }

    #[test]
    fn short_but_real_segments_still_lock() {
        let sr = 16_000u32;
        let truth = synth(21, 40.0, sr);
        let a = (5.5 * sr as f64) as usize;
        let seg = truth[a..a + (9.0 * sr as f64) as usize].to_vec();
        let (offset, _) = measure_offset(&seg, &truth, sr).expect("lock");
        assert!((offset - 5.5).abs() < 0.02, "offset {offset} != 5.5");
    }

    #[test]
    fn tail_shortfall_flags_only_a_truncated_clip_window() {
        // Covered: anchor 782, 85s of media, clip ends 859 (+margin) < 867.
        assert_eq!(tail_shortfall_s(782.0, 85.0, 859.0), 0.0);
        // The measured bug: anchor 782.16, 73.03s media -> ends 855.19, clip
        // needs 859.5 -> ~4.3s short.
        let short = tail_shortfall_s(782.16, 73.03, 859.0);
        assert!((short - 4.31).abs() < 0.02, "shortfall {short}");
        // Unknown duration (probe N/A) is not reported as missing coverage.
        assert_eq!(tail_shortfall_s(782.0, 0.0, 859.0), 0.0);
    }
}
