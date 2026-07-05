//! Voice embeddings for speaker diarization — the SPIKE half (ADR 0042 gate
//! pending): "remember the voice" so attribution can tell speakers apart when
//! mouth motion can't (overlapping speech, off-screen voices, low-motion
//! talkers — ADR 0038's documented upgrade path).
//!
//! Split mirrors [`crate::infer`]: everything measurable without a model —
//! the Kaldi-compatible log-mel fbank frontend, per-window mean normalization,
//! and agglomerative cosine clustering — is pure and unit-tested here; only
//! [`VoiceEmbedder`] (the CAM++ ONNX session) sits behind the `voice` cargo
//! feature. Nothing in the production analysis path calls this module yet:
//! the `speaker_diag` harness drives it, and integration waits on the
//! operator's A/B gate (ADR 0029 lesson: analysis-side spikes over-promise).
//!
//! The CAM++ exports (sherpa-onnx release) do NOT embed a feature frontend in
//! the graph: they take 80-dim Kaldi-style log-mel fbank `[1, T, 80]`, so the
//! frontend below must match kaldi-native-fbank's defaults exactly — 16 kHz,
//! 25 ms frames / 10 ms shift, povey window, no dither, preemphasis 0.97,
//! per-frame DC removal, snip_edges=false (reflected edges), mel 20..7600 Hz.
//! WeSpeaker models additionally expect samples at int16 scale and their
//! reference inference subtracts the per-window mean per mel bin (CMN); the
//! 3D-Speaker exports keep samples in [-1, 1] with the same mean subtraction.

use anyhow::Result;

/// Mel bins per frame — the CAM++ input width.
pub const N_MELS: usize = 80;
/// Embedding input sample rate (the analysis.wav rate).
pub const VOICE_SR: u32 = 16_000;
/// 25 ms frame / 10 ms shift at 16 kHz — kaldi-native-fbank defaults.
const FRAME_LEN: usize = 400;
const FRAME_SHIFT: usize = 160;
/// FFT size: next power of two above the frame length.
const N_FFT: usize = 512;
const PREEMPH: f32 = 0.97;
/// Mel filter band edges (Hz). 7600 is kaldi's `high_freq = -400` at 16 kHz.
const MEL_LOW_HZ: f32 = 20.0;
const MEL_HIGH_HZ: f32 = 7600.0;

/// Kaldi-compatible 80-mel log filterbank frontend. Construct once (the mel
/// weights and FFT twiddles are precomputed), then [`Self::compute`] per
/// window.
pub struct Fbank {
    window: Vec<f32>,
    /// Per mel bank: (first FFT bin, triangle weights from that bin on).
    banks: Vec<(usize, Vec<f32>)>,
    /// e^(-2πik/N_FFT) for k in 0..N_FFT/2.
    tw_re: Vec<f32>,
    tw_im: Vec<f32>,
}

