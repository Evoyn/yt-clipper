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
/// per-bin series (activity, VAD, speaker). Sampled near the source frame rate
/// so a **cut lands on the exact frame**: at 5 fps the plan could only place a
/// cut on a 0.2 s boundary, so the render held the old crop for up to ~5 frames
/// after the source had already cut (the operator's "1-5 frame blank before the
/// transition"). 24 fps quantizes a cut to ~1 frame. Everything downstream is
/// derived from this one rate (all windows are `<seconds> * SPEAKER_FPS` or
/// `<seconds> / bin_s`), so raising it just sharpens timing — a ~3-minute clip
/// is ~4 k CPU face detections (~30 s), the "quality over speed" the operator
/// asked for. Detection cost scales linearly with it.
pub const SPEAKER_FPS: f64 = 24.0;

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
/// Minimum time a face must be on screen (total, at [`SPEAKER_FPS`]) for its
/// track to survive — an **absolute duration**, not a fraction of the clip.
/// A false positive flickers for a frame or two; a real camera framing holds
/// for at least this. Crucially in a **multicam edit** the same person appears
/// at a different screen position in each camera, so each framing is its own
/// position track: a 10 s wide-shot inside a 90 s clip is only ~11 % of frames
/// (below the old 20 %-of-clip gate, which dropped it and left the camera with
/// nothing to follow during the wide shot — the operator's blank crop), yet is
/// unmistakably a real shot. Gate on seconds and it survives.
const MIN_TRACK_SECONDS: f64 = 1.5;
/// At most this many position tracks are kept. A multicam edit yields several
/// framings **per person** (each a distinct screen position), so this is well
/// above the number of people — 2 guests across 2-3 cameras each is already 4-6.
pub const MAX_TRACKS: usize = 6;
/// Printed-face prop filter (a book cover, a poster, a photo on the set —
/// Ultraface fires on any face-like texture). A track is a prop, not a speaker,
/// when it is **shorter than [`PROP_MAX_H_FRAC`]× the tallest track** and either
/// barely moves (peak mouth motion under [`PROP_MOTION_FRAC`]× the liveliest —
/// a printed face only shimmers with codec noise) **or is tiny**
/// ([`PROP_HARD_MIN_FRAC`]× the tallest): a set prop between two guests catches
/// hands and cups passing in front, so *nearby* motion alone must not save a
/// clearly-too-small face. Real people in a podcast sit at similar distances, so
/// a genuine speaker is never a third the height of another. Guarded so it can
/// never empty the track list.
const PROP_MAX_H_FRAC: f32 = 0.5;
const PROP_MOTION_FRAC: f32 = 0.5;
const PROP_HARD_MIN_FRAC: f32 = 0.4;

/// Activity smoothing window, seconds (moving average over the grid).
const ACTIVITY_SMOOTH_S: f64 = 0.6;
/// A challenger must out-move the incumbent by this factor to take the camera.
const SWITCH_MARGIN: f32 = 1.35;
/// ...and hold that lead for this long before the switch commits.
const SWITCH_CONFIRM_S: f64 = 0.8;
/// Mouth activity below this is "nobody visibly talking" — attribution then
/// holds the incumbent rather than guessing (voice-over, off-screen speech).
/// Pub so the diag harness can tell a genuine mouth attribution from such a
/// hold when scoring the voice lane against it.
pub const MIN_ACTIVITY: f32 = 0.004;

/// VAD: a bin is voiced when its RMS clears both an absolute floor (true
/// silence) and a fraction of the clip's loud (p90) reference.
const VAD_ABS_FLOOR: f32 = 0.004;
const VAD_REL_FRAC: f32 = 0.22;

/// Minimum shot length, seconds — cutting faster than this reads as jumpy.
const MIN_SHOT_S: f64 = 2.4;
/// Minimum length of a per-angle piece when an EDITED source splits an
/// attribution shot at its scene cuts (see `plan_shots` step 5): a cut closer
/// than this to a piece boundary folds into the neighbour — a sliver re-frame
/// is a flash, and the neighbouring angle's framing carries those frames.
const MIN_PIECE_S: f64 = 0.35;
/// Consecutive shots shorter than this each mark a rapid exchange; a run of
/// them collapses into one group shot instead of a cut storm.
const RAPID_SHOT_S: f64 = 4.0;
const RAPID_RUN: usize = 3;

/// Scene-type threshold: mean tracked faces visible per bin. A **static wide
/// shot** keeps everyone in frame at once (≈ the track count), so the camera
/// plan must *choose* the speaker (attribution + cuts, ADR 0038). A **multicam
/// edit or a solo shot** shows ≈1 face at a time — the source already cut to
/// its subject, so the plan instead *follows the visible face* and mirrors
/// those cuts. Below this mean → follow-visible; at/above → attribution.
const MULTICAM_MAX_MEAN_FACES: f32 = 1.5;
/// Follow-visible minimum shot: a subject run shorter than this is a transition
/// blip (a real source camera holds longer), folded into its neighbour so a
/// one- or two-bin detection wobble never becomes a cut, and a momentary
/// two-face overlap at a cut never flashes a split screen.
const MULTICAM_MIN_SHOT_S: f64 = 0.7;
/// Solo framing: crop height as a multiple of the face-box height (the zoom),
/// and where the face center sits vertically in the crop (headroom bias).
const SOLO_ZOOM: f32 = 3.6;
const SOLO_FACE_Y_FRAC: f32 = 0.38;
/// Split-screen panel framing: a touch tighter than solo (each panel is half a
/// canvas tall).
const SPLIT_ZOOM: f32 = 3.0;

/// Within-shot follow (the "smooth movement" half of the podcast camera): a
/// solo shot pans only when the subject's opening→closing drift exceeds this
/// fraction of the crop's span — a dead-zone, so a speaker who merely sways
/// gets a perfectly static shot, and cuts stay the grammar for speaker
/// changes (ADR 0038).
const FOLLOW_DEADZONE_FRAC: f32 = 0.12;
/// A follow pan is only modelled when the head→tail drift EXPLAINS the
/// subject's excursion: the off-drift remainder of the center band ("extra")
/// must stay under this fraction of the drift itself. Measured on the Deddy
/// fixture: genuine one-way drifts carry extra ≤ 0.36× their drift, while
/// the 22–27 s out-and-back lunge carried 1.7× — a linear glide there chases
/// a mid-lunge average while the subject moves the other way (the operator's
/// "jitter to the left"). An excursion holds a grown static crop instead.
const PAN_EXTRA_FRAC: f32 = 0.6;
/// ...and a pan must also be NECESSARY: when ONE static crop can contain the
/// whole span band by growing no more than this factor over the base zoom,
/// hold static — camera motion needs a reason a slightly wider frame can't
/// supply. Measured on the operator-flagged Deddy pans: the 11 s
/// wander-and-settle glide (30–41 s, ~5 px/s — permanent micro-motion over a
/// mostly-still subject) needed only 1.06× to contain statically, and the
/// other pans 1.09–1.15×, while genuine cross-frame travel (the Leon case
/// the follow exists for) needs ~3.8×. A seated podcast almost never
/// justifies a glide.
const PAN_STATIC_GROWTH: f32 = 1.25;
/// Seconds at a shot's head/tail whose median face center anchors the shot's
/// opening/closing framing (and the follow pan between them).
const FOLLOW_EDGE_S: f64 = 1.6;
/// Face-center percentiles a static framing must contain — the mid-shot
/// bobbing guard: a lean that comes back still stays inside the crop.
const SPAN_P_LO: f32 = 0.10;
const SPAN_P_HI: f32 = 0.90;

/// Piece-to-piece framing memory (the jump-cut crop stability): a solo angle
/// piece REUSES a remembered framing instead of re-deriving one while its
/// subject's measured geometry stays inside the dead-zone — center within
/// [`REUSE_CENTER_FH`] anchor face heights AND face height within
/// [`REUSE_H_FRAC`] of the anchor's. Measured on the production fixtures
/// (Deddy + ANTITESA): returns to an already-framed camera sit at ≤0.26 fh
/// center / ≤7% height while real angle changes sit at ≥0.39 fh or ≥21%
/// height — the thresholds live in that gap. Any reuse also requires the
/// piece's whole [`SPAN_P_LO`]..[`SPAN_P_HI`] center band to stay at least a
/// face's own extent plus air inside the reused crop (per-axis insets — a
/// face is ~0.4 fh half-wide but 0.5 fh half-tall, and a solo crop is only
/// ~2 fh wide against 3.6 fh tall), so a remembered framing can never crop
/// through a bobbing face.
const REUSE_CENTER_FH: f32 = 0.30;
const REUSE_H_FRAC: f32 = 0.12;
const REUSE_GUARD_X_FH: f32 = 0.55;
const REUSE_GUARD_Y_FH: f32 = 0.75;

