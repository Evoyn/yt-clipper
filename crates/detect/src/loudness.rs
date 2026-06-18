//! Loudness signal (ADR 0002, ADR 0007): windowed RMS energy of the whole-VOD
//! analysis wav. Later z-scored over the VOD so only loudness *above the
//! rolling baseline* counts - constant game loudness lands near zero and does
//! not, on its own, surface a Moment.
//!
//! The analysis wav is mono 16 kHz PCM (see `yc-ingest`). We stream it bin by
//! bin so a multi-hour VOD never has to sit in memory at once.

use anyhow::{Context, Result};
use std::path::Path;

/// RMS energy per fixed-size bin over mono f32 samples in [-1, 1]. Pure, so it
/// is unit-tested without a wav. A trailing partial bin is kept (it is the tail
/// of the VOD); an empty input yields no bins.
pub fn rms_bins(samples: &[f32], sr: u32, bin_s: f64) -> Vec<f32> {
    let bin = ((sr as f64) * bin_s).round().max(1.0) as usize;
    samples
        .chunks(bin)
        .map(|c| {
            let sum_sq: f64 = c.iter().map(|&s| (s as f64) * (s as f64)).sum();
            ((sum_sq / c.len() as f64).sqrt()) as f32
        })
        .collect()
}

/// Stream a mono 16 kHz PCM wav and return its per-bin RMS series. Bins are
/// `bin_s` wide; the i16 samples are scaled to roughly [-1, 1] to match
/// `read_range_samples` in `yc-ingest`.
pub fn read_rms_bins(wav: &Path, bin_s: f64) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(wav).with_context(|| format!("opening {}", wav.display()))?;
    let spec = reader.spec();
    anyhow::ensure!(spec.channels == 1, "expected mono analysis wav, got {} channels", spec.channels);
    let bin = ((spec.sample_rate as f64) * bin_s).round().max(1.0) as usize;

    let mut bins = Vec::new();
    let mut sum_sq = 0.0_f64;
    let mut n = 0_usize;
    for s in reader.samples::<i16>() {
        let x = s.context("decoding wav sample")? as f64 / 32768.0;
        sum_sq += x * x;
        n += 1;
        if n == bin {
            bins.push(((sum_sq / n as f64).sqrt()) as f32);
            sum_sq = 0.0;
            n = 0;
        }
    }
    if n > 0 {
        bins.push(((sum_sq / n as f64).sqrt()) as f32); // trailing partial bin
    }
    Ok(bins)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_of_a_constant_signal_is_its_amplitude() {
        // 4 samples of 0.5 at sr=4, 1 s bins -> one bin, rms = 0.5.
        let bins = rms_bins(&[0.5, 0.5, 0.5, 0.5], 4, 1.0);
        assert_eq!(bins.len(), 1);
        assert!((bins[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn splits_into_bins_and_keeps_a_partial_tail() {
        // sr=2, 1 s bins => 2 samples per bin. 5 samples -> 3 bins (last partial).
        let bins = rms_bins(&[1.0, 1.0, 0.0, 0.0, 1.0], 2, 1.0);
        assert_eq!(bins.len(), 3);
        assert!((bins[0] - 1.0).abs() < 1e-6); // sqrt((1+1)/2)
        assert!((bins[1] - 0.0).abs() < 1e-6);
        assert!((bins[2] - 1.0).abs() < 1e-6); // lone sample
    }

    #[test]
    fn empty_input_has_no_bins() {
        assert!(rms_bins(&[], 16_000, 1.0).is_empty());
    }
}
