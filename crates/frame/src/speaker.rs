//! Podcast speaker detection + the active-speaker camera plan (focus 2026-07).
//!
//! A podcast VOD is a static wide shot with 2-4 visible people; the Short needs
//! the camera on whoever is talking. This module is the **pure** half of that:
//!
//! 1. **Tracks** — per-frame face detections (Ultraface, sampled at
//!    [`SPEAKER_FPS`]) are matched into persistent tracks by screen position
//!    (podcast cameras are static, so a track's box barely moves).
//! 2. **Activity** — per track, per frame: mean absolute luma change inside the
//!    mouth region of the face box, sampled on a fixed grid so the measure is
//!    resolution-independent. A talking mouth moves; a listening one doesn't.
//! 3. **VAD** — an RMS gate over the clip audio on the same grid: nobody is
//!    "speaking" through silence.
//! 4. **Attribution** — voiced bin by voiced bin, the speaker is the track with
//!    the most (smoothed) mouth activity, with a switch margin + confirmation
//!    hold so a random lip twitch never steals the camera.
//! 5. **Shot plan** — the speaker timeline becomes a *cut-based*
//!    [`CameraPlan`]: minimum shot length, rapid exchanges collapse into a
//!    group shot (2 people: a stacked split screen), silence holds the current
//!    shot. Human podcast editors cut; they do not pan a virtual camera across
//!    a static wide shot.
//!
//! The Ultraface inference and the ffmpeg frame streaming live in the app's
//! pipeline (they need `ort` + sidecars); everything here is unit-tested pure
//! logic, mirroring how `lib.rs` splits framing geometry from `infer`.

use crate::FaceBox;
use yc_core::{CameraPlan, Crop, Layout, Shot, CANVAS_H, CANVAS_W};

/// Analysis grid rate: tracking frames per second, and the width of every
/// per-bin series (activity, VAD, speaker). 5 fps resolves conversational
/// turn-taking (~200 ms) while keeping a 3-minute clip under 900 frames.
pub const SPEAKER_FPS: f64 = 5.0;

/// Fixed mouth-patch sample grid (w, h): activity is measured on this many
/// luma samples regardless of face size, so a near face and a far face score
/// on the same scale.
const PATCH_W: usize = 16;
const PATCH_H: usize = 10;

/// The mouth region within a face box: the lower band (nose-to-chin), central
/// width — eyes/brows move when listening, mouths move when talking.
const MOUTH_Y_FRAC: f32 = 0.55;
const MOUTH_H_FRAC: f32 = 0.45;
const MOUTH_X_FRAC: f32 = 0.18;
const MOUTH_W_FRAC: f32 = 0.64;

/// Track-match radius as a fraction of the frame diagonal (same constant family
/// as `cluster_static_faces`' merge distance).
const TRACK_MATCH_FRAC: f32 = 0.06;
/// A track must appear in at least this fraction of frames to survive (a
/// glancing background face or a false positive doesn't).
const MIN_TRACK_PRESENCE: f32 = 0.20;
/// At most this many speakers are tracked (podcast panels beyond 4 are rare,
/// and a 9:16 canvas can't meaningfully frame more).
pub const MAX_TRACKS: usize = 4;

/// Activity smoothing window, seconds (moving average over the grid).
const ACTIVITY_SMOOTH_S: f64 = 0.6;
/// A challenger must out-move the incumbent by this factor to take the camera.
const SWITCH_MARGIN: f32 = 1.35;
/// ...and hold that lead for this long before the switch commits.
const SWITCH_CONFIRM_S: f64 = 0.8;
/// Mouth activity below this is "nobody visibly talking" — attribution then
/// holds the incumbent rather than guessing (voice-over, off-screen speech).
const MIN_ACTIVITY: f32 = 0.004;

/// VAD: a bin is voiced when its RMS clears both an absolute floor (true
/// silence) and a fraction of the clip's loud (p90) reference.
const VAD_ABS_FLOOR: f32 = 0.004;
const VAD_REL_FRAC: f32 = 0.22;

/// Minimum shot length, seconds — cutting faster than this reads as jumpy.
const MIN_SHOT_S: f64 = 2.4;
/// Consecutive shots shorter than this each mark a rapid exchange; a run of
/// them collapses into one group shot instead of a cut storm.
const RAPID_SHOT_S: f64 = 4.0;
const RAPID_RUN: usize = 3;
/// Solo framing: crop height as a multiple of the face-box height (the zoom),
/// and where the face center sits vertically in the crop (headroom bias).
const SOLO_ZOOM: f32 = 3.6;
const SOLO_FACE_Y_FRAC: f32 = 0.38;
/// Split-screen panel framing: a touch tighter than solo (each panel is half a
/// canvas tall).
const SPLIT_ZOOM: f32 = 3.0;

