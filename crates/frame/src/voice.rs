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

use crate::speaker::{self, SpeakerAnalysis};
#[cfg(feature = "voice")]
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

// --- the voice lane (ADR 0042, integrated): clusters joined to seats -------

/// A voice-cluster must co-occur with a genuine mouth attribution for at
/// least this long (bins at [`speaker::SPEAKER_FPS`]) and win this share of
/// its own co-occurrence mass to join a seat; anything less stays unjoined
/// (an off-screen voice, an impure cluster, or a shared class like
/// laughter). Measured on the Deddy fixture: pure single-voice clusters
/// co-occur with their seat at 0.71-1.00 share, while the impure
/// both-voices blob sat at 0.55 — the floor lives in that gap.
const JOIN_MIN_BINS: usize = 24;
const JOIN_MIN_SHARE: f32 = 0.65;
/// Evidence floor for the per-angle-segment join (bins at
/// [`speaker::SPEAKER_FPS`]): less than the whole-clip floor because a
/// segment is short, but still half a second of co-occurrence before a
/// voice claims a seat within one angle.
const JOIN_MIN_BINS_SEG: usize = 12;
/// Clustering thresholds the CV-scored sweep considers (cosine distance).
const SWEEP_THRESHOLDS: [f32; 7] = [0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60];

/// The voice lane on the analysis grid: the diarization evidence
/// [`build_lane`] measured and [`fuse_attribution`] acted on, carried inside
/// [`SpeakerAnalysis`] for the planner (off-screen split, interjection
/// rescue) and the Studio timeline's voice row.
#[derive(Debug, Clone, Default)]
pub struct VoiceLane {
    /// The raw voice cluster per bin (`None` = unvoiced, or no window).
    pub cluster: Vec<Option<usize>>,
    /// The seat each bin's voice maps to through the angle-scoped join
    /// (`None` = unjoined, off-screen, or silence).
    pub seat: Vec<Option<usize>>,
    /// Bins where a KNOWN voice (joined somewhere) holds no seat in the
    /// on-screen angle — the off-screen-speaker signal (all false when the
    /// regime makes the whole-clip join valid).
    pub offscreen: Vec<bool>,
    /// Bins where the fused attribution overrode the mouth lane — filled by
    /// [`fuse_attribution`], all false straight out of [`build_lane`]. The
    /// planner's interjection rescue reads these.
    pub overridden: Vec<bool>,
}

/// The evidence trail from one [`build_lane`] run: the printable forensics
/// (the diag harness prints them verbatim; the pipeline logs them), the
/// per-window rows for offline digging, and the headline numbers.
pub struct VoiceDiag {
    pub lines: Vec<String>,
    /// `(start_s, end_s, cluster, whole-clip joined seat or -1)` per window.
    pub windows: Vec<(f64, f64, usize, i64)>,
    pub picked_thr: f32,
    /// Seconds of voiced time the joined lane claims a seat for.
    pub claimed_s: f64,
    /// Agreement with genuine mouth attribution where both claim (0..1).
    pub agreement: f64,
    /// Seconds flagged as a known off-screen voice.
    pub offscreen_s: f64,
}

/// Clip-relative segment boundaries from the source's scene cuts: 0, every
/// in-range cut, `duration_s`. The voice lane's angle grouping and the face
/// lane's occupant map (ADR 0043 spike) slice the clip identically through
/// this, so their per-segment verdicts line up bin for bin.
pub fn segment_bounds(cuts: &[f64], duration_s: f64) -> Vec<f64> {
    let mut seg_bounds: Vec<f64> = vec![0.0];
    for &c in cuts {
        if c > 0.03 && c < duration_s - 0.03 {
            seg_bounds.push(c);
        }
    }
    seg_bounds.push(duration_s);
    seg_bounds
}

