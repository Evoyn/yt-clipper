//! Turning per-bin signals into ranked candidate Moments (ADR 0007).
//!
//! Pipeline: bin each signal on a common grid -> robust z-score over the VOD
//! (so a "spike" is relative, not absolute) -> smooth over ~a clip length ->
//! weighted combine -> peak-detect -> non-maximum suppression -> a fixed
//! lead-window range per surviving peak. Every step here is pure and
//! unit-tested; `lib::discover` reads the files and threads them through.

use yc_core::TimeRange;

/// Count message offsets (seconds) into `n_bins` bins of width `bin_s`. Offsets
/// past the grid are dropped (the grid spans the analysis audio).
pub fn bin_counts(offsets: &[f64], bin_s: f64, n_bins: usize) -> Vec<f32> {
    let mut counts = vec![0.0_f32; n_bins];
    for &t in offsets {
        if t < 0.0 {
            continue;
        }
        let i = (t / bin_s) as usize;
        if i < n_bins {
            counts[i] += 1.0;
        }
    }
    counts
}

fn median(sorted: &[f32]) -> f32 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        0.5 * (sorted[n / 2 - 1] + sorted[n / 2])
    }
}

/// Robust z-score: `(x - median) / (1.4826 * MAD)`. When the MAD is degenerate
/// (e.g. a count series that is mostly zeros, so both median and MAD are 0), we
/// fall back to mean/standard-deviation, which still lifts the spikes; a truly
/// flat series returns all zeros (no spikes to find).
pub fn robust_z(series: &[f32]) -> Vec<f32> {
    let n = series.len();
    if n == 0 {
        return Vec::new();
    }
    let mut sorted = series.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med = median(&sorted);
    let mut dev: Vec<f32> = series.iter().map(|x| (x - med).abs()).collect();
    dev.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let scale = 1.4826 * median(&dev);
    if scale > 1e-9 {
        return series.iter().map(|x| (x - med) / scale).collect();
    }
    // Fallback: mean / population std.
    let mean = series.iter().sum::<f32>() / n as f32;
    let var = series.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n as f32;
    let std = var.sqrt();
    if std > 1e-9 {
        series.iter().map(|x| (x - mean) / std).collect()
    } else {
        vec![0.0; n]
    }
}

/// Centered moving average over `win` bins (clamped at the edges). `win <= 1`
/// is a no-op. Smoothing finds sustained spikes rather than single-bin noise.
pub fn smooth(series: &[f32], win: usize) -> Vec<f32> {
    if win <= 1 || series.is_empty() {
        return series.to_vec();
    }
    let half = win / 2;
    let n = series.len();
    (0..n)
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(n);
            series[lo..hi].iter().sum::<f32>() / (hi - lo) as f32
        })
        .collect()
}

/// Centered rolling *maximum* over `win` bins. Unlike [`smooth`] (mean), this is
/// peak-preserving: a brief, sharp spike (a scream, a hype burst) lifts its whole
/// neighborhood instead of being averaged away — so transient events still
/// surface, and the Moment window lands on them rather than on a nearby stretch
/// of merely-sustained loudness. `win <= 1` is a no-op.
pub fn smooth_max(series: &[f32], win: usize) -> Vec<f32> {
    if win <= 1 || series.is_empty() {
        return series.to_vec();
    }
    let half = win / 2;
    let n = series.len();
    (0..n)
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(n);
            series[lo..hi].iter().copied().fold(f32::MIN, f32::max)
        })
        .collect()
}

/// Weighted sum of equal-length signal series, index by index. Series shorter
/// than the first are treated as zero past their end (defensive; callers align
/// lengths). Used for the combined rank only - the per-signal values are stored
/// unblended on each Moment (ADR 0002).
pub fn combine(series: &[(&[f32], f32)], n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| series.iter().map(|(s, w)| w * s.get(i).copied().unwrap_or(0.0)).sum())
        .collect()
}