/// One tracked person: a (static) representative face box, how often they were
/// visible, and their per-bin mouth activity. `id` is the track's index after
/// the left-to-right relabel — 0 is "Person A".
#[derive(Debug, Clone)]
pub struct SpeakerTrack {
    pub id: usize,
    pub bbox: FaceBox,
    /// Fraction of frames this track's face was detected in.
    pub presence: f32,
    /// Smoothed mouth activity per grid bin (0 where the face was absent).
    pub activity: Vec<f32>,
}

/// Display label for a track: "Person A", "Person B", ...
pub fn track_label(id: usize) -> String {
    let letter = (b'A' + (id % 26) as u8) as char;
    format!("Person {letter}")
}

/// The full speaker analysis for one Clip, on the [`SPEAKER_FPS`] grid: the
/// tracks, the audio gate, and the attributed speaker per bin (with a 0..1
/// confidence — the winner's share of total mouth activity).
#[derive(Debug, Clone, Default)]
pub struct SpeakerAnalysis {
    pub bin_s: f64,
    pub tracks: Vec<SpeakerTrack>,
    pub voiced: Vec<bool>,
    pub speaking: Vec<Option<usize>>,
    pub confidence: Vec<f32>,
}

impl SpeakerAnalysis {
    /// The attributed speaker at clip-relative time `t`, if any.
    pub fn speaker_at(&self, t: f64) -> Option<usize> {
        if self.bin_s <= 0.0 {
            return None;
        }
        let i = (t / self.bin_s) as usize;
        self.speaking.get(i).copied().flatten()
    }

    /// The attribution confidence at time `t` (0 where nothing is attributed).
    pub fn confidence_at(&self, t: f64) -> f32 {
        if self.bin_s <= 0.0 {
            return 0.0;
        }
        let i = (t / self.bin_s) as usize;
        self.confidence.get(i).copied().unwrap_or(0.0)
    }
}

// --- track building over streamed frames -------------------------------------

struct Track {
    boxes: Vec<FaceBox>,
    sum_cx: f32,
    sum_cy: f32,
    /// Per-frame raw mouth motion (None where absent this frame).
    motion: Vec<Option<f32>>,
    prev_patch: Option<[f32; PATCH_W * PATCH_H]>,
    frames_seen: usize,
}

impl Track {
    fn mean_cx(&self) -> f32 {
        self.sum_cx / self.boxes.len() as f32
    }
    fn mean_cy(&self) -> f32 {
        self.sum_cy / self.boxes.len() as f32
    }
}

/// Accumulates per-frame detections + pixels into [`SpeakerTrack`]s. Feed every
/// tracking frame in order via [`Self::observe`], then [`Self::finish`].
///
/// `frame_w`/`frame_h` are the **tracking frame** dimensions (the pixels given
/// to `observe`); face boxes arrive in **source** pixels (as `Detector::detect`
/// returns them) with `src_w`/`src_h` the source dimensions — the builder maps
/// between the two.
pub struct TrackBuilder {
    src_w: f32,
    src_h: f32,
    frame_w: usize,
    frame_h: usize,
    match_dist: f32,
    tracks: Vec<Track>,
    n_frames: usize,
}

impl TrackBuilder {
    pub fn new(src_w: f32, src_h: f32, frame_w: usize, frame_h: usize) -> Self {
        Self {
            src_w,
            src_h,
            frame_w,
            frame_h,
            match_dist: TRACK_MATCH_FRAC * (src_w * src_w + src_h * src_h).sqrt(),
            tracks: Vec::new(),
            n_frames: 0,
        }
    }

    /// Ingest one tracking frame: its face detections (source pixels) and its
    /// rgb24 pixels (`frame_w * frame_h * 3`). Detections match to tracks by
    /// center distance; each matched track measures its mouth motion against
    /// its previous patch.
    pub fn observe(&mut self, faces: &[FaceBox], rgb: &[u8]) {
        let frame_idx = self.n_frames;
        self.n_frames += 1;
        for f in faces {
            let ti = self.match_track(f);
            let ti = match ti {
                Some(ti) => {
                    let t = &mut self.tracks[ti];
                    t.boxes.push(*f);
                    t.sum_cx += f.cx();
                    t.sum_cy += f.cy();
                    ti
                }
                None => {
                    self.tracks.push(Track {
                        boxes: vec![*f],
                        sum_cx: f.cx(),
                        sum_cy: f.cy(),
                        motion: Vec::new(),
                        prev_patch: None,
                        frames_seen: 0,
                    });
                    self.tracks.len() - 1
                }
            };
            let patch = self.sample_mouth_patch(f, rgb);
            let t = &mut self.tracks[ti];
            // Pad any frames this track missed with None, then record this one.
            while t.motion.len() < frame_idx {
                t.motion.push(None);
            }
            let motion = t
                .prev_patch
                .as_ref()
                .map(|prev| {
                    let sum: f32 =
                        patch.iter().zip(prev.iter()).map(|(a, b)| (a - b).abs()).sum();
                    sum / (PATCH_W * PATCH_H) as f32 / 255.0
                })
                .unwrap_or(0.0);
            t.motion.push(Some(motion));
            t.prev_patch = Some(patch);
            t.frames_seen += 1;
        }
    }