/// Build the voice lane from embedded windows: a threshold sweep scored
/// **out-of-sample end to end** through the cluster→seat join picks the
/// clustering cut, clusters join seats **per camera angle** (segments
/// grouped by seat geometry; a single-visit angle can only echo the mouth
/// lane, so it may not claim), and a joined voice with no seat in the
/// on-screen angle marks an **off-screen speaker**. In the attribution
/// regime with several angles the whole-clip join is banned (a seat is a
/// screen position, not a person — ADR 0042 measured it leaking one
/// person's voice onto another person's seat); in follow-visible each track
/// is one person's framing, so the whole-clip join is the identity.
///
/// `angle_override` replaces the seat-geometry angle grouping with a caller's
/// own segment→angle map (one entry per [`segment_bounds`] segment) — the
/// ADR 0043 harness replays the join over occupant-merged cameras through
/// it. Production callers pass `None`: the signature grouping is the shipped
/// behavior (pinned by the ANTITESA `camera_diag.fg` byte hash).
///
/// `None` when fewer than two windows embedded (nothing to cluster) —
/// callers degrade to the mouth-only analysis.
pub fn build_lane(
    embs: &[Vec<f32>],
    kept: &[(f64, f64)],
    analysis: &SpeakerAnalysis,
    cuts: &[f64],
    duration_s: f64,
    attribution_regime: bool,
    angle_override: Option<&[usize]>,
) -> Option<(VoiceLane, VoiceDiag)> {
    let n_bins = analysis.speaking.len();
    let bin_s = analysis.bin_s;
    if embs.len() < 2 || n_bins == 0 || bin_s <= 0.0 {
        return None;
    }
    let mut lines: Vec<String> = Vec::new();
    // Genuine mouth attribution per bin (not an off-screen hold) — the
    // join's and the scorer's reference lane.
    let genuine: Vec<Option<usize>> = (0..n_bins)
        .map(|b| {
            let id = analysis.speaking[b]?;
            let act = analysis
                .tracks
                .iter()
                .map(|t| t.activity.get(b).copied().unwrap_or(0.0))
                .fold(0.0f32, f32::max);
            (analysis.voiced[b] && act >= speaker::MIN_ACTIVITY).then_some(id)
        })
        .collect();
    // Per voiced bin: the nearest covering window's cluster.
    let bin_clusters = |assignment: &[usize]| -> Vec<Option<usize>> {
        (0..n_bins)
            .map(|b| {
                if !analysis.voiced.get(b).copied().unwrap_or(false) {
                    return None;
                }
                let t = (b as f64 + 0.5) * bin_s;
                let mut best: Option<(f64, usize)> = None;
                for (i, &(s, e)) in kept.iter().enumerate() {
                    if t >= s && t < e {
                        let d = (t - (s + e) * 0.5).abs();
                        if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                            best = Some((d, assignment[i]));
                        }
                    }
                }
                best.map(|(_, c)| c)
            })
            .collect()
    };
    // Join clusters to seats by co-occurrence on genuine-mouth bins (several
    // clusters may join one seat — an over-split voice is harmless, the join
    // reunifies it; an impure cluster joins nothing), restricted to bins the
    // `keep` filter admits so the join can be cross-validated.
    let join_on = |lane: &[Option<usize>], k: usize, keep: &dyn Fn(usize) -> bool| -> Vec<Option<usize>> {
        let mut counts = vec![std::collections::HashMap::<usize, usize>::new(); k];
        let mut totals = vec![0usize; k];
        for b in 0..n_bins {
            if !keep(b) {
                continue;
            }
            let (Some(c), Some(s)) = (lane[b], genuine[b]) else { continue };
            *counts[c].entry(s).or_default() += 1;
            totals[c] += 1;
        }
        (0..k)
            .map(|c| {
                let (&s, &n) = counts[c].iter().max_by_key(|(_, &n)| n)?;
                (totals[c] >= JOIN_MIN_BINS && n as f32 >= JOIN_MIN_SHARE * totals[c] as f32)
                    .then_some(s)
            })
            .collect()
    };
    let seat_lane = |lane: &[Option<usize>], joined: &[Option<usize>]| -> Vec<Option<usize>> {
        (0..n_bins).map(|b| lane[b].and_then(|c| joined[c])).collect()
    };
    // Agreement with the mouth lane where both claim; coverage = claimed time.
    let score = |seat: &[Option<usize>]| -> (usize, usize, usize) {
        let (mut both, mut agree, mut cov) = (0usize, 0usize, 0usize);
        for b in 0..n_bins {
            if seat[b].is_some() {
                cov += 1;
            }
            let (Some(v), Some(m)) = (seat[b], genuine[b]) else { continue };
            both += 1;
            if v == m {
                agree += 1;
            }
        }
        (cov, agree, both)
    };

    // Threshold sweep, scored OUT-OF-SAMPLE: the join is computed on
    // alternating 2 s blocks and the seat lane scored on the complementary
    // blocks (both directions). In-sample scoring is circular — with tiny
    // clusters every join copies the mouth lane on its own bins and "agrees"
    // 100% while carrying no identity (measured on the Deddy fixture: thr
    // 0.30 in-sample looked perfect and was pure overfit). A real voice
    // cluster joins the same seat from either half; an overfit singleton
    // claims nothing out-of-sample.
    let block_bins = ((2.0 / bin_s).round() as usize).max(1);
    let block = |b: usize| (b / block_bins) % 2 == 0;
    lines.push(format!(
        "{} windows | thr:  k joined  in-cov in-agr | cv-cov cv-agr  score",
        embs.len()
    ));
    let mut pick: Option<(f32, f64)> = None;
    for &t in &SWEEP_THRESHOLDS {
        let cl = cluster_cosine(embs, t);
        let lane = bin_clusters(&cl.assignment);
        let joined = join_on(&lane, cl.k, &|_| true);
        let seat = seat_lane(&lane, &joined);
        let (cov, agree, both) = score(&seat);
        // Cross-validated: even-block join claims odd blocks and vice versa.
        let join_even = join_on(&lane, cl.k, &|b| block(b));
        let join_odd = join_on(&lane, cl.k, &|b| !block(b));
        let seat_cv: Vec<Option<usize>> = (0..n_bins)
            .map(|b| {
                let j = if block(b) { &join_odd } else { &join_even };
                lane[b].and_then(|c| j[c])
            })
            .collect();
        let (cv_cov, cv_agree, cv_both) = score(&seat_cv);
        let cv_frac = cv_agree as f64 / cv_both.max(1) as f64;
        let s = cv_frac * cv_cov as f64 * bin_s;
        lines.push(format!(
            "    {t:.2}: {:>2} {:>6}  {:>5.1}s  {:>4.0}% | {:>5.1}s  {:>4.0}%  {s:>5.1}",
            cl.k,
            joined.iter().flatten().count(),
            cov as f64 * bin_s,
            100.0 * agree as f64 / both.max(1) as f64,
            cv_cov as f64 * bin_s,
            100.0 * cv_frac
        ));
        if pick.map(|(_, ps)| s > ps).unwrap_or(true) {
            pick = Some((t, s));
        }
    }
    let thr = pick.map(|(t, _)| t).unwrap_or(0.45);
    lines.push(format!("picked thr {thr:.2} (best cv score)"));
    let cl = cluster_cosine(embs, thr);
    let cluster = bin_clusters(&cl.assignment);
    let joined = join_on(&cluster, cl.k, &|_| true);

    // Detail rows for the picked threshold.
    let mut counts = vec![std::collections::HashMap::<usize, usize>::new(); cl.k];
    let mut totals = vec![0usize; cl.k];
    for b in 0..n_bins {
        let (Some(c), Some(s)) = (cluster[b], genuine[b]) else { continue };
        *counts[c].entry(s).or_default() += 1;
        totals[c] += 1;
    }
    lines.push("cluster <-> seat co-occurrence (genuine-mouth bins):".into());
    for c in 0..cl.k {
        let n_windows = cl.assignment.iter().filter(|&&a| a == c).count();
        let voiced_s = cluster.iter().filter(|&&v| v == Some(c)).count() as f64 * bin_s;
        if voiced_s < 0.75 && joined[c].is_none() {
            continue; // singleton noise — not worth a row
        }
        let mut row: Vec<(usize, usize)> = counts[c].iter().map(|(&s, &n)| (s, n)).collect();
        row.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        let desc: Vec<String> = row
            .iter()
            .map(|(s, n)| format!("{} {:.1}s", speaker::track_label(*s), *n as f64 * bin_s))
            .collect();
        let verdict = match joined[c] {
            Some(s) => format!("-> {}", speaker::track_label(s)),
            None if totals[c] == 0 => "-> OFF-SCREEN? (never co-occurs with a moving mouth)".into(),
            None => "-> unjoined (impure or shared, e.g. laughter)".into(),
        };
        // Overlap forensic: how often BOTH mouths move during this cluster's
        // bins — a shared class (laughter, cross-talk) shows both mouths at
        // once, which no voice embedding can attribute to one person.
        let (mut vis, mut multi) = (0usize, 0usize);
        for b in 0..n_bins {
            if cluster[b] != Some(c) {
                continue;
            }
            vis += 1;
            let moving = analysis
                .tracks
                .iter()
                .filter(|t| t.activity.get(b).copied().unwrap_or(0.0) >= speaker::MIN_ACTIVITY)
                .count();
            if moving >= 2 {
                multi += 1;
            }
        }
        lines.push(format!(
            "  V{c}: {n_windows} windows, {voiced_s:.1}s of voiced bins [{}] both-mouths {:.0}% {verdict}",
            desc.join(", "),
            100.0 * multi as f64 / vis.max(1) as f64
        ));
    }
    // ANGLE-AWARE JOIN (measured necessity on the Deddy fixture): the same
    // screen seat holds DIFFERENT humans in different camera angles — the
    // source cuts between two-person angles of a 4+-person table, and the
    // same-seat merge welds a position's framings into one track (ADR 0038:
    // labels are seats, not identities). A voice cluster therefore joins a
    // seat PER inter-cut segment; the whole-clip join is only the fallback
    // where a segment lacks evidence. The cluster itself is the person; the
    // per-segment map says which seat that person occupies in the angle on
    // screen (no seat = off-screen there).
    let seg_bounds = segment_bounds(cuts, duration_s);
    let n_segs = seg_bounds.len() - 1;
    let seg_of = |b: usize| -> usize {
        let t = (b as f64 + 0.5) * bin_s;
        seg_bounds.windows(2).position(|w| t >= w[0] && t < w[1]).unwrap_or(n_segs - 1)
    };
    // Group segments into ANGLES by seat geometry: jump cuts return to the
    // same camera over and over, and within one camera each seat's face sits
    // at the same position/size. Joining per (cluster, angle) accumulates
    // identity evidence across ALL of an angle's segments — so a claim at
    // one moment rests on other moments of the same camera, not only on the
    // mouth lane's opinion of the moment being judged (a purely per-segment
    // join just echoed the mouth lane: 99% "agreement" with no information).
    let seg_angle: Vec<usize> = if let Some(map) = angle_override.filter(|m| m.len() == n_segs) {
        map.to_vec()
    } else {
        let mut sigs: Vec<String> = Vec::new();
        let mut ids: Vec<usize> = Vec::new();
        for g in 0..n_segs {
            let (b0, b1) = (
                (seg_bounds[g] / bin_s).round() as usize,
                ((seg_bounds[g + 1] / bin_s).round() as usize).min(n_bins),
            );
            let len = b1.saturating_sub(b0).max(1);
            let mut sig = String::new();
            for t in &analysis.tracks {
                let mut xs: Vec<f32> = Vec::new();
                let mut ys: Vec<f32> = Vec::new();
                let mut hs: Vec<f32> = Vec::new();
                for b in b0..b1 {
                    if let Some(f) = t.path.get(b).and_then(|p| p.as_ref()) {
                        xs.push(f.cx());
                        ys.push(f.cy());
                        hs.push(f.h);
                    }
                }
                if xs.len() * 5 < len * 2 {
                    continue; // seat absent from this camera (<40%)
                }
                let med = |v: &mut Vec<f32>| -> f32 {
                    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    v[v.len() / 2]
                };
                sig.push_str(&format!(
                    "{}:{},{},{};",
                    t.id,
                    (med(&mut xs) / 60.0).round() as i32,
                    (med(&mut ys) / 60.0).round() as i32,
                    (med(&mut hs) / 40.0).round() as i32
                ));
            }
            let id = sigs.iter().position(|s| *s == sig).unwrap_or_else(|| {
                sigs.push(sig.clone());
                sigs.len() - 1
            });
            ids.push(id);
        }
        ids
    };
    let n_angles = seg_angle.iter().copied().max().map(|m| m + 1).unwrap_or(1);
    let mut ang_counts = vec![std::collections::HashMap::<(usize, usize), usize>::new(); n_angles];
    let mut ang_totals = vec![std::collections::HashMap::<usize, usize>::new(); n_angles];
    for b in 0..n_bins {
        let (Some(c), Some(s)) = (cluster[b], genuine[b]) else { continue };
        let a = seg_angle[seg_of(b)];
        *ang_counts[a].entry((c, s)).or_default() += 1;
        *ang_totals[a].entry(c).or_default() += 1;
    }
    // A single-segment angle's co-occurrence is pure echo of the mouth lane
    // over that one stretch (it can never disagree with it, so it carries no
    // identity information) — only an angle seen 2+ times may override the
    // whole-clip join. On the Deddy fixture this is what lets the voice keep
    // saying "seat B" at 20.8s where the mouth lane held A for that entire
    // one-off segment (the strip shows B exclaiming).
    let ang_segments: Vec<usize> =
        (0..n_angles).map(|a| seg_angle.iter().filter(|&&x| x == a).count()).collect();
    let ang_join = |c: usize, a: usize| -> Option<usize> {
        if ang_segments[a] < 2 {
            return None;
        }
        let total = *ang_totals[a].get(&c)?;
        let (&(_, s), &n) = ang_counts[a]
            .iter()
            .filter(|((cc, _), _)| *cc == c)
            .max_by_key(|&(_, &n)| n)?;
        (total >= JOIN_MIN_BINS_SEG && n as f32 >= JOIN_MIN_SHARE * total as f32).then_some(s)
    };
    lines.push("angles (segments grouped by seat geometry) + voice->seat per angle:".into());
    for a in 0..n_angles {
        let spans: Vec<String> = (0..n_segs)
            .filter(|&g| seg_angle[g] == a)
            .map(|g| format!("{:.1}-{:.1}", seg_bounds[g], seg_bounds[g + 1]))
            .collect();
        let items: Vec<String> = (0..cl.k)
            .filter_map(|c| ang_join(c, a).map(|s| format!("V{c}->{}", speaker::track_label(s))))
            .collect();
        lines.push(format!(
            "  angle {a}: [{}]  {}",
            spans.join(" "),
            if items.is_empty() { "(no joined voice)".into() } else { items.join("  ") }
        ));
    }
    // In the ATTRIBUTION regime with several camera angles, a seat track is a
    // SCREEN POSITION shared by different humans across angles (proven on the
    // Deddy fixture: V1's voice articulates as the left man of one angle and
    // is off-screen in another, where the left seat is a different person) —
    // so a whole-clip join must NOT leak across angles there. In the
    // follow-visible regime each track is one person's framing, so the
    // whole-clip join is the identity and stays.
    let ban_global = attribution_regime && n_angles > 1;
    let seat: Vec<Option<usize>> = (0..n_bins)
        .map(|b| {
            let c = cluster[b]?;
            let a = seg_angle[seg_of(b)];
            ang_join(c, a).or(if ban_global { None } else { joined[c] })
        })
        .collect();
    // Off-screen suspects: the voice is a KNOWN person (joined in some other
    // angle, or clip-wide) but holds no seat in the angle on screen — the
    // speaker the camera cannot show. The mouth lane can only mis-attribute
    // these (it holds a visible mouth); they are the "off-screen voice" gap
    // diarization exists to fill (ADR 0038).
    let mut offscreen = vec![false; n_bins];
    if ban_global {
        let known_elsewhere = |c: usize| -> bool {
            joined[c].is_some() || (0..n_angles).any(|a| ang_join(c, a).is_some())
        };
        for b in 0..n_bins {
            offscreen[b] =
                cluster[b].filter(|&c| seat[b].is_none() && known_elsewhere(c)).is_some();
        }
        lines.push("off-screen suspects (known voice, no seat in the on-screen angle):".into());
        let mut b = 0usize;
        while b < n_bins {
            if !offscreen[b] {
                b += 1;
                continue;
            }
            let (s0, c0) = (b, cluster[b].unwrap());
            while b < n_bins && cluster[b] == Some(c0) && seat[b].is_none() {
                b += 1;
            }
            let dur_run = (b - s0) as f64 * bin_s;
            if dur_run >= 0.5 {
                lines.push(format!(
                    "  {:>5.1}s..{:>5.1}s ({dur_run:.1}s): V{c0} speaks (mouth lane says {})",
                    s0 as f64 * bin_s,
                    b as f64 * bin_s,
                    analysis.speaking[s0.min(n_bins - 1)]
                        .map(speaker::track_label)
                        .unwrap_or_else(|| "nobody".into())
                ));
            }
        }
    }
    let (cov, agree, both) = score(&seat);
    lines.push(format!(
        "agreement with mouth attribution: {:.0}% over {:.1}s co-claimed ({:.1}s claimed total)",
        100.0 * agree as f64 / both.max(1) as f64,
        both as f64 * bin_s,
        cov as f64 * bin_s
    ));
    let mut b = 0usize;
    let mut printed = 0usize;
    while b < n_bins {
        let (Some(v), Some(m)) = (seat[b], genuine[b]) else {
            b += 1;
            continue;
        };
        if v == m {
            b += 1;
            continue;
        }
        let s0 = b;
        while b < n_bins && seat[b] == Some(v) && genuine[b] == Some(m) {
            b += 1;
        }
        let dur = (b - s0) as f64 * bin_s;
        if dur >= 0.4 && printed < 14 {
            lines.push(format!(
                "  DISAGREE {:>5.1}s..{:>5.1}s ({dur:.1}s): mouth={} voice={}",
                s0 as f64 * bin_s,
                b as f64 * bin_s,
                speaker::track_label(m),
                speaker::track_label(v)
            ));
            printed += 1;
        }
    }
    let mut sw = String::from("voice switches:");
    let mut last: Option<usize> = None;
    let mut n_sw = 0usize;
    for b in 0..n_bins {
        if let Some(v) = seat[b] {
            if last != Some(v) {
                sw.push_str(&format!(" {:.1}s->{}", b as f64 * bin_s, speaker::track_label(v)));
                last = Some(v);
                n_sw += 1;
                if n_sw > 24 {
                    sw.push_str(" ...");
                    break;
                }
            }
        }
    }
    lines.push(sw);
    let windows: Vec<(f64, f64, usize, i64)> = kept
        .iter()
        .enumerate()
        .map(|(i, &(s, e))| {
            let c = cl.assignment[i];
            (s, e, c, joined[c].map(|v| v as i64).unwrap_or(-1))
        })
        .collect();
    let diag = VoiceDiag {
        lines,
        windows,
        picked_thr: thr,
        claimed_s: cov as f64 * bin_s,
        agreement: agree as f64 / both.max(1) as f64,
        offscreen_s: offscreen.iter().filter(|o| **o).count() as f64 * bin_s,
    };
    let lane = VoiceLane { cluster, seat, offscreen, overridden: vec![false; n_bins] };
    Some((lane, diag))
}

