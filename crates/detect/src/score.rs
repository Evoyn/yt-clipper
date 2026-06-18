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

/// A peak bin -> a fixed-duration Moment range that *leads* the peak (chat lags
/// the on-screen event), clamped to `[0, vod_dur_s]`.
pub fn peak_to_range(peak_bin: usize, bin_s: f64, lead_s: f64, dur_s: f64, vod_dur_s: f64) -> TimeRange {
    let peak_t = peak_bin as f64 * bin_s + bin_s / 2.0;
    let start_s = (peak_t - lead_s).max(0.0);
    let end_s = (start_s + dur_s).min(vod_dur_s.max(start_s));
    TimeRange { start_s, end_s }
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