impl Fbank {
    pub fn new() -> Self {
        // Povey window: hann^0.85 — kaldi's default window function.
        let window: Vec<f32> = (0..FRAME_LEN)
            .map(|i| {
                let hann =
                    0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FRAME_LEN - 1) as f32).cos();
                hann.powf(0.85)
            })
            .collect();
        let mel = |f: f32| 1127.0 * (1.0 + f / 700.0).ln();
        let (mlo, mhi) = (mel(MEL_LOW_HZ), mel(MEL_HIGH_HZ));
        let delta = (mhi - mlo) / (N_MELS + 1) as f32;
        let bin_hz = VOICE_SR as f32 / N_FFT as f32;
        let banks = (0..N_MELS)
            .map(|m| {
                let (l, c, r) = (
                    mlo + m as f32 * delta,
                    mlo + (m + 1) as f32 * delta,
                    mlo + (m + 2) as f32 * delta,
                );
                let mut first = None;
                let mut weights = Vec::new();
                // Nyquist bin excluded, matching kaldi's num_fft_bins = N/2.
                for b in 0..N_FFT / 2 {
                    let fm = mel(bin_hz * b as f32);
                    let w = if fm > l && fm < r {
                        if fm <= c { (fm - l) / (c - l) } else { (r - fm) / (r - c) }
                    } else {
                        0.0
                    };
                    if w > 0.0 {
                        first.get_or_insert(b);
                        weights.push(w);
                    } else if first.is_some() {
                        break;
                    }
                }
                (first.unwrap_or(0), weights)
            })
            .collect();
        let (tw_re, tw_im) = (0..N_FFT / 2)
            .map(|k| {
                let a = -2.0 * std::f32::consts::PI * k as f32 / N_FFT as f32;
                (a.cos(), a.sin())
            })
            .unzip();
        Self { window, banks, tw_re, tw_im }
    }

    /// Frames produced for `n` samples (kaldi `snip_edges=false`: frames are
    /// centered on the shift grid and edges reflect, so short windows still
    /// yield `n / shift` frames rather than losing a frame length).
    pub fn n_frames(n_samples: usize) -> usize {
        (n_samples + FRAME_SHIFT / 2) / FRAME_SHIFT
    }

    /// Log-mel features for one window of 16 kHz mono samples, flattened
    /// `n_frames x N_MELS`. Apply [`cmn`] before feeding a speaker model.
    pub fn compute(&self, samples: &[f32]) -> Vec<f32> {
        let n = samples.len();
        let n_frames = Self::n_frames(n);
        let mut out = vec![0f32; n_frames * N_MELS];
        if n == 0 {
            return out;
        }
        let mut re = vec![0f32; N_FFT];
        let mut im = vec![0f32; N_FFT];
        let mut frame = vec![0f32; FRAME_LEN];
        for f in 0..n_frames {
            // snip_edges=false extraction: frame midpoint on the shift grid,
            // out-of-range samples reflected at the signal edges.
            let begin = (f * FRAME_SHIFT + FRAME_SHIFT / 2) as isize - (FRAME_LEN / 2) as isize;
            for (i, dst) in frame.iter_mut().enumerate() {
                let mut s = begin + i as isize;
                if s < 0 {
                    s = -s - 1;
                }
                if s >= n as isize {
                    s = 2 * n as isize - 1 - s;
                }
                *dst = samples[s.clamp(0, n as isize - 1) as usize];
            }
            // Kaldi order: DC removal, then preemphasis (in reverse), then the
            // window function.
            let mean = frame.iter().sum::<f32>() / FRAME_LEN as f32;
            for v in frame.iter_mut() {
                *v -= mean;
            }
            for i in (1..FRAME_LEN).rev() {
                frame[i] -= PREEMPH * frame[i - 1];
            }
            frame[0] -= PREEMPH * frame[0];
            re[..FRAME_LEN]
                .iter_mut()
                .zip(frame.iter().zip(self.window.iter()))
                .for_each(|(r, (v, w))| *r = v * w);
            re[FRAME_LEN..].fill(0.0);
            im.fill(0.0);
            self.fft(&mut re, &mut im);
            for (m, (first, weights)) in self.banks.iter().enumerate() {
                let mut e = 0f32;
                for (j, w) in weights.iter().enumerate() {
                    let b = first + j;
                    e += w * (re[b] * re[b] + im[b] * im[b]);
                }
                out[f * N_MELS + m] = e.max(f32::EPSILON).ln();
            }
        }
        out
    }

    /// Iterative radix-2 FFT over `N_FFT` points, in place.
    fn fft(&self, re: &mut [f32], im: &mut [f32]) {
        let n = N_FFT;
        let bits = n.trailing_zeros();
        for i in 0..n {
            let j = i.reverse_bits() >> (usize::BITS - bits);
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for base in (0..n).step_by(len) {
                for k in 0..half {
                    let (tr, ti) = (self.tw_re[k * step], self.tw_im[k * step]);
                    let (vr, vi) = (re[base + k + half], im[base + k + half]);
                    let (wr, wi) = (vr * tr - vi * ti, vr * ti + vi * tr);
                    let (ur, ui) = (re[base + k], im[base + k]);
                    re[base + k] = ur + wr;
                    im[base + k] = ui + wi;
                    re[base + k + half] = ur - wr;
                    im[base + k + half] = ui - wi;
                }
            }
            len *= 2;
        }
    }
}