    fn match_track(&self, f: &FaceBox) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (i, t) in self.tracks.iter().enumerate() {
            let dx = f.cx() - t.mean_cx();
            let dy = f.cy() - t.mean_cy();
            let d = (dx * dx + dy * dy).sqrt();
            if d <= self.match_dist && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Sample the face's mouth region (source-pixel box mapped onto the
    /// tracking frame) as a fixed PATCH_W x PATCH_H luma grid.
    fn sample_mouth_patch(&self, f: &FaceBox, rgb: &[u8]) -> [f32; PATCH_W * PATCH_H] {
        let sx = self.frame_w as f32 / self.src_w.max(1.0);
        let sy = self.frame_h as f32 / self.src_h.max(1.0);
        let mx = (f.x + f.w * MOUTH_X_FRAC) * sx;
        let my = (f.y + f.h * MOUTH_Y_FRAC) * sy;
        let mw = (f.w * MOUTH_W_FRAC * sx).max(1.0);
        let mh = (f.h * MOUTH_H_FRAC * sy).max(1.0);
        let mut patch = [0f32; PATCH_W * PATCH_H];
        for py in 0..PATCH_H {
            for px in 0..PATCH_W {
                let x = (mx + (px as f32 + 0.5) / PATCH_W as f32 * mw) as usize;
                let y = (my + (py as f32 + 0.5) / PATCH_H as f32 * mh) as usize;
                let x = x.min(self.frame_w.saturating_sub(1));
                let y = y.min(self.frame_h.saturating_sub(1));
                let o = (y * self.frame_w + x) * 3;
                if o + 2 < rgb.len() {
                    // ITU-R BT.601 luma.
                    patch[py * PATCH_W + px] = 0.299 * rgb[o] as f32
                        + 0.587 * rgb[o + 1] as f32
                        + 0.114 * rgb[o + 2] as f32;
                }
            }
        }
        patch
    }

    /// Close the builder: keep persistent tracks, relabel left-to-right
    /// ("Person A" is the leftmost — stable reading order), smooth activity.
    pub fn finish(self) -> Vec<SpeakerTrack> {
        let n = self.n_frames.max(1);
        let win = (ACTIVITY_SMOOTH_S * SPEAKER_FPS).round().max(1.0) as usize;
        let mut kept: Vec<(f32, SpeakerTrack)> = Vec::new();
        for t in self.tracks {
            let presence = t.frames_seen as f32 / n as f32;
            if presence < MIN_TRACK_PRESENCE {
                continue;
            }
            let mut raw = vec![0f32; n];
            for (i, m) in t.motion.iter().enumerate() {
                if let Some(v) = *m {
                    raw[i] = v;
                }
            }
            kept.push((
                t.mean_cx(),
                SpeakerTrack {
                    id: 0, // assigned after the left-to-right sort
                    bbox: median_box(&t.boxes),
                    presence,
                    activity: smooth(&raw, win),
                },
            ));
        }
        // Most-present first for the cap, then left-to-right for labels.
        kept.sort_by(|a, b| {
            b.1.presence.partial_cmp(&a.1.presence).unwrap_or(std::cmp::Ordering::Equal)
        });
        kept.truncate(MAX_TRACKS);
        kept.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        kept.into_iter()
            .enumerate()
            .map(|(i, (_, mut t))| {
                t.id = i;
                t
            })
            .collect()
    }
}

fn median_box(boxes: &[FaceBox]) -> FaceBox {
    let med = |mut v: Vec<f32>| -> f32 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    };
    FaceBox {
        x: med(boxes.iter().map(|b| b.x).collect()),
        y: med(boxes.iter().map(|b| b.y).collect()),
        w: med(boxes.iter().map(|b| b.w).collect()),
        h: med(boxes.iter().map(|b| b.h).collect()),
        score: med(boxes.iter().map(|b| b.score).collect()),
    }
}