/// Indices that are local maxima of `series` and at least `min`. The asymmetric
/// comparison picks the left edge of a flat top once; NMS dedupes the rest.
pub fn find_peaks(series: &[f32], min: f32) -> Vec<usize> {
    let n = series.len();
    (0..n)
        .filter(|&i| {
            let up = i == 0 || series[i] > series[i - 1];
            let down = i + 1 == n || series[i] >= series[i + 1];
            up && down && series[i] >= min
        })
        .collect()
}

/// Non-maximum suppression: keep peaks strongest-first, rejecting any within
/// `min_gap` bins of an already-kept (higher) peak, so candidate ranges do not
/// overlap. Returns kept bin indices sorted by descending score.
pub fn nms(mut peaks: Vec<usize>, scores: &[f32], min_gap: usize) -> Vec<usize> {
    peaks.sort_by(|&a, &b| {
        scores[b].partial_cmp(&scores[a]).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut kept: Vec<usize> = Vec::new();
    for p in peaks {
        if kept.iter().all(|&k| k.abs_diff(p) >= min_gap) {
            kept.push(p);
        }
    }
    kept
}

/// Per-peak pre-roll lead (ADR 0020): a loud-driven peak (a jumpscare, a loud
/// mediashare donation) needs more build-up captured *before* the peak than a
/// chat-driven one. Linear from `base_lead_s` at loudness z `lo_z` up to
/// `loud_lead_s` at `hi_z`, clamped outside `[lo_z, hi_z]`. A chat-driven peak
/// (low loudness z) keeps the base chat-lag lead; the louder the peak, the longer
/// the pre-roll. Pure, so the ramp is unit-tested.
pub fn peak_lead_s(loud_z: f32, lo_z: f32, hi_z: f32, base_lead_s: f64, loud_lead_s: f64) -> f64 {
    let t = if hi_z > lo_z {
        ((loud_z - lo_z) / (hi_z - lo_z)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    base_lead_s + (loud_lead_s - base_lead_s) * t as f64
}

/// A peak bin -> a fixed-duration Moment range that *leads* the peak (chat lags
/// the on-screen event), clamped to `[0, vod_dur_s]`. `lead_s` is the per-peak
/// pre-roll ([`peak_lead_s`]); a longer lead just shifts the window earlier.
pub fn peak_to_range(peak_bin: usize, bin_s: f64, lead_s: f64, dur_s: f64, vod_dur_s: f64) -> TimeRange {
    let peak_t = peak_bin as f64 * bin_s + bin_s / 2.0;
    let start_s = (peak_t - lead_s).max(0.0);
    let end_s = (start_s + dur_s).min(vod_dur_s.max(start_s));
    TimeRange { start_s, end_s }
}

/// Fraction of the peak's combined score below which the moment is considered
/// "over" — the sustain walk in [`adaptive_range`] stops here. Floored at
/// [`SUSTAIN_FLOOR_Z`] so a barely-over-threshold peak doesn't chase noise.
const SUSTAIN_FRAC: f32 = 0.45;
const SUSTAIN_FLOOR_Z: f32 = 0.5;
/// Breathing room appended after the last elevated bin (the reaction's tail) and
/// prepended before an early build-up, seconds.
const TAIL_PAD_S: f64 = 2.0;
const BUILDUP_PAD_S: f64 = 1.0;

/// A peak bin -> a **naturally-sized** Moment range (the fix for "every clip is
/// ~30 s"): instead of a fixed duration, the window grows to cover the span
/// where the combined signal stays *elevated* — a sustained hype moment keeps
/// its whole arc (40-60 s+), a sharp one-off stays tight — bounded by
/// `[min_dur_s, max_dur_s]` and clamped to the VOD.
///
/// - **start**: the earlier of (peak − `lead_s`, the ADR 0020 signal-aware
///   pre-roll, which stays the *minimum*) and the elevated region's left edge
///   minus a small build-up pad.
/// - **end**: the elevated region's right edge plus a tail pad, floored so the
///   range is at least `min_dur_s` and capped at `max_dur_s` (the tail is
///   trimmed first — the build-up and peak are what make the clip land).
///
/// Elevated = `combined >= max(SUSTAIN_FRAC * peak_value, SUSTAIN_FLOOR_Z)`,
/// walked contiguously outward from the peak so an unrelated later spike never
/// glues two moments together.
pub fn adaptive_range(
    combined: &[f32],
    peak_bin: usize,
    bin_s: f64,
    lead_s: f64,
    min_dur_s: f64,
    max_dur_s: f64,
    vod_dur_s: f64,
) -> TimeRange {
    let n = combined.len();
    if n == 0 || peak_bin >= n {
        return TimeRange { start_s: 0.0, end_s: min_dur_s.min(vod_dur_s) };
    }
    let peak_t = peak_bin as f64 * bin_s + bin_s / 2.0;
    let sustain = (SUSTAIN_FRAC * combined[peak_bin]).max(SUSTAIN_FLOOR_Z);
    // Bound each walk so a pathological flat-hot series can't scan the whole VOD.
    let max_walk = (max_dur_s / bin_s).ceil() as usize;

    let mut left = peak_bin;
    while left > 0 && peak_bin - (left - 1) <= max_walk && combined[left - 1] >= sustain {
        left -= 1;
    }
    let mut right = peak_bin;
    while right + 1 < n && (right + 1) - peak_bin <= max_walk && combined[right + 1] >= sustain {
        right += 1;
    }

    // Start: the ADR 0020 lead is the minimum pre-roll; an earlier build-up
    // (elevation before the lead window) extends it, padded — but the build-up
    // never takes more than ~a third of the duration budget, so the tail cap
    // below can never push the peak itself out of the window.
    let elev_start = left as f64 * bin_s - BUILDUP_PAD_S;
    let max_buildup = (max_dur_s * 0.35).max(lead_s);
    let mut start_s = (peak_t - lead_s).min(elev_start).max(peak_t - max_buildup).max(0.0);
    // End: the elevation's tail plus breathing room, floored at min_dur_s.
    let elev_end = (right + 1) as f64 * bin_s + TAIL_PAD_S;
    let mut end_s = elev_end.max(start_s + min_dur_s);
    // Cap: trim the tail first (never the build-up/peak), then clamp to the VOD.
    if end_s - start_s > max_dur_s {
        end_s = start_s + max_dur_s;
    }
    end_s = end_s.min(vod_dur_s.max(start_s));
    // A peak near the VOD end can leave less than min_dur after clamping; pull
    // the start back so short-but-real moments at the tail keep their floor.
    if end_s - start_s < min_dur_s {
        start_s = (end_s - min_dur_s).max(0.0);
    }
    TimeRange { start_s, end_s }
}

/// Overlap-suppress adaptive candidate ranges, strongest-first: a later
/// (weaker) candidate is trimmed away from every already-kept range; if what
/// remains is shorter than `min_dur_s` or no longer contains its peak, it is
/// dropped. Returns the surviving `(peak_bin, range)`s in the input (strength)
/// order. This replaces the fixed-gap NMS spacing guarantee now that ranges
/// vary in length.
pub fn suppress_overlaps(
    candidates: Vec<(usize, TimeRange)>,
    bin_s: f64,
    min_dur_s: f64,
) -> Vec<(usize, TimeRange)> {
    let mut kept: Vec<(usize, TimeRange)> = Vec::new();
    'cand: for (bin, mut range) in candidates {
        let peak_t = bin as f64 * bin_s + bin_s / 2.0;
        for (_, k) in &kept {
            // No overlap with this kept range.
            if range.end_s <= k.start_s || range.start_s >= k.end_s {
                continue;
            }
            // Trim the side that intrudes; if the peak itself sits inside the
            // kept range, this candidate is a shoulder of it — drop.
            if peak_t >= k.start_s && peak_t < k.end_s {
                continue 'cand;
            }
            if peak_t < k.start_s {
                range.end_s = range.end_s.min(k.start_s);
            } else {
                range.start_s = range.start_s.max(k.end_s);
            }
        }
        if range.duration_s() >= min_dur_s && peak_t >= range.start_s && peak_t < range.end_s {
            kept.push((bin, range));
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bins_count_offsets_and_drop_out_of_grid() {
        // 1 s bins, 3 bins (grid = [0,3) s). 0.1/0.9/0.95 share bin 0; 3.5 s
        // is past the grid and dropped.
        let c = bin_counts(&[0.1, 0.9, 0.95, 2.2, 3.5], 1.0, 3);
        assert_eq!(c, vec![3.0, 0.0, 1.0]);
    }

    #[test]
    fn robust_z_lifts_a_spike_above_a_baseline() {
        // A baseline with mild spread (so the MAD path, not the fallback, runs)
        // plus a 9.0 spike at index 3: the spike should be strongly positive.
        let z = robust_z(&[1.0, 2.0, 1.0, 9.0, 2.0, 1.0]);
        assert!(z[3] > 3.0, "spike z = {}", z[3]);
        assert!(z[0] < 0.5);
    }

    #[test]
    fn robust_z_handles_mostly_zero_counts_via_fallback() {
        // Median and MAD are both 0 here; the fallback (mean/std) must still
        // make the spike the largest value rather than dividing by zero.
        let z = robust_z(&[0.0, 0.0, 0.0, 0.0, 5.0, 0.0]);
        assert!(z.iter().all(|v| v.is_finite()));
        let max_i = z.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0;
        assert_eq!(max_i, 4);
    }

    #[test]
    fn flat_series_has_no_spikes() {
        assert_eq!(robust_z(&[2.0, 2.0, 2.0]), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn smooth_averages_neighbours() {
        let s = smooth(&[0.0, 0.0, 9.0, 0.0, 0.0], 3);
        assert!((s[2] - 3.0).abs() < 1e-6); // (0+9+0)/3
        assert!((s[1] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn smooth_max_preserves_a_spike_across_its_neighbourhood() {
        // A lone spike lifts the bins within the window to the spike's value,
        // instead of averaging it down (a 2 s scream vs sustained talk).
        let s = smooth_max(&[0.0, 0.0, 9.0, 0.0, 0.0], 3);
        assert_eq!(s[1], 9.0);
        assert_eq!(s[2], 9.0);
        assert_eq!(s[3], 9.0);
        assert_eq!(s[0], 0.0); // outside the window, untouched
    }

    #[test]
    fn find_peaks_and_nms_pick_the_strongest_and_space_them() {
        // Two close peaks (bins 2 and 4) and one far (bin 8).
        let series = vec![0.0, 1.0, 5.0, 1.0, 4.0, 0.0, 0.0, 1.0, 6.0, 1.0];
        let peaks = find_peaks(&series, 2.0);
        assert!(peaks.contains(&2) && peaks.contains(&4) && peaks.contains(&8));
        // min_gap 3: bin 4 is within 3 of the stronger bin 2, so it is dropped.
        let kept = nms(peaks, &series, 3);
        assert_eq!(kept, vec![8, 2]); // sorted by descending score
    }

    #[test]
    fn peak_lead_scales_with_loudness_between_the_anchors() {
        // base 5 s at lo_z 1.0, max 10 s at hi_z 3.0.
        assert!((peak_lead_s(1.0, 1.0, 3.0, 5.0, 10.0) - 5.0).abs() < 1e-6); // at lo -> base
        assert!((peak_lead_s(3.0, 1.0, 3.0, 5.0, 10.0) - 10.0).abs() < 1e-6); // at hi -> max
        assert!((peak_lead_s(2.0, 1.0, 3.0, 5.0, 10.0) - 7.5).abs() < 1e-6); // midpoint
        // A chat-driven peak (loudness z below lo) keeps the base lead.
        assert!((peak_lead_s(-0.5, 1.0, 3.0, 5.0, 10.0) - 5.0).abs() < 1e-6);
        // A very loud peak (above hi) is capped at the max lead.
        assert!((peak_lead_s(9.0, 1.0, 3.0, 5.0, 10.0) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn adaptive_range_stays_tight_on_a_spike_and_grows_over_a_plateau() {
        // A lone spike: nothing elevated around it -> the minimum window (lead
        // pre-roll + min_dur floor), exactly the old fixed behaviour.
        let mut spike = vec![0.0f32; 300];
        spike[100] = 4.0;
        let r = adaptive_range(&spike, 100, 1.0, 5.0, 15.0, 90.0, 300.0);
        assert!((r.start_s - 95.5).abs() < 1e-6, "start {}", r.start_s); // peak_t 100.5 - lead 5
        assert!((r.duration_s() - 15.0).abs() < 1e-6, "dur {}", r.duration_s());

        // A 40 s sustained plateau (hype arc): the window covers the whole arc
        // plus pads, well past the old fixed 30 s.
        let mut plateau = vec![0.0f32; 300];
        for v in plateau.iter_mut().skip(100).take(40) {
            *v = 3.0;
        }
        plateau[110] = 4.0; // the peak inside the arc
        let r = adaptive_range(&plateau, 110, 1.0, 5.0, 15.0, 90.0, 300.0);
        assert!(r.start_s <= 99.0 + 1e-6, "covers the arc start, got {}", r.start_s);
        assert!(r.end_s >= 140.0, "covers the arc tail, got {}", r.end_s);
        assert!(r.duration_s() > 40.0 && r.duration_s() <= 90.0, "dur {}", r.duration_s());
    }

    #[test]
    fn adaptive_range_caps_at_max_and_respects_the_vod_end() {
        // A plateau far longer than the cap: trimmed to max_dur_s, tail first
        // (the start keeps the build-up + peak).
        let hot = vec![3.0f32; 600];
        let r = adaptive_range(&hot, 300, 1.0, 5.0, 15.0, 60.0, 600.0);
        assert!((r.duration_s() - 60.0).abs() < 1e-6, "dur {}", r.duration_s());
        assert!(r.start_s <= 295.5 && 300.5 < r.end_s, "peak stays inside");

        // A peak at the VOD tail: the end clamps to the VOD and the start pulls
        // back to keep the min_dur floor.
        let mut tail = vec![0.0f32; 100];
        tail[98] = 4.0;
        let r = adaptive_range(&tail, 98, 1.0, 5.0, 15.0, 90.0, 100.0);
        assert!(r.end_s <= 100.0 + 1e-6);
        assert!((r.duration_s() - 15.0).abs() < 1e-6, "dur {}", r.duration_s());
    }

    #[test]
    fn suppress_overlaps_trims_weaker_and_drops_contained() {
        let a = (50usize, TimeRange { start_s: 40.0, end_s: 100.0 }); // strongest
        // Peak inside a's range -> a shoulder of the same moment, dropped.
        let b = (60usize, TimeRange { start_s: 55.0, end_s: 120.0 });
        // Overlapping tail but its own peak outside a -> trimmed to start at 100.
        let c = (110usize, TimeRange { start_s: 90.0, end_s: 140.0 });
        // Too little left after the trim (would be 100..104) -> dropped.
        let d = (102usize, TimeRange { start_s: 95.0, end_s: 104.0 });
        let kept = suppress_overlaps(vec![a, b, c, d], 1.0, 15.0);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].0, 50);
        assert_eq!(kept[1].0, 110);
        assert!((kept[1].1.start_s - 100.0).abs() < 1e-6, "trimmed to the kept edge");
        assert!((kept[1].1.end_s - 140.0).abs() < 1e-6);
    }

    #[test]
    fn range_leads_the_peak_and_clamps() {
        // peak bin 10 @ 1 s bins -> peak_t 10.5; lead 5 -> start 5.5; dur 30.
        let r = peak_to_range(10, 1.0, 5.0, 30.0, 3600.0);
        assert!((r.start_s - 5.5).abs() < 1e-9);
        assert!((r.end_s - 35.5).abs() < 1e-9);
        // Near t=0 the lead clamps to 0, never negative.
        let head = peak_to_range(1, 1.0, 5.0, 30.0, 3600.0);
        assert_eq!(head.start_s, 0.0);
        // Near the end, the window clamps to the VOD duration.
        let tail = peak_to_range(3590, 1.0, 5.0, 30.0, 3600.0);
        assert!(tail.end_s <= 3600.0);
    }
}