impl Default for Fbank {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-window cepstral-style mean normalization: subtract each mel bin's mean
/// over the window's frames, in place. Both model families expect it
/// (WeSpeaker `cmn=True`, 3D-Speaker `feature_normalize_type=global-mean`) —
/// and it makes the families' differing sample scales moot, since a constant
/// gain is a constant log offset that the mean subtraction removes.
pub fn cmn(feats: &mut [f32]) {
    let t = feats.len() / N_MELS;
    if t == 0 {
        return;
    }
    for m in 0..N_MELS {
        let mean = (0..t).map(|f| feats[f * N_MELS + m]).sum::<f32>() / t as f32;
        for f in 0..t {
            feats[f * N_MELS + m] -= mean;
        }
    }
}

/// Agglomerative average-linkage clustering over cosine distance, for
/// L2-normalized embeddings. Clusters merge while the closest pair sits under
/// `threshold`; the returned assignment ids are ordered by cluster size
/// (0 = the most windows). `merges` records every accepted merge distance in
/// order — print it to SEE the same-voice/different-voice gap the threshold
/// sits in, instead of trusting a magic number.
pub struct Clustering {
    pub assignment: Vec<usize>,
    pub merges: Vec<f32>,
    pub k: usize,
}

pub fn cluster_cosine(embs: &[Vec<f32>], threshold: f32) -> Clustering {
    let n = embs.len();
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut merges = Vec::new();
    // Pairwise cosine distances between windows (embeddings are unit-norm).
    let dist = |a: usize, b: usize| -> f32 {
        1.0 - embs[a].iter().zip(embs[b].iter()).map(|(x, y)| x * y).sum::<f32>()
    };
    loop {
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..members.len() {
            for j in i + 1..members.len() {
                let mut sum = 0f32;
                for &a in &members[i] {
                    for &b in &members[j] {
                        sum += dist(a, b);
                    }
                }
                let d = sum / (members[i].len() * members[j].len()) as f32;
                if best.map(|(_, _, bd)| d < bd).unwrap_or(true) {
                    best = Some((i, j, d));
                }
            }
        }
        match best {
            Some((i, j, d)) if d < threshold => {
                let b = members.remove(j);
                members[i].extend(b);
                merges.push(d);
            }
            _ => break,
        }
    }
    members.sort_by_key(|m| std::cmp::Reverse(m.len()));
    let mut assignment = vec![0usize; n];
    for (c, m) in members.iter().enumerate() {
        for &i in m {
            assignment[i] = c;
        }
    }
    Clustering { assignment, merges, k: members.len() }
}

/// Embedding window length / hop (seconds) — WeSpeaker's own diarizer
/// defaults, and the operating point the spike's gate measures at.
pub const WIN_S: f64 = 1.5;
pub const HOP_S: f64 = 0.75;
/// VAD gaps at most this long are bridged into one voiced span (a breath or a
/// plosive dip is not a turn boundary)...
const BRIDGE_S: f64 = 0.2;
/// ...and a bridged span shorter than this embeds nothing (too little speech
/// to identify a voice; WeSpeaker's floor is 0.255 s).
const MIN_SPAN_S: f64 = 0.3;

/// Slice the VAD's voiced bins into embedding windows: contiguous voiced
/// spans (short gaps bridged, tiny spans dropped), each cut into [`WIN_S`]
/// windows hopped [`HOP_S`]; a span shorter than one window embeds whole, and
/// a span tail longer than [`MIN_SPAN_S`] gets a final full-size window
/// snapped to the span end (so turn endings — where speakers change — are
/// always covered). Returns clip-relative `(start_s, end_s)` per window.
pub fn plan_windows(voiced: &[bool], bin_s: f64) -> Vec<(f64, f64)> {
    let max_gap = (BRIDGE_S / bin_s).round() as usize;
    // Voiced spans over bins, bridging gaps <= max_gap.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (b, &v) in voiced.iter().enumerate() {
        if !v {
            continue;
        }
        match spans.last_mut() {
            Some((_, e)) if b <= *e + max_gap => *e = b + 1,
            _ => spans.push((b, b + 1)),
        }
    }
    let mut windows = Vec::new();
    for (b0, b1) in spans {
        let (s, e) = (b0 as f64 * bin_s, b1 as f64 * bin_s);
        if e - s < MIN_SPAN_S {
            continue;
        }
        if e - s <= WIN_S {
            windows.push((s, e));
            continue;
        }
        let mut t = s;
        while t + WIN_S <= e {
            windows.push((t, t + WIN_S));
            t += HOP_S;
        }
        let covered = windows.last().map(|w| w.1).unwrap_or(s);
        if e - covered >= MIN_SPAN_S {
            windows.push((e - WIN_S, e));
        }
    }
    windows
}