fn smooth(series: &[f32], win: usize) -> Vec<f32> {
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

// --- audio gate ---------------------------------------------------------------

/// Per-bin voice-activity gate over the clip's 16 kHz mono samples: RMS above
/// both an absolute floor and a fraction of the clip's loud (p90) reference.
pub fn voiced_bins(samples: &[f32], sr: u32, bin_s: f64, n_bins: usize) -> Vec<bool> {
    let mut rms = vec![0f32; n_bins];
    let per = ((sr as f64) * bin_s).max(1.0) as usize;
    for (i, r) in rms.iter_mut().enumerate() {
        let lo = i * per;
        if lo >= samples.len() {
            break;
        }
        let hi = ((i + 1) * per).min(samples.len());
        let w = &samples[lo..hi];
        *r = (w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt();
    }
    let mut sorted = rms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p90 = sorted[((n_bins as f64 * 0.9) as usize).min(n_bins.saturating_sub(1))];
    let thresh = (VAD_REL_FRAC * p90).max(VAD_ABS_FLOOR);
    rms.into_iter().map(|r| r > thresh).collect()
}

// --- attribution ----------------------------------------------------------------

/// Attribute a speaker to every voiced bin: the track with the most smoothed
/// mouth activity, with a switch margin ([`SWITCH_MARGIN`]) held for
/// [`SWITCH_CONFIRM_S`] before a change commits — a lip twitch never steals the
/// camera. Unvoiced bins attribute `None`. Returns `(speaking, confidence)`.
pub fn attribute_speakers(
    tracks: &[SpeakerTrack],
    voiced: &[bool],
) -> (Vec<Option<usize>>, Vec<f32>) {
    let n = voiced.len();
    let mut speaking: Vec<Option<usize>> = vec![None; n];
    let mut confidence = vec![0f32; n];
    if tracks.is_empty() {
        return (speaking, confidence);
    }
    let confirm = (SWITCH_CONFIRM_S * SPEAKER_FPS).round().max(1.0) as usize;
    let mut current: Option<usize> = None;
    let mut challenger: Option<usize> = None;
    let mut challenge_len = 0usize;
    for i in 0..n {
        if !voiced[i] {
            // Silence: nobody speaks; the shot planner decides what to hold.
            challenger = None;
            challenge_len = 0;
            continue;
        }
        let act = |t: &SpeakerTrack| t.activity.get(i).copied().unwrap_or(0.0);
        let total: f32 = tracks.iter().map(act).sum();
        let (best_id, best) = tracks
            .iter()
            .map(|t| (t.id, act(t)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .expect("non-empty tracks");
        if best < MIN_ACTIVITY {
            // Voice with no visible mouth movement (off-screen / voice-over):
            // hold the incumbent rather than guessing.
            speaking[i] = current;
            confidence[i] = if current.is_some() { 0.34 } else { 0.0 };
            continue;
        }
        let cur_act = current
            .and_then(|id| tracks.iter().find(|t| t.id == id))
            .map(act)
            .unwrap_or(0.0);
        let next = match current {
            None => Some(best_id), // first attribution: take the leader
            Some(cur) if cur == best_id => {
                challenger = None;
                challenge_len = 0;
                Some(cur)
            }
            Some(cur) => {
                // A different leader must clear the margin and hold it.
                if best > SWITCH_MARGIN * cur_act.max(MIN_ACTIVITY) {
                    if challenger == Some(best_id) {
                        challenge_len += 1;
                    } else {
                        challenger = Some(best_id);
                        challenge_len = 1;
                    }
                    if challenge_len >= confirm {
                        challenger = None;
                        challenge_len = 0;
                        Some(best_id)
                    } else {
                        Some(cur)
                    }
                } else {
                    challenger = None;
                    challenge_len = 0;
                    Some(cur)
                }
            }
        };
        current = next;
        speaking[i] = current;
        confidence[i] = if total > 0.0 {
            current
                .and_then(|id| tracks.iter().find(|t| t.id == id))
                .map(|t| act(t) / total)
                .unwrap_or(0.0)
        } else {
            0.0
        };
    }
    (speaking, confidence)
}

// --- shot planning ---------------------------------------------------------------

/// A shot's subject before framing: one track, or everyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subject {
    Track(usize),
    Group,
}

/// Turn the attributed speaker timeline into a **cut-based** [`CameraPlan`]:
/// silence holds the current shot, flickers are absorbed, every shot lasts at
/// least [`MIN_SHOT_S`], and a rapid exchange ([`RAPID_RUN`] consecutive short
/// shots) collapses into one group shot. Shots are contiguous over
/// `0..duration_s`.
pub fn plan_shots(
    analysis: &SpeakerAnalysis,
    src_w: f32,
    src_h: f32,
    duration_s: f64,
) -> CameraPlan {
    let n = analysis.speaking.len();
    let bin_s = if analysis.bin_s > 0.0 { analysis.bin_s } else { 1.0 / SPEAKER_FPS };
    if n == 0 || analysis.tracks.is_empty() {
        return CameraPlan {
            shots: vec![Shot {
                start_s: 0.0,
                end_s: duration_s,
                track: None,
                layout: group_layout(&analysis.tracks, src_w, src_h),
            }],
        };
    }

    // 1. Carry the last speaker through silence; lead-in silence gets the first
    //    speaker (the camera opens on whoever talks first).
    let mut series: Vec<Option<usize>> = analysis.speaking.clone();
    let first = series.iter().flatten().next().copied();
    let mut last: Option<usize> = first;
    for s in series.iter_mut() {
        match *s {
            Some(id) => last = Some(id),
            None => *s = last,
        }
    }
    let Some(_) = first else {
        // Nobody ever attributed: one group shot.
        return CameraPlan {
            shots: vec![Shot {
                start_s: 0.0,
                end_s: duration_s,
                track: None,
                layout: group_layout(&analysis.tracks, src_w, src_h),
            }],
        };
    };

    // 2. Raw runs of the (silence-filled) speaker series.
    let min_bins = (MIN_SHOT_S / bin_s).round().max(1.0) as usize;
    let mut runs: Vec<(Subject, usize)> = Vec::new(); // (subject, len_bins)
    for s in series.iter() {
        let subj = Subject::Track(s.expect("filled above"));
        match runs.last_mut() {
            Some((r, len)) if *r == subj => *len += 1,
            _ => runs.push((subj, 1)),
        }
    }

    // 3. Rapid exchanges FIRST (before short runs are absorbed away): a streak
    //    of RAPID_RUN+ consecutive short runs (each < RAPID_SHOT_S) becomes one
    //    group shot — show the back-and-forth, not a cut storm. Runs on the raw
    //    runs so the evidence (many short turns) is still visible.
    let rapid_bins = (RAPID_SHOT_S / bin_s).round().max(1.0) as usize;
    let mut grouped: Vec<(Subject, usize)> = Vec::new();
    let mut i = 0;
    while i < runs.len() {
        let mut j = i;
        while j < runs.len() && runs[j].1 < rapid_bins {
            j += 1;
        }
        if j - i >= RAPID_RUN && analysis.tracks.len() >= 2 {
            let total: usize = runs[i..j].iter().map(|(_, l)| l).sum();
            match grouped.last_mut() {
                Some((Subject::Group, plen)) => *plen += total,
                _ => grouped.push((Subject::Group, total)),
            }
            i = j;
        } else {
            let (subj, len) = runs[i];
            match grouped.last_mut() {
                Some((prev, plen)) if *prev == subj => *plen += len,
                _ => grouped.push((subj, len)),
            }
            i += 1;
        }
    }

    // 4. Absorb remaining too-short runs into their predecessor (a lone quick
    //    interjection isn't worth a cut), merging same-subject neighbours.
    let mut final_runs: Vec<(Subject, usize)> = Vec::new();
    for (subj, len) in grouped {
        match final_runs.last_mut() {
            Some((prev, plen)) if *prev == subj => *plen += len,
            Some((_, plen)) if len < min_bins => *plen += len,
            _ => final_runs.push((subj, len)),
        }
    }

    // 5. Frame each run. The last shot's end snaps to the clip end.
    let mut shots = Vec::with_capacity(final_runs.len());
    let mut t = 0.0f64;
    for (k, (subj, len)) in final_runs.iter().enumerate() {
        let start_s = t;
        let end_s = if k + 1 == final_runs.len() {
            duration_s
        } else {
            t + *len as f64 * bin_s
        };
        t = end_s;
        let (track, layout) = match subj {
            Subject::Track(id) => {
                let face =
                    analysis.tracks.iter().find(|tr| tr.id == *id).map(|tr| tr.bbox);
                match face {
                    Some(f) => (Some(*id), Layout::FullFrame { crop: solo_crop(&f, src_w, src_h) }),
                    None => (None, group_layout(&analysis.tracks, src_w, src_h)),
                }
            }
            Subject::Group => (None, group_layout(&analysis.tracks, src_w, src_h)),
        };
        shots.push(Shot { start_s, end_s, track, layout });
    }
    CameraPlan { shots }
}

/// The solo framing for one speaker: a 9:16 crop zoomed so the face reads big
/// ([`SOLO_ZOOM`] face-heights tall), face centered horizontally, biased up for
/// headroom — clamped inside the frame, falling back to a full-height column
/// when the source is too small to zoom.
pub fn solo_crop(face: &FaceBox, src_w: f32, src_h: f32) -> Crop {
    let aspect = CANVAS_W as f32 / CANVAS_H as f32;
    let mut h = (face.h * SOLO_ZOOM).min(src_h);
    let mut w = h * aspect;
    if w > src_w {
        w = src_w;
        h = w / aspect;
    }
    let x = (face.cx() - w * 0.5).clamp(0.0, (src_w - w).max(0.0));
    let y = (face.cy() - h * SOLO_FACE_Y_FRAC).clamp(0.0, (src_h - h).max(0.0));
    Crop { x, y, w, h }
}

/// A split-screen panel's crop for one face: the panel's aspect, zoomed to
/// [`SPLIT_ZOOM`] face-heights, headroom-biased, clamped.
fn panel_crop(face: &FaceBox, src_w: f32, src_h: f32, panel_aspect: f32) -> Crop {
    let mut h = (face.h * SPLIT_ZOOM).min(src_h);
    let mut w = h * panel_aspect;
    if w > src_w {
        w = src_w;
        h = w / panel_aspect;
    }
    let x = (face.cx() - w * 0.5).clamp(0.0, (src_w - w).max(0.0));
    let y = (face.cy() - h * SOLO_FACE_Y_FRAC).clamp(0.0, (src_h - h).max(0.0));
    Crop { x, y, w, h }
}

/// The group framing for every tracked face (focus: "Group Mode"):
/// - 2 people: a stacked **split screen**, one panel each (a 9:16 window can't
///   hold two people sitting apart in a 16:9 wide shot);
/// - 1 person: their solo crop;
/// - 0 or 3+: a centered 9:16 column over the table (the widest honest view).
pub fn group_layout(tracks: &[SpeakerTrack], src_w: f32, src_h: f32) -> Layout {
    match tracks {
        [] => Layout::FullFrame { crop: crate::centered_fullcam_crop(src_w, src_h) },
        [t] => Layout::FullFrame { crop: solo_crop(&t.bbox, src_w, src_h) },
        [a, b] => {
            let (_, half_aspect) = (0, CANVAS_W as f32 / (CANVAS_H as f32 * 0.5));
            Layout::Stacked {
                seam: 0.5,
                gameplay: panel_crop(&a.bbox, src_w, src_h, half_aspect),
                facecam: panel_crop(&b.bbox, src_w, src_h, half_aspect),
            }
        }
        many => {
            // Center the column on the union of faces.
            let cx = many.iter().map(|t| t.bbox.cx()).sum::<f32>() / many.len() as f32;
            let aspect = CANVAS_W as f32 / CANVAS_H as f32;
            let w = (src_h * aspect).min(src_w);
            let h = w / aspect;
            let x = (cx - w * 0.5).clamp(0.0, (src_w - w).max(0.0));
            let y = ((src_h - h) * 0.5).max(0.0);
            Layout::FullFrame { crop: Crop { x, y, w, h } }
        }
    }
}

/// Nearest-neighbour rgb24 downscale: the tracking frame (long edge ~640, for
/// mouth motion) feeds Ultraface's fixed 320x240 input without a second ffmpeg
/// decode. Detection is threshold-based and robust to nearest sampling.
pub fn downscale_rgb(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    dst: &mut [u8],
    dst_w: usize,
    dst_h: usize,
) {
    debug_assert!(src.len() >= src_w * src_h * 3);
    debug_assert!(dst.len() >= dst_w * dst_h * 3);
    for y in 0..dst_h {
        let sy = (y * src_h / dst_h).min(src_h - 1);
        for x in 0..dst_w {
            let sx = (x * src_w / dst_w).min(src_w - 1);
            let s = (sy * src_w + sx) * 3;
            let d = (y * dst_w + x) * 3;
            dst[d] = src[s];
            dst[d + 1] = src[s + 1];
            dst[d + 2] = src[s + 2];
        }
    }
}

/// A static Layout for the editor's non-tracking AI camera modes: the
/// most-persistent face (AutoFace) or everyone (Group), from the same analysis.
pub fn static_mode_layout(
    tracks: &[SpeakerTrack],
    src_w: f32,
    src_h: f32,
    group: bool,
) -> Layout {
    if group {
        return group_layout(tracks, src_w, src_h);
    }
    match tracks.iter().max_by(|a, b| {
        a.presence.partial_cmp(&b.presence).unwrap_or(std::cmp::Ordering::Equal)
    }) {
        Some(t) => Layout::FullFrame { crop: solo_crop(&t.bbox, src_w, src_h) },
        None => Layout::FullFrame { crop: crate::centered_fullcam_crop(src_w, src_h) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fb(x: f32, y: f32, w: f32, h: f32) -> FaceBox {
        FaceBox { x, y, w, h, score: 0.9 }
    }

    /// A synthetic rgb frame with a bright rectangle where a mouth would be.
    fn frame_with_patch(w: usize, h: usize, rect: Option<(usize, usize, usize, usize)>) -> Vec<u8> {
        let mut rgb = vec![20u8; w * h * 3];
        if let Some((rx, ry, rw, rh)) = rect {
            for y in ry..(ry + rh).min(h) {
                for x in rx..(rx + rw).min(w) {
                    let o = (y * w + x) * 3;
                    rgb[o] = 230;
                    rgb[o + 1] = 230;
                    rgb[o + 2] = 230;
                }
            }
        }
        rgb
    }

    #[test]
    fn tracks_build_and_measure_mouth_motion() {
        // Source = tracking frame (1:1) for simplicity. Two static faces; the
        // LEFT one's mouth region flickers every other frame (talking), the
        // right one's stays constant (listening).
        let (w, h) = (640usize, 360usize);
        let left = fb(80.0, 100.0, 80.0, 80.0);
        let right = fb(460.0, 100.0, 80.0, 80.0);
        // Left mouth region: y in [144..180), x in [94..145) roughly.
        let mouth = (96usize, 150usize, 40usize, 24usize);
        let mut b = TrackBuilder::new(w as f32, h as f32, w, h);
        for i in 0..30 {
            let rgb = frame_with_patch(w, h, if i % 2 == 0 { Some(mouth) } else { None });
            b.observe(&[left, right], &rgb);
        }
        let tracks = b.finish();
        assert_eq!(tracks.len(), 2);
        // Left-to-right labels: track 0 is the left face.
        assert!(tracks[0].bbox.cx() < tracks[1].bbox.cx());
        assert_eq!(tracks[0].id, 0);
        assert!((tracks[0].presence - 1.0).abs() < 1e-6);
        let a0: f32 = tracks[0].activity.iter().sum();
        let a1: f32 = tracks[1].activity.iter().sum();
        assert!(a0 > a1 * 5.0, "talking mouth must out-move listening one: {a0} vs {a1}");
    }

    #[test]
    fn transient_faces_are_dropped() {
        let (w, h) = (640usize, 360usize);
        let stat = fb(80.0, 100.0, 80.0, 80.0);
        let rgb = frame_with_patch(w, h, None);
        let mut b = TrackBuilder::new(w as f32, h as f32, w, h);
        for i in 0..20 {
            if i == 3 {
                // A one-frame false positive far away.
                b.observe(&[stat, fb(500.0, 250.0, 60.0, 60.0)], &rgb);
            } else {
                b.observe(&[stat], &rgb);
            }
        }
        let tracks = b.finish();
        assert_eq!(tracks.len(), 1, "the 5% face must not survive");
    }

    #[test]
    fn voiced_bins_gate_silence() {
        let sr = 16_000u32;
        let bin = 0.2f64;
        // 2 s: first second silent, second second loud.
        let mut samples = vec![0.0f32; 2 * sr as usize];
        for s in samples.iter_mut().skip(sr as usize) {
            *s = 0.3;
        }
        let v = voiced_bins(&samples, sr, bin, 10);
        assert!(v[..5].iter().all(|x| !x), "silent half: {v:?}");
        assert!(v[5..].iter().all(|x| *x), "loud half: {v:?}");
    }

    fn track(id: usize, x: f32, activity: Vec<f32>) -> SpeakerTrack {
        SpeakerTrack { id, bbox: fb(x, 100.0, 80.0, 80.0), presence: 1.0, activity }
    }

    #[test]
    fn attribution_follows_the_moving_mouth_with_hysteresis() {
        let n = 50;
        // A talks bins 0..25, B talks 25..50.
        let a_act: Vec<f32> = (0..n).map(|i| if i < 25 { 0.05 } else { 0.002 }).collect();
        let b_act: Vec<f32> = (0..n).map(|i| if i < 25 { 0.002 } else { 0.05 }).collect();
        let tracks = vec![track(0, 100.0, a_act), track(1, 500.0, b_act)];
        let voiced = vec![true; n];
        let (speaking, conf) = attribute_speakers(&tracks, &voiced);
        assert_eq!(speaking[5], Some(0));
        assert_eq!(speaking[45], Some(1));
        // The switch commits only after the confirm hold (0.8 s = 4 bins).
        assert_eq!(speaking[25], Some(0), "switch must not be instant");
        assert!(speaking[25 + 6].unwrap() == 1, "switch commits after the hold");
        assert!(conf[5] > 0.8, "confident when one mouth dominates: {}", conf[5]);
    }

    #[test]
    fn attribution_holds_through_a_lip_twitch() {
        let n = 30;
        // A talks throughout; B twitches for 2 bins (below the confirm hold).
        let a_act = vec![0.05f32; n];
        let mut b_act = vec![0.001f32; n];
        b_act[10] = 0.2;
        b_act[11] = 0.2;
        let tracks = vec![track(0, 100.0, a_act), track(1, 500.0, b_act)];
        let (speaking, _) = attribute_speakers(&tracks, &vec![true; n]);
        assert!(speaking.iter().all(|s| *s == Some(0)), "twitch stole the camera: {speaking:?}");
    }

    fn analysis(speaking: Vec<Option<usize>>, tracks: Vec<SpeakerTrack>) -> SpeakerAnalysis {
        let n = speaking.len();
        SpeakerAnalysis {
            bin_s: 1.0 / SPEAKER_FPS,
            voiced: vec![true; n],
            confidence: vec![1.0; n],
            speaking,
            tracks,
        }
    }

    #[test]
    fn shots_cut_between_speakers_and_respect_min_length() {
        // 20 s at 5 fps = 100 bins: A for 8 s, B for 12 s.
        let speaking: Vec<Option<usize>> =
            (0..100).map(|i| Some(if i < 40 { 0 } else { 1 })).collect();
        let tracks =
            vec![track(0, 200.0, vec![0.0; 100]), track(1, 1400.0, vec![0.0; 100])];
        let plan = plan_shots(&analysis(speaking, tracks), 1920.0, 1080.0, 20.0);
        assert_eq!(plan.shots.len(), 2);
        assert_eq!(plan.shots[0].track, Some(0));
        assert_eq!(plan.shots[1].track, Some(1));
        assert!((plan.shots[0].end_s - 8.0).abs() < 0.21, "cut at the switch");
        assert!((plan.shots[1].end_s - 20.0).abs() < 1e-9, "last shot ends at the clip end");
        // Contiguous.
        assert_eq!(plan.shots[0].end_s, plan.shots[1].start_s);
        // Solo framing is a 9:16 crop around the speaker.
        match &plan.shots[0].layout {
            Layout::FullFrame { crop } => {
                let aspect = crop.w / crop.h;
                assert!((aspect - 0.5625).abs() < 1e-2, "9:16 crop, got {aspect}");
                assert!(crop.x <= 200.0 && 200.0 <= crop.x + crop.w, "framed on A");
            }
            other => panic!("expected solo FullFrame, got {other:?}"),
        }
    }

    #[test]
    fn flicker_is_absorbed_and_silence_holds() {
        // A talks 0..40; a 2-bin flicker to B at 20; silence (None) 40..60
        // holds A; then B 60..100.
        let mut speaking: Vec<Option<usize>> = (0..100)
            .map(|i| {
                if i < 40 {
                    Some(0)
                } else if i < 60 {
                    None
                } else {
                    Some(1)
                }
            })
            .collect();
        speaking[20] = Some(1);
        speaking[21] = Some(1);
        let tracks =
            vec![track(0, 200.0, vec![0.0; 100]), track(1, 1400.0, vec![0.0; 100])];
        let plan = plan_shots(&analysis(speaking, tracks), 1920.0, 1080.0, 20.0);
        assert_eq!(plan.shots.len(), 2, "flicker + silence must not add shots: {plan:?}");
        assert_eq!(plan.shots[0].track, Some(0));
        // Silence held A: the cut lands at 12 s (bin 60), not 8 s (bin 40).
        assert!((plan.shots[0].end_s - 12.0).abs() < 0.21, "got {}", plan.shots[0].end_s);
    }

    #[test]
    fn rapid_exchange_collapses_into_a_group_split() {
        // 30 s: a rapid A/B exchange every 1.6 s for the first 16 s (10 runs),
        // then B holds for 14 s.
        let bin = 1.0 / SPEAKER_FPS;
        let n = (30.0 / bin) as usize;
        let speaking: Vec<Option<usize>> = (0..n)
            .map(|i| {
                let t = i as f64 * bin;
                if t < 16.0 {
                    Some(if (t / 1.6) as usize % 2 == 0 { 0 } else { 1 })
                } else {
                    Some(1)
                }
            })
            .collect();
        let tracks = vec![track(0, 300.0, vec![0.0; n]), track(1, 1500.0, vec![0.0; n])];
        let plan = plan_shots(&analysis(speaking, tracks), 1920.0, 1080.0, 30.0);
        // The exchange is one Group shot (split screen), then B's solo.
        assert!(plan.shots.len() <= 3, "cut storm survived: {plan:?}");
        assert_eq!(plan.shots[0].track, None, "exchange must be a group shot");
        assert!(matches!(plan.shots[0].layout, Layout::Stacked { .. }), "2-person split");
        assert_eq!(plan.shots.last().unwrap().track, Some(1));
    }

    #[test]
    fn group_layout_shapes() {
        let t0 = track(0, 300.0, vec![]);
        let t1 = track(1, 1500.0, vec![]);
        let t2 = track(2, 900.0, vec![]);
        // 2 people: stacked split, each panel framing its person.
        match group_layout(&[t0.clone(), t1.clone()], 1920.0, 1080.0) {
            Layout::Stacked { seam, gameplay, facecam } => {
                assert!((seam - 0.5).abs() < 1e-6);
                assert!(gameplay.x <= t0.bbox.cx() && t0.bbox.cx() <= gameplay.x + gameplay.w);
                assert!(facecam.x <= t1.bbox.cx() && t1.bbox.cx() <= facecam.x + facecam.w);
                // Panel aspect = 1080 : 960.
                assert!((gameplay.w / gameplay.h - 1.125).abs() < 1e-2);
            }
            other => panic!("expected split screen, got {other:?}"),
        }
        // 3 people: a centered 9:16 column.
        match group_layout(&[t0, t1, t2], 1920.0, 1080.0) {
            Layout::FullFrame { crop } => {
                assert!((crop.w / crop.h - 0.5625).abs() < 1e-2);
            }
            other => panic!("expected column, got {other:?}"),
        }
    }

    #[test]
    fn solo_crop_zooms_and_clamps() {
        // A modest face: crop = SOLO_ZOOM face-heights tall, 9:16, inside frame.
        let f = fb(900.0, 300.0, 150.0, 150.0);
        let c = solo_crop(&f, 1920.0, 1080.0);
        assert!((c.w / c.h - 0.5625).abs() < 1e-3);
        assert!((c.h - 540.0).abs() < 1.0, "3.6 x 150 = 540, got {}", c.h);
        assert!(c.x >= 0.0 && c.y >= 0.0 && c.x + c.w <= 1920.0 && c.y + c.h <= 1080.0);
        // Face center inside, biased above the crop middle (headroom).
        let fcy = f.cy();
        assert!(c.y <= fcy && fcy <= c.y + c.h);
        assert!(fcy < c.y + c.h * 0.5, "headroom bias");
        // A huge face clamps to a full-height column.
        let big = fb(400.0, 100.0, 600.0, 600.0);
        let c = solo_crop(&big, 1920.0, 1080.0);
        assert!(c.h <= 1080.0 + 1e-3);
    }

    #[test]
    fn track_labels_read_a_b_c() {
        assert_eq!(track_label(0), "Person A");
        assert_eq!(track_label(1), "Person B");
        assert_eq!(track_label(2), "Person C");
    }
}