/// The ADR 0042 fusion rule: the mouth lane stands wherever it can defend
/// its bin by its own switch margin; a JOINED voice seat that the mouth
/// cannot refute by that margin takes the bin instead, and a switch of the
/// fused lane still needs the same confirmation hold. The voice never
/// replaces the visual join — it tiebreaks it; bins where the voice claims
/// nothing (shared/unjoined clusters, silence) follow the mouth lane
/// unchanged. Returns `(speaking, confidence, overridden)` — the fused
/// attribution plus the bins where it differs from the mouth lane (the
/// planner's interjection-rescue evidence).
pub fn fuse_attribution(
    analysis: &SpeakerAnalysis,
    voice_seat: &[Option<usize>],
) -> (Vec<Option<usize>>, Vec<f32>, Vec<bool>) {
    let n = analysis.speaking.len();
    let margin = speaker::SWITCH_MARGIN;
    let confirm = (speaker::SWITCH_CONFIRM_S * speaker::SPEAKER_FPS).round() as usize;
    let act = |id: usize, b: usize| -> f32 {
        analysis
            .tracks
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.activity.get(b).copied())
            .unwrap_or(0.0)
    };
    // The mouth lane's own commitments pass through untouched — they already
    // went through the production margin + hold (an early draft re-held them
    // and VAD-gap resets pushed the mouth's legitimate 50.9s switch on the
    // Deddy fixture out to 69.2s). Only OVERRIDES hold: an override run
    // counts claimed bins (a breath does not reset it — voice windows
    // straddle breaths by construction) and commits RETROACTIVELY to its
    // start once it has lasted the confirm time. The hold exists to stop
    // flicker, not to shorten the interjection it rescues; the analysis is
    // offline, so back-filling is legitimate.
    let mut out: Vec<Option<usize>> = analysis.speaking.clone();
    let mut conf = analysis.confidence.clone();
    let mut challenger: Option<usize> = None;
    let mut run: Vec<usize> = Vec::new();
    for b in 0..n {
        if !analysis.voiced[b] {
            continue; // a breath neither advances nor resets an override run
        }
        let over = match (analysis.speaking[b], voice_seat[b]) {
            (Some(m), Some(v))
                if m != v && act(m, b) < margin * act(v, b).max(speaker::MIN_ACTIVITY) =>
            {
                Some(v) // the mouth cannot refute the voice by its own margin
            }
            _ => None,
        };
        match (over, challenger) {
            (Some(v), Some(c)) if v == c => {
                run.push(b);
                if run.len() >= confirm {
                    for &rb in &run {
                        out[rb] = Some(v);
                    }
                }
            }
            (Some(v), _) => {
                challenger = Some(v);
                run = vec![b];
            }
            (None, _) => {
                challenger = None;
                run.clear();
            }
        }
    }
    for b in 0..n {
        if out[b] != analysis.speaking[b] {
            if let Some(id) = out[b] {
                let total: f32 = analysis
                    .tracks
                    .iter()
                    .map(|t| t.activity.get(b).copied().unwrap_or(0.0))
                    .sum();
                conf[b] = if total > 0.0 { (act(id, b) / total).max(0.5) } else { 0.5 };
            }
        }
    }
    let overridden: Vec<bool> = (0..n).map(|b| out[b] != analysis.speaking[b]).collect();
    (out, conf, overridden)
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