/// How a model expects raw samples scaled before the fbank. WeSpeaker trained
/// on int16-scaled waveforms; 3D-Speaker on [-1, 1]. (CMN cancels the
/// difference in principle — kept explicit so each model runs its reference
/// convention.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleScale {
    Int16,
    Unit,
}

/// Resident CAM++ speaker-embedding session (`voice` feature): 16 kHz samples
/// → fbank+CMN → `[1, T, 80]` → L2-normalized embedding. Mirrors
/// [`crate::infer::Detector`]'s loading shape.
#[cfg(feature = "voice")]
pub struct VoiceEmbedder {
    session: ort::session::Session,
    input_name: String,
    output_name: String,
    scale: SampleScale,
    /// Whether to subtract the per-window mel means before inference — a
    /// per-model convention: 3D-Speaker exports declare it
    /// (`feature_normalize_type=global-mean`); sherpa runs its WeSpeaker
    /// exports without it. (With CMN on, the sample scale provably cancels —
    /// the selftest measured bit-identical cosines for int16 and unit input.)
    apply_cmn: bool,
    fbank: Fbank,
}

#[cfg(feature = "voice")]
impl VoiceEmbedder {
    /// Load a speaker-embedding ONNX on the CPU execution provider.
    pub fn load(onnx: &std::path::Path, scale: SampleScale, apply_cmn: bool) -> Result<Self> {
        use anyhow::Context;
        let oerr = |e: ort::Error| anyhow::anyhow!("{e}");
        let session = ort::session::Session::builder()
            .and_then(|mut b| b.commit_from_file(onnx))
            .map_err(oerr)
            .with_context(|| format!("loading voice model {}", onnx.display()))?;
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| anyhow::anyhow!("voice model has no inputs"))?;
        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| anyhow::anyhow!("voice model has no outputs"))?;
        tracing::info!(input = %input_name, output = %output_name, "voice model loaded");
        Ok(Self { session, input_name, output_name, scale, apply_cmn, fbank: Fbank::new() })
    }

    /// Embed one voiced window of 16 kHz mono samples. Returns a unit-norm
    /// embedding (cosine similarity = dot product).
    pub fn embed(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let oerr = |e: ort::Error| anyhow::anyhow!("{e}");
        anyhow::ensure!(!samples.is_empty(), "empty window");
        let mut feats = match self.scale {
            SampleScale::Unit => self.fbank.compute(samples),
            SampleScale::Int16 => {
                let scaled: Vec<f32> = samples.iter().map(|s| s * 32768.0).collect();
                self.fbank.compute(&scaled)
            }
        };
        if self.apply_cmn {
            cmn(&mut feats);
        }
        let t = feats.len() / N_MELS;
        let input =
            ort::value::Tensor::from_array(([1_i64, t as i64, N_MELS as i64], feats)).map_err(oerr)?;
        let outputs =
            self.session.run(ort::inputs![self.input_name.as_str() => input]).map_err(oerr)?;
        let (_shape, emb) =
            outputs[self.output_name.as_str()].try_extract_tensor::<f32>().map_err(oerr)?;
        let norm = emb.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
        Ok(emb.iter().map(|v| v / norm).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn povey_window_shape() {
        let fb = Fbank::new();
        // Endpoints ~0, center ~1, symmetric — the hann^0.85 profile.
        assert!(fb.window[0] < 1e-3);
        assert!(fb.window[FRAME_LEN - 1] < 1e-3);
        let mid = fb.window[FRAME_LEN / 2];
        assert!(mid > 0.99 && mid <= 1.0, "center {mid}");
        assert!((fb.window[10] - fb.window[FRAME_LEN - 11]).abs() < 1e-4);
    }

    #[test]
    fn frame_count_matches_kaldi_snip_edges_false() {
        // (n + shift/2) / shift: 1.5 s @ 16 kHz -> 150 frames; one shift -> 1.
        assert_eq!(Fbank::n_frames(24_000), 150);
        assert_eq!(Fbank::n_frames(160), 1);
        assert_eq!(Fbank::n_frames(400), 3);
        assert_eq!(Fbank::n_frames(0), 0);
    }

    #[test]
    fn fft_impulse_is_flat_and_sine_is_a_line() {
        let fb = Fbank::new();
        // Impulse -> unit magnitude everywhere.
        let mut re = vec![0f32; N_FFT];
        let mut im = vec![0f32; N_FFT];
        re[0] = 1.0;
        fb.fft(&mut re, &mut im);
        for k in 0..N_FFT {
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt();
            assert!((mag - 1.0).abs() < 1e-4, "bin {k} mag {mag}");
        }
        // A pure tone on an exact bin -> energy only at k and N-k.
        let mut re = (0..N_FFT)
            .map(|i| (2.0 * std::f32::consts::PI * 8.0 * i as f32 / N_FFT as f32).cos())
            .collect::<Vec<_>>();
        let mut im = vec![0f32; N_FFT];
        fb.fft(&mut re, &mut im);
        let pow = |k: usize| re[k] * re[k] + im[k] * im[k];
        assert!(pow(8) > 1000.0 * pow(7).max(pow(9)).max(1e-6));
    }

    #[test]
    fn mel_banks_cover_the_speech_band() {
        let fb = Fbank::new();
        assert_eq!(fb.banks.len(), N_MELS);
        for (m, (_, w)) in fb.banks.iter().enumerate() {
            assert!(!w.is_empty(), "bank {m} empty");
        }
        // A 1 kHz tone lights a low-mid bank; a bin near nyquist stays dark
        // (filters end at 7600 Hz).
        let bin_hz = VOICE_SR as f32 / N_FFT as f32;
        let last_bin_covered =
            fb.banks.iter().map(|(f, w)| f + w.len()).max().unwrap_or(0);
        assert!(
            (last_bin_covered as f32) * bin_hz <= MEL_HIGH_HZ + bin_hz,
            "filters leak past high_freq: bin {last_bin_covered}"
        );
    }

    #[test]
    fn tone_lands_on_a_stable_mel_bin() {
        let fb = Fbank::new();
        let sr = VOICE_SR as f32;
        let samples: Vec<f32> =
            (0..16_000).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr).sin()).collect();
        let feats = fb.compute(&samples);
        let t = feats.len() / N_MELS;
        assert_eq!(t, Fbank::n_frames(16_000));
        // The argmax mel bin should be identical across interior frames.
        let argmax = |f: usize| {
            (0..N_MELS)
                .max_by(|&a, &b| {
                    feats[f * N_MELS + a].partial_cmp(&feats[f * N_MELS + b]).unwrap()
                })
                .unwrap()
        };
        let mid = argmax(t / 2);
        for f in 5..t - 5 {
            assert_eq!(argmax(f), mid, "frame {f}");
        }
        // 1 kHz sits in the lower half of an 80-bank 20..7600 Hz mel scale.
        assert!(mid > 20 && mid < 60, "1 kHz argmax bank {mid}");
    }

    #[test]
    fn cmn_zeroes_every_bin_mean() {
        let fb = Fbank::new();
        let samples: Vec<f32> =
            (0..8_000).map(|i| ((i * 37 % 101) as f32 / 101.0 - 0.5) * 0.1).collect();
        let mut feats = fb.compute(&samples);
        cmn(&mut feats);
        let t = feats.len() / N_MELS;
        for m in 0..N_MELS {
            let mean = (0..t).map(|f| feats[f * N_MELS + m]).sum::<f32>() / t as f32;
            assert!(mean.abs() < 1e-4, "bin {m} mean {mean}");
        }
    }

    #[test]
    fn windows_cover_voiced_spans_and_bridge_breaths() {
        let bin_s = 1.0 / 24.0;
        let nb = |s: f64| (s / bin_s).round() as usize;
        // 4 s of speech, a 0.15 s breath, 1.0 s more; then a long gap and a
        // 0.2 s blip (dropped).
        let mut voiced = vec![false; nb(10.0)];
        for b in 0..nb(4.0) {
            voiced[b] = true;
        }
        for b in nb(4.15)..nb(5.15) {
            voiced[b] = true;
        }
        for b in nb(8.0)..nb(8.2) {
            voiced[b] = true;
        }
        let w = plan_windows(&voiced, bin_s);
        // One bridged span 0..5.15: hopped full windows + a tail window
        // snapped to the span end; the 0.2 s blip embeds nothing.
        assert!(w.len() >= 5, "windows: {w:?}");
        let (first, last) = (w[0], *w.last().unwrap());
        assert!(first.0.abs() < 1e-9);
        assert!((last.1 - 5.125).abs() < 0.1, "span end covered: {last:?}");
        assert!(w.iter().all(|&(s, e)| e - s <= WIN_S + 1e-9 && e - s >= 0.29));
        assert!(w.iter().all(|&(s, _)| s < 6.0), "blip must not embed: {w:?}");
        // A lone short-but-real span embeds whole.
        let mut v2 = vec![false; nb(2.0)];
        for b in nb(0.5)..nb(1.3) {
            v2[b] = true;
        }
        let w2 = plan_windows(&v2, bin_s);
        assert_eq!(w2.len(), 1);
        assert!((w2[0].1 - w2[0].0 - 0.8).abs() < 0.1, "{w2:?}");
    }

    #[test]
    fn clustering_separates_three_synthetic_voices() {
        // Three well-separated unit vectors + jitter, shuffled.
        let base = [[1.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut embs = Vec::new();
        let mut truth = Vec::new();
        for i in 0..18 {
            let b = base[i % 3];
            let j = (i as f32 * 0.61) % 0.2;
            let v = [b[0] + j, b[1] + j * 0.7, b[2] - j * 0.3];
            let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            embs.push(vec![v[0] / n, v[1] / n, v[2] / n]);
            truth.push(i % 3);
        }
        let c = cluster_cosine(&embs, 0.5);
        assert_eq!(c.k, 3, "merges: {:?}", c.merges);
        // Same-truth windows share a cluster; different-truth never do.
        for i in 0..embs.len() {
            for j in 0..embs.len() {
                assert_eq!(
                    truth[i] == truth[j],
                    c.assignment[i] == c.assignment[j],
                    "windows {i},{j}"
                );
            }
        }
        // Degenerate thresholds: 0 keeps singletons, 2.0 fuses everything.
        assert_eq!(cluster_cosine(&embs, 0.0).k, embs.len());
        assert_eq!(cluster_cosine(&embs, 2.0).k, 1);
    }
}