/// One tracked person: a representative face box, how often they were
/// visible, their per-bin mouth activity, and where the face actually was per
/// bin. `id` is the track's index after the left-to-right relabel — 0 is
/// "Person A".
#[derive(Debug, Clone)]
pub struct SpeakerTrack {
    pub id: usize,
    /// Whole-clip median face box — a stable landmark for labels/overlays.
    /// Framing uses [`Self::path`] so a shot frames where the person is
    /// *during that shot*, not where they sat on average.
    pub bbox: FaceBox,
    /// Fraction of frames this track's face was detected in.
    pub presence: f32,
    /// Smoothed mouth activity per grid bin (0 where the face was absent).
    pub activity: Vec<f32>,
    /// The detected face box per grid bin (`None` where absent that frame).
    pub path: Vec<Option<FaceBox>>,
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
    /// Where the face was **last seen** — matching follows this, not the
    /// all-time mean: a person who leans toward the mic or shifts in their
    /// chair must stay on their own track, not shed detections into a new one
    /// (the "camera loses the moving speaker" failure).
    last_cx: f32,
    last_cy: f32,
    /// Per-frame raw mouth motion (None where absent this frame).
    motion: Vec<Option<f32>>,
    /// Per-frame face box (None where absent this frame) — the framing path.
    path: Vec<Option<FaceBox>>,
    prev_patch: Option<[f32; PATCH_W * PATCH_H]>,
    frames_seen: usize,
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
                    t.last_cx = f.cx();
                    t.last_cy = f.cy();
                    ti
                }
                None => {
                    self.tracks.push(Track {
                        boxes: vec![*f],
                        sum_cx: f.cx(),
                        last_cx: f.cx(),
                        last_cy: f.cy(),
                        motion: Vec::new(),
                        path: Vec::new(),
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
                t.path.push(None);
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
            t.path.push(Some(*f));
            t.prev_patch = Some(patch);
            t.frames_seen += 1;
        }
    }

    /// Match a detection to the track whose **last seen** position is nearest
    /// (within the radius): a slow-moving face drags its track along frame by
    /// frame instead of falling off an all-time-mean anchor.
    fn match_track(&self, f: &FaceBox) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (i, t) in self.tracks.iter().enumerate() {
            let dx = f.cx() - t.last_cx;
            let dy = f.cy() - t.last_cy;
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

    /// Close the builder: merge same-seat fragments, keep persistent tracks,
    /// relabel left-to-right ("Person A" is the leftmost — stable reading
    /// order), smooth activity.
    pub fn finish(self) -> Vec<SpeakerTrack> {
        let n = self.n_frames.max(1);
        let win = (ACTIVITY_SMOOTH_S * SPEAKER_FPS).round().max(1.0) as usize;
        let min_frames = (MIN_TRACK_SECONDS * SPEAKER_FPS).round().max(1.0) as usize;
        // Build EVERY track first (no persistence gate yet): a real seat can be
        // fragmented into pieces individually too short to survive, and the
        // merge below must see the fragments to reunite them.
        let mut all: Vec<Seat> = Vec::new();
        for t in self.tracks {
            if t.frames_seen == 0 {
                continue;
            }
            let presence = t.frames_seen as f32 / n as f32;
            let mut raw = vec![0f32; n];
            for (i, m) in t.motion.iter().enumerate() {
                if let Some(v) = *m {
                    raw[i] = v;
                }
            }
            let mut path = t.path;
            path.resize(n, None);
            all.push(Seat {
                sum_cx: t.sum_cx,
                boxes: t.boxes,
                tr: SpeakerTrack {
                    id: 0, // assigned after the left-to-right sort
                    bbox: FaceBox { x: 0.0, y: 0.0, w: 0.0, h: 0.0, score: 0.0 },
                    presence,
                    activity: smooth(&raw, win),
                    path,
                },
            });
        }
        for s in all.iter_mut() {
            s.tr.bbox = median_box(&s.boxes);
        }
        merge_same_seat_fragments(&mut all, n);
        let mut kept: Vec<(f32, SpeakerTrack)> = all
            .into_iter()
            .filter(|s| s.tr.path.iter().filter(|p| p.is_some()).count() >= min_frames)
            .map(|s| (s.sum_cx / s.boxes.len().max(1) as f32, s.tr))
            .collect();
        // Drop printed-face props (a book cover, a poster on the set): much
        // smaller than the real speakers AND far less mouth motion (a printed
        // face only shimmers with codec noise). Guarded so it can't empty the
        // list (a lone small track is still a real subject).
        if kept.len() > 1 {
            let max_h = kept.iter().map(|(_, t)| t.bbox.h).fold(0.0f32, f32::max);
            let peak = |t: &SpeakerTrack| t.activity.iter().copied().fold(0.0f32, f32::max);
            let max_peak = kept.iter().map(|(_, t)| peak(t)).fold(0.0f32, f32::max);
            let has_speaker = kept.iter().any(|(_, t)| t.bbox.h >= PROP_MAX_H_FRAC * max_h);
            if has_speaker {
                kept.retain(|(_, t)| {
                    let big_enough = t.bbox.h >= PROP_MAX_H_FRAC * max_h;
                    let tiny = t.bbox.h < PROP_HARD_MIN_FRAC * max_h;
                    let lively = peak(t) >= PROP_MOTION_FRAC * max_peak;
                    // Keep a big face always; keep a moderately-small one only if
                    // it is genuinely lively; drop a tiny face regardless.
                    big_enough || (lively && !tiny)
                });
            }
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

/// A track being assembled in [`TrackBuilder::finish`], with the raw
/// aggregates the same-seat merge needs to recombine.
struct Seat {
    sum_cx: f32,
    boxes: Vec<FaceBox>,
    tr: SpeakerTrack,
}

/// Two tracks are fragments of the SAME seat when their median boxes sit
/// within this fraction of the wider face of each other. Two real adjacent
/// people never overlap this closely (measured Deddy: same-seat fragments 20
/// and 104 px apart vs 170 px faces; the other person 519+ px away).
const MERGE_NEAR_FRAC: f32 = 0.8;
/// ...and they are on screen AT THE SAME TIME for at most this long. Same-seat
/// fragments alternate (one person can't be detected twice), so co-visibility
/// stays at stray double-fires (measured: 0.0-0.1 s); two real people sit
/// co-visible for most of a shared framing.
const MERGE_CO_VISIBLE_S: f64 = 0.35;
/// ...and each fragment must share the screen with somebody OUTSIDE the pair
/// for at least this fraction of its visible bins. This is what keeps the rule
/// safe on a solo-camera multicam edit (the Leon/ANTITESA source): there, two
/// different people's solo framings can also be near + alternating — but a
/// solo framing shows NO other face, so it never clears this bar, while a
/// two-person angle (the Deddy source) always does.
const MERGE_CONTEXT_FRAC: f32 = 0.4;

/// Reunite tracks that are fragments of one on-screen seat (person position).
///
/// A detection gap (a hand, a mic, a profile turn) plus a shift past the
/// match radius mints a NEW track, and the stale one later reclaims its old
/// spot — one person becomes several interleaved half-tracks. Measured on the
/// Deddy clip: 2 people became 4 tracks (pairs 20 px and 104 px apart,
/// co-visible 0.0-0.1 s), which fragmented the mouth-activity signal
/// (attribution flip-flopped between one person's own halves, mean confidence
/// 0.21), drew twin labelled boxes on one head in the editor, and fed framing
/// windows that landed inside a fragment's gap (the drifting "follow" pans
/// toward stale positions). Merging is transitive (union-find by repeated
/// scan) and recombines paths (first-Some), activity (per-bin max — the same
/// mouth seen via different framings), boxes, and box-count aggregates.
fn merge_same_seat_fragments(all: &mut Vec<Seat>, n: usize) {
    let max_co = (MERGE_CO_VISIBLE_S * SPEAKER_FPS).round() as usize;
    loop {
        let mut merged_any = false;
        'scan: for i in 0..all.len() {
            for j in i + 1..all.len() {
                if !same_seat(&all[i], &all[j], all, max_co, n) {
                    continue;
                }
                let b = all.remove(j);
                let a = &mut all[i];
                for k in 0..n {
                    if a.tr.path[k].is_none() {
                        a.tr.path[k] = b.tr.path[k];
                    }
                    a.tr.activity[k] = a.tr.activity[k].max(b.tr.activity[k]);
                }
                a.sum_cx += b.sum_cx;
                a.boxes.extend(b.boxes);
                a.tr.bbox = median_box(&a.boxes);
                a.tr.presence =
                    a.tr.path.iter().filter(|p| p.is_some()).count() as f32 / n.max(1) as f32;
                merged_any = true;
                break 'scan;
            }
        }
        if !merged_any {
            return;
        }
    }
}

/// The [`merge_same_seat_fragments`] test for one pair: near, temporally
/// complementary, and both fragments live in multi-person framings.
fn same_seat(a: &Seat, b: &Seat, all: &[Seat], max_co: usize, n: usize) -> bool {
    let dx = a.tr.bbox.cx() - b.tr.bbox.cx();
    let dy = a.tr.bbox.cy() - b.tr.bbox.cy();
    let near = (dx * dx + dy * dy).sqrt() < MERGE_NEAR_FRAC * a.tr.bbox.w.max(b.tr.bbox.w);
    if !near {
        return false;
    }
    let co = (0..n)
        .filter(|&k| a.tr.path[k].is_some() && b.tr.path[k].is_some())
        .count();
    if co > max_co {
        return false;
    }
    let with_context = |s: &Seat, other: &Seat| {
        let vis: Vec<usize> = (0..n).filter(|&k| s.tr.path[k].is_some()).collect();
        if vis.is_empty() {
            return false;
        }
        let ctx = vis
            .iter()
            .filter(|&&k| {
                all.iter().any(|o| {
                    !std::ptr::eq(o, s) && !std::ptr::eq(o, other) && o.tr.path[k].is_some()
                })
            })
            .count();
        ctx as f32 >= MERGE_CONTEXT_FRAC * vis.len() as f32
    };
    with_context(a, b) && with_context(b, a)
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

/// Mean tracked faces visible per bin over the analysis — the scene-type
/// signal (see [`MULTICAM_MAX_MEAN_FACES`]). A static wide shot ≈ track count;
/// a multicam edit or solo shot ≈ 1.
fn mean_visible_faces(tracks: &[SpeakerTrack], n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let total: usize = (0..n)
        .map(|b| tracks.iter().filter(|t| t.path.get(b).map(|p| p.is_some()).unwrap_or(false)).count())
        .sum();
    total as f32 / n as f32
}

/// Turn the speaker analysis into a **cut-based** [`CameraPlan`]. Two regimes,
/// chosen by how many faces are on screen at once ([`mean_visible_faces`]):
///
/// - **Static wide shot** (everyone visible): the classic active-speaker plan —
///   attribution picks the talker, silence holds, flickers absorb, a rapid
///   exchange collapses into a group shot, shots respect [`MIN_SHOT_S`]. When
///   the source turns out to be an **edit anyway** (`cuts` non-empty — e.g. a
///   show cutting between two-person angles, where faces-per-frame alone can't
///   tell), each shot re-frames at the source's cut frames: WHO stays
///   attribution's choice, WHERE is per-angle (see step 5).
/// - **Multicam edit / solo** (≈1 face at a time): the source already cut to
///   its subject, so the plan *follows the visible face* and mirrors those cuts,
///   never parking a crop on an off-screen position (the empty-crop failure on
///   already-edited podcast VODs). When the source's **exact cut frames** are
///   known (`cuts`, from ffmpeg scene detection), [`plan_by_scene_cuts`] uses
///   them as the shot boundaries — frame-accurate regardless of the analysis
///   sample rate; otherwise [`plan_follow_visible`] approximates them from the
///   per-bin subject.
///
/// `cuts` are clip-relative source cut times (seconds); empty is fine (the
/// static-wide path ignores them, the multicam path falls back to per-bin).
/// Shots are contiguous over `0..duration_s`.
pub fn plan_shots(
    analysis: &SpeakerAnalysis,
    src_w: f32,
    src_h: f32,
    duration_s: f64,
    cuts: &[f64],
) -> CameraPlan {
    let n = analysis.speaking.len();
    let bin_s = if analysis.bin_s > 0.0 { analysis.bin_s } else { 1.0 / SPEAKER_FPS };
    if n == 0 || analysis.tracks.is_empty() {
        return CameraPlan {
            shots: vec![Shot {
                pan_to: None,
                start_s: 0.0,
                end_s: duration_s,
                track: None,
                layout: group_layout(&analysis.tracks, src_w, src_h),
            }],
        };
    }

    // Multicam / solo source: follow whoever the source is showing. With the
    // source's real cut frames, cut exactly there; else approximate per-bin.
    if mean_visible_faces(&analysis.tracks, n) < MULTICAM_MAX_MEAN_FACES {
        if !cuts.is_empty() {
            return plan_by_scene_cuts(analysis, src_w, src_h, duration_s, bin_s, cuts);
        }
        return plan_follow_visible(analysis, src_w, src_h, duration_s, bin_s);
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
                pan_to: None,
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
    // A short FIRST run has no predecessor to absorb it: fold it into the run
    // that follows, so the camera never opens on a sub-minimum flash shot.
    if final_runs.len() >= 2 && final_runs[0].1 < min_bins {
        let (_, len) = final_runs.remove(0);
        final_runs[0].1 += len;
    }

    // 5. Frame each run **from where its subject is during the run** (the
    //    track's path over the run's bins), not the whole-clip landmark box —
    //    a speaker who has shifted since the clip's start is still centered,
    //    and a drift across the run becomes the slow follow pan.
    //
    //    In an EDITED source (`cuts` non-empty: camera angles / jump cuts —
    //    a genuinely static wide shot has none), each run additionally splits
    //    at the source's own cut frames and each piece is framed from ITS
    //    bins alone. A source cut re-positions its people (~100 px jumps on
    //    the Deddy clip), so one crop — or worse, one glide — spanning a cut
    //    is framed on a position average that exists in NEITHER angle: the
    //    operator's drifting camera and cropped-off face. A piece where the
    //    subject was never detected (occluded through that whole angle) keeps
    //    the previous piece's framing rather than snapping to a stale
    //    landmark.
    //
    //    Piece framing carries a whole-clip FRAMING MEMORY per seat
    //    ([`piece_framing`]): a piece whose subject is still inside a
    //    remembered framing's dead-zone reuses that crop verbatim, so a jump
    //    cut back to an already-framed camera never twitches the zoom (the
    //    re-derive-per-piece breathing measured 894->702->884->736 px across
    //    27 s on the Deddy fixture), while a real angle change — a different
    //    position or face size, including a different human in the same seat
    //    (ADR 0042) — fails the dead-zone and re-frames fully at the cut,
    //    where a re-frame is perceptually free.
    let mut cut_list: Vec<f64> = cuts.to_vec();
    cut_list.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut memory: std::collections::HashMap<usize, Vec<FramingAnchor>> =
        std::collections::HashMap::new();
    let mut shots = Vec::with_capacity(final_runs.len());
    let mut t = 0.0f64;
    let mut bin = 0usize;
    for (k, (subj, len)) in final_runs.iter().enumerate() {
        let start_s = t;
        let end_s = if k + 1 == final_runs.len() {
            duration_s
        } else {
            t + *len as f64 * bin_s
        };
        t = end_s;
        let (b0, b1) = (bin, bin + len);
        bin = b1;
        // Piece boundaries: the run's span, split at in-run cuts, slivers
        // folded into their neighbour (MIN_PIECE_S).
        let mut bounds: Vec<f64> = vec![start_s];
        for &c in &cut_list {
            if c > bounds.last().copied().unwrap_or(start_s) + MIN_PIECE_S
                && c < end_s - MIN_PIECE_S
            {
                bounds.push(c);
            }
        }
        bounds.push(end_s);
        let mut prev: Option<(Option<usize>, Layout)> = None;
        for w in bounds.windows(2) {
            let (p0, p1) = (w[0], w[1]);
            let pb0 = ((p0 / bin_s).round() as usize).clamp(b0, b1);
            let pb1 = ((p1 / bin_s).round() as usize).clamp(pb0, b1);
            let (track, layout, pan_to) = match subj {
                Subject::Track(id) => match analysis.tracks.iter().find(|tr| tr.id == *id) {
                    Some(tr) => match piece_framing(
                        tr,
                        pb0,
                        pb1,
                        src_w,
                        src_h,
                        memory.entry(*id).or_default(),
                    ) {
                        Some((c0, pan)) => (Some(*id), Layout::FullFrame { crop: c0 }, pan),
                        // Subject undetected through this piece: continuity
                        // first (the previous angle's framing), then the run,
                        // then the whole-clip landmark box.
                        None => match &prev {
                            Some((tid, l)) => (*tid, l.clone(), None),
                            None => match solo_span_framing(tr, b0, b1, src_w, src_h) {
                                Some((c0, _)) => {
                                    (Some(*id), Layout::FullFrame { crop: c0 }, None)
                                }
                                None => (
                                    Some(*id),
                                    Layout::FullFrame {
                                        crop: solo_crop(&tr.bbox, src_w, src_h),
                                    },
                                    None,
                                ),
                            },
                        },
                    },
                    None => {
                        (None, group_layout_span(&analysis.tracks, pb0, pb1, src_w, src_h), None)
                    }
                },
                Subject::Group => {
                    (None, group_layout_span(&analysis.tracks, pb0, pb1, src_w, src_h), None)
                }
            };
            prev = Some((track, layout.clone()));
            shots.push(Shot { start_s: p0, end_s: p1, track, layout, pan_to });
        }
    }
    CameraPlan { shots }
}

/// The on-screen subject over bins `[b0, b1)` of one source shot: a `Group`
/// when two or more faces share most of the segment, else the single face
/// visible in the most bins, or `None` when the segment holds no face.
fn segment_subject(tracks: &[SpeakerTrack], b0: usize, b1: usize) -> Option<Subject> {
    let total = b1.saturating_sub(b0).max(1);
    let mut vis = vec![0usize; tracks.len()];
    let mut group_bins = 0usize;
    for b in b0..b1 {
        let mut c = 0usize;
        for (i, t) in tracks.iter().enumerate() {
            if t.path.get(b).map(|p| p.is_some()).unwrap_or(false) {
                vis[i] += 1;
                c += 1;
            }
        }
        if c >= 2 {
            group_bins += 1;
        }
    }
    if group_bins * 2 > total {
        return Some(Subject::Group);
    }
    let (best_i, best) = vis.iter().copied().enumerate().max_by_key(|(_, c)| *c).unwrap_or((0, 0));
    (best > 0).then(|| Subject::Track(tracks[best_i].id))
}

/// Frame one source shot on its `subject` over bins `[b0, b1)` — a solo crop
/// on that track's span position, or a split of the people in a group shot.
fn frame_subject(
    subject: Subject,
    tracks: &[SpeakerTrack],
    b0: usize,
    b1: usize,
    src_w: f32,
    src_h: f32,
) -> (Option<usize>, Layout) {
    match subject {
        Subject::Track(id) => match tracks.iter().find(|t| t.id == id) {
            Some(tr) => {
                let crop = static_span_crop(tr, b0, b1, src_w, src_h)
                    .unwrap_or_else(|| solo_crop(&tr.bbox, src_w, src_h));
                (Some(id), Layout::FullFrame { crop })
            }
            None => (None, group_layout_span(tracks, b0, b1, src_w, src_h)),
        },
        Subject::Group => (None, group_layout_span(tracks, b0, b1, src_w, src_h)),
    }
}

/// Multicam plan using the source's **own cut frames** as the shot boundaries
/// (`cuts`: clip-relative seconds, from ffmpeg scene detection). Each inter-cut
/// span is one source shot, framed on whoever is on screen during it; adjacent
/// spans that frame the same subject merge (a scene cut that changes nothing
/// visible — a false trigger, or a re-cut to the same person's camera — is not
/// a cut). Because the boundaries are the *actual* cut frames, every cut lands
/// exactly on the source's, with no sampling grid to lag it — the frame-precise
/// version of [`plan_follow_visible`], which can only quantize a cut to a bin.
fn plan_by_scene_cuts(
    analysis: &SpeakerAnalysis,
    src_w: f32,
    src_h: f32,
    duration_s: f64,
    bin_s: f64,
    cuts: &[f64],
) -> CameraPlan {
    let tracks = &analysis.tracks;
    let n = analysis.speaking.len();
    // Boundary times: 0, the in-range cuts, duration — sorted and de-duplicated.
    let mut bounds: Vec<f64> = vec![0.0];
    for &c in cuts {
        if c > 0.03 && c < duration_s - 0.03 {
            bounds.push(c);
        }
    }
    bounds.push(duration_s);
    bounds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    bounds.dedup_by(|a, b| (*a - *b).abs() < 0.04);

    // One (subject, time span, bin span) per inter-cut segment.
    let mut subs: Vec<Option<Subject>> = Vec::new();
    let mut ranges: Vec<(f64, f64, usize, usize)> = Vec::new();
    for w in bounds.windows(2) {
        let (t0, t1) = (w[0], w[1]);
        let b0 = ((t0 / bin_s).round() as usize).min(n);
        let b1 = ((t1 / bin_s).round() as usize).clamp(b0, n);
        subs.push(segment_subject(tracks, b0, b1));
        ranges.push((t0, t1, b0, b1));
    }
    // A segment with no face holds the previous subject (lead-in holds the
    // first). Nothing anywhere → one honest centered shot.
    let Some(first) = subs.iter().flatten().copied().next() else {
        return CameraPlan {
            shots: vec![Shot {
                start_s: 0.0,
                end_s: duration_s,
                track: None,
                layout: group_layout(tracks, src_w, src_h),
                pan_to: None,
            }],
        };
    };
    let mut last = first;
    for s in subs.iter_mut() {
        match *s {
            Some(v) => last = v,
            None => *s = Some(last),
        }
    }
    // Merge adjacent same-subject segments, then frame each merged run.
    let mut shots = Vec::new();
    let mut i = 0;
    while i < subs.len() {
        let subject = subs[i].expect("filled above");
        let mut j = i;
        while j + 1 < subs.len() && subs[j + 1] == Some(subject) {
            j += 1;
        }
        let start_s = ranges[i].0;
        let end_s = if j + 1 == subs.len() { duration_s } else { ranges[j].1 };
        let (b0, b1) = (ranges[i].2, ranges[j].3);
        let (track, layout) = frame_subject(subject, tracks, b0, b1, src_w, src_h);
        shots.push(Shot { start_s, end_s, track, layout, pan_to: None });
        i = j + 1;
    }
    CameraPlan { shots }
}

/// Plan for a **multicam edit or solo source** (≈1 face visible at a time): the
/// source already cut to its subject, so follow the **visible** face and mirror
/// those cuts instead of choosing a speaker from audio. Per bin the subject is
/// the largest visible face (the one the source is showing); a cutaway with no
/// face holds the previous subject; sub-[`MULTICAM_FLICKER_S`] runs are absorbed
/// as detection noise but every real cut is kept — so the crop is only ever on a
/// face that is actually on screen (no empty-position parking). No group shots:
/// with one person on screen there is nothing to combine.
fn plan_follow_visible(
    analysis: &SpeakerAnalysis,
    src_w: f32,
    src_h: f32,
    duration_s: f64,
    bin_s: f64,
) -> CameraPlan {
    let tracks = &analysis.tracks;
    let n = analysis.speaking.len();
    if n == 0 || tracks.iter().all(|t| t.path.iter().all(|p| p.is_none())) {
        // No face ever visible: one honest centered shot.
        return CameraPlan {
            shots: vec![Shot {
                start_s: 0.0,
                end_s: duration_s,
                track: None,
                layout: group_layout(tracks, src_w, src_h),
                pan_to: None,
            }],
        };
    }
    let subject = subject_series(tracks, n);
    // Runs of the same subject = the source's own shots. Fold sub-minimum blips
    // (a transition wobble, or a momentary two-face overlap at a cut) into the
    // neighbour; a real source camera holds longer.
    let mut runs: Vec<(Subject, usize)> = Vec::new();
    for s in &subject {
        match runs.last_mut() {
            Some((r, len)) if r == s => *len += 1,
            _ => runs.push((*s, 1)),
        }
    }
    let min_bins = (MULTICAM_MIN_SHOT_S / bin_s).round().max(1.0) as usize;
    let mut merged: Vec<(Subject, usize)> = Vec::new();
    for (subj, len) in runs {
        match merged.last_mut() {
            Some((prev, plen)) if *prev == subj => *plen += len,
            Some((_, plen)) if len < min_bins => *plen += len,
            _ => merged.push((subj, len)),
        }
    }
    if merged.len() >= 2 && merged[0].1 < min_bins {
        let (_, len) = merged.remove(0);
        merged[0].1 += len;
    }
    // Frame each source shot STATICALLY on its subject's median position — no
    // leading-edge pan (which lagged the cut for ~1 s) and no per-bin follow
    // (which jittered): a static source camera wants a static crop, correct
    // from the first frame. A Group run (source showing 2+ people) splits them.
    let mut shots = Vec::with_capacity(merged.len());
    let mut t = 0.0f64;
    let mut bin = 0usize;
    for (k, (subj, len)) in merged.iter().enumerate() {
        let start_s = t;
        let end_s = if k + 1 == merged.len() { duration_s } else { t + *len as f64 * bin_s };
        t = end_s;
        let (b0, b1) = (bin, bin + len);
        bin = b1;
        let (track, layout) = match subj {
            Subject::Track(id) => match tracks.iter().find(|t| t.id == *id) {
                Some(tr) => {
                    let crop = static_span_crop(tr, b0, b1, src_w, src_h)
                        .unwrap_or_else(|| solo_crop(&tr.bbox, src_w, src_h));
                    (Some(*id), Layout::FullFrame { crop })
                }
                None => (None, group_layout_span(tracks, b0, b1, src_w, src_h)),
            },
            Subject::Group => (None, group_layout_span(tracks, b0, b1, src_w, src_h)),
        };
        shots.push(Shot { start_s, end_s, track, layout, pan_to: None });
    }
    CameraPlan { shots }
}

/// Per-bin on-screen subject for a multicam/solo source, mirroring what the
/// source shows: **one** visible face → follow that face; **two or more** →
/// a `Group` (the source cut to a shared/wide shot, so show everyone rather
/// than guess which one). A cutaway with no face holds the previous subject.
/// Because a single visible face is unambiguous and a two-face frame is a
/// group (not a pick-the-largest contest), there is no flicker to damp.
fn subject_series(tracks: &[SpeakerTrack], n: usize) -> Vec<Subject> {
    let mut out: Vec<Option<Subject>> = (0..n)
        .map(|b| {
            let visible: Vec<usize> = tracks
                .iter()
                .filter_map(|t| t.path.get(b).and_then(|p| p.as_ref()).map(|_| t.id))
                .collect();
            match visible.as_slice() {
                [] => None,
                [id] => Some(Subject::Track(*id)),
                _ => Some(Subject::Group),
            }
        })
        .collect();
    // Hold the last real subject through cutaway bins (and a lead-in cutaway
    // through to the first face the source shows).
    if let Some(first) = out.iter().flatten().copied().next() {
        let mut last = first;
        for s in out.iter_mut() {
            match *s {
                Some(v) => last = v,
                None => *s = Some(last),
            }
        }
    }
    out.into_iter().map(|s| s.unwrap_or(Subject::Group)).collect()
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

// --- span framing: frame the person where they ARE during the shot ----------

/// Median face box over `path[lo..hi)`; `None` when the face never appears.
fn span_median_box(path: &[Option<FaceBox>], lo: usize, hi: usize) -> Option<FaceBox> {
    let hi = hi.min(path.len());
    let boxes: Vec<FaceBox> = path.get(lo.min(hi)..hi)?.iter().flatten().copied().collect();
    if boxes.is_empty() {
        return None;
    }
    Some(median_box(&boxes))
}

/// The ([`SPAN_P_LO`], [`SPAN_P_HI`]) percentile band of the face centers over
/// `path[lo..hi)`: `(cx_lo, cx_hi, cy_lo, cy_hi)`.
fn span_center_band(path: &[Option<FaceBox>], lo: usize, hi: usize) -> Option<(f32, f32, f32, f32)> {
    let hi = hi.min(path.len());
    let mut cxs: Vec<f32> = Vec::new();
    let mut cys: Vec<f32> = Vec::new();
    for b in path.get(lo.min(hi)..hi)?.iter().flatten() {
        cxs.push(b.cx());
        cys.push(b.cy());
    }
    if cxs.is_empty() {
        return None;
    }
    let pct = |v: &mut Vec<f32>, p: f32| -> f32 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[((v.len() - 1) as f32 * p).round() as usize]
    };
    let (xl, xh) = (pct(&mut cxs.clone(), SPAN_P_LO), pct(&mut cxs, SPAN_P_HI));
    let (yl, yh) = (pct(&mut cys.clone(), SPAN_P_LO), pct(&mut cys, SPAN_P_HI));
    Some((xl, xh, yl, yh))
}

/// Median face center over the span's leading (or trailing) [`FOLLOW_EDGE_S`]
/// worth of bins that actually contain the face.
fn edge_center(path: &[Option<FaceBox>], lo: usize, hi: usize, trailing: bool) -> Option<(f32, f32)> {
    let hi = hi.min(path.len());
    let present: Vec<&FaceBox> = path.get(lo.min(hi)..hi)?.iter().flatten().collect();
    if present.is_empty() {
        return None;
    }
    let edge = ((FOLLOW_EDGE_S * SPEAKER_FPS).round() as usize).clamp(1, present.len());
    let slice: Vec<&&FaceBox> = if trailing {
        present.iter().rev().take(edge).collect()
    } else {
        present.iter().take(edge).collect()
    };
    let med = |mut v: Vec<f32>| -> f32 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    };
    Some((
        med(slice.iter().map(|b| b.cx()).collect()),
        med(slice.iter().map(|b| b.cy()).collect()),
    ))
}

/// A 9:16 crop of `(w, h)` placed with its subject center at `(cx, cy)`
/// (headroom-biased vertically), clamped inside the source.
fn place_crop(cx: f32, cy: f32, w: f32, h: f32, src_w: f32, src_h: f32) -> Crop {
    Crop {
        x: (cx - w * 0.5).clamp(0.0, (src_w - w).max(0.0)),
        y: (cy - h * SOLO_FACE_Y_FRAC).clamp(0.0, (src_h - h).max(0.0)),
        w,
        h,
    }
}

/// The static solo crop for a track over `path[lo..hi)`: base zoom from the
/// span's median face, grown so the whole [`SPAN_P_LO`]..[`SPAN_P_HI`] center
/// band stays comfortably inside (the bobbing guard) — a mover is framed
/// wider, never cropped through the face. `None` when the face never appears.
/// The (w, h) a static crop needs to contain `band` plus most of the base
/// framing as margin — before any source clamping. Shared by
/// [`static_span_crop`] and the pan-necessity test (a pan is only justified
/// when this growth would be excessive).
fn span_contain_size(band: (f32, f32, f32, f32), base_h: f32, aspect: f32) -> (f32, f32) {
    let (cx_lo, cx_hi, cy_lo, cy_hi) = band;
    let h = base_h.max((cy_hi - cy_lo) + base_h * 0.9);
    let w = (h * aspect).max((cx_hi - cx_lo) + base_h * aspect * 0.9);
    (w, w / aspect)
}

fn static_span_crop(
    track: &SpeakerTrack,
    lo: usize,
    hi: usize,
    src_w: f32,
    src_h: f32,
) -> Option<Crop> {
    let med = span_median_box(&track.path, lo, hi)?;
    let (cx_lo, cx_hi, cy_lo, cy_hi) = span_center_band(&track.path, lo, hi)?;
    let aspect = CANVAS_W as f32 / CANVAS_H as f32;
    let base_h = med.h * SOLO_ZOOM;
    // Contain the band plus most of the base framing as margin around it.
    let (mut w, mut h) = span_contain_size((cx_lo, cx_hi, cy_lo, cy_hi), base_h, aspect);
    if h > src_h {
        h = src_h;
        w = h * aspect;
    }
    if w > src_w {
        w = src_w;
        h = w / aspect;
    }
    Some(place_crop((cx_lo + cx_hi) * 0.5, (cy_lo + cy_hi) * 0.5, w, h, src_w, src_h))
}

/// Solo framing over a shot's bin span `[lo, hi)`: a static crop when the
/// subject's opening→closing drift sits inside the dead-zone, else a
/// same-sized `(start, end)` crop pair — the render glides between them (the
/// slow follow), so a speaker who shifts across a long shot stays in frame
/// without a jump cut. `None` when the face never appears in the span.
pub fn solo_span_framing(
    track: &SpeakerTrack,
    lo: usize,
    hi: usize,
    src_w: f32,
    src_h: f32,
) -> Option<(Crop, Option<Crop>)> {
    let med = span_median_box(&track.path, lo, hi)?;
    let (sx, sy) = edge_center(&track.path, lo, hi, false)?;
    let (ex, ey) = edge_center(&track.path, lo, hi, true)?;
    let aspect = CANVAS_W as f32 / CANVAS_H as f32;
    let base_h = med.h * SOLO_ZOOM;
    let base_w = base_h * aspect;
    if (ex - sx).abs() < base_w * FOLLOW_DEADZONE_FRAC
        && (ey - sy).abs() < base_h * FOLLOW_DEADZONE_FRAC
    {
        return Some((static_span_crop(track, lo, hi, src_w, src_h)?, None));
    }
    // Pan: the endpoints absorb the drift; the size only has to cover the
    // bobbing *beyond* the drift line, with the base framing as margin.
    let (cx_lo, cx_hi, cy_lo, cy_hi) = span_center_band(&track.path, lo, hi)?;
    let extra_x = ((cx_hi - cx_lo) - (ex - sx).abs()).max(0.0);
    let extra_y = ((cy_hi - cy_lo) - (ey - sy).abs()).max(0.0);
    // A glide only fits a one-way drift. When the subject swept far beyond
    // the drift line and came back (extra >> drift — an out-and-back lunge,
    // not a move), a linear pan chases a mid-lunge average while the subject
    // returns the other way: hold one static crop over the whole band
    // instead ([`PAN_EXTRA_FRAC`], measured).
    if (extra_x * extra_x + extra_y * extra_y).sqrt()
        > PAN_EXTRA_FRAC * ((ex - sx).powi(2) + (ey - sy).powi(2)).sqrt()
    {
        return Some((static_span_crop(track, lo, hi, src_w, src_h)?, None));
    }
    // ...and it must be NECESSARY: a wander that a modest static growth
    // contains gets the static frame — a multi-second crawl over a
    // mostly-still subject is permanent micro-motion, the operator's
    // "jitter" ([`PAN_STATIC_GROWTH`], measured). Only travel too large to
    // hold in one frame earns camera motion.
    let (_, contain_h) =
        span_contain_size((cx_lo, cx_hi, cy_lo, cy_hi), base_h, aspect);
    if contain_h <= PAN_STATIC_GROWTH * base_h {
        return Some((static_span_crop(track, lo, hi, src_w, src_h)?, None));
    }
    let mut h = base_h + extra_y;
    let mut w = (h * aspect).max(base_w + extra_x);
    h = w / aspect;
    if h > src_h {
        h = src_h;
        w = h * aspect;
    }
    if w > src_w {
        w = src_w;
        h = w / aspect;
    }
    let c0 = place_crop(sx, sy, w, h, src_w, src_h);
    let c1 = place_crop(ex, ey, w, h, src_w, src_h);
    // Clamping can collapse the two onto the same spot: then it is static.
    if (c0.x - c1.x).abs() < 1.0 && (c0.y - c1.y).abs() < 1.0 {
        return Some((c0, None));
    }
    Some((c0, Some(c1)))
}

/// One remembered solo framing for a seat: where the subject was measured
/// (the piece's center-band midpoint and median face height) and the crop
/// that piece emitted. Anchors are FIXED — a reuse never re-baselines one —
/// so a slow drift accumulates delta against the original anchor and earns
/// one honest re-frame when it becomes real, instead of creeping the camera
/// along in sub-dead-zone steps that never re-frame at all.
struct FramingAnchor {
    cx: f32,
    cy: f32,
    fh: f32,
    crop: Crop,
}

/// Solo framing for one angle piece, stabilized by the seat's framing memory
/// (`anchors`, whole-clip). The fresh framing is [`solo_span_framing`]'s;
/// then, most-recent anchor first:
///
/// - a piece that PANS (real within-piece drift) never reuses — it frames
///   fresh and remembers its CLOSING state (tail center, closing crop), so
///   the next piece matches where the subject ended up;
/// - a static piece whose subject still sits inside an anchor's dead-zone
///   ([`REUSE_CENTER_FH`] / [`REUSE_H_FRAC`]) reuses that anchor's crop
///   VERBATIM — the zero-twitch jump cut (a human editor cutting back to
///   the same camera reuses the same framing);
/// - failing the center test but matching an anchor's face height, it
///   reuses that anchor's SIZE re-placed at its own center — a lean moves
///   the camera, never the zoom — and becomes a new anchor;
/// - otherwise it frames fresh and becomes a new anchor.
///
/// Every reuse also passes the [`REUSE_GUARD_X_FH`]/[`REUSE_GUARD_Y_FH`]
/// band-containment guard; a piece that bobs beyond the remembered framing
/// falls through to fresh.
fn piece_framing(
    track: &SpeakerTrack,
    lo: usize,
    hi: usize,
    src_w: f32,
    src_h: f32,
    anchors: &mut Vec<FramingAnchor>,
) -> Option<(Crop, Option<Crop>)> {
    let (c0, pan) = solo_span_framing(track, lo, hi, src_w, src_h)?;
    let med = span_median_box(&track.path, lo, hi)?;
    let (cx_lo, cx_hi, cy_lo, cy_hi) = span_center_band(&track.path, lo, hi)?;
    if let Some(c1) = pan {
        let (ex, ey) = edge_center(&track.path, lo, hi, true)?;
        anchors.push(FramingAnchor { cx: ex, cy: ey, fh: med.h, crop: c1 });
        return Some((c0, Some(c1)));
    }
    let (sx, sy) = ((cx_lo + cx_hi) * 0.5, (cy_lo + cy_hi) * 0.5);
    let fits = |crop: &Crop, fh: f32| {
        let (ix, iy) = (REUSE_GUARD_X_FH * fh, REUSE_GUARD_Y_FH * fh);
        cx_lo >= crop.x + ix
            && cx_hi <= crop.x + crop.w - ix
            && cy_lo >= crop.y + iy
            && cy_hi <= crop.y + crop.h - iy
    };
    // Verbatim reuse: the subject is still inside a remembered framing's
    // dead-zone, so the crop must not move at all.
    if let Some(a) = anchors.iter().rev().find(|a| {
        ((sx - a.cx).powi(2) + (sy - a.cy).powi(2)).sqrt() <= REUSE_CENTER_FH * a.fh
            && (med.h - a.fh).abs() <= REUSE_H_FRAC * a.fh
            && fits(&a.crop, a.fh)
    }) {
        return Some((a.crop, None));
    }
    // Size reuse: the same face height at a genuinely new position — keep
    // the remembered zoom, re-place it on the subject.
    if let Some(a) = anchors.iter().rev().find(|a| (med.h - a.fh).abs() <= REUSE_H_FRAC * a.fh) {
        let c = place_crop(sx, sy, a.crop.w, a.crop.h, src_w, src_h);
        if fits(&c, a.fh) {
            anchors.push(FramingAnchor { cx: sx, cy: sy, fh: med.h, crop: c });
            return Some((c, None));
        }
    }
    // Fresh framing, new anchor.
    anchors.push(FramingAnchor { cx: sx, cy: sy, fh: med.h, crop: c0 });
    Some((c0, None))
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

/// Fraction of a group shot's bins a track must be on screen to count as one of
/// its people. A multicam wide shot holds its people the whole time; a passing
/// flicker (or the same person's *other* camera framing catching a few frames
/// as they lean between two position tracks) does not — without this, those
/// strays inflate a clean two-person split into a three-way column.
const GROUP_PRESENCE_FRAC: f32 = 0.4;

/// [`group_layout`] framed from the tracks **substantially on screen during**
/// bins `[lo, hi)` (present for at least [`GROUP_PRESENCE_FRAC`] of them), each
/// at its median position over the span. So a multicam source splits only the
/// people actually in the wide shot for its duration — not every stray framing
/// that flickers through. Falls back to the most-present track (or all) if none
/// clears the bar. Pub so the diag harness can compose demo plans (e.g. the
/// diarization spike's off-screen-override preview) from the same framing the
/// planner uses.
pub fn group_layout_span(
    tracks: &[SpeakerTrack],
    lo: usize,
    hi: usize,
    src_w: f32,
    src_h: f32,
) -> Layout {
    let span = (hi.min(tracks.first().map(|t| t.path.len()).unwrap_or(hi)).saturating_sub(lo)).max(1);
    let seen = |t: &SpeakerTrack| {
        t.path.get(lo..hi.min(t.path.len())).map(|s| s.iter().flatten().count()).unwrap_or(0)
    };
    let mut present: Vec<SpeakerTrack> = tracks
        .iter()
        .filter(|t| seen(t) as f32 >= GROUP_PRESENCE_FRAC * span as f32)
        .filter_map(|t| span_median_box(&t.path, lo, hi).map(|bbox| SpeakerTrack { bbox, ..t.clone() }))
        .collect();
    if present.is_empty() {
        // Nobody sustained: frame the single most-present face, or fall back.
        if let Some(t) = tracks.iter().max_by_key(|t| seen(*t)) {
            if let Some(bbox) = span_median_box(&t.path, lo, hi) {
                present.push(SpeakerTrack { bbox, ..t.clone() });
            }
        }
    }
    if present.is_empty() {
        group_layout(tracks, src_w, src_h)
    } else {
        group_layout(&present, src_w, src_h)
    }
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
/// most-persistent face (AutoFace) or everyone (Group), from the same
/// analysis. AutoFace is one crop for the whole clip, so it is sized over the
/// track's **entire path** (the bobbing guard): a subject who leans and moves
/// stays inside it, framed a little wider, instead of being cropped mid-face.
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
        Some(t) => Layout::FullFrame {
            crop: static_span_crop(t, 0, t.path.len(), src_w, src_h)
                .unwrap_or_else(|| solo_crop(&t.bbox, src_w, src_h)),
        },
        None => Layout::FullFrame { crop: crate::centered_fullcam_crop(src_w, src_h) },
    }
}

// --- camera plan audit: catch jitter-class defects BEFORE an export ---------

/// Camera creep: within one shot, the crop must not move more than this per
/// audit window while the subject moved less than half of it — a camera in
/// sustained micro-motion over a still subject is the defect the operator
/// reads as jitter.
const AUDIT_CREEP_CAM_PX: f32 = 4.0;
/// ...and the creep must persist this long to report (a single window is
/// detector noise).
const AUDIT_CREEP_MIN_S: f64 = 2.0;
/// Subject-adrift: the subject's detected center must not sit outside the
/// crop's safe region (the [`REUSE_GUARD_X_FH`]/[`REUSE_GUARD_Y_FH`] insets)
/// for longer than this.
const AUDIT_ADRIFT_MIN_S: f64 = 1.0;

/// Audit a camera plan against the subject evidence: find stretches where
/// the CAMERA does something the SUBJECT didn't cause — the defect class the
/// operator reads as jitter. Three checks, each born from a shipped bug
/// (ADR 0038, 2026-07-05):
///
/// - **camera creep**: a panning shot whose crop keeps moving while its
///   subject is still (the 30–41 s wander-crawl and 22–27 s lunge-chase);
/// - **re-frame without cause**: consecutive same-seat solo shots whose crop
///   jumps position or zoom while the subject's measured geometry barely
///   changed (the jump-cut twitch the framing memory kills — audited so any
///   future planner path that regresses it is caught);
/// - **subject adrift**: a subject riding outside a crop's safe region for
///   a sustained stretch (a mis-parked or under-grown framing).
///
/// Pure and cheap (O(shots × bins)). The diag harness prints it per plan,
/// `AnalyzeSpeakers` logs each finding, and the Studio Camera panel shows
/// them before an export — "detect first" (operator ask, 2026-07-05). Zero
/// findings on the production fixtures is a regression bar; a finding on
/// new footage means the plan would render with a visible camera defect.
pub fn audit_camera_plan(analysis: &SpeakerAnalysis, plan: &CameraPlan) -> Vec<String> {
    let bin_s = if analysis.bin_s > 0.0 { analysis.bin_s } else { 1.0 / SPEAKER_FPS };
    let n = analysis.speaking.len();
    let mut findings = Vec::new();
    let track = |id: usize| analysis.tracks.iter().find(|t| t.id == id);
    let center = |id: usize, b: usize| -> Option<(f32, f32)> {
        track(id)?.path.get(b)?.as_ref().map(|f| (f.cx(), f.cy()))
    };
    let med2 = |pts: &[(f32, f32)]| -> (f32, f32) {
        let m = |mut v: Vec<f32>| -> f32 {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            v[v.len() / 2]
        };
        (m(pts.iter().map(|p| p.0).collect()), m(pts.iter().map(|p| p.1).collect()))
    };

    // Previous solo shot's (track, end_s, closing crop, subject stats) for
    // the re-frame check.
    let mut prev: Option<(usize, f64, Crop, (f32, f32, f32))> = None;
    for s in &plan.shots {
        let (Some(id), Layout::FullFrame { crop }) = (s.track, &s.layout) else {
            prev = None;
            continue;
        };
        let b0 = ((s.start_s / bin_s).round() as usize).min(n);
        let b1 = ((s.end_s / bin_s).round() as usize).clamp(b0, n);
        let Some(tr) = track(id) else { continue };
        let subj = match (span_median_box(&tr.path, b0, b1), span_center_band(&tr.path, b0, b1)) {
            (Some(med), Some((xl, xh, yl, yh))) => {
                ((xl + xh) * 0.5, (yl + yh) * 0.5, med.h)
            }
            _ => {
                prev = None;
                continue;
            }
        };
        let dur = (s.end_s - s.start_s).max(1e-6);
        let crop_at = |t: f64| -> Crop {
            match &s.pan_to {
                Some(p) => crop.lerp(p, (((t - s.start_s) / dur) as f32).clamp(0.0, 1.0)),
                None => *crop,
            }
        };

        // --- camera creep (pan shots only; a static crop cannot creep).
        if s.pan_to.is_some() {
            let win = ((1.0 / bin_s).round() as usize).max(2);
            let step = (win / 2).max(1);
            let mut flagged: Vec<(f64, f64)> = Vec::new();
            let mut b = b0;
            while b + win <= b1 {
                let (t0, t1) = (b as f64 * bin_s, (b + win) as f64 * bin_s);
                let det: Vec<(f32, f32)> = (b..b + win).filter_map(|k| center(id, k)).collect();
                if det.len() * 2 >= win {
                    let q = (det.len() / 4).max(1);
                    let a = med2(&det[..q]);
                    let z = med2(&det[det.len() - q..]);
                    let subj_d = ((z.0 - a.0).powi(2) + (z.1 - a.1).powi(2)).sqrt();
                    let (c0, c1) = (crop_at(t0), crop_at(t1));
                    let cam_d = ((c1.x - c0.x).powi(2) + (c1.y - c0.y).powi(2)).sqrt();
                    if cam_d >= AUDIT_CREEP_CAM_PX && subj_d < cam_d * 0.5 {
                        match flagged.last_mut() {
                            Some((_, e)) if *e >= t0 => *e = t1,
                            _ => flagged.push((t0, t1)),
                        }
                    }
                }
                b += step;
            }
            for (t0, t1) in flagged {
                if t1 - t0 >= AUDIT_CREEP_MIN_S {
                    findings.push(format!(
                        "{t0:.1}-{t1:.1}s: camera creeps over a still {} (crop glides while the subject isn't moving)",
                        track_label(id)
                    ));
                }
            }
        }

        // --- re-frame without cause at a boundary (consecutive same seat).
        if let Some((pid, pend, pcrop, (px, py, pfh))) = &prev {
            if *pid == id && (s.start_s - pend).abs() < 0.05 {
                let (sx, sy, fh) = subj;
                let zoom_pop = (crop.h - pcrop.h).abs() > 0.10 * pcrop.h;
                let pos_jump = {
                    let (ccx, ccy) = (crop.x + crop.w * 0.5, crop.y + crop.h * 0.5);
                    let (pcx, pcy) = (pcrop.x + pcrop.w * 0.5, pcrop.y + pcrop.h * 0.5);
                    ((ccx - pcx).powi(2) + (ccy - pcy).powi(2)).sqrt() > 0.20 * pcrop.w
                };
                let cause = ((sx - px).powi(2) + (sy - py).powi(2)).sqrt()
                    > REUSE_CENTER_FH * pfh
                    || (fh - pfh).abs() > REUSE_H_FRAC * pfh;
                if (zoom_pop || pos_jump) && !cause {
                    findings.push(format!(
                        "{:.1}s: crop re-frames on {} without subject cause (zoom {:+.0}%, position {:.0} px)",
                        s.start_s,
                        track_label(id),
                        100.0 * (crop.h - pcrop.h) / pcrop.h.max(1.0),
                        {
                            let (ccx, ccy) = (crop.x + crop.w * 0.5, crop.y + crop.h * 0.5);
                            let (pcx, pcy) = (pcrop.x + pcrop.w * 0.5, pcrop.y + pcrop.h * 0.5);
                            ((ccx - pcx).powi(2) + (ccy - pcy).powi(2)).sqrt()
                        }
                    ));
                }
            }
        }

        // --- subject adrift: outside the crop's safe region, sustained.
        let (ix, iy) = (REUSE_GUARD_X_FH * subj.2, REUSE_GUARD_Y_FH * subj.2);
        let mut out_start: Option<f64> = None;
        for b in b0..=b1 {
            let t = (b as f64 * bin_s).min(s.end_s);
            let inside = (b < b1)
                .then(|| center(id, b))
                .flatten()
                .map(|(cx, cy)| {
                    let c = crop_at(t);
                    cx >= c.x + ix && cx <= c.x + c.w - ix && cy >= c.y + iy && cy <= c.y + c.h - iy
                });
            match inside {
                Some(false) if out_start.is_none() => out_start = Some(t),
                Some(true) | None if b == b1 => {}
                Some(true) => {
                    if let Some(t0) = out_start.take() {
                        if t - t0 >= AUDIT_ADRIFT_MIN_S {
                            findings.push(format!(
                                "{t0:.1}-{t:.1}s: {} rides outside the crop's safe region",
                                track_label(id)
                            ));
                        }
                    }
                }
                _ => {}
            }
            if b == b1 {
                if let Some(t0) = out_start.take() {
                    if t - t0 >= AUDIT_ADRIFT_MIN_S {
                        findings.push(format!(
                            "{t0:.1}-{t:.1}s: {} rides outside the crop's safe region",
                            track_label(id)
                        ));
                    }
                }
            }
        }

        prev = Some((id, s.end_s, s.pan_to.unwrap_or(*crop), subj));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fb(x: f32, y: f32, w: f32, h: f32) -> FaceBox {
        FaceBox { x, y, w, h, score: 0.9 }
    }

    /// Bins spanning `secs` at the analysis rate — keeps tests fps-agnostic, so
    /// changing [`SPEAKER_FPS`] never silently invalidates a bin-count constant.
    fn nbins(secs: f64) -> usize {
        (secs * SPEAKER_FPS).round() as usize
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
        for i in 0..nbins(2.0) {
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
    fn a_short_real_framing_survives_a_long_clip() {
        // A face on screen ~2 s (10 bins) inside a 60 s clip (300 bins) is only
        // ~3 % of frames — a real multicam wide-shot the old 20 %-of-clip gate
        // wrongly dropped (leaving the camera nothing to follow, the blank crop).
        // The absolute-duration gate keeps it.
        let (w, h) = (640usize, 360usize);
        let rgb = frame_with_patch(w, h, None);
        let mut b = TrackBuilder::new(w as f32, h as f32, w, h);
        let face = fb(300.0, 100.0, 90.0, 90.0);
        let (on, off) = (nbins(8.0), nbins(10.0)); // a 2 s framing
        for i in 0..nbins(20.0) {
            if (on..off).contains(&i) {
                b.observe(&[face], &rgb);
            } else {
                b.observe(&[], &rgb);
            }
        }
        let tracks = b.finish();
        assert_eq!(tracks.len(), 1, "a 2 s framing must survive a 20 s clip");
        assert_eq!(tracks[0].path.iter().filter(|p| p.is_some()).count(), off - on);
    }

    #[test]
    fn transient_faces_are_dropped() {
        let (w, h) = (640usize, 360usize);
        let stat = fb(80.0, 100.0, 80.0, 80.0);
        let rgb = frame_with_patch(w, h, None);
        let mut b = TrackBuilder::new(w as f32, h as f32, w, h);
        for i in 0..nbins(3.0) {
            if i == 3 {
                // A one-frame false positive far away.
                b.observe(&[stat, fb(500.0, 250.0, 60.0, 60.0)], &rgb);
            } else {
                b.observe(&[stat], &rgb);
            }
        }
        let tracks = b.finish();
        assert_eq!(tracks.len(), 1, "the one-frame face must not survive");
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

    // A static-wide-shot track: face at a fixed position, visible every bin.
    fn track(id: usize, x: f32, activity: Vec<f32>) -> SpeakerTrack {
        let path = (0..activity.len()).map(|_| Some(fb(x, 100.0, 80.0, 80.0))).collect();
        SpeakerTrack { id, bbox: fb(x, 100.0, 80.0, 80.0), presence: 1.0, activity, path }
    }

    #[test]
    fn attribution_follows_the_moving_mouth_with_hysteresis() {
        let n = nbins(10.0);
        let half = n / 2;
        // A talks the first half, B the second.
        let a_act: Vec<f32> = (0..n).map(|i| if i < half { 0.05 } else { 0.002 }).collect();
        let b_act: Vec<f32> = (0..n).map(|i| if i < half { 0.002 } else { 0.05 }).collect();
        let tracks = vec![track(0, 100.0, a_act), track(1, 500.0, b_act)];
        let (speaking, conf) = attribute_speakers(&tracks, &vec![true; n]);
        assert_eq!(speaking[nbins(1.0)], Some(0));
        assert_eq!(speaking[n - nbins(1.0)], Some(1));
        // The switch commits only after the confirm hold, not instantly.
        let confirm = nbins(SWITCH_CONFIRM_S);
        assert_eq!(speaking[half], Some(0), "switch must not be instant");
        assert_eq!(speaking[half + confirm + 2], Some(1), "switch commits after the hold");
        assert!(conf[nbins(1.0)] > 0.8, "confident when one mouth dominates");
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

    /// Drive a [`TrackBuilder`] with one face per frame at the given center xs
    /// (None = the face is undetected that frame), plus an optional always-on
    /// context face — the same-seat merge fixtures.
    fn built_tracks(seat_xs: &[Option<f32>], context_x: Option<f32>) -> Vec<SpeakerTrack> {
        let (fw, fh) = (64usize, 36usize);
        let rgb = frame_with_patch(fw, fh, None);
        let mut b = TrackBuilder::new(1920.0, 1080.0, fw, fh);
        for x in seat_xs {
            let mut faces = Vec::new();
            if let Some(cx) = x {
                faces.push(fb(cx - 85.0, 300.0, 170.0, 170.0));
            }
            if let Some(cx) = context_x {
                faces.push(fb(cx - 85.0, 280.0, 170.0, 170.0));
            }
            b.observe(&faces, &rgb);
        }
        b.finish()
    }

    /// One person drifting 600→780 px, an occlusion gap, then re-detected back
    /// at 615 px: the last-seen match radius mints a SECOND track (the Deddy
    /// fragmentation). The xs series shared by the two merge tests.
    fn split_seat_xs() -> Vec<Option<f32>> {
        let mut xs: Vec<Option<f32>> = Vec::new();
        for i in 0..nbins(5.0) {
            xs.push(Some(600.0 + 180.0 * i as f32 / nbins(5.0) as f32));
        }
        for _ in 0..6 {
            xs.push(None); // the occlusion gap
        }
        for _ in 0..nbins(5.0) {
            xs.push(Some(615.0)); // back near the seat: > match radius from 780
        }
        xs
    }

    #[test]
    fn same_seat_fragments_merge_when_other_people_share_the_frame() {
        // A two-person framing (a context face is always on screen), so the
        // near + alternating pair must reunite into ONE seat track — the
        // Deddy failure was 2 people becoming 4 tracks, splitting the mouth
        // signal and drawing twin labels on one head.
        let tracks = built_tracks(&split_seat_xs(), Some(1400.0));
        assert_eq!(tracks.len(), 2, "seat + context, not fragments: {tracks:?}");
        let seat = tracks
            .iter()
            .find(|t| t.bbox.cx() < 1000.0)
            .expect("left seat track");
        let vis = seat.path.iter().filter(|p| p.is_some()).count();
        assert!(vis >= nbins(9.5), "merged seat spans both fragments: {vis} bins");
    }

    #[test]
    fn solo_camera_alternation_does_not_merge() {
        // The same near + alternating pair with NOBODY else on screen — the
        // signature of a solo-camera multicam edit (Leon/ANTITESA), where two
        // near-positioned alternating tracks are two DIFFERENT people's
        // framings. Merging here would fuse two people into one subject.
        let tracks = built_tracks(&split_seat_xs(), None);
        assert_eq!(tracks.len(), 2, "solo framings stay separate: {tracks:?}");
        let max_vis = tracks
            .iter()
            .map(|t| t.path.iter().filter(|p| p.is_some()).count())
            .max()
            .unwrap();
        assert!(max_vis <= nbins(5.5), "no track absorbed the other: {max_vis} bins");
    }

    #[test]
    fn attribution_shots_reframe_at_the_sources_own_cuts() {
        // Static wide regime (both faces always visible), one speaker
        // throughout — but the source is itself an edit: a cut at 5.0 s moves
        // the speaker's on-screen position 120 px (a new angle). The plan must
        // re-frame AT the cut (two shots, same subject, different crops)
        // instead of averaging one crop/glide across both angles (the Deddy
        // drifting-camera + cropped-face failure).
        let n = nbins(12.0);
        let cut_bin = nbins(5.0);
        let a_path: Vec<Option<FaceBox>> = (0..n)
            .map(|i| {
                Some(if i < cut_bin {
                    fb(325.0, 325.0, 150.0, 150.0) // cx 400
                } else {
                    fb(445.0, 325.0, 150.0, 150.0) // cx 520: the angle jump
                })
            })
            .collect();
        let b_path: Vec<Option<FaceBox>> = (0..n).map(|_| Some(fb(1325.0, 325.0, 150.0, 150.0))).collect();
        let ta = SpeakerTrack { id: 0, bbox: fb(325.0, 325.0, 150.0, 150.0), presence: 1.0, activity: vec![0.0; n], path: a_path };
        let tb = SpeakerTrack { id: 1, bbox: fb(1325.0, 325.0, 150.0, 150.0), presence: 1.0, activity: vec![0.0; n], path: b_path };
        let plan =
            plan_shots(&analysis(vec![Some(0); n], vec![ta, tb]), 1920.0, 1080.0, 12.0, &[5.0]);
        assert_eq!(plan.shots.len(), 2, "one piece per angle: {plan:?}");
        assert!((plan.shots[0].end_s - 5.0).abs() < 1e-9, "re-frame exactly at the cut");
        assert_eq!((plan.shots[0].track, plan.shots[1].track), (Some(0), Some(0)));
        let crop_x = |s: &Shot| match &s.layout {
            Layout::FullFrame { crop } => crop.x,
            other => panic!("solo piece: {other:?}"),
        };
        let dx = crop_x(&plan.shots[1]) - crop_x(&plan.shots[0]);
        assert!(dx > 60.0, "each piece framed on its own angle's position: dx {dx}");
        assert!(
            plan.shots.iter().all(|s| s.pan_to.is_none()),
            "static positions per angle must not glide: {plan:?}"
        );
    }

    #[test]
    fn a_cut_hugging_the_shot_edge_does_not_make_a_sliver_piece() {
        let n = nbins(12.0);
        let path: Vec<Option<FaceBox>> = (0..n).map(|_| Some(fb(325.0, 325.0, 150.0, 150.0))).collect();
        let ta = SpeakerTrack { id: 0, bbox: fb(325.0, 325.0, 150.0, 150.0), presence: 1.0, activity: vec![0.0; n], path: path.clone() };
        let tb = SpeakerTrack { id: 1, bbox: fb(1325.0, 325.0, 150.0, 150.0), presence: 1.0, activity: vec![0.0; n], path };
        let plan =
            plan_shots(&analysis(vec![Some(0); n], vec![ta, tb]), 1920.0, 1080.0, 12.0, &[0.2]);
        assert_eq!(plan.shots.len(), 1, "a 0.2 s sliver folds into the shot: {plan:?}");
    }

    /// A subject path built from back-to-back camera visits: the face sits at
    /// (center x, face height) for `secs` per visit, with a source cut between
    /// visits. Returns the path, the cut times, and the total duration.
    fn visits_path(visits: &[(f32, f32, f64)]) -> (Vec<Option<FaceBox>>, Vec<f64>, f64) {
        let mut path = Vec::new();
        let mut cuts = Vec::new();
        let mut t = 0.0f64;
        for &(cx, h, secs) in visits {
            for _ in 0..nbins(secs) {
                path.push(Some(fb(cx - h * 0.5, 325.0, h, h)));
            }
            t += secs;
            cuts.push(t);
        }
        cuts.pop();
        (path, cuts, t)
    }

    /// Attribution-regime plan for one speaker on `path`: a context face keeps
    /// both seats always visible (static-wide regime), the given source cuts
    /// split the speaker's run into angle pieces.
    fn plan_for_visits(path: Vec<Option<FaceBox>>, cuts: &[f64], dur: f64) -> CameraPlan {
        let n = path.len();
        let med = median_box(&path.iter().flatten().copied().collect::<Vec<_>>());
        let ta = SpeakerTrack { id: 0, bbox: med, presence: 1.0, activity: vec![0.0; n], path };
        let tb = SpeakerTrack {
            id: 1,
            bbox: fb(1500.0, 325.0, 150.0, 150.0),
            presence: 1.0,
            activity: vec![0.0; n],
            path: (0..n).map(|_| Some(fb(1500.0, 325.0, 150.0, 150.0))).collect(),
        };
        plan_shots(&analysis(vec![Some(0); n], vec![ta, tb]), 1920.0, 1080.0, dur, cuts)
    }

    fn solo_crop_of(plan: &CameraPlan, i: usize) -> Crop {
        match &plan.shots[i].layout {
            Layout::FullFrame { crop } => *crop,
            other => panic!("expected solo piece, got {other:?}"),
        }
    }

    #[test]
    fn a_jump_cut_back_to_the_same_camera_reuses_the_framing() {
        // A-B-A camera alternation while one seat keeps talking: the return
        // piece must reuse the FIRST piece's crop VERBATIM (the measured
        // jump-cut twitch: re-deriving each piece from its own bins re-zoomed
        // 894->702->884 px on Deddy), while the middle piece — a real angle
        // change, new position AND face size — re-frames fully.
        let (path, cuts, dur) =
            visits_path(&[(400.0, 150.0, 4.0), (900.0, 100.0, 4.0), (404.0, 152.0, 4.0)]);
        let plan = plan_for_visits(path, &cuts, dur);
        assert_eq!(plan.shots.len(), 3, "{plan:?}");
        let (c0, c1, c2) = (solo_crop_of(&plan, 0), solo_crop_of(&plan, 1), solo_crop_of(&plan, 2));
        assert_eq!(c0, c2, "the return to the first camera reuses its framing verbatim");
        assert!((c1.h - c0.h).abs() > 100.0, "the real angle change re-frames: {c0:?} vs {c1:?}");
    }

    #[test]
    fn a_real_zoom_change_refuses_the_remembered_framing() {
        // Same seat position, but the source cut to a tighter framing (face
        // height +33%): the height gate must re-frame — reusing the wide crop
        // would shrink a face the source meant to be big.
        let (path, cuts, dur) = visits_path(&[(400.0, 150.0, 4.0), (400.0, 200.0, 4.0)]);
        let plan = plan_for_visits(path, &cuts, dur);
        assert_eq!(plan.shots.len(), 2, "{plan:?}");
        let (c0, c1) = (solo_crop_of(&plan, 0), solo_crop_of(&plan, 1));
        assert!(c1.h > c0.h * 1.2, "a zoomed source piece frames fresh: {c0:?} vs {c1:?}");
    }

    #[test]
    fn a_lean_past_the_deadzone_moves_the_camera_but_not_the_zoom() {
        // The subject re-appears 80 px away (past the 0.30 fh dead-zone) at
        // the same face height: the piece re-places the REMEMBERED size on the
        // new position — a lean moves the camera, never the zoom.
        let (path, cuts, dur) = visits_path(&[(400.0, 150.0, 4.0), (480.0, 150.0, 4.0)]);
        let plan = plan_for_visits(path, &cuts, dur);
        assert_eq!(plan.shots.len(), 2, "{plan:?}");
        let (c0, c1) = (solo_crop_of(&plan, 0), solo_crop_of(&plan, 1));
        assert_eq!((c1.w, c1.h), (c0.w, c0.h), "size locks to the remembered framing");
        assert!(c1.x > c0.x + 40.0, "position follows the subject: {c0:?} -> {c1:?}");
    }

    #[test]
    fn fixed_anchors_earn_one_reframe_per_real_drift() {
        // A slow slide in sub-dead-zone steps (25 px per piece, dead-zone
        // 45 px): fixed anchors accumulate delta against the ORIGINAL anchor,
        // so the camera reuses, re-frames ONCE when the drift becomes real,
        // then holds the new anchor — a rolling baseline would never re-frame
        // and let the face creep out of the crop.
        let (path, cuts, dur) = visits_path(&[
            (400.0, 150.0, 4.0),
            (425.0, 150.0, 4.0),
            (450.0, 150.0, 4.0),
            (475.0, 150.0, 4.0),
        ]);
        let plan = plan_for_visits(path, &cuts, dur);
        assert_eq!(plan.shots.len(), 4, "{plan:?}");
        let crops: Vec<Crop> = (0..4).map(|i| solo_crop_of(&plan, i)).collect();
        assert_eq!(crops[0], crops[1], "25 px sits inside the dead-zone: reuse");
        assert_ne!(crops[1], crops[2], "50 px of accumulated drift re-frames once");
        assert_eq!(crops[2], crops[3], "the new anchor holds again");
    }

    #[test]
    fn a_panning_piece_anchors_its_closing_framing() {
        // Piece 1 follows a real drift (a pan) and the subject then holds the
        // new spot: the NEXT piece must reuse the pan's CLOSING crop verbatim
        // (measured on Deddy: the return piece sat 16 px from the pan's
        // closing crop but 79 px from its opening).
        let mut path: Vec<Option<FaceBox>> = Vec::new();
        for _ in 0..nbins(4.0) {
            path.push(Some(fb(400.0 - 75.0, 325.0, 150.0, 150.0)));
        }
        let drift = nbins(2.0);
        for i in 0..drift {
            let cx = 400.0 + 300.0 * i as f32 / drift as f32;
            path.push(Some(fb(cx - 75.0, 325.0, 150.0, 150.0)));
        }
        for _ in 0..nbins(2.0) + nbins(4.0) {
            path.push(Some(fb(700.0 - 75.0, 325.0, 150.0, 150.0)));
        }
        let plan = plan_for_visits(path, &[4.0, 8.0], 12.0);
        assert_eq!(plan.shots.len(), 3, "{plan:?}");
        let pan_close = plan.shots[1].pan_to.expect("the drifting piece pans");
        assert_eq!(solo_crop_of(&plan, 2), pan_close, "the return reuses the pan's closing crop");
    }

    #[test]
    fn audit_flags_a_creeping_pan() {
        // The 30-41 s class: a crop gliding for seconds over a still subject.
        let n = nbins(8.0);
        let a = analysis(vec![Some(0); n], vec![track_with_path(0, &vec![600.0; n])]);
        let crop = Crop { x: 448.0, y: 52.0, w: 304.0, h: 540.0 };
        let plan = CameraPlan {
            shots: vec![Shot {
                start_s: 0.0,
                end_s: 8.0,
                track: Some(0),
                layout: Layout::FullFrame { crop },
                pan_to: Some(Crop { x: 528.0, ..crop }),
            }],
        };
        let f = audit_camera_plan(&a, &plan);
        assert!(f.iter().any(|l| l.contains("creeps")), "{f:?}");
    }

    #[test]
    fn audit_flags_a_zoom_pop_without_cause() {
        // The jump-cut-twitch class: adjacent same-seat shots re-zooming while
        // the subject's measured geometry is unchanged.
        let n = nbins(8.0);
        let a = analysis(vec![Some(0); n], vec![track_with_path(0, &vec![600.0; n])]);
        let c1 = Crop { x: 448.0, y: 52.0, w: 304.0, h: 540.0 };
        let c2 = Crop { x: 411.0, y: 2.0, w: 378.0, h: 672.0 }; // +24% zoom, same seat
        let plan = CameraPlan {
            shots: vec![
                Shot {
                    start_s: 0.0,
                    end_s: 4.0,
                    track: Some(0),
                    layout: Layout::FullFrame { crop: c1 },
                    pan_to: None,
                },
                Shot {
                    start_s: 4.0,
                    end_s: 8.0,
                    track: Some(0),
                    layout: Layout::FullFrame { crop: c2 },
                    pan_to: None,
                },
            ],
        };
        let f = audit_camera_plan(&a, &plan);
        assert!(f.iter().any(|l| l.contains("without subject cause")), "{f:?}");
    }

    #[test]
    fn audit_passes_clean_static_and_true_follow_plans() {
        let n = nbins(8.0);
        // A still subject inside a contained static crop: clean.
        let a = analysis(vec![Some(0); n], vec![track_with_path(0, &vec![600.0; n])]);
        let crop = Crop { x: 448.0, y: 52.0, w: 304.0, h: 540.0 };
        let plan = CameraPlan {
            shots: vec![Shot {
                start_s: 0.0,
                end_s: 8.0,
                track: Some(0),
                layout: Layout::FullFrame { crop },
                pan_to: None,
            }],
        };
        assert!(audit_camera_plan(&a, &plan).is_empty(), "{:?}", audit_camera_plan(&a, &plan));
        // A subject genuinely travelling WITH the camera: also clean.
        let xs: Vec<f32> = (0..n).map(|i| 400.0 + 400.0 * i as f32 / n as f32).collect();
        let a = analysis(vec![Some(0); n], vec![track_with_path(0, &xs)]);
        let c0 = Crop { x: 248.0, y: 52.0, w: 304.0, h: 540.0 };
        let plan = CameraPlan {
            shots: vec![Shot {
                start_s: 0.0,
                end_s: 8.0,
                track: Some(0),
                layout: Layout::FullFrame { crop: c0 },
                pan_to: Some(Crop { x: 648.0, ..c0 }),
            }],
        };
        assert!(audit_camera_plan(&a, &plan).is_empty(), "{:?}", audit_camera_plan(&a, &plan));
    }

    #[test]
    fn shots_cut_between_speakers_and_respect_min_length() {
        // 20 s clip: A for 8 s, B for 12 s.
        let n = nbins(20.0);
        let speaking: Vec<Option<usize>> =
            (0..n).map(|i| Some(if i < nbins(8.0) { 0 } else { 1 })).collect();
        let tracks = vec![track(0, 200.0, vec![0.0; n]), track(1, 1400.0, vec![0.0; n])];
        let plan = plan_shots(&analysis(speaking, tracks), 1920.0, 1080.0, 20.0, &[]);
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
        // A talks 0..8 s; a brief flicker to B at 4 s; silence (None) 8..12 s
        // holds A; then B 12..20 s.
        let n = nbins(20.0);
        let mut speaking: Vec<Option<usize>> = (0..n)
            .map(|i| {
                if i < nbins(8.0) {
                    Some(0)
                } else if i < nbins(12.0) {
                    None
                } else {
                    Some(1)
                }
            })
            .collect();
        for b in nbins(4.0)..nbins(4.0) + 2 {
            speaking[b] = Some(1);
        }
        let tracks = vec![track(0, 200.0, vec![0.0; n]), track(1, 1400.0, vec![0.0; n])];
        let plan = plan_shots(&analysis(speaking, tracks), 1920.0, 1080.0, 20.0, &[]);
        assert_eq!(plan.shots.len(), 2, "flicker + silence must not add shots: {plan:?}");
        assert_eq!(plan.shots[0].track, Some(0));
        // Silence held A: the cut lands at 12 s, not 8 s.
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
        let plan = plan_shots(&analysis(speaking, tracks), 1920.0, 1080.0, 30.0, &[]);
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

    /// A track whose face sits at `xs[i]` (y fixed) in bin i.
    fn track_with_path(id: usize, xs: &[f32]) -> SpeakerTrack {
        let path: Vec<Option<FaceBox>> =
            xs.iter().map(|x| Some(fb(*x - 40.0, 300.0, 80.0, 80.0))).collect();
        SpeakerTrack {
            id,
            bbox: median_box(&path.iter().flatten().copied().collect::<Vec<_>>()),
            presence: 1.0,
            activity: vec![0.0; xs.len()],
            path,
        }
    }

    #[test]
    fn moving_face_stays_on_its_track() {
        // One face gliding steadily across half the frame: last-seen matching
        // must keep it a single track (all-time-mean matching sheds it).
        let (w, h) = (640usize, 360usize);
        let rgb = frame_with_patch(w, h, None);
        let mut b = TrackBuilder::new(w as f32, h as f32, w, h);
        for i in 0..60 {
            let x = 60.0 + i as f32 * 6.0; // 360 px total drift, ~7 px/frame
            b.observe(&[fb(x, 100.0, 80.0, 80.0)], &rgb);
        }
        let tracks = b.finish();
        assert_eq!(tracks.len(), 1, "a moving face must not split into tracks");
        assert!((tracks[0].presence - 1.0).abs() < 1e-6);
    }

    #[test]
    fn shots_frame_the_subject_where_they_are_during_the_shot() {
        // A sits at x=300 while talking (first 8 s), then wanders to x=900
        // while B talks. A's shot must frame x~300 — the in-shot position —
        // not the whole-clip median pulled toward 900.
        let n = nbins(20.0);
        let cut = nbins(8.0);
        let a_xs: Vec<f32> = (0..n).map(|i| if i < cut { 300.0 } else { 900.0 }).collect();
        let b_xs: Vec<f32> = (0..n).map(|_| 1500.0).collect();
        let speaking: Vec<Option<usize>> =
            (0..n).map(|i| Some(if i < cut { 0 } else { 1 })).collect();
        let tracks = vec![track_with_path(0, &a_xs), track_with_path(1, &b_xs)];
        let plan = plan_shots(&analysis(speaking, tracks), 1920.0, 1080.0, 20.0, &[]);
        assert_eq!(plan.shots.len(), 2);
        match &plan.shots[0].layout {
            Layout::FullFrame { crop } => {
                let cx = crop.x + crop.w * 0.5;
                assert!((cx - 300.0).abs() < 60.0, "framed at {cx}, want ~300");
            }
            other => panic!("expected solo, got {other:?}"),
        }
    }

    #[test]
    fn drift_beyond_the_deadzone_becomes_a_follow_pan() {
        // STATIC WIDE shot (both always visible -> attribution path): the
        // speaker (track 0) holds the floor while sliding x=300 -> x=900; the
        // shot follows with a same-size pan instead of losing them.
        let n = 100;
        let xs: Vec<f32> = (0..n).map(|i| 300.0 + i as f32 * 6.0).collect();
        let tracks = vec![track_with_path(0, &xs), track_with_path(1, &vec![1500.0; n])];
        let plan = plan_shots(&analysis(vec![Some(0); n], tracks), 1920.0, 1080.0, 20.0, &[]);
        assert_eq!(plan.shots.len(), 1);
        let shot = &plan.shots[0];
        let pan = shot.pan_to.expect("drift must engage the follow pan");
        let (Layout::FullFrame { crop }) = &shot.layout else { panic!("solo") };
        assert_eq!(pan.w, crop.w, "pan must not resize the crop");
        assert_eq!(pan.h, crop.h);
        assert!(pan.x > crop.x + 100.0, "pan moves toward the drift: {} -> {}", crop.x, pan.x);
    }

    #[test]
    fn an_out_and_back_excursion_holds_a_static_frame_not_a_glide() {
        // The subject lunges 300 px left and comes most of the way BACK
        // within one shot (the Deddy 22-27 s excursion): a head->tail glide
        // models it as a slow leftward drift, so the camera slides away from
        // a subject who has already returned — the operator's "jitter to the
        // left". The excursion (band far wider than the drift) must refuse
        // the pan and hold ONE static crop containing the whole band.
        let n = nbins(6.0);
        let xs: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / n as f32;
                if t < 0.5 {
                    700.0 - 600.0 * t // out: 700 -> 400
                } else {
                    400.0 + 300.0 * (t - 0.5) // back: 400 -> 550
                }
            })
            .collect();
        let tracks = vec![track_with_path(0, &xs), track_with_path(1, &vec![1500.0; n])];
        let plan = plan_shots(&analysis(vec![Some(0); n], tracks), 1920.0, 1080.0, 6.0, &[]);
        assert_eq!(plan.shots.len(), 1);
        let shot = &plan.shots[0];
        assert!(shot.pan_to.is_none(), "an excursion must not glide: {shot:?}");
        let Layout::FullFrame { crop } = &shot.layout else { panic!("solo") };
        assert!(crop.w > 2.0 * 162.0, "the static crop grows to hold the excursion: {crop:?}");
        let c = crop.x + crop.w * 0.5;
        assert!((500.0..=580.0).contains(&c), "framed on the band center, got {c}");
    }

    #[test]
    fn a_wander_and_settle_holds_a_static_frame_not_a_crawl() {
        // The subject shifts +25 px, settles, shifts +25 px again, settles
        // (the Deddy 30-41 s shape): the net head->tail drift clears the
        // dead-zone and is monotonic, but there is no sustained travel — a
        // multi-second linear crawl over a mostly-still subject reads as
        // jitter. A modest static growth contains the whole band, so the
        // shot must hold ONE static crop, not glide.
        let n = nbins(8.0);
        let xs: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / n as f64 * 8.0;
                if t < 2.0 {
                    600.0
                } else if t < 2.5 {
                    600.0 + 50.0 * ((t - 2.0) / 0.5) as f32
                } else if t < 4.5 {
                    650.0
                } else {
                    650.0
                }
            })
            .collect();
        let tracks = vec![track_with_path(0, &xs), track_with_path(1, &vec![1500.0; n])];
        let plan = plan_shots(&analysis(vec![Some(0); n], tracks), 1920.0, 1080.0, 8.0, &[]);
        assert_eq!(plan.shots.len(), 1);
        let shot = &plan.shots[0];
        assert!(shot.pan_to.is_none(), "wander-and-settle must not crawl: {shot:?}");
        let Layout::FullFrame { crop } = &shot.layout else { panic!("solo") };
        assert!(crop.x + 60.0 <= 600.0, "home position inside: {crop:?}");
        assert!(crop.x + crop.w - 60.0 >= 650.0, "settled position inside: {crop:?}");
    }

    #[test]
    fn a_sway_inside_the_deadzone_stays_static() {
        // Static wide shot, the speaker only sways: no pan.
        let n = 100;
        let xs: Vec<f32> = (0..n).map(|i| 600.0 + ((i % 7) as f32 - 3.0) * 4.0).collect();
        let tracks = vec![track_with_path(0, &xs), track_with_path(1, &vec![1500.0; n])];
        let plan = plan_shots(&analysis(vec![Some(0); n], tracks), 1920.0, 1080.0, 20.0, &[]);
        assert_eq!(plan.shots.len(), 1);
        assert!(plan.shots[0].pan_to.is_none(), "a sway must stay static");
    }

    #[test]
    fn multicam_wide_shot_becomes_a_group_not_a_flicker() {
        // When the source shows two faces at once, the follow-visible subject
        // is a Group (a split screen), never a pick-the-largest flicker between
        // them — even as their detected sizes wobble frame to frame.
        let n = 30;
        let p0: Vec<Option<FaceBox>> =
            (0..n).map(|i| Some(fb(300.0, 300.0, 80.0, if i % 2 == 0 { 82.0 } else { 78.0 }))).collect();
        let p1: Vec<Option<FaceBox>> =
            (0..n).map(|i| Some(fb(1500.0, 300.0, 80.0, if i % 2 == 0 { 78.0 } else { 82.0 }))).collect();
        let t0 = SpeakerTrack { id: 0, bbox: fb(300.0, 300.0, 80.0, 80.0), presence: 1.0, activity: vec![0.0; n], path: p0 };
        let t1 = SpeakerTrack { id: 1, bbox: fb(1500.0, 300.0, 80.0, 80.0), presence: 1.0, activity: vec![0.0; n], path: p1 };
        let subj = subject_series(&[t0, t1], n);
        assert!(subj.iter().all(|s| *s == Subject::Group), "two visible faces = Group, got {subj:?}");
    }

    #[test]
    fn scene_cuts_are_the_exact_shot_boundaries() {
        // A visible until a source cut at 7.34 s (a time no 24 fps bin lands on),
        // B after. The plan must cut AT 7.34, not a rounded bin.
        let n = nbins(15.0);
        let cut_bin = nbins(7.34);
        let a_path: Vec<Option<FaceBox>> =
            (0..n).map(|i| (i < cut_bin).then(|| fb(260.0, 300.0, 90.0, 90.0))).collect();
        let b_path: Vec<Option<FaceBox>> =
            (0..n).map(|i| (i >= cut_bin).then(|| fb(1500.0, 300.0, 90.0, 90.0))).collect();
        let ta = SpeakerTrack { id: 0, bbox: fb(300.0, 300.0, 90.0, 90.0), presence: 0.5, activity: vec![0.0; n], path: a_path };
        let tb = SpeakerTrack { id: 1, bbox: fb(1500.0, 300.0, 90.0, 90.0), presence: 0.5, activity: vec![0.0; n], path: b_path };
        let plan = plan_shots(&analysis(vec![None; n], vec![ta, tb]), 1920.0, 1080.0, 15.0, &[7.34]);
        assert_eq!(plan.shots.len(), 2, "one shot per source segment: {plan:?}");
        assert!((plan.shots[0].end_s - 7.34).abs() < 1e-6, "cut on the exact frame, got {}", plan.shots[0].end_s);
        assert_eq!(plan.shots[0].track, Some(0));
        assert_eq!(plan.shots[1].track, Some(1));
    }

    #[test]
    fn a_scene_cut_that_changes_nothing_is_merged() {
        // A spurious cut mid-way through A's shot (A visible on both sides) must
        // not split it into two identical shots.
        let n = nbins(10.0);
        let path: Vec<Option<FaceBox>> = (0..n).map(|_| Some(fb(260.0, 300.0, 90.0, 90.0))).collect();
        let ta = SpeakerTrack { id: 0, bbox: fb(300.0, 300.0, 90.0, 90.0), presence: 1.0, activity: vec![0.0; n], path };
        let plan = plan_shots(&analysis(vec![None; n], vec![ta]), 1920.0, 1080.0, 10.0, &[5.0]);
        assert_eq!(plan.shots.len(), 1, "same subject across a cut = one shot: {plan:?}");
    }

    #[test]
    fn multicam_single_face_follows_and_cuts_cleanly() {
        // One face at a time (a multicam edit): follow it, cutting when the
        // source cuts to the other camera — no flicker, no group.
        let n = 40;
        let p0: Vec<Option<FaceBox>> =
            (0..n).map(|i| (i < 20).then(|| fb(300.0, 300.0, 90.0, 90.0))).collect();
        let p1: Vec<Option<FaceBox>> =
            (0..n).map(|i| (i >= 20).then(|| fb(1500.0, 300.0, 90.0, 90.0))).collect();
        let t0 = SpeakerTrack { id: 0, bbox: fb(300.0, 300.0, 90.0, 90.0), presence: 0.5, activity: vec![0.0; n], path: p0 };
        let t1 = SpeakerTrack { id: 1, bbox: fb(1500.0, 300.0, 90.0, 90.0), presence: 0.5, activity: vec![0.0; n], path: p1 };
        let subj = subject_series(&[t0, t1], n);
        assert!(subj[..20].iter().all(|s| *s == Subject::Track(0)), "first camera follows track 0");
        assert!(subj[20..].iter().all(|s| *s == Subject::Track(1)), "cut to track 1");
    }

    #[test]
    fn auto_face_static_crop_contains_a_mover() {
        // AutoFace = one static crop for the whole clip: a subject who moves
        // between x=500 and x=900 must stay inside it (framed wider), not be
        // cropped through the face at either end.
        let n = 100;
        let xs: Vec<f32> = (0..n).map(|i| if i < 50 { 500.0 } else { 900.0 }).collect();
        let t = track_with_path(0, &xs);
        match static_mode_layout(&[t], 1920.0, 1080.0, false) {
            Layout::FullFrame { crop } => {
                assert!(crop.x + 40.0 <= 500.0, "left face edge inside: {crop:?}");
                assert!(crop.x + crop.w - 40.0 >= 900.0, "right face edge inside: {crop:?}");
                assert!((crop.w / crop.h - 0.5625).abs() < 1e-2, "9:16 kept");
            }
            other => panic!("expected FullFrame, got {other:?}"),
        }
    }

    #[test]
    fn multicam_follows_the_visible_face_not_a_fixed_position() {
        // A finished multicam edit: person A (left) is on screen bins 0..40 in
        // his own camera, person B (right) bins 40..100 in his — never both at
        // once. The source already cut to its subject, so the plan must FOLLOW
        // the visible face. Attribution is deliberately wrong (all B) to prove
        // the multicam path ignores it and never parks on an off-screen crop.
        let n = 100;
        let a_path: Vec<Option<FaceBox>> =
            (0..n).map(|i| (i < 40).then(|| fb(260.0, 300.0, 90.0, 90.0))).collect();
        let b_path: Vec<Option<FaceBox>> =
            (0..n).map(|i| (i >= 40).then(|| fb(1500.0, 300.0, 90.0, 90.0))).collect();
        let ta = SpeakerTrack { id: 0, bbox: fb(300.0, 300.0, 90.0, 90.0), presence: 0.4, activity: vec![0.0; n], path: a_path };
        let tb = SpeakerTrack { id: 1, bbox: fb(1500.0, 300.0, 90.0, 90.0), presence: 0.6, activity: vec![0.0; n], path: b_path };
        let plan = plan_shots(&analysis(vec![Some(1); n], vec![ta, tb]), 1920.0, 1080.0, 20.0, &[]);
        assert_eq!(plan.shots.len(), 2, "one shot per visible person: {plan:?}");
        assert_eq!(plan.shots[0].track, Some(0), "opens on the visible left person");
        assert_eq!(plan.shots[1].track, Some(1));
        // A's shot frames A (left), NOT B's off-screen right position.
        let Layout::FullFrame { crop } = &plan.shots[0].layout else { panic!("solo") };
        assert!(crop.x + crop.w * 0.5 < 700.0, "A's shot must frame A (left): {crop:?}");
        let Layout::FullFrame { crop } = &plan.shots[1].layout else { panic!("solo") };
        assert!(crop.x + crop.w * 0.5 > 1200.0, "B's shot must frame B (right): {crop:?}");
    }

    #[test]
    fn printed_face_prop_is_dropped() {
        // A real speaker (large, talking) beside a book-cover face (~1/3 the
        // size, never moves): the prop is both smaller AND far less lively, so
        // it is filtered. It must neither frame nor inflate the scene test.
        let (w, h) = (640usize, 360usize);
        let real = fb(80.0, 90.0, 96.0, 96.0);
        let book = fb(500.0, 250.0, 30.0, 30.0);
        // The real speaker's mouth flickers (talking); the book stays constant.
        let mouth = (100usize, 145usize, 50usize, 36usize);
        let mut b = TrackBuilder::new(w as f32, h as f32, w, h);
        for i in 0..nbins(3.0) {
            let rgb = frame_with_patch(w, h, if i % 2 == 0 { Some(mouth) } else { None });
            b.observe(&[real, book], &rgb);
        }
        let tracks = b.finish();
        assert_eq!(tracks.len(), 1, "the small motionless prop must be dropped: {tracks:?}");
        assert!(tracks[0].bbox.w > 60.0, "the real (large) face survives");
    }

    #[test]
    fn a_short_first_run_folds_into_the_next_shot() {
        // B flashes for 2 bins (0.4 s) before A holds the floor: the camera
        // must open on A's shot, not a sub-minimum flash of B.
        let n = 100;
        let speaking: Vec<Option<usize>> =
            (0..n).map(|i| Some(if i < 2 { 1 } else { 0 })).collect();
        let tracks =
            vec![track(0, 200.0, vec![0.0; n]), track(1, 1400.0, vec![0.0; n])];
        let plan = plan_shots(&analysis(speaking, tracks), 1920.0, 1080.0, 20.0, &[]);
        assert_eq!(plan.shots.len(), 1, "flash open must fold: {plan:?}");
        assert_eq!(plan.shots[0].track, Some(0));
        assert!((plan.shots[0].start_s - 0.0).abs() < 1e-9);
    }
}