/// Embed every voiced window of a clip: [`plan_windows`] over the VAD, skip
/// sub-0.25 s scraps (below the model's identification floor), return the
/// embeddings with the spans they cover — [`build_lane`]'s input. The caller
/// picks the scale/CMN convention: the production CAM++ model runs
/// [`SampleScale::Unit`] + CMN (its reference convention, selftest-validated);
/// the diag harness also A/Bs candidates with theirs.
#[cfg(feature = "voice")]
pub fn embed_windows(
    model: &std::path::Path,
    scale: SampleScale,
    apply_cmn: bool,
    samples: &[f32],
    voiced: &[bool],
    bin_s: f64,
) -> Result<(Vec<Vec<f32>>, Vec<(f64, f64)>)> {
    let mut embedder = VoiceEmbedder::load(model, scale, apply_cmn)?;
    let sr = VOICE_SR as f64;
    let mut embs: Vec<Vec<f32>> = Vec::new();
    let mut kept: Vec<(f64, f64)> = Vec::new();
    for (s, e) in plan_windows(voiced, bin_s) {
        let (i0, i1) = ((s * sr).round() as usize, ((e * sr).round() as usize).min(samples.len()));
        if i1 <= i0 || (i1 - i0) as f64 / sr < 0.25 {
            continue;
        }
        embs.push(embedder.embed(&samples[i0..i1])?);
        kept.push((s, e));
    }
    Ok((embs, kept))
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

    use crate::speaker::{SpeakerAnalysis, SpeakerTrack, SPEAKER_FPS};
    use crate::FaceBox;

    const BIN_S: f64 = 1.0 / SPEAKER_FPS;

    fn nb(s: f64) -> usize {
        (s / BIN_S).round() as usize
    }

    /// A track whose face sits at `(cx, cy, h)` over each `(b0, b1)` span and
    /// is absent everywhere else — the seat-geometry fixtures for the
    /// angle-scoped join.
    fn track_at(id: usize, n: usize, spans: &[(usize, usize, f32, f32, f32)]) -> SpeakerTrack {
        let mut path: Vec<Option<FaceBox>> = vec![None; n];
        for &(b0, b1, cx, cy, h) in spans {
            for b in b0..b1.min(n) {
                path[b] =
                    Some(FaceBox { x: cx - h * 0.5, y: cy - h * 0.5, w: h, h, score: 0.9 });
            }
        }
        let bbox = path
            .iter()
            .flatten()
            .next()
            .copied()
            .unwrap_or(FaceBox { x: 0.0, y: 0.0, w: 1.0, h: 1.0, score: 0.9 });
        SpeakerTrack { id, bbox, presence: 1.0, activity: vec![0.0; n], path }
    }

    /// Contiguous 1 s embed windows over `secs` with identical embeddings —
    /// one voice, every bin covered, stable at every sweep threshold.
    fn one_voice_windows(secs: f64) -> (Vec<Vec<f32>>, Vec<(f64, f64)>) {
        let n = secs.ceil() as usize;
        let embs = vec![vec![1.0f32]; n];
        let kept = (0..n).map(|i| (i as f64, i as f64 + 1.0)).collect();
        (embs, kept)
    }

    #[test]
    fn fusion_overrides_retroactively_after_the_hold() {
        // The mouth says A throughout with activity at the attribution floor;
        // the voice says B for 2 s. The mouth cannot refute B by its margin,
        // so after the confirm hold the override commits RETROACTIVELY to the
        // run's start — the interjection it rescues keeps its full length —
        // and a breath inside the run neither advances nor resets it.
        let n = nb(10.0);
        let (v0, v1) = (nb(2.0), nb(4.0));
        let mut voiced = vec![true; n];
        for b in nb(2.5)..nb(2.7) {
            voiced[b] = false; // the breath
        }
        let mut a = SpeakerAnalysis {
            bin_s: BIN_S,
            tracks: vec![
                track_at(0, n, &[(0, n, 300.0, 325.0, 150.0)]),
                track_at(1, n, &[(0, n, 1500.0, 325.0, 150.0)]),
            ],
            voiced,
            speaking: vec![Some(0); n],
            confidence: vec![1.0; n],
            voice: None,
        };
        a.tracks[0].activity = vec![0.004; n]; // the floor: refutable
        a.tracks[1].activity = vec![0.001; n];
        let mut seat: Vec<Option<usize>> = vec![None; n];
        for s in seat[v0..v1].iter_mut() {
            *s = Some(1);
        }
        // A too-short claim elsewhere must not override.
        for s in seat[nb(7.0)..nb(7.0) + 10].iter_mut() {
            *s = Some(1);
        }
        let (out, conf, overridden) = fuse_attribution(&a, &seat);
        assert_eq!(out[v0], Some(1), "retroactive commit to the run start");
        assert_eq!(out[v1 - 1], Some(1));
        assert_eq!(out[v0 - 1], Some(0), "nothing before the run changes");
        assert!(overridden[v0] && overridden[v1 - 1]);
        assert_eq!(out[nb(2.6)], Some(0), "a breath bin keeps the mouth lane");
        assert!(!overridden[nb(2.6)]);
        assert!(
            (nb(7.0)..nb(7.0) + 10).all(|b| out[b] == Some(0)),
            "a sub-hold claim never overrides"
        );
        assert!((conf[v0 + 2] - 0.5).abs() < 1e-6, "override confidence floors at 0.5");
    }

    #[test]
    fn fusion_respects_the_mouth_margin() {
        // The mouth's own speaker is CLEARLY moving (5x the challenger's
        // floor): the voice's disagreement must not take a single bin.
        let n = nb(6.0);
        let mut a = SpeakerAnalysis {
            bin_s: BIN_S,
            tracks: vec![
                track_at(0, n, &[(0, n, 300.0, 325.0, 150.0)]),
                track_at(1, n, &[(0, n, 1500.0, 325.0, 150.0)]),
            ],
            voiced: vec![true; n],
            speaking: vec![Some(0); n],
            confidence: vec![1.0; n],
            voice: None,
        };
        a.tracks[0].activity = vec![0.02; n];
        a.tracks[1].activity = vec![0.001; n];
        let seat: Vec<Option<usize>> = vec![Some(1); n];
        let (out, _, overridden) = fuse_attribution(&a, &seat);
        assert!(out.iter().all(|s| *s == Some(0)), "the mouth refutes by its margin");
        assert!(overridden.iter().all(|o| !o));
    }

    /// The Deddy-shaped fixture: 4 alternating 2 s segments, two angles by
    /// seat geometry. In angle 0 seat A speaks with genuine mouth motion; in
    /// angle 1 the same voice keeps talking but NO mouth moves (the speaker
    /// is off camera there).
    fn two_angle_fixture() -> (SpeakerAnalysis, Vec<f64>) {
        let n = nb(8.0);
        let a_spans = [
            (0, nb(2.0), 300.0, 325.0, 150.0),
            (nb(2.0), nb(4.0), 1200.0, 325.0, 150.0),
            (nb(4.0), nb(6.0), 300.0, 325.0, 150.0),
            (nb(6.0), n, 1200.0, 325.0, 150.0),
        ];
        let b_spans = [
            (0, nb(2.0), 900.0, 325.0, 150.0),
            (nb(4.0), nb(6.0), 900.0, 325.0, 150.0),
        ];
        let mut ta = track_at(0, n, &a_spans);
        let tb = track_at(1, n, &b_spans);
        let mut speaking: Vec<Option<usize>> = vec![None; n];
        for b in 0..n {
            let angle0 = (0..nb(2.0)).contains(&b) || (nb(4.0)..nb(6.0)).contains(&b);
            if angle0 {
                ta.activity[b] = 0.05; // genuine mouth motion, seat A
                speaking[b] = Some(0);
            }
        }
        let analysis = SpeakerAnalysis {
            bin_s: BIN_S,
            tracks: vec![ta, tb],
            voiced: vec![true; n],
            speaking,
            confidence: vec![1.0; n],
            voice: None,
        };
        (analysis, vec![2.0, 4.0, 6.0])
    }

    #[test]
    fn the_join_is_angle_scoped_and_flags_the_offscreen_voice() {
        // Attribution regime, two angles: the voice joins seat A inside
        // angle 0 (seen twice, solid co-occurrence) and must NOT leak that
        // seat into angle 1 (the whole-clip join is banned there) — where it
        // keeps speaking with no seat, it is the off-screen speaker.
        let (analysis, cuts) = two_angle_fixture();
        let (embs, kept) = one_voice_windows(8.0);
        let (lane, diag) =
            build_lane(&embs, &kept, &analysis, &cuts, 8.0, true, None).expect("lane builds");
        assert_eq!(lane.seat[nb(1.0)], Some(0), "angle 0 joins seat A");
        assert_eq!(lane.seat[nb(5.0)], Some(0));
        assert_eq!(lane.seat[nb(3.0)], None, "the join must not leak across angles");
        assert!(lane.offscreen[nb(3.0)], "a known voice with no seat = off-screen");
        assert!(lane.offscreen[nb(7.0)]);
        assert!(!lane.offscreen[nb(1.0)]);
        assert!(lane.overridden.iter().all(|o| !o), "build_lane never marks overrides");
        assert!(diag.offscreen_s > 3.0, "both angle-1 visits flagged: {}", diag.offscreen_s);
        assert!(diag.lines.iter().any(|l| l.contains("off-screen suspects")));
    }

    #[test]
    fn follow_visible_keeps_the_whole_clip_join() {
        // Same evidence, follow-visible regime: each track is one person's
        // framing, so the whole-clip join IS the identity — the voice claims
        // its seat everywhere it speaks and nothing is off-screen.
        let (analysis, cuts) = two_angle_fixture();
        let (embs, kept) = one_voice_windows(8.0);
        let (lane, _) =
            build_lane(&embs, &kept, &analysis, &cuts, 8.0, false, None).expect("lane builds");
        assert_eq!(lane.seat[nb(3.0)], Some(0), "whole-clip join stands in follow-visible");
        assert!(lane.offscreen.iter().all(|o| !o));
    }

    #[test]
    fn a_single_visit_angle_cannot_claim() {
        // A-B-A segments: the middle angle is seen ONCE, so its co-occurrence
        // is a pure echo of the mouth lane (it can never disagree with it) —
        // the voice may not claim a seat there, even though the mouth says B
        // with conviction. It stays an off-screen SUSPECT for the operator's
        // ear, exactly the weaker 27.0-30.2 s Deddy flag.
        let n = nb(6.0);
        let ta = track_at(0, n, &[(0, nb(2.0), 300.0, 325.0, 150.0), (nb(4.0), n, 300.0, 325.0, 150.0)]);
        let tb = track_at(1, n, &[(nb(2.0), nb(4.0), 900.0, 325.0, 150.0)]);
        let mut analysis = SpeakerAnalysis {
            bin_s: BIN_S,
            tracks: vec![ta, tb],
            voiced: vec![true; n],
            speaking: (0..n)
                .map(|b| Some(if (nb(2.0)..nb(4.0)).contains(&b) { 1 } else { 0 }))
                .collect(),
            confidence: vec![1.0; n],
            voice: None,
        };
        for b in 0..n {
            if (nb(2.0)..nb(4.0)).contains(&b) {
                analysis.tracks[1].activity[b] = 0.05;
            } else {
                analysis.tracks[0].activity[b] = 0.05;
            }
        }
        let (embs, kept) = one_voice_windows(6.0);
        let (lane, _) =
            build_lane(&embs, &kept, &analysis, &[2.0, 4.0], 6.0, true, None).expect("lane builds");
        assert_eq!(lane.seat[nb(1.0)], Some(0), "the twice-seen angle joins");
        assert_eq!(lane.seat[nb(3.0)], None, "a single-visit angle only echoes — no claim");
        assert!(lane.offscreen[nb(3.0)], "it stays a suspect for the operator's ear");
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
