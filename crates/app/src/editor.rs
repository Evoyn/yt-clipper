//! The Studio — the full-window video preview editor a Promote opens into
//! (focus 2026-07, growing the ADR 0012 nudge editor + ADR 0036 caption
//! preview into a CapCut-style page for Shorts):
//!
//! ```text
//! ┌──────────────────── toolbar: back · title · view toggle · Export ──┐
//! │ Captions        │        Video preview          │  Properties      │
//! │ (transcript     │  Original: full frame +       │  Camera mode     │
//! │  editor: edit / │  draggable 9:16 crop box,     │  Faces (A/B/C)   │
//! │  add / split /  │  face overlays                │  Caption presets │
//! │  merge / censor │  Preview: the 9:16 output +   │  + style knobs   │
//! │  / delete)      │  captions + safe area         │  Export summary  │
//! ├──── timeline: headers │ ruler · video · captions · speakers · cuts ┤
//! ```
//!
//! The timeline is directly editable (feature plan #2/#3): caption blocks
//! drag in time and trim at their edges, camera cut markers drag / split at
//! the playhead / delete via right-click — all with magnetic snapping. Edits
//! flow through the same truths as the panels (the transcript override, the
//! operator-overridable camera plan), so the render burns exactly what the
//! timeline shows.
//!
//! Plan #13 (ADR 0066) gave the strip its multi-track shell: a header column
//! (painted eye/lock toggles — eye gates the burn AND the preview together,
//! lock gates timeline gestures) and a zoom/scroll `Viewport` every t↔x
//! conversion routes through. AI artifacts and operator artifacts never
//! share a track (operator ruling 2026-07-13).
//!
//! All framing geometry stays the pure `yc_frame` layer; the speaker analysis
//! and camera plan come from the worker (`Progress::Speakers`); the caption
//! line model stays `yc_render`'s (the preview cannot drift from the burn-in,
//! ADR 0036). This module is only the egui surface.

use std::time::Instant;

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind};
use yc_core::{
    CameraMode, CameraPlan, CaptionEngine, CaptionGenre, CaptionPlacement, CaptionStyle,
    CaptionUnit, Crop, Layout, Shot, TimeRange, Transcript, CANVAS_H, CANVAS_W,
};
use yc_frame::speaker::{track_label, SpeakerAnalysis};
use yc_frame::FaceCluster;
use yc_render::{
    preview_lines, resolve_manual_placement, resolve_placement, word_states, PreviewLine,
    WordState,
};

use crate::player::PreviewPlayer;
use crate::presets::caption_presets;
use crate::theme;

/// Direct-manipulation floor: a caption unit never trims below this.
const CAP_MIN_S: f64 = 0.05;
/// A manually-edited camera shot never shrinks below this (a sub-perceptual
/// shot reads as a glitch frame, and the planner never emits one either).
const MIN_SHOT_S: f64 = 0.15;
/// A razor segment never shrinks below this — a shorter keep/remove span is
/// operator error, not intent.
const MIN_SEG_S: f64 = 0.1;
/// Pointer radius (points) within which a dragged time snaps to an anchor.
const SNAP_PX: f32 = 8.0;
/// The track header column's width (plan #13's Premiere/CapCut shape).
const HDR_W: f32 = 118.0;
/// The scrollbar row's height at the strip's bottom (ADR 0066).
const SCROLL_H: f32 = 8.0;

/// Which caption lane a timeline gesture edits: the pipeline's transcript
/// (auto) or the operator's own added captions — the dedicated track the
/// operator asked for (2026-07-12; plan #2's "manual alongside AI").
#[derive(Clone, Copy, PartialEq, Eq)]
enum CapLane {
    Auto,
    Manual,
}

/// One in-flight direct-manipulation gesture on the timeline strip. Tracked
/// by pointer position plus the ORIGINAL times (applied absolutely every
/// frame — no incremental drift), never by egui widget-id continuity: caption
/// blocks and razor segments reshape live while a gesture runs, so the widget
/// a gesture started on can vanish mid-drag without breaking the gesture.
#[derive(Clone)]
enum TimelineDrag {
    /// Move caption units together in time: one block, or a whole burn line
    /// via its rail (which can span BOTH lanes — targets are explicit).
    CapMove {
        targets: Vec<(CapLane, usize)>,
        /// Each target's (start_s, end_s) at drag start (same order).
        orig: Vec<(f64, f64)>,
        /// Timeline time under the pointer at drag start.
        grab_t: f64,
    },
    /// Trim a caption's onset (end stays).
    CapTrimStart { lane: CapLane, unit: usize, orig_start: f64, grab_t: f64 },
    /// Trim a caption's on-screen end.
    CapTrimEnd { lane: CapLane, unit: usize, orig_end: f64, grab_t: f64 },
    /// Move the camera cut between plan shots `boundary-1` and `boundary`.
    Cut { boundary: usize, orig_t: f64, grab_t: f64 },
    /// Move timeline razor cut `idx` (the boundary between two segments).
    Razor { idx: usize, orig_t: f64, grab_t: f64 },
    /// Move the razor's ▼ cut marker (the ✂←/→✂ reference point).
    Marker { orig_t: f64, grab_t: f64 },
}

/// The timeline viewport (plan #13, ADR 0066): horizontal zoom + scroll.
/// `zoom` is a factor over "the whole clip fits" (1 = fit; the max keeps at
/// least [`Self::MIN_SPAN_S`] visible); `left_t` is the clip time at the
/// lanes' left edge. Every strip t↔x conversion goes through it, so the
/// filmstrip, blocks, markers, and gestures all see one window. Pure and
/// unit-tested. Mid-gesture scroll/zoom is safe by construction: gestures
/// are pointer-tracked in TIME (`TimelineDrag`), re-derived from the
/// pointer's x each frame.
#[derive(Clone, Copy)]
struct Viewport {
    zoom: f64,
    left_t: f64,
}

impl Viewport {
    /// The floor a full zoom-in still shows — razor precision wants frames
    /// on screen, not a single one.
    const MIN_SPAN_S: f64 = 1.0;

    fn max_zoom(dur: f64) -> f64 {
        (dur / Self::MIN_SPAN_S).max(1.0)
    }

    /// Seconds visible across the lanes.
    fn span(&self, dur: f64) -> f64 {
        dur.max(0.001) / self.zoom
    }

    fn clamp(&mut self, dur: f64) {
        self.zoom = self.zoom.clamp(1.0, Self::max_zoom(dur));
        self.left_t = self.left_t.clamp(0.0, (dur - self.span(dur)).max(0.0));
    }

    fn t_to_x(&self, t: f64, left: f32, width: f32, dur: f64) -> f32 {
        left + ((t - self.left_t) / self.span(dur)) as f32 * width
    }

    /// Inverse of `t_to_x`, clamped to the visible window (⊆ the clip).
    fn x_to_t(&self, x: f32, left: f32, width: f32, dur: f64) -> f64 {
        let frac = ((x - left) / width.max(1.0)).clamp(0.0, 1.0) as f64;
        self.left_t + frac * self.span(dur)
    }

    /// Zoom by `factor` keeping the time under the pointer stationary:
    /// `anchor_t` stays at `frac` (0..1) of the lanes width.
    fn zoom_about(&mut self, anchor_t: f64, frac: f64, factor: f64, dur: f64) {
        self.zoom = (self.zoom * factor).clamp(1.0, Self::max_zoom(dur));
        self.left_t = anchor_t - frac * self.span(dur);
        self.clamp(dur);
    }

    /// Scroll by a pixel delta (positive = later content).
    fn scroll_px(&mut self, px: f32, width: f32, dur: f64) {
        self.left_t += px as f64 * self.span(dur) / width.max(1.0) as f64;
        self.clamp(dur);
    }

    /// Bring `t` into view after a seek-jump: park it at 30% when outside.
    fn ensure_visible(&mut self, t: f64, dur: f64) {
        let s = self.span(dur);
        if t < self.left_t || t > self.left_t + s {
            self.left_t = t - 0.3 * s;
            self.clamp(dur);
        }
    }

    /// Playback follow: page when the playhead runs off the window — a
    /// page-turn that parks it near the left edge, not a per-frame chase.
    fn follow(&mut self, t: f64, dur: f64) {
        let s = self.span(dur);
        if t < self.left_t || t > self.left_t + s * 0.98 {
            self.left_t = t - 0.05 * s;
            self.clamp(dur);
        }
    }
}

impl Default for Viewport {
    fn default() -> Self {
        Self { zoom: 1.0, left_t: 0.0 }
    }
}

/// Per-track view/edit flags (plan #13's shell, ADR 0066). A static named
/// set today, deliberately — the track LIST becomes data (`Vec<Track>`,
/// project.json) when track CONTENTS become data (music files, images).
#[derive(Clone, Copy)]
struct TrackFlags {
    /// Output visibility: the preview overlay AND the burn together
    /// (ADR 0036: the preview cannot drift from the burn).
    eye: bool,
    /// This track's timeline gestures are ignored (the panel stays live —
    /// it is the deliberate precision surface, ADR 0066).
    lock: bool,
}

impl Default for TrackFlags {
    fn default() -> Self {
        Self { eye: true, lock: false }
    }
}

/// The timeline razor's state (feature plan #3, operator ask 2026-07-12
/// "make another to cut the timeline"): interior cut times segmenting the
/// clip, plus a removed flag per segment. Removed spans are dropped from the
/// export — video, audio, and captions together (ADR 0065); preview playback
/// skips them. Pure and unit-tested; the strip draws/edits it.
#[derive(Default)]
struct RazorState {
    /// Sorted interior cut times (clip-relative, exclusive of 0 and dur).
    cuts: Vec<f64>,
    /// One flag per segment (`cuts.len() + 1`): true = cut out of the export.
    removed: Vec<bool>,
}

impl RazorState {
    /// Segment bounds, `cuts.len() + 1` of them covering `0..dur`.
    fn segments(&self, dur: f64) -> Vec<(f64, f64)> {
        let mut edges = Vec::with_capacity(self.cuts.len() + 2);
        edges.push(0.0);
        edges.extend_from_slice(&self.cuts);
        edges.push(dur);
        edges.windows(2).map(|w| (w[0], w[1])).collect()
    }

    fn seg_index_at(&self, t: f64) -> usize {
        self.cuts.iter().take_while(|c| **c <= t).count()
    }

    /// Split the segment containing `t`. Both halves inherit its removed
    /// flag. Refused within [`MIN_SEG_S`] of an existing edge.
    fn add_cut(&mut self, t: f64, dur: f64) -> bool {
        if self.removed.is_empty() {
            self.removed.push(false);
        }
        let near_edge = t < MIN_SEG_S
            || t > dur - MIN_SEG_S
            || self.cuts.iter().any(|c| (c - t).abs() < MIN_SEG_S);
        if near_edge {
            return false;
        }
        let i = self.seg_index_at(t);
        self.cuts.insert(i, t);
        self.removed.insert(i + 1, self.removed[i]);
        true
    }

    /// Merge the two segments around cut `idx`. The merged segment is removed
    /// only if BOTH halves were (keeping content is the safe default).
    fn delete_cut(&mut self, idx: usize) -> bool {
        if idx >= self.cuts.len() {
            return false;
        }
        self.cuts.remove(idx);
        let later = self.removed.remove(idx + 1);
        self.removed[idx] = self.removed[idx] && later;
        true
    }

    /// Toggle a segment's removal. Refuses to remove the LAST kept segment —
    /// an export of nothing is never intent.
    fn toggle_segment(&mut self, i: usize) -> bool {
        if self.removed.is_empty() {
            self.removed.push(false);
        }
        if i >= self.removed.len() {
            return false;
        }
        if !self.removed[i] && self.removed.iter().filter(|r| !**r).count() <= 1 {
            return false;
        }
        self.removed[i] = !self.removed[i];
        true
    }

    /// The kept spans, adjacent kept segments merged — what the export keeps.
    /// `None` when nothing is removed (razor cuts alone are just markers).
    fn kept_spans(&self, dur: f64) -> Option<Vec<TimeRange>> {
        if !self.removed.iter().any(|r| *r) {
            return None;
        }
        let mut out: Vec<TimeRange> = Vec::new();
        for (i, (a, b)) in self.segments(dur).iter().enumerate() {
            if *self.removed.get(i).unwrap_or(&false) {
                continue;
            }
            match out.last_mut() {
                Some(last) if (last.end_s - *a).abs() < 1e-9 => last.end_s = *b,
                _ => out.push(TimeRange { start_s: *a, end_s: *b }),
            }
        }
        Some(out)
    }

    /// The removed span containing `t`, if any (playback skips it).
    fn removed_span_at(&self, t: f64, dur: f64) -> Option<(f64, f64)> {
        let i = self.seg_index_at(t);
        if *self.removed.get(i).unwrap_or(&false) {
            let segs = self.segments(dur);
            return segs.get(i).copied();
        }
        None
    }

    /// Remove the span `[a, b]` from the export: ensure razor cuts at both
    /// bounds (reusing any within the min-segment radius; a bound at the
    /// clip edge needs none) and mark every segment inside removed. Refused
    /// when the span is sub-segment or removing it would empty the export.
    fn remove_span(&mut self, a: f64, b: f64, dur: f64) -> bool {
        let (a, b) = (a.max(0.0), b.min(dur));
        if b - a < MIN_SEG_S {
            return false;
        }
        for bound in [a, b] {
            let interior = bound > MIN_SEG_S && bound < dur - MIN_SEG_S;
            if interior
                && !self.cuts.iter().any(|c| (c - bound).abs() < MIN_SEG_S)
                && !self.add_cut(bound, dur)
            {
                return false;
            }
        }
        if self.removed.is_empty() {
            self.removed.push(false);
        }
        let segs = self.segments(dur);
        let doomed: Vec<usize> = segs
            .iter()
            .enumerate()
            .filter(|(i, (s, e))| {
                !*self.removed.get(*i).unwrap_or(&false)
                    && *s >= a - MIN_SEG_S
                    && *e <= b + MIN_SEG_S
            })
            .map(|(i, _)| i)
            .collect();
        let kept_after = self
            .removed
            .iter()
            .enumerate()
            .filter(|(i, r)| !**r && !doomed.contains(i))
            .count();
        if doomed.is_empty() || kept_after == 0 {
            return false;
        }
        for i in doomed {
            self.removed[i] = true;
        }
        true
    }

    /// ✂← — cut LEFT from the playhead: removes back to the marker when one
    /// sits left of the playhead, else to the clip start (the operator's
    /// marker-scissors flow, 2026-07-12).
    fn cut_left(&mut self, playhead: f64, marker: Option<f64>, dur: f64) -> bool {
        let a = marker.filter(|m| *m < playhead - MIN_SEG_S).unwrap_or(0.0);
        self.remove_span(a, playhead, dur)
    }

    /// →✂ — cut RIGHT from the playhead: removes up to the marker when one
    /// sits right of the playhead, else to the clip end.
    fn cut_right(&mut self, playhead: f64, marker: Option<f64>, dur: f64) -> bool {
        let b = marker.filter(|m| *m > playhead + MIN_SEG_S).unwrap_or(dur);
        self.remove_span(playhead, b, dur)
    }

    /// Remove the segment under `t` (the CapCut "cut the middle" finisher:
    /// two cuts around it, park the playhead inside, remove). Restore stays
    /// on the segment's right-click menu.
    fn remove_segment_at(&mut self, t: f64) -> bool {
        let i = self.seg_index_at(t);
        if *self.removed.get(i).unwrap_or(&false) {
            return false; // already removed
        }
        self.toggle_segment(i)
    }
}

/// A one-click motion preset (feature plan #9): rendered as a [`CameraPlan`]
/// glide/cut over the operator's manual full-frame crop — the same `camera.fg`
/// machinery as the Active Speaker plan, so preview and export agree.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MotionPreset {
    ZoomIn,
    ZoomOut,
    PanLeft,
    PanRight,
    KenBurns,
    PunchIn,
}

const MOTION_PRESETS: [(MotionPreset, &str); 6] = [
    (MotionPreset::ZoomIn, "Zoom in"),
    (MotionPreset::ZoomOut, "Zoom out"),
    (MotionPreset::PanLeft, "Pan left"),
    (MotionPreset::PanRight, "Pan right"),
    (MotionPreset::KenBurns, "Ken Burns"),
    (MotionPreset::PunchIn, "Punch in"),
];

fn motion_label(m: MotionPreset) -> &'static str {
    MOTION_PRESETS.iter().find(|(p, _)| *p == m).map(|(_, l)| *l).unwrap_or("Motion")
}

/// Build the camera plan a motion preset renders as, over the operator's
/// manual crop. Pans first tighten the crop slightly (a full-width crop has
/// no room to travel); Punch in is a hard CUT to a tighter crop at 55% — the
/// grammar is a punch, not a glide.
fn motion_plan(base: Crop, sw: f32, sh: f32, dur: f64, preset: MotionPreset) -> CameraPlan {
    let full = |crop: Crop| Layout::FullFrame { crop };
    let zoomed = |c: Crop, f: f32| {
        yc_frame::zoom_crop(c, sw, sh, f, c.x + c.w * 0.5, c.y + c.h * 0.5)
    };
    let shot = |start_s: f64, end_s: f64, layout: Layout, pan_to: Option<Crop>| Shot {
        start_s,
        end_s,
        track: None,
        layout,
        pan_to,
    };
    let shots = match preset {
        MotionPreset::ZoomIn => vec![shot(0.0, dur, full(base), Some(zoomed(base, 0.82)))],
        MotionPreset::ZoomOut => vec![shot(0.0, dur, full(zoomed(base, 0.82)), Some(base))],
        MotionPreset::PanLeft | MotionPreset::PanRight => {
            let z = zoomed(base, 0.88);
            let travel = z.w * 0.12;
            let (from, to) = if preset == MotionPreset::PanLeft {
                (yc_frame::pan_crop(z, sw, sh, travel, 0.0), yc_frame::pan_crop(z, sw, sh, -travel, 0.0))
            } else {
                (yc_frame::pan_crop(z, sw, sh, -travel, 0.0), yc_frame::pan_crop(z, sw, sh, travel, 0.0))
            };
            vec![shot(0.0, dur, full(from), Some(to))]
        }
        MotionPreset::KenBurns => {
            let drift = base.w * 0.05;
            let from = yc_frame::pan_crop(zoomed(base, 0.94), sw, sh, -drift, -drift);
            let to = yc_frame::pan_crop(zoomed(base, 0.82), sw, sh, drift, drift);
            vec![shot(0.0, dur, full(from), Some(to))]
        }
        MotionPreset::PunchIn => {
            let at = (dur * 0.55).max(MIN_SHOT_S).min(dur - MIN_SHOT_S);
            if at <= MIN_SHOT_S {
                vec![shot(0.0, dur, full(base), None)]
            } else {
                vec![
                    shot(0.0, at, full(base), None),
                    shot(at, dur, full(zoomed(base, 0.75)), None),
                ]
            }
        }
    };
    CameraPlan { shots }
}

/// Which of the three Layouts the operator has selected. `Layout::FullFrame`
/// collapses the talking-cam and gameplay cases into one variant, so the
/// editor tracks the intended kind separately (it drives seeding and the
/// segmented-control highlight; both full kinds still render as `FullFrame`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutKind {
    Stacked,
    FullCam,
    FullGameplay,
}

/// The preview's two sides of the Before/After toggle.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    /// The 9:16 output as it will render (captions, safe area).
    Output,
    /// The full source frame with the crop box tools + face overlays.
    Source,
}

/// Where the speaker analysis stands, for the Camera panel's status line.
#[derive(Clone, PartialEq)]
pub enum SpeakerJob {
    NotRun,
    Running,
    Ready,
    Failed(String),
}

/// Everything a Render needs from the editor, bundled so the action enum stays
/// readable.
pub struct RenderSpec {
    pub layout: Layout,
    pub style: CaptionStyle,
    pub placement: Option<CaptionPlacement>,
    /// The dynamic camera — the active-speaker cut plan, or a Manual-mode
    /// motion preset synthesized as a plan (feature plan #9).
    pub camera: Option<CameraPlan>,
    /// The operator's edited transcript — `Some` only when they touched it.
    pub transcript_override: Option<Transcript>,
    /// The timeline razor's kept spans (ADR 0065) — `Some` only when a
    /// segment is removed; the export drops everything between them.
    pub keep: Option<Vec<TimeRange>>,
    /// The operator's own caption stream (ADR 0065): burned as separate
    /// simultaneous events — each at its own placement (default: above the
    /// auto captions) — captions may share TIME because the two streams do
    /// not share SPACE.
    pub manual_captions: Vec<yc_core::ManualCaption>,
}

/// What `show` reports back to the app each frame.
pub enum EditorAction {
    /// Nothing to do this frame.
    None,
    /// The operator confirmed the export summary: render this spec.
    Render(Box<RenderSpec>),
    /// The operator dismissed the editor without rendering.
    Cancel,
    /// Start clip-audio playback over this absolute VOD range (Play pressed, or
    /// a scrub while playing). The app owns the audio sink (ADR 0036).
    Play(TimeRange),
    /// Stop clip audio: Pause pressed, playback reached the clip end, or a
    /// render started (playback pauses so the repaint throttle that protects
    /// whisper from the wgpu loop stays in force).
    StopAudio,
    /// Run (or re-run) the podcast speaker analysis on the worker.
    AnalyzeSpeakers,
}

/// The editable state. All four Crops stay resident so switching Layout kind
/// never discards a nudge; the speaker analysis and camera plan arrive later
/// and slot in without disturbing anything.
pub struct EditorState {
    src_w: f32,
    src_h: f32,
    range: TimeRange,
    /// The promoted Moment's generated Title (names the Short; toolbar text).
    title: Option<String>,
    /// The preview filmstrip (ADR 0036): frames sampled at `frame_fps` across
    /// the clip range, as textures. Always non-empty (Prepare fails otherwise);
    /// the playhead shows the nearest frame.
    frames: Vec<egui::TextureHandle>,
    frame_fps: f64,
    /// Clip-relative playhead (seconds) everything draws at.
    playhead_s: f64,
    /// `Some((anchor, offset))` while playing: playhead = offset + since(anchor).
    playing: Option<(Instant, f64)>,
    /// Whether the live video's first frame has been aligned to the playhead
    /// yet. The decoder takes ~0.1-0.5 s to spawn + seek + decode frame one,
    /// but the playhead-driven crop and the audio start immediately — so
    /// without this the crop leads the video and cuts flash blank. On the first
    /// frame we re-anchor the playhead and restart the audio to it (see `show`).
    video_aligned: bool,
    /// Live playback decode (streaming ffmpeg → one texture, ~24 fps): the
    /// motion upgrade over the 4 fps filmstrip, alive only while playing.
    live: Option<PreviewPlayer>,
    /// What the live player needs to spawn: the pinned ffmpeg, the resolved
    /// render source, and the clip's in-source seek offset.
    ffmpeg: std::path::PathBuf,
    render_src: std::path::PathBuf,
    seek_s: f64,
    /// Probed source frame rate (0 = unknown → the player's fallback). The
    /// live decode runs on this grid so delivered-frame counts convert
    /// exactly to content time (see [`Self::display_time`]).
    src_fps: f64,
    /// The auto-detected seed, kept for "Reset to auto".
    auto_layout: Layout,
    kind: LayoutKind,
    seam: f32,
    gameplay: Crop,
    facecam: Crop,
    fullcam: Crop,
    fullgameplay: Crop,
    /// The full Caption Style for this Clip (preset pick + customization).
    style: CaptionStyle,
    /// Which preset chip is highlighted (`None` after any manual tweak).
    preset: Option<usize>,
    /// The Caption engine transcribing this Clip (ADR 0035) — shown while the
    /// pre-pass runs so the operator knows WHAT is working and roughly how
    /// long it takes (the ensemble adds ~60-90 s over plain whisper).
    engine: CaptionEngine,
    /// The refined transcript (`Progress::Captions` or the Transcribe
    /// pre-pass) — the render's truth, editable here (focus task 2).
    transcript: Option<Transcript>,
    /// Set once the operator edits any unit: the render then burns the edited
    /// transcript verbatim.
    transcript_dirty: bool,
    /// `transcript` regrouped via the render's own line model, lazily rebuilt
    /// whenever the genre changes or an edit lands.
    lines: Vec<PreviewLine>,
    lines_genre: CaptionGenre,
    lines_dirty: bool,
    /// The shaped caption galleys (text + shadow) for the overlay, keyed by
    /// what they depend on.
    overlay_cache: Option<(OverlayKey, std::sync::Arc<egui::Galley>, std::sync::Arc<egui::Galley>)>,
    /// Caption placement (ADR 0036): `None` until the operator drags/resizes.
    placement: Option<CaptionPlacement>,
    /// Eye toggle: hide the caption overlay.
    show_captions: bool,
    /// Safe-area guide overlay (Shorts/TikTok UI zones) in Output view.
    show_safe_area: bool,
    view: ViewMode,
    camera_mode: CameraMode,
    /// The Prepare pass's persistent face clusters (pre-analysis seed: the
    /// AutoFace/Group modes work from these until the full analysis lands).
    faces: Vec<FaceCluster>,
    /// The podcast speaker analysis (worker), once run.
    pub speakers: Option<SpeakerAnalysis>,
    /// The active-speaker cut plan (worker seed, operator-overridable).
    plan: Option<CameraPlan>,
    /// Camera-plan audit findings (`audit_camera_plan`): stretches where the
    /// planned camera moves without subject cause — shown BEFORE an export so
    /// a jitter-class defect is flagged, not discovered in the render.
    camera_audit: Vec<String>,
    /// Why the voice lane is off when `speakers.voice` is `None` (missing
    /// model, too little speech, a broken session) — the Camera panel line.
    voice_note: Option<String>,
    pub speaker_job: SpeakerJob,
    /// Selected caption row (click focuses + seeks).
    sel_unit: Option<usize>,
    /// One-shot: scroll the transcript panel to `sel_unit` this frame (set by
    /// clicking a caption block on the timeline).
    scroll_to_sel: bool,
    /// The operator's OWN captions (the dedicated manual lane, plan #2):
    /// editor artifacts burned as their own simultaneous stream — never
    /// written into any store.
    manual_units: Vec<CaptionUnit>,
    /// Where each manual caption sits on the canvas, 1:1 with
    /// `manual_units` (every structural edit maintains the pairing —
    /// insert/remove/sort go through the shared helpers). `None` = the
    /// default anchor above the auto captions; `Some` = the operator's drag.
    manual_places: Vec<Option<CaptionPlacement>>,
    /// Selected row in the manual lane / "Your captions" panel section.
    sel_manual: Option<usize>,
    /// One-shot: focus this caption's text field in the panel (set by
    /// double-clicking a caption on the PREVIEW — edit what you see).
    focus_caption: Option<(CapLane, usize)>,
    /// The razor's cut marker (▼): the reference point the ✂← / →✂ verbs
    /// cut to from the playhead (operator's CapCut flow, 2026-07-12).
    cut_marker: Option<f64>,
    /// A panel time-edit changed unit order: sort once no time widget is
    /// active (sorting mid-edit swapped the row under the operator's cursor —
    /// the "it replaced the other caption" bug, 2026-07-12).
    panel_sort_pending: bool,
    /// The timeline gesture in flight (caption move/trim, cut drag), if any.
    drag: Option<TimelineDrag>,
    /// The timeline razor: segment cuts + removed spans (feature plan #3).
    razor: RazorState,
    /// The strip's zoom + horizontal scroll window (plan #13, ADR 0066).
    /// Session-only view state, deliberately never persisted.
    viewport: Viewport,
    /// Captions · auto — the pipeline's track (ADR 0066: eye gates the burn
    /// AND the preview overlay together; lock gates timeline gestures).
    trk_auto: TrackFlags,
    /// Captions · yours — the operator's track (same flag semantics). The
    /// two caption tracks never merge: AI artifacts and operator artifacts
    /// never share a track (operator ruling 2026-07-13, ADR 0066).
    trk_manual: TrackFlags,
    /// The Speakers analysis zone's eye (view-only: analysis never burns).
    speakers_eye: bool,
    /// Where the strip's right-click menu opened, so its actions (cut here,
    /// remove segment) land at the click, not wherever the pointer went next.
    strip_menu_t: Option<f64>,
    /// Manual-mode motion preset (feature plan #9); `None` = static framing.
    motion: Option<MotionPreset>,
    /// Name buffer for saving the current caption style as a preset (#12).
    preset_name: String,
    /// Export-summary modal visibility.
    show_export: bool,
}

impl EditorState {
    /// Seed the editor from the auto-detected Layout (ADR 0012). Crops the auto
    /// pick does not provide are seeded with sensible defaults so a Layout-type
    /// override has something to start from.
    #[allow(clippy::too_many_arguments)]
    pub fn from_seed(
        auto_layout: Layout,
        src_w: f32,
        src_h: f32,
        range: TimeRange,
        title: Option<String>,
        frames: Vec<egui::TextureHandle>,
        frame_fps: f64,
        caption_genre: CaptionGenre,
        caption_engine: CaptionEngine,
        faces: Vec<FaceCluster>,
        ffmpeg: std::path::PathBuf,
        render_src: std::path::PathBuf,
        seek_s: f64,
        src_fps: f64,
    ) -> Self {
        let (kind, seam, gameplay, facecam, fullcam, fullgameplay) =
            seed_fields(&auto_layout, src_w, src_h);
        let style = CaptionStyle::for_genre(caption_genre);
        let preset = caption_presets().iter().position(|p| {
            p.genre == style.genre && p.name == "Classic" && caption_genre == CaptionGenre::RollingPop
        });
        Self {
            src_w,
            src_h,
            range,
            title,
            frames,
            frame_fps: frame_fps.max(0.1),
            playhead_s: range.duration_s() * 0.5, // a representative middle frame
            playing: None,
            video_aligned: false,
            live: None,
            ffmpeg,
            render_src,
            seek_s,
            src_fps,
            auto_layout,
            kind,
            seam,
            gameplay,
            facecam,
            fullcam,
            fullgameplay,
            style,
            preset,
            engine: caption_engine,
            transcript: None,
            transcript_dirty: false,
            lines: Vec::new(),
            lines_genre: caption_genre,
            lines_dirty: false,
            overlay_cache: None,
            placement: None,
            show_captions: true,
            show_safe_area: false,
            view: ViewMode::Output,
            camera_mode: CameraMode::Manual,
            faces,
            speakers: None,
            plan: None,
            camera_audit: Vec::new(),
            voice_note: None,
            speaker_job: SpeakerJob::NotRun,
            sel_unit: None,
            scroll_to_sel: false,
            manual_units: Vec::new(),
            manual_places: Vec::new(),
            sel_manual: None,
            focus_caption: None,
            cut_marker: None,
            panel_sort_pending: false,
            drag: None,
            razor: RazorState::default(),
            viewport: Viewport::default(),
            trk_auto: TrackFlags::default(),
            trk_manual: TrackFlags::default(),
            speakers_eye: true,
            strip_menu_t: None,
            motion: None,
            preset_name: String::new(),
            show_export: false,
        }
    }

    /// Receive the refined transcript a Transcribe/Render produced
    /// (`Progress::Captions`, ADR 0036). An operator-edited transcript is
    /// never overwritten by a late worker echo of the same content.
    pub fn set_captions(&mut self, transcript: Transcript) {
        if self.transcript_dirty {
            return;
        }
        self.transcript = Some(transcript);
        // Rebuild the line model MERGED with the manual lane on the next
        // frame (a late worker echo must not hide the operator's captions).
        self.lines_dirty = true;
        self.overlay_cache = None;
        // A gesture on the OLD units would land on the wrong words.
        self.drag = None;
    }

    /// The units vec a caption lane edits. The two lanes are two RENDER
    /// streams (ADR 0065): the auto captions burn as today, the operator's
    /// lane burns as its own simultaneous events one block above — captions
    /// may share time across streams because they don't share space.
    fn lane_units_mut(&mut self, lane: CapLane) -> Option<&mut Vec<CaptionUnit>> {
        match lane {
            CapLane::Auto => self.transcript.as_mut().map(|t| &mut t.units),
            CapLane::Manual => Some(&mut self.manual_units),
        }
    }

    /// Receive the speaker analysis + camera plan (`Progress::Speakers`).
    pub fn set_speakers(
        &mut self,
        analysis: SpeakerAnalysis,
        plan: CameraPlan,
        voice_note: Option<String>,
    ) {
        // Auto-arm Active Speaker when the analysis proves multi-person and
        // the operator hasn't chosen a mode deliberately (Manual = the seed;
        // Manual WITH a motion preset is deliberate — never override it).
        if analysis.tracks.len() >= 2
            && self.camera_mode == CameraMode::Manual
            && self.motion.is_none()
        {
            self.camera_mode = CameraMode::ActiveSpeaker;
        }
        self.speakers = Some(analysis);
        self.plan = Some(plan);
        self.voice_note = voice_note;
        self.speaker_job = SpeakerJob::Ready;
        // A cut drag on the REPLACED plan would land on the wrong boundary.
        if matches!(self.drag, Some(TimelineDrag::Cut { .. })) {
            self.drag = None;
        }
        self.refresh_camera_audit();
    }

    /// Re-audit the current camera plan against the analysis (on receipt and
    /// after any operator override) so the Camera panel's warnings never go
    /// stale.
    fn refresh_camera_audit(&mut self) {
        self.camera_audit = match (&self.speakers, &self.plan) {
            // No presence artifact in the editor: a re-audit of an
            // operator-edited plan reads jitter only (ADR 0048).
            (Some(a), Some(p)) => yc_frame::speaker::audit_camera_plan(a, p, &[]),
            _ => Vec::new(),
        };
    }

    /// Keep `lines` in step with the current genre and any operator edits.
    /// The line model covers the AUTO stream only — the operator's captions
    /// are their own simultaneous stream, drawn separately.
    fn sync_lines(&mut self) {
        if self.lines_genre != self.style.genre || self.lines_dirty {
            if let Some(t) = &self.transcript {
                self.lines = preview_lines(t, self.style.genre);
            }
            self.lines_genre = self.style.genre;
            self.lines_dirty = false;
            self.overlay_cache = None;
        }
    }

    /// The absolute VOD range audio playback covers when started at a
    /// clip-relative offset: `offset` into the Clip, through its end.
    fn play_range_from(&self, offset_s: f64) -> TimeRange {
        TimeRange { start_s: self.range.start_s + offset_s, end_s: self.range.end_s }
    }

    /// The static Layout the operator's manual edits describe.
    fn manual_layout(&self) -> Layout {
        match self.kind {
            LayoutKind::Stacked => Layout::Stacked {
                seam: self.seam,
                gameplay: self.gameplay,
                facecam: self.facecam,
            },
            LayoutKind::FullCam => Layout::FullFrame { crop: self.fullcam },
            LayoutKind::FullGameplay => Layout::FullFrame { crop: self.fullgameplay },
        }
    }

    /// The Manual-mode motion plan, when a preset is active on a full-frame
    /// kind (feature plan #9). Stacked framing has no single crop to glide.
    fn motion_camera(&self) -> Option<CameraPlan> {
        let preset = self.motion?;
        let base = match self.kind {
            LayoutKind::Stacked => return None,
            LayoutKind::FullCam => self.fullcam,
            LayoutKind::FullGameplay => self.fullgameplay,
        };
        Some(motion_plan(base, self.src_w, self.src_h, self.range.duration_s(), preset))
    }

    /// The Layout the preview shows at clip time `t` under the current camera
    /// mode — and what a static-mode render exports.
    fn effective_layout(&self, t: f64) -> Layout {
        match self.camera_mode {
            CameraMode::Manual => match self.motion_camera() {
                // The motion preset's glide, exactly as the render bakes it.
                Some(plan) => plan
                    .shot_at(t)
                    .map(|s| s.layout_at(t))
                    .unwrap_or_else(|| self.manual_layout()),
                None => self.manual_layout(),
            },
            CameraMode::Center => Layout::FullFrame {
                crop: yc_frame::centered_fullcam_crop(self.src_w, self.src_h),
            },
            CameraMode::AutoFace => match &self.speakers {
                Some(a) if !a.tracks.is_empty() => {
                    yc_frame::speaker::static_mode_layout(&a.tracks, self.src_w, self.src_h, false)
                }
                _ => match self.faces.first() {
                    Some(f) => Layout::FullFrame {
                        crop: yc_frame::speaker::solo_crop(&f.bbox, self.src_w, self.src_h),
                    },
                    None => Layout::FullFrame {
                        crop: yc_frame::centered_fullcam_crop(self.src_w, self.src_h),
                    },
                },
            },
            CameraMode::ActiveSpeaker => match &self.plan {
                // layout_at glides a follow-pan shot's crop at the playhead, so
                // the preview shows the same motion the render bakes in.
                Some(plan) => plan
                    .shot_at(t)
                    .map(|s| s.layout_at(t))
                    .unwrap_or_else(|| self.manual_layout()),
                None => self.manual_layout(),
            },
            CameraMode::Group => match &self.speakers {
                Some(a) if !a.tracks.is_empty() => {
                    yc_frame::speaker::static_mode_layout(&a.tracks, self.src_w, self.src_h, true)
                }
                _ => {
                    // Pre-analysis: group over Prepare's face clusters.
                    let tracks: Vec<yc_frame::speaker::SpeakerTrack> = self
                        .faces
                        .iter()
                        .enumerate()
                        .map(|(i, f)| yc_frame::speaker::SpeakerTrack {
                            id: i,
                            bbox: f.bbox,
                            presence: f.persistence,
                            activity: Vec::new(),
                            path: Vec::new(),
                        })
                        .collect();
                    yc_frame::speaker::group_layout(&tracks, self.src_w, self.src_h)
                }
            },
        }
    }

    fn reset_to_auto(&mut self) {
        let (kind, seam, gameplay, facecam, fullcam, fullgameplay) =
            seed_fields(&self.auto_layout, self.src_w, self.src_h);
        self.kind = kind;
        self.seam = seam;
        self.gameplay = gameplay;
        self.facecam = facecam;
        self.fullcam = fullcam;
        self.fullgameplay = fullgameplay;
    }

    /// The RenderSpec the current editor state describes.
    fn render_spec(&self) -> RenderSpec {
        let camera = match self.camera_mode {
            CameraMode::ActiveSpeaker => self.plan.clone().filter(|p| !p.shots.is_empty()),
            // A Manual-mode motion preset renders through the same per-shot
            // machinery (feature plan #9).
            CameraMode::Manual => self.motion_camera(),
            _ => None,
        };
        // A motion plan glides; exporting the playhead's instantaneous frame
        // as the static layout would double-apply it — send the BASE framing.
        let layout = if camera.is_some() && self.camera_mode == CameraMode::Manual {
            self.manual_layout()
        } else {
            self.effective_layout(self.playhead_s)
        };
        RenderSpec {
            layout,
            style: self.style.clone(),
            placement: self.placement,
            camera,
            // The track eye gates the burn (ADR 0066): eye-off burns NO auto
            // captions. The override is the spec's word for "burn exactly
            // this", so an empty one is the honest "none" — no new plumbing.
            transcript_override: if !self.trk_auto.eye {
                self.transcript.clone().map(|mut t| {
                    t.units.clear();
                    t
                })
            } else if self.transcript_dirty {
                self.transcript.clone()
            } else {
                None
            },
            keep: self.razor.kept_spans(self.range.duration_s()),
            // The operator's own stream travels beside the transcript, never
            // inside it (and never into any store) — placements included.
            // Their track's eye gates it the same way (empty = burn none).
            manual_captions: if self.trk_manual.eye {
                self.manual_units
                    .iter()
                    .zip(&self.manual_places)
                    .map(|(u, p)| yc_core::ManualCaption { unit: u.clone(), placement: *p })
                    .collect()
            } else {
                Vec::new()
            },
        }
    }

    // ------------------------------------------------------------------ show --

    /// Draw the whole Studio page and return the operator's action.
    ///
    /// `busy` = any worker job in flight (gates job-starting buttons so they
    /// can't double-queue).
    /// `rendering` = an export is in flight: it gates ONLY Export/Render, so
    /// the operator can't stack renders — everything else stays editable and a
    /// render started during the caption pre-pass simply queues behind it.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        busy: bool,
        rendering: bool,
        prefs: &mut crate::settings::AppSettings,
    ) -> EditorAction {
        let mut action = EditorAction::None;

        // Pull the newest decoded frame(s) up front, so the frame count the
        // playhead reads below and the texture the crop is drawn over are the
        // same frame this repaint (no off-by-one flash at a cut). `frame_tex`
        // re-polls during draw — a no-op drain that returns the same texture.
        if self.playing.is_some() {
            if let Some(live) = &mut self.live {
                let _ = live.poll(ui.ctx());
            }
        }

        // Drive the playhead by the LIVE VIDEO's delivered-frame time, not
        // wall-clock — so the crop follows the frame actually on screen and can
        // never flash the next shot before it is visible (the "blank before the
        // cut": the decoder isn't perfectly real-time, so a wall-clock playhead
        // outruns it and switches the crop early). Sequence: on Play the crop
        // freezes at the spawn offset and the audio waits; the moment the first
        // frame lands we re-anchor the audio to it, then the playhead advances
        // frame-by-frame with the video. Filmstrip fallback (no live decode, or
        // > 1.5 s with no frame) keeps wall-clock so playback is never frozen.
        let dur = self.range.duration_s();
        if let Some((_, offset)) = self.playing {
            let video_secs = self.live.as_ref().and_then(|l| l.video_secs());
            let waited = self.playing.expect("playing").0.elapsed().as_secs_f64();
            let stalled = self.live.is_none() || (video_secs.is_none() && waited > 1.5);
            if !self.video_aligned && (video_secs.is_some() || stalled) {
                // First frame on screen (or give-up): start the audio here so it
                // runs with the video, not the ~0.1-0.5 s-earlier Play instant.
                self.video_aligned = true;
                self.playing = Some((Instant::now(), offset));
                action = EditorAction::Play(self.play_range_from(offset));
            }
            self.playhead_s = if !self.video_aligned {
                offset // frozen on the first frame's content until it is on screen
            } else if let Some(v) = self.live.as_ref().and_then(|l| l.video_secs()) {
                offset + v // follow the video, frame for frame
            } else {
                offset + self.playing.expect("playing").0.elapsed().as_secs_f64() // filmstrip
            };
            if self.playhead_s >= dur {
                self.playhead_s = dur;
                self.playing = None;
                self.stop_video();
                action = EditorAction::StopAudio;
            } else if let Some((_, span_end)) =
                self.razor.removed_span_at(self.playhead_s, dur).filter(|_| {
                    // Skip only while the live decode is actually delivering:
                    // during a scrub drag (live stopped) the pointer owns the
                    // playhead — a skip would yank it out from under the drag.
                    self.live.as_ref().and_then(|l| l.video_secs()).is_some()
                })
            {
                // Playback entered a razor-removed span: skip to where the
                // export resumes, exactly like a scrub (the preview must play
                // what the render will show). The clip END being removed
                // stops playback outright.
                if span_end >= dur - 1e-6 {
                    self.playhead_s = dur;
                    self.playing = None;
                    self.stop_video();
                    action = EditorAction::StopAudio;
                } else {
                    self.playhead_s = span_end;
                    self.playing = Some((Instant::now(), span_end));
                    self.start_video();
                    action = EditorAction::Play(self.play_range_from(span_end));
                }
            } else {
                // 60 fps visual tick (vsync-capped).
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
            }
        }

        // Keyboard (only when no widget owns focus): space = play/pause,
        // arrows nudge the crop (Manual) or scrub, +/- zoom, 0 = reset framing.
        if ui.ctx().memory(|m| m.focused().is_none()) {
            if let Some(a) = self.handle_keys(ui) {
                action = a;
            }
        }

        // --- Toolbar ---
        egui::Panel::top("studio-toolbar").show_inside(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("‹ Back").on_hover_text("Close the editor (Esc)").clicked() {
                    action = EditorAction::Cancel;
                }
                ui.add_space(8.0);
                let title = self.title.clone().unwrap_or_else(|| "Untitled clip".into());
                ui.label(egui::RichText::new(ellipsize(&title, 46)).strong());
                ui.weak(format!("{:.1}s · 1080x1920", dur));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!rendering, |ui| {
                        theme::primary_button(ui, "Export…")
                            .on_disabled_hover_text("A render is already in progress")
                            .clicked()
                            .then(|| self.show_export = true);
                    });
                    ui.add_space(6.0);
                    let safe = self.show_safe_area;
                    if ui
                        .add(theme::chip(safe, "Safe area"))
                        .on_hover_text("Show the zones YouTube/TikTok UI covers")
                        .clicked()
                    {
                        self.show_safe_area = !safe;
                    }
                    let cap = self.show_captions;
                    if ui
                        .add(theme::chip(cap, "Captions"))
                        .on_hover_text(
                            "Show/hide the caption overlay in the preview ONLY — the export \
                             is untouched. To keep a caption track out of the export, use \
                             that track's eye in the timeline headers.",
                        )
                        .clicked()
                    {
                        self.show_captions = !cap;
                    }
                    ui.add_space(10.0);
                    // Before/After: the source ("Original") vs the framed
                    // output ("Preview").
                    if let Some(i) = theme::segmented(
                        ui,
                        match self.view {
                            ViewMode::Output => 0,
                            ViewMode::Source => 1,
                        },
                        &["Preview", "Original"],
                    ) {
                        self.view = if i == 0 { ViewMode::Output } else { ViewMode::Source };
                    }
                });
            });
            ui.add_space(6.0);
        });

        // --- Timeline (bottom) ---
        // Resizable by the operator (drag the top edge, like the side panels):
        // the fixed 150px strip squeezed the voice row + seat lanes into
        // near-invisible bars (2026-07-06 feedback). The lanes flex to fill
        // whatever height is dragged; min = the old size, so it never gets
        // smaller than the pre-resize UI.
        egui::Panel::bottom("studio-timeline")
            .resizable(true)
            .default_size(290.0)
            .size_range(200.0..=460.0)
            .show_inside(ui, |ui| {
                if let Some(a) = self.ui_timeline(ui, busy, rendering, &mut prefs.volume) {
                    action = a;
                }
            });

        // --- Transcript editor (left) ---
        egui::Panel::left("studio-captions")
            .resizable(true)
            .default_size(310.0)
            .size_range(240.0..=460.0)
            .show_inside(ui, |ui| {
                if let Some(a) = self.ui_transcript_panel(ui, true) {
                    action = a;
                }
            });

        // --- Properties (right) ---
        egui::Panel::right("studio-props")
            .resizable(true)
            .default_size(300.0)
            .size_range(250.0..=420.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("props").show(ui, |ui| {
                    if let Some(a) = self.ui_properties(ui, busy, prefs) {
                        action = a;
                    }
                });
            });

        // --- Preview (center) ---
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::WELL).inner_margin(egui::Margin::same(10)))
            .show_inside(ui, |ui| {
                self.ui_preview(ui);
            });

        // --- Export summary modal ---
        if self.show_export {
            if let Some(a) = self.ui_export_modal(ui.ctx(), rendering) {
                action = a;
            }
        }

        action
    }

    /// Keyboard shortcuts. Returns an action when one needs the app (play).
    fn handle_keys(&mut self, ui: &egui::Ui) -> Option<EditorAction> {
        let dur = self.range.duration_s();
        let (space, esc, left, right, up, down, plus, minus, zero, shift, del) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals),
                i.key_pressed(egui::Key::Minus),
                i.key_pressed(egui::Key::Num0),
                i.modifiers.shift,
                i.key_pressed(egui::Key::Delete),
            )
        });
        if esc {
            if self.show_export {
                self.show_export = false;
                return None;
            }
            return Some(EditorAction::Cancel);
        }
        if space {
            return Some(self.toggle_play());
        }
        if del {
            // The razor's remove verb (CapCut Delete): drop the segment
            // under the playhead from the export.
            self.razor.remove_segment_at(self.playhead_s);
        }
        let crop_mode = self.view == ViewMode::Source && self.camera_mode == CameraMode::Manual;
        let step = if shift { 20.0 } else { 4.0 };
        let (sw, sh) = (self.src_w, self.src_h);
        if crop_mode {
            let (dx, dy) = (
                (right as i8 - left as i8) as f32 * step,
                (down as i8 - up as i8) as f32 * step,
            );
            if dx != 0.0 || dy != 0.0 {
                let crop = self.active_crop_mut();
                *crop = yc_frame::pan_crop(*crop, sw, sh, dx, dy);
            }
            if plus || minus {
                let f = if plus { 0.92 } else { 1.0 / 0.92 };
                let crop = self.active_crop_mut();
                let (ax, ay) = (crop.x + crop.w * 0.5, crop.y + crop.h * 0.5);
                *crop = yc_frame::zoom_crop(*crop, sw, sh, f, ax, ay);
            }
            if zero {
                self.reset_to_auto();
            }
        } else {
            // Scrub the playhead.
            let step_s = if shift { 5.0 } else { 1.0 };
            if left || right {
                self.playhead_s =
                    (self.playhead_s + if right { step_s } else { -step_s }).clamp(0.0, dur);
                self.viewport.ensure_visible(self.playhead_s, dur);
                if self.playing.is_some() {
                    self.playing = Some((Instant::now(), self.playhead_s));
                    self.start_video();
                    return Some(EditorAction::Play(self.play_range_from(self.playhead_s)));
                }
            }
        }
        None
    }

    fn toggle_play(&mut self) -> EditorAction {
        if self.playing.is_some() {
            self.playing = None;
            self.stop_video();
            EditorAction::StopAudio
        } else {
            let dur = self.range.duration_s();
            if self.playhead_s >= dur {
                self.playhead_s = 0.0;
            }
            // Never START inside a razor-removed span — begin where the
            // export would resume.
            if let Some((_, span_end)) = self.razor.removed_span_at(self.playhead_s, dur) {
                self.playhead_s = span_end.min(dur);
            }
            self.playing = Some((Instant::now(), self.playhead_s));
            self.start_video();
            EditorAction::Play(self.play_range_from(self.playhead_s))
        }
    }

    /// Spawn the live decode at the current playhead (Play, or a seek while
    /// playing). Failure is soft: the filmstrip keeps carrying playback.
    fn start_video(&mut self) {
        self.live = None; // kill any previous stream first
        self.video_aligned = false; // re-align the playhead to the new stream's first frame
        let abs = self.seek_s + self.playhead_s;
        let remaining = (self.range.duration_s() - self.playhead_s).max(0.05);
        match PreviewPlayer::spawn(
            &self.ffmpeg,
            &self.render_src,
            abs,
            remaining,
            self.src_w,
            self.src_h,
            self.src_fps,
        ) {
            Ok(p) => self.live = Some(p),
            Err(e) => tracing::warn!("live preview unavailable ({e}); filmstrip playback"),
        }
    }

    fn stop_video(&mut self) {
        self.live = None; // Drop kills the decoder
    }

    /// The Manual-mode crop the arrow keys / drag act on (the full-frame crop,
    /// or the facecam Panel of a stacked layout — the one people adjust).
    fn active_crop_mut(&mut self) -> &mut Crop {
        match self.kind {
            LayoutKind::Stacked => &mut self.facecam,
            LayoutKind::FullCam => &mut self.fullcam,
            LayoutKind::FullGameplay => &mut self.fullgameplay,
        }
    }

    // ------------------------------------------------------------- preview --

    fn ui_preview(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_size();
        match self.view {
            ViewMode::Output => {
                // A 9:16 canvas centered in the well.
                let h = avail.y.min(avail.x * CANVAS_H as f32 / CANVAS_W as f32);
                let w = h * CANVAS_W as f32 / CANVAS_H as f32;
                let origin = ui.min_rect().min
                    + egui::vec2((avail.x - w) * 0.5, (avail.y - h).max(0.0) * 0.5);
                let canvas = Rect::from_min_size(origin, egui::vec2(w, h));
                self.draw_output(ui, canvas);
            }
            ViewMode::Source => {
                // The full source frame fit into the well.
                let aspect = self.src_w / self.src_h.max(1.0);
                let w = avail.x.min(avail.y * aspect);
                let h = w / aspect;
                let origin = ui.min_rect().min
                    + egui::vec2((avail.x - w) * 0.5, (avail.y - h).max(0.0) * 0.5);
                let frame_rect = Rect::from_min_size(origin, egui::vec2(w, h));
                self.draw_source(ui, frame_rect);
            }
        }
    }

    /// The frame texture to draw at the playhead: the LIVE stream while
    /// playing (full-rate motion), else the filmstrip frame nearest the
    /// playhead (paused / scrubbing / before the first live frame lands).
    fn frame_tex(&mut self, ctx: &egui::Context) -> egui::TextureId {
        if self.playing.is_some() {
            if let Some(live) = &mut self.live {
                if let Some(id) = live.poll(ctx) {
                    return id;
                }
            }
        }
        self.frames[self.strip_idx()].id()
    }

    /// The filmstrip frame [`Self::frame_tex`] shows for the current playhead.
    fn strip_idx(&self) -> usize {
        ((self.playhead_s * self.frame_fps).round() as usize)
            .min(self.frames.len().saturating_sub(1))
    }

    /// The clip time of the frame the preview is actually SHOWING — the only
    /// correct time to pick the camera shot / face overlays with. The playhead
    /// is a *clock* (audio and captions follow it); the picture quantizes it:
    /// the ~2-4 fps filmstrip while paused/scrubbing (the nearest strip frame
    /// can sit up to half a strip interval — hundreds of ms — away), the live
    /// stream's newest delivered frame while playing. Picking the crop by the
    /// clock instead of the picture paints the incoming shot's crop over the
    /// outgoing shot's pixels around every cut: the editor's "blank at a cut"
    /// (the render-side twin was the trim rounding fixed in `yc-render`).
    /// Frame MIDPOINTS make the boundary comparison robust to sub-frame phase
    /// (a cut boundary is an exact frame pts).
    fn display_time(&self) -> f64 {
        if let Some((_, offset)) = self.playing {
            if let Some(mid) = self.live.as_ref().and_then(|l| l.shown_frame_mid_s()) {
                return offset + mid;
            }
        }
        strip_frame_time_s(self.strip_idx(), self.frame_fps)
    }

    /// Output view: the composited 9:16 result at the playhead — panels,
    /// captions, safe area, tracking chip.
    fn draw_output(&mut self, ui: &mut egui::Ui, canvas: Rect) {
        let tex = self.frame_tex(ui.ctx());
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, CornerRadius::same(4), Color32::BLACK);
        // The crop follows the frame the texture is showing, NOT the playhead
        // clock — see display_time (the "blank at a cut" otherwise).
        let shown_t = self.display_time();
        let layout = self.effective_layout(shown_t);
        let manual = self.camera_mode == CameraMode::Manual;
        match &layout {
            Layout::Stacked { seam, gameplay, facecam } => {
                let top_h = (seam * canvas.height()).clamp(8.0, canvas.height() - 8.0);
                let top = Rect::from_min_size(canvas.min, egui::vec2(canvas.width(), top_h));
                let bot = Rect::from_min_max(
                    egui::pos2(canvas.left(), canvas.top() + top_h),
                    canvas.max,
                );
                draw_panel(&painter, top, tex, gameplay, self.src_w, self.src_h, "");
                draw_panel(&painter, bot, tex, facecam, self.src_w, self.src_h, "");
                if manual {
                    pan_zoom(ui, top, "gp", &mut self.gameplay, self.src_w, self.src_h);
                    pan_zoom(ui, bot, "fc", &mut self.facecam, self.src_w, self.src_h);
                    self.drag_seam(ui, canvas, canvas.height());
                }
                let seam_y = canvas.top() + seam * canvas.height();
                painter.line_segment(
                    [egui::pos2(canvas.left(), seam_y), egui::pos2(canvas.right(), seam_y)],
                    Stroke::new(2.0, theme::GOLD.gamma_multiply(if manual { 1.0 } else { 0.4 })),
                );
            }
            Layout::FullFrame { crop } => {
                draw_panel(&painter, canvas, tex, crop, self.src_w, self.src_h, "");
                if manual {
                    let target = match self.kind {
                        LayoutKind::FullCam => &mut self.fullcam,
                        _ => &mut self.fullgameplay,
                    };
                    pan_zoom(ui, canvas, "full", target, self.src_w, self.src_h);
                } else if self.camera_mode == CameraMode::ActiveSpeaker {
                    // Reframe THE SHOT by dragging the output directly (the
                    // Original view's crop box is the precision tool; this is
                    // the quick nudge). Same contract: manual frame = static.
                    let mut c = *crop;
                    pan_zoom(ui, canvas, "as-shot-out", &mut c, self.src_w, self.src_h);
                    if c != *crop {
                        if let Some(plan) = &mut self.plan {
                            if let Some(shot) = plan.shot_at_mut(shown_t) {
                                shot.layout = Layout::FullFrame { crop: c };
                                shot.pan_to = None;
                            }
                        }
                        self.refresh_camera_audit();
                    }
                }
            }
        }
        painter.rect_stroke(
            canvas,
            CornerRadius::same(4),
            Stroke::new(1.0, Color32::from_gray(70)),
            StrokeKind::Inside,
        );

        // Caption overlay (drag to move, scroll to resize).
        if self.show_captions {
            self.caption_overlay(ui, canvas, true);
        }
        // Safe-area guides above everything.
        if self.show_safe_area {
            draw_safe_area(&painter, canvas);
        }
        // Tracking chip (Active Speaker): who the camera is on, how sure —
        // for the shot of the frame on screen (same time as the crop above).
        // When the bin under the frame was voice-overridden the chip says so,
        // and a group shot held for a KNOWN off-screen voice reads as the
        // off-screen split, not a generic group (ADR 0042).
        if self.camera_mode == CameraMode::ActiveSpeaker {
            if let Some(a) = &self.speakers {
                let bin = (shown_t / a.bin_s.max(1e-9)) as usize;
                let voice_bin = |v: &Vec<bool>| v.get(bin).copied().unwrap_or(false);
                let text = match self
                    .plan
                    .as_ref()
                    .and_then(|p| p.shot_at(shown_t))
                    .and_then(|s| s.track)
                {
                    Some(id) => format!(
                        "Tracking {} · {:.0}%{}",
                        track_label(id),
                        (a.confidence_at(shown_t) * 100.0).clamp(0.0, 100.0),
                        if a.voice.as_ref().map(|v| voice_bin(&v.overridden)).unwrap_or(false) {
                            " · voice"
                        } else {
                            ""
                        }
                    ),
                    None => {
                        if a.voice.as_ref().map(|v| voice_bin(&v.offscreen)).unwrap_or(false) {
                            "Off-screen voice · split".to_string()
                        } else {
                            "Group shot".to_string()
                        }
                    }
                };
                chip(&painter, canvas.min + egui::vec2(8.0, 8.0), &text, theme::GOLD);
            }
        }
        if !manual {
            // A gentle reminder that the panels aren't hand-editable right now.
            chip(
                &painter,
                egui::pos2(canvas.left() + 8.0, canvas.bottom() - 26.0),
                &format!("{} camera", camera_mode_label(self.camera_mode)),
                Color32::from_gray(185),
            );
        }
    }

    /// Source view: the whole frame, the crop box tool, and face overlays.
    fn draw_source(&mut self, ui: &mut egui::Ui, frame_rect: Rect) {
        let tex = self.frame_tex(ui.ctx());
        let painter = ui.painter_at(frame_rect);
        painter.rect_filled(frame_rect, CornerRadius::same(4), Color32::BLACK);
        let full = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(tex, frame_rect, full, Color32::WHITE);
        painter.rect_stroke(
            frame_rect,
            CornerRadius::same(4),
            Stroke::new(1.0, Color32::from_gray(70)),
            StrokeKind::Inside,
        );

        let (sw, sh) = (self.src_w, self.src_h);
        let to_screen = move |c: &Crop| -> Rect {
            let sx = frame_rect.width() / sw;
            let sy = frame_rect.height() / sh;
            Rect::from_min_size(
                frame_rect.min + egui::vec2(c.x * sx, c.y * sy),
                egui::vec2(c.w * sx, c.h * sy),
            )
        };

        // Dim everything outside the effective crop(s), so the kept region pops.
        let layout = self.effective_layout(self.playhead_s);
        let crops: Vec<(Crop, &str, Color32)> = match &layout {
            Layout::Stacked { gameplay, facecam, .. } => vec![
                (*gameplay, "Top panel", theme::INFO),
                (*facecam, "Bottom panel", theme::GOLD),
            ],
            Layout::FullFrame { crop } => vec![(*crop, "9:16 crop", theme::GOLD)],
        };
        dim_outside(&painter, frame_rect, &crops.iter().map(|(c, ..)| to_screen(c)).collect::<Vec<_>>());
        for (crop, label, color) in &crops {
            let r = to_screen(crop);
            painter.rect_stroke(r, CornerRadius::ZERO, Stroke::new(2.0, *color), StrokeKind::Inside);
            thirds_grid(&painter, r, *color);
            painter.text(
                r.left_top() + egui::vec2(6.0, 4.0),
                Align2::LEFT_TOP,
                *label,
                FontId::proportional(11.0),
                color.gamma_multiply(0.9),
            );
        }

        // Manual mode: the crop boxes are direct-manipulation targets.
        if self.camera_mode == CameraMode::Manual {
            match self.kind {
                LayoutKind::Stacked => {
                    let (g, f) = (self.gameplay, self.facecam);
                    crop_box_interaction(ui, frame_rect, to_screen(&g), "src-gp", &mut self.gameplay, sw, sh);
                    crop_box_interaction(ui, frame_rect, to_screen(&f), "src-fc", &mut self.facecam, sw, sh);
                }
                LayoutKind::FullCam => {
                    let c = self.fullcam;
                    crop_box_interaction(ui, frame_rect, to_screen(&c), "src-cam", &mut self.fullcam, sw, sh);
                }
                LayoutKind::FullGameplay => {
                    let c = self.fullgameplay;
                    crop_box_interaction(ui, frame_rect, to_screen(&c), "src-gpl", &mut self.fullgameplay, sw, sh);
                }
            }
        } else if self.camera_mode == CameraMode::ActiveSpeaker {
            // Per-shot manual framing (operator ask 2026-07-12): the crop box
            // is a direct-manipulation target for THE SHOT under the frame on
            // screen — the operator's own camera frame, not just face-click
            // retargeting. A manual reframe drops the shot's follow (same
            // contract as click_face). A split-screen (group) shot has no
            // single crop; clicking a face there still retargets.
            let t = self.display_time();
            let shot_crop = self
                .plan
                .as_ref()
                .and_then(|p| p.shot_at(t))
                .and_then(|s| match s.layout_at(t) {
                    Layout::FullFrame { crop } => Some(crop),
                    _ => None,
                });
            if let Some(c0) = shot_crop {
                let mut c = c0;
                if crop_box_interaction(ui, frame_rect, to_screen(&c0), "src-shot", &mut c, sw, sh)
                {
                    if let Some(plan) = &mut self.plan {
                        if let Some(shot) = plan.shot_at_mut(t) {
                            shot.layout = Layout::FullFrame { crop: c };
                            shot.pan_to = None; // a manual frame is static
                        }
                    }
                    self.refresh_camera_audit();
                }
            }
        } else {
            // In the other AI modes a drag on the frame flips to Manual
            // (CapCut-style: touching the framing takes control), seeding
            // from the AI crop.
            let resp = ui.interact(frame_rect, ui.id().with("src-takeover"), Sense::click_and_drag());
            if resp.drag_started() || resp.double_clicked() {
                if let Layout::FullFrame { crop } = layout {
                    self.kind = LayoutKind::FullGameplay;
                    self.fullgameplay = crop;
                }
                self.camera_mode = CameraMode::Manual;
            }
        }

        // Face overlays: tracks (post-analysis) or Prepare's clusters, with
        // labels; clicking one retargets the camera.
        self.face_overlays(ui, &painter, frame_rect);
    }

    // (Crop-box interaction is the free `crop_box_interaction` below — it
    // writes any `&mut Crop`, so the Manual fields AND a camera-plan shot's
    // crop share one direct-manipulation implementation.)

    /// Face boxes + labels over the Source view; click to retarget the camera.
    fn face_overlays(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, frame_rect: Rect) {
        let sx = frame_rect.width() / self.src_w;
        let sy = frame_rect.height() / self.src_h;
        let boxes: Vec<(usize, yc_frame::FaceBox, String)> = match &self.speakers {
            Some(a) => a
                .tracks
                .iter()
                .map(|t| (t.id, t.bbox, track_label(t.id)))
                .collect(),
            None => self
                .faces
                .iter()
                .enumerate()
                .map(|(i, f)| (i, f.bbox, track_label(i)))
                .collect(),
        };
        // "Active" = the shot of the frame ON SCREEN (display_time, not the
        // playhead clock), so the highlighted face always matches the picture.
        let active = self
            .plan
            .as_ref()
            .and_then(|p| p.shot_at(self.display_time()))
            .and_then(|s| s.track)
            .filter(|_| self.camera_mode == CameraMode::ActiveSpeaker);
        for (id, b, label) in &boxes {
            let r = Rect::from_min_size(
                frame_rect.min + egui::vec2(b.x * sx, b.y * sy),
                egui::vec2(b.w * sx, b.h * sy),
            );
            let color = theme::track_color(*id);
            let is_active = active == Some(*id);
            painter.rect_stroke(
                r,
                CornerRadius::same(4),
                Stroke::new(if is_active { 3.0 } else { 1.5 }, color),
                StrokeKind::Outside,
            );
            let tag = if is_active { format!("● {label}") } else { label.clone() };
            chip(painter, r.left_top() - egui::vec2(0.0, 22.0), &tag, color);
            let resp = ui.interact(r, ui.id().with(("face", *id)), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.click_face(*id);
            }
        }
    }

    /// Clicking a face: in Active Speaker mode, retarget the shot under the
    /// DISPLAYED frame to that person (the manual override focus asks for);
    /// in the static modes, frame that face.
    fn click_face(&mut self, id: usize) {
        let Some(track_bbox) = self
            .speakers
            .as_ref()
            .and_then(|a| a.tracks.iter().find(|t| t.id == id).map(|t| t.bbox))
            .or_else(|| self.faces.get(id).map(|f| f.bbox))
        else {
            return;
        };
        match self.camera_mode {
            CameraMode::ActiveSpeaker => {
                let (src_w, src_h) = (self.src_w, self.src_h);
                // Retarget the shot of the frame the operator is LOOKING AT
                // (display_time): near a cut the playhead clock can sit one
                // shot over from the picture that prompted the click.
                let t = self.display_time();
                if let Some(plan) = &mut self.plan {
                    if let Some(shot) = plan.shot_at_mut(t) {
                        shot.track = Some(id);
                        shot.layout = Layout::FullFrame {
                            crop: yc_frame::speaker::solo_crop(&track_bbox, src_w, src_h),
                        };
                        // The operator picked a static framing: drop any follow.
                        shot.pan_to = None;
                    }
                }
                self.refresh_camera_audit();
            }
            _ => {
                // Frame this face, hand control to Manual (full-cam kind).
                self.fullcam = yc_frame::speaker::solo_crop(&track_bbox, self.src_w, self.src_h);
                self.kind = LayoutKind::FullCam;
                self.camera_mode = CameraMode::Manual;
            }
        }
    }

    // ------------------------------------------------------------ timeline --

    /// Insert a fresh caption at the playhead onto the operator's OWN lane
    /// (feature plan #2's New Caption; the dedicated track): burned as its
    /// own SIMULTANEOUS stream above the auto captions (ADR 0065), so it can
    /// share time with them without ever fighting them.
    fn add_caption_at_playhead(&mut self) {
        if self.transcript.is_none() {
            return; // the pre-pass still owns captions
        }
        let at = self.playhead_s;
        let idx =
            self.manual_units.iter().position(|u| u.start_s > at).unwrap_or(self.manual_units.len());
        self.manual_units
            .insert(idx, CaptionUnit { text: "text".into(), start_s: at, end_s: at + 0.8 });
        self.manual_places.insert(idx, None); // starts at the default anchor
        self.sel_manual = Some(idx);
        self.lines_dirty = true;
    }

    /// Remove one of the operator's captions, keeping the placement pairing.
    fn remove_manual_caption(&mut self, i: usize) {
        if i < self.manual_units.len() {
            self.manual_units.remove(i);
            self.manual_places.remove(i);
            self.sel_manual = None;
            self.lines_dirty = true;
        }
    }

    /// Sort the operator's captions by start, placements riding along.
    fn sort_manual_units(&mut self) {
        let mut zipped: Vec<(CaptionUnit, Option<CaptionPlacement>)> =
            self.manual_units.drain(..).zip(self.manual_places.drain(..)).collect();
        zipped.sort_by(|a, b| {
            a.0.start_s.partial_cmp(&b.0.start_s).unwrap_or(std::cmp::Ordering::Equal)
        });
        for (u, p) in zipped {
            self.manual_units.push(u);
            self.manual_places.push(p);
        }
    }

    /// Add a CAMERA cut at the (frame-snapped) playhead: split the shot under
    /// it in two. The operator then reframes either half by dragging the crop
    /// (per-shot framing) or clicking a face. Framing only — no time removed
    /// (that is the razor's job).
    fn split_shot_at_playhead(&mut self) {
        let mut t = self.playhead_s;
        if self.src_fps > 0.0 {
            t = (t * self.src_fps).round() / self.src_fps;
        }
        if let Some(plan) = &mut self.plan {
            if split_plan_at(plan, t) {
                self.refresh_camera_audit();
            }
        }
    }

    /// Remove the cut between shots `boundary-1` and `boundary` (the marker's
    /// right-click menu).
    fn delete_cut(&mut self, boundary: usize) {
        if let Some(plan) = &mut self.plan {
            if delete_plan_cut(plan, boundary) {
                self.refresh_camera_audit();
            }
        }
    }


    /// Apply the in-flight timeline gesture at pointer time `pointer_t`:
    /// clamp, snap (anchors within `tol_t`), and write the result into the
    /// transcript / camera plan so THIS frame draws it. Returns the anchor a
    /// magnet snapped to (for the guide line) and the manipulated time (for
    /// the chip beside the pointer).
    fn apply_timeline_drag(&mut self, pointer_t: f64, tol_t: f64) -> (Option<f64>, f64) {
        let dur = self.range.duration_s();
        let playhead = self.playhead_s;
        // Cut boundaries double as caption-snap anchors; a dragged cut snaps
        // to the playhead only (it IS the boundary set).
        let cuts: Vec<f64> = self
            .plan
            .as_ref()
            .map(|p| p.shots.iter().skip(1).map(|s| s.start_s).collect())
            .unwrap_or_default();
        let mut snapped = None;
        let mut shown = pointer_t;
        match self.drag.clone() {
            Some(TimelineDrag::CapMove { targets, orig, grab_t }) => {
                if targets.is_empty() || targets.len() != orig.len() {
                    return (None, shown); // stale gesture
                }
                let first_start = orig.iter().map(|o| o.0).fold(f64::INFINITY, f64::min);
                let last_end = orig.iter().map(|o| o.1).fold(0.0f64, f64::max);
                // Clip bounds only: captions move FREELY past each other
                // (operator ruling 2026-07-12 — "stuck between captions");
                // lanes sort on release, the burn-order warning marks any
                // caption another one would hide.
                let lo = -first_start;
                let hi = dur - last_end;
                if lo > hi {
                    return (None, shown);
                }
                let mut dt = (pointer_t - grab_t).clamp(lo, hi);
                let anchors: Vec<f64> = std::iter::once(playhead)
                    .chain(cuts.iter().copied())
                    .filter(|c| (first_start + lo..=first_start + hi).contains(c))
                    .collect();
                match magnet(first_start + dt, &anchors, tol_t) {
                    Some(anchor) => {
                        dt = anchor - first_start;
                        snapped = Some(anchor);
                    }
                    // The panel shows m:ss.cc — never bake in precision the
                    // operator can't see or reproduce there.
                    None => dt = quantize_cs(first_start + dt) - first_start,
                }
                let mut touched_auto = false;
                for ((lane, idx), (os, oe)) in targets.iter().zip(orig.iter()) {
                    let Some(units) = self.lane_units_mut(*lane) else { continue };
                    if let Some(u) = units.get_mut(*idx) {
                        u.start_s = os + dt;
                        u.end_s = oe + dt;
                        touched_auto |= *lane == CapLane::Auto;
                    }
                }
                shown = first_start + dt;
                if touched_auto {
                    self.transcript_dirty = true;
                }
                self.lines_dirty = true;
            }
            Some(TimelineDrag::CapTrimStart { lane, unit, orig_start, grab_t }) => {
                let Some(units) = self.lane_units_mut(lane) else { return (None, shown) };
                if unit >= units.len() {
                    return (None, shown);
                }
                let lo = 0.0;
                let hi = units[unit].end_s - CAP_MIN_S;
                if lo > hi {
                    return (None, shown);
                }
                let v = (orig_start + (pointer_t - grab_t)).clamp(lo, hi);
                let anchors: Vec<f64> = std::iter::once(playhead)
                    .chain(cuts.iter().copied())
                    .filter(|c| (lo..=hi).contains(c))
                    .collect();
                let v = match magnet(v, &anchors, tol_t) {
                    Some(anchor) => {
                        snapped = Some(anchor);
                        anchor
                    }
                    None => quantize_cs(v).clamp(lo, hi),
                };
                let Some(units) = self.lane_units_mut(lane) else { return (None, shown) };
                units[unit].start_s = v;
                shown = v;
                if lane == CapLane::Auto {
                    self.transcript_dirty = true;
                }
                self.lines_dirty = true;
            }
            Some(TimelineDrag::CapTrimEnd { lane, unit, orig_end, grab_t }) => {
                let Some(units) = self.lane_units_mut(lane) else { return (None, shown) };
                if unit >= units.len() {
                    return (None, shown);
                }
                let lo = units[unit].start_s + CAP_MIN_S;
                let hi = dur;
                if lo > hi {
                    return (None, shown);
                }
                let v = (orig_end + (pointer_t - grab_t)).clamp(lo, hi);
                // The workflow is "park the playhead where the word should
                // end, drag the edge to it" — so the playhead is the anchor.
                let anchors: Vec<f64> =
                    std::iter::once(playhead).filter(|c| (lo..=hi).contains(c)).collect();
                let v = match magnet(v, &anchors, tol_t) {
                    Some(anchor) => {
                        snapped = Some(anchor);
                        anchor
                    }
                    None => quantize_cs(v).clamp(lo, hi),
                };
                let Some(units) = self.lane_units_mut(lane) else { return (None, shown) };
                units[unit].end_s = v;
                shown = v;
                if lane == CapLane::Auto {
                    self.transcript_dirty = true;
                }
                self.lines_dirty = true;
            }
            Some(TimelineDrag::Cut { boundary, orig_t, grab_t }) => {
                let src_fps = self.src_fps;
                let Some(plan) = self.plan.as_mut() else { return (None, shown) };
                if boundary == 0 || boundary >= plan.shots.len() {
                    return (None, shown);
                }
                let lo = plan.shots[boundary - 1].start_s + MIN_SHOT_S;
                let hi = plan.shots[boundary].end_s - MIN_SHOT_S;
                if lo > hi {
                    return (None, shown);
                }
                let mut v = (orig_t + (pointer_t - grab_t)).clamp(lo, hi);
                let anchors: Vec<f64> =
                    std::iter::once(playhead).filter(|c| (lo..=hi).contains(c)).collect();
                match magnet(v, &anchors, tol_t) {
                    Some(anchor) => {
                        v = anchor;
                        snapped = Some(anchor);
                    }
                    // Feature plan #3's frame snap: the render trims at full
                    // pts precision, so a frame-exact boundary cuts clean.
                    None if src_fps > 0.0 => {
                        v = ((v * src_fps).round() / src_fps).clamp(lo, hi);
                    }
                    None => {}
                }
                plan.shots[boundary - 1].end_s = v;
                plan.shots[boundary].start_s = v;
                shown = v;
            }
            Some(TimelineDrag::Razor { idx, orig_t, grab_t }) => {
                if idx >= self.razor.cuts.len() {
                    return (None, shown);
                }
                let lo = if idx > 0 { self.razor.cuts[idx - 1] } else { 0.0 } + MIN_SEG_S;
                let hi = self.razor.cuts.get(idx + 1).copied().unwrap_or(dur) - MIN_SEG_S;
                if lo > hi {
                    return (None, shown);
                }
                let mut v = (orig_t + (pointer_t - grab_t)).clamp(lo, hi);
                let anchors: Vec<f64> =
                    std::iter::once(playhead).filter(|c| (lo..=hi).contains(c)).collect();
                match magnet(v, &anchors, tol_t) {
                    Some(anchor) => {
                        v = anchor;
                        snapped = Some(anchor);
                    }
                    // Frame-exact razor edges cut clean (feature plan #3).
                    None if self.src_fps > 0.0 => {
                        v = ((v * self.src_fps).round() / self.src_fps).clamp(lo, hi);
                    }
                    None => {}
                }
                self.razor.cuts[idx] = v;
                shown = v;
            }
            Some(TimelineDrag::Marker { orig_t, grab_t }) => {
                let mut v = (orig_t + (pointer_t - grab_t)).clamp(0.0, dur);
                let anchors: Vec<f64> = std::iter::once(playhead)
                    .chain(cuts.iter().copied())
                    .filter(|c| (0.0..=dur).contains(c))
                    .collect();
                match magnet(v, &anchors, tol_t) {
                    Some(anchor) => {
                        v = anchor;
                        snapped = Some(anchor);
                    }
                    None if self.src_fps > 0.0 => {
                        v = ((v * self.src_fps).round() / self.src_fps).clamp(0.0, dur);
                    }
                    None => {}
                }
                self.cut_marker = Some(v);
                shown = v;
            }
            None => {}
        }
        (snapped, shown)
    }

    /// Land the in-flight gesture: restore each caption lane's ordering
    /// (captions drag freely past each other now — the sort happens HERE, on
    /// release, never mid-gesture) and re-audit an edited camera plan
    /// (jitter-class defects surface BEFORE the export).
    fn finish_timeline_drag(&mut self) {
        match self.drag.take() {
            Some(TimelineDrag::Cut { .. }) => self.refresh_camera_audit(),
            Some(TimelineDrag::Razor { .. }) | Some(TimelineDrag::Marker { .. }) => {}
            Some(_) => {
                if let Some(t) = &mut self.transcript {
                    let sorted = t.units.windows(2).all(|w| w[0].start_s <= w[1].start_s);
                    if !sorted {
                        t.units.sort_by(|x, y| {
                            x.start_s
                                .partial_cmp(&y.start_s)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });
                        self.sel_unit = None; // row indices shifted under the sort
                    }
                }
                let sorted =
                    self.manual_units.windows(2).all(|w| w[0].start_s <= w[1].start_s);
                if !sorted {
                    self.sort_manual_units(); // placements ride along
                    self.sel_manual = None;
                }
                self.lines_dirty = true;
            }
            None => {}
        }
    }

    /// The bottom strip: transport, ruler + scrub, caption blocks, speaker
    /// lanes, cut markers.
    fn ui_timeline(
        &mut self,
        ui: &mut egui::Ui,
        busy: bool,
        rendering: bool,
        volume: &mut f32,
    ) -> Option<EditorAction> {
        let mut action = None;
        let dur = self.range.duration_s().max(0.001);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = if self.playing.is_some() { "⏸" } else { "▶" };
            if ui
                .add(egui::Button::new(egui::RichText::new(label).size(16.0)))
                .on_hover_text("Space")
                .clicked()
            {
                action = Some(self.toggle_play());
            }
            ui.monospace(format!("{} / {}", fmt_mmss_cc(self.playhead_s), fmt_mmss_cc(dur)));
            // Master playback volume (feature plan #7): every preview sink,
            // never the export. The app owns the sink; it applies + persists
            // any change after this frame.
            ui.add_space(6.0);
            ui.scope(|ui| {
                ui.spacing_mut().slider_width = 74.0;
                ui.label("🔊");
                ui.add(egui::Slider::new(volume, 0.0..=2.0).show_value(false)).on_hover_text(
                    format!(
                        "Playback volume {:.0}% — persists; never affects the export",
                        *volume * 100.0
                    ),
                );
            });
            // Timeline edit verbs (feature plan #2/#3).
            ui.add_space(6.0);
            if ui
                .add_enabled(
                    self.transcript.is_some() && !self.trk_manual.lock,
                    egui::Button::new("+ Caption"),
                )
                .on_hover_text(
                    "Insert a caption at the playhead onto YOUR caption track (the second \
                     timeline row) — drag it on the preview to place it anywhere, double-click \
                     it there to edit the text",
                )
                .on_disabled_hover_text(if self.trk_manual.lock {
                    "Your caption track is locked — unlock it in its header"
                } else {
                    "Captions are still transcribing"
                })
                .clicked()
            {
                self.add_caption_at_playhead();
            }
            // The timeline razor, marker-scissors style (operator ask
            // 2026-07-12): one merged three-part control — cut-left | place
            // marker | cut-right (zero gap, shared corners, their round-6
            // sketch). Delete drops the segment under the playhead; restore
            // via the segment's right-click menu. All of it drops video +
            // audio + captions from the export together.
            // (The camera-cut button is parked until the keyframe arc gives
            // per-cut framing real function — operator call 2026-07-13; the
            // strip's right-click menu still carries it for AS clips.)
            let dur_full = self.range.duration_s();
            ui.add_space(6.0);
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if ui
                    .add(egui::Button::new("✂⏴").corner_radius(CornerRadius {
                        nw: 4,
                        sw: 4,
                        ne: 0,
                        se: 0,
                    }))
                    .on_hover_text(
                        "Cut LEFT of the playhead: removes back to the marker (or the clip \
                         start when no marker is on the left)",
                    )
                    .clicked()
                {
                    self.razor.cut_left(self.playhead_s, self.cut_marker, dur_full);
                }
                if ui
                    .add(egui::Button::new("⏷ Mark").corner_radius(CornerRadius::ZERO))
                    .on_hover_text(
                        "Place the cut marker at the playhead — the point both scissors cut \
                         to. Drag it on the timeline to adjust; right-click it to clear.",
                    )
                    .clicked()
                {
                    self.cut_marker = Some(if self.src_fps > 0.0 {
                        (self.playhead_s * self.src_fps).round() / self.src_fps
                    } else {
                        self.playhead_s
                    });
                }
                if ui
                    .add(egui::Button::new("⏵✂").corner_radius(CornerRadius {
                        nw: 0,
                        sw: 0,
                        ne: 4,
                        se: 4,
                    }))
                    .on_hover_text(
                        "Cut RIGHT of the playhead: removes up to the marker (or the clip \
                         end when no marker is on the right)",
                    )
                    .clicked()
                {
                    self.razor.cut_right(self.playhead_s, self.cut_marker, dur_full);
                }
            });
            // Say WHAT the worker is doing — a mystery-disabled UI reads as a
            // hang (operator feedback). Everything here stays usable meanwhile.
            if busy {
                crate::throttled_spinner(ui);
                let doing = if rendering {
                    "Rendering the Short…"
                } else if self.transcript.is_none() {
                    "Transcribing captions…"
                } else if self.speaker_job == SpeakerJob::Running {
                    "Analyzing speakers…"
                } else {
                    "Working…"
                };
                ui.weak(doing);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(a) = &self.speakers {
                    if a.voice.is_some() {
                        ui.label(
                            egui::RichText::new("▪ off-screen")
                                .color(theme::ERR.gamma_multiply(0.9))
                                .size(11.5),
                        )
                        .on_hover_text(
                            "Voice lane (diarization): who the VOICE is, joined per camera \
                             angle. Red = a known voice with no seat on screen — the planner \
                             shows the visible pair's split there.",
                        );
                        ui.label(egui::RichText::new("▪ voice").size(11.5).weak());
                    }
                    for t in a.tracks.iter().rev() {
                        ui.label(
                            egui::RichText::new(format!("■ {}", track_label(t.id)))
                                .color(theme::track_color(t.id))
                                .size(11.5),
                        );
                    }
                    ui.weak("Speakers:");
                }
            });
        });
        ui.add_space(4.0);

        // The strip fills the height the operator dragged the panel to. The
        // chrome rows (ruler, caption blocks, cut markers) keep their fixed
        // sizes; the seat lanes + voice row split ALL the remaining height
        // equally, floored at the old 13px — so the minimum panel degrades to
        // exactly the old layout, and dragging taller always visibly fattens
        // the lanes (no cap: the panel's own max bounds them).
        let n_tracks = self.speakers.as_ref().map(|a| a.tracks.len()).unwrap_or(0);
        // The evidence row exists when either audio lane does — a machine
        // without the CAM++ model still shows reaction spans (ADR 0046).
        let has_voice = self
            .speakers
            .as_ref()
            .map(|a| a.voice.is_some() || a.reaction.is_some())
            .unwrap_or(false);
        let n_lanes = n_tracks + usize::from(has_voice);
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ui.available_height().max(60.0)),
            Sense::hover(),
        );
        // The Premiere/CapCut shape (plan #13, ADR 0066): a header column on
        // the left (track names + painted eye/lock toggles), the lanes to the
        // right. Scrubbing, gestures, and the strip menu live on the LANES
        // surface only; lane content clips at the header boundary.
        let lanes = Rect::from_min_max(
            egui::pos2((rect.left() + HDR_W).min(rect.right() - 1.0), rect.top()),
            rect.max,
        );
        let resp = ui.interact(lanes, ui.id().with("strip-lanes"), Sense::click_and_drag());
        // 125 = the fixed chrome: ruler 18 + the filmstrip track 29 + the two
        // caption tracks (22 each) + the burn-line rail 6 above the speaker
        // lanes, the camera badge gutter 20 and the scrollbar row 8 below.
        let content_bottom = rect.bottom() - SCROLL_H;
        let lane_h = if n_lanes == 0 {
            13.0
        } else {
            ((rect.height() - 125.0) / n_lanes as f32).max(13.0)
        };
        // Row geometry, top → bottom. Header cards span EXACTLY these bands
        // (the operator's symmetry verdict, ADR 0066 round 2).
        let film_y0 = rect.top() + 18.0;
        let film_y1 = rect.top() + 44.0;
        let cap_y0 = rect.top() + 47.0;
        let man_y0 = cap_y0 + 22.0;
        let rail_y = man_y0 + 22.0;
        // Two painters: `ph` (unclipped) owns the background + header column;
        // `p` clips lane content at the header boundary so a scrolled-out
        // block can neither paint nor be read under the headers.
        let ph = ui.painter().clone();
        let p = ui.painter_at(lanes);
        ph.rect_filled(rect, CornerRadius::same(4), theme::WELL);

        // Viewport input (ADR 0066): ctrl+wheel / pinch zooms about the
        // pointer, plain wheel / shift+wheel / trackpad-x scrolls — only
        // while the pointer is over the lanes. Mid-gesture moves are safe:
        // gestures are pointer-tracked in TIME and re-read x each frame.
        if ui.rect_contains_pointer(lanes) {
            let (zoomf, scroll, mods, pos) = ui.input(|i| {
                (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers, i.pointer.latest_pos())
            });
            if let Some(pos) = pos {
                if (zoomf - 1.0).abs() > 1e-4 {
                    let frac = ((pos.x - lanes.left()) / lanes.width().max(1.0)).clamp(0.0, 1.0);
                    let anchor = self.viewport.x_to_t(pos.x, lanes.left(), lanes.width(), dur);
                    self.viewport.zoom_about(anchor, frac as f64, zoomf as f64, dur);
                } else if !mods.ctrl
                    && !mods.command
                    && (scroll.x != 0.0 || scroll.y != 0.0)
                {
                    // Wheel-up = earlier (scroll left), matching ScrollArea.
                    self.viewport.scroll_px(-(scroll.x + scroll.y), lanes.width(), dur);
                }
            }
        }
        // Playback follow: page the window when the playhead runs off it.
        if self.playing.is_some() {
            self.viewport.follow(self.playhead_s, dur);
        }
        let vp = self.viewport;
        let t_to_x = |t: f64| vp.t_to_x(t, lanes.left(), lanes.width(), dur);
        let x_to_t = |x: f32| vp.x_to_t(x, lanes.left(), lanes.width(), dur);

        // Row bands mirror the header cards across the lanes (the operator's
        // symmetry verdict, ADR 0066 round 2): a whisper of fill + a hairline
        // under each track so header card and lane read as one bar.
        let mut bands: Vec<(f32, f32)> = vec![
            (film_y0, film_y1),
            (cap_y0, cap_y0 + 22.0),
            (man_y0, man_y0 + 22.0),
        ];
        if self.speakers.is_some() {
            bands.push((rail_y + 6.0, content_bottom - 20.0));
        }
        if self.camera_mode == CameraMode::ActiveSpeaker && self.plan.is_some() {
            bands.push((content_bottom - 20.0, content_bottom - 2.0));
        }
        for (y0, y1) in &bands {
            p.rect_filled(
                Rect::from_min_max(egui::pos2(lanes.left(), *y0), egui::pos2(lanes.right(), *y1)),
                CornerRadius::ZERO,
                Color32::from_rgb(0x14, 0x17, 0x1D),
            );
            p.line_segment(
                [egui::pos2(lanes.left(), *y1 + 0.5), egui::pos2(lanes.right(), *y1 + 0.5)],
                Stroke::new(1.0, Color32::from_gray(36)),
            );
        }

        // Apply any in-flight direct-manipulation gesture BEFORE drawing, so
        // this frame already shows the result (responsive by construction);
        // land it on release. Pointer-tracked, not widget-id-tracked — see
        // `TimelineDrag`.
        let mut snap_x: Option<f32> = None;
        let mut drag_chip_t: Option<f64> = None;
        if self.drag.is_some() {
            ui.ctx().set_cursor_icon(match self.drag {
                Some(TimelineDrag::CapMove { .. }) => egui::CursorIcon::Grabbing,
                _ => egui::CursorIcon::ResizeHorizontal,
            });
            if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                // Snap tolerance stays in POINTS: convert through the
                // VISIBLE span, so zooming in tightens the time tolerance.
                let tol_t = SNAP_PX as f64 * vp.span(dur) / lanes.width().max(1.0) as f64;
                let (snapped, shown) = self.apply_timeline_drag(x_to_t(pos.x), tol_t);
                snap_x = snapped.map(t_to_x);
                drag_chip_t = Some(shown);
            }
            if ui.input(|i| !i.pointer.primary_down()) {
                self.finish_timeline_drag();
                drag_chip_t = None;
            }
        }

        // Ruler ticks: a major every ~1/8 of the VISIBLE span, rounded to a
        // nice step (sub-second rungs appear zoomed in — labels grow cs).
        let span = vp.span(dur);
        let step = nice_step(span / 8.0);
        let mut t = (vp.left_t / step).ceil() * step;
        while t <= (vp.left_t + span).min(dur) + 1e-9 {
            let x = t_to_x(t);
            p.line_segment(
                [egui::pos2(x, rect.top() + 2.0), egui::pos2(x, rect.top() + 12.0)],
                Stroke::new(1.0, Color32::from_gray(90)),
            );
            p.text(
                egui::pos2(x + 3.0, rect.top() + 1.0),
                Align2::LEFT_TOP,
                if step < 1.0 { fmt_mmss_cc(t) } else { fmt_mmss(t) },
                FontId::monospace(9.5),
                Color32::from_gray(175),
            );
            t += step;
        }

        // The video track (operator ask 2026-07-12 "so we know where to
        // cut"): the whole clip as filmstrip thumbnails — the frames Prepare
        // already extracted. Clicks/drags on it scrub (it is strip surface);
        // razor veils cover it, so a removed span visibly dims its video.
        let film = Rect::from_min_max(
            egui::pos2(lanes.left(), film_y0),
            egui::pos2(lanes.right(), film_y1),
        );
        if !self.frames.is_empty() {
            let aspect = (self.src_w / self.src_h.max(1.0)).max(0.1);
            let thumb_w = film.height() * aspect;
            let n_thumbs = (film.width() / thumb_w).ceil().max(1.0) as usize;
            for k in 0..n_thumbs {
                let x0 = film.left() + k as f32 * thumb_w;
                let slot = Rect::from_min_max(
                    egui::pos2(x0, film.top()),
                    egui::pos2((x0 + thumb_w).min(film.right()), film.bottom()),
                );
                let t_mid = x_to_t(x0 + thumb_w * 0.5);
                let idx = ((t_mid * self.frame_fps).round() as usize)
                    .min(self.frames.len().saturating_sub(1));
                // A partial rightmost slot crops the thumb instead of
                // squeezing it (uv trimmed to the visible fraction).
                let frac = (slot.width() / thumb_w).clamp(0.0, 1.0);
                p.image(
                    self.frames[idx].id(),
                    slot,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(frac, 1.0)),
                    Color32::WHITE,
                );
            }
            p.rect_stroke(
                film,
                CornerRadius::ZERO,
                Stroke::new(1.0, Color32::from_gray(60)),
                StrokeKind::Inside,
            );
        }

        // Caption blocks — ONE BLOCK PER UNIT (= per panel row) on TWO lanes:
        // the pipeline's captions (top row) and the operator's own dedicated
        // track below it (plan #2's "manual alongside AI" — the 2026-07-12
        // ask). Blocks are stable objects: the body drags FREELY in time
        // (past other captions too — lanes sort on release), edges trim,
        // click selects, right-click deletes. A caption the burn would hide
        // (huge-word: the next merged caption starts at/before it) is flagged
        // red instead of silently swallowed.
        self.sync_lines();
        let mut clicked_unit: Option<(CapLane, usize)> = None;
        let mut delete_unit: Option<(CapLane, usize)> = None;
        let mut begin_drag: Option<TimelineDrag> = None;
        let drag_targets: Option<Vec<(CapLane, usize)>> = match &self.drag {
            Some(TimelineDrag::CapMove { targets, .. }) => Some(targets.clone()),
            Some(TimelineDrag::CapTrimStart { lane, unit, .. })
            | Some(TimelineDrag::CapTrimEnd { lane, unit, .. }) => Some(vec![(*lane, *unit)]),
            _ => None,
        };
        // Per-lane collision warnings. Auto lane (huge-word): a caption whose
        // successor starts at/before it gets zero display — flag it. Manual
        // lane: two of the operator's captions sharing time overprint at the
        // same anchor — flag both. Cross-lane overlap is FINE: the operator's
        // stream burns at its own anchor above the auto captions (ADR 0065).
        // Warnings describe the BURN, so a stream whose eye is off (it burns
        // nothing, ADR 0066) contributes none.
        let hidden: std::collections::HashSet<(u8, usize)> = {
            let mut set = std::collections::HashSet::new();
            if self.style.genre == CaptionGenre::HugeWord && self.trk_auto.eye {
                if let Some(t) = &self.transcript {
                    let mut order: Vec<(f64, usize)> =
                        t.units.iter().enumerate().map(|(i, u)| (u.start_s, i)).collect();
                    order.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                    for w in order.windows(2) {
                        if w[1].0 <= w[0].0 + 1e-9 {
                            set.insert((CapLane::Auto as u8, w[0].1));
                        }
                    }
                }
            }
            if self.trk_manual.eye {
                for (i, a) in self.manual_units.iter().enumerate() {
                    for (j, b) in self.manual_units.iter().enumerate() {
                        // Overprint needs shared time AND shared space: two of
                        // the operator's captions both at the DEFAULT anchor.
                        // Dragged-apart captions may overlap freely.
                        let both_default = self.manual_places.get(i).copied().flatten().is_none()
                            && self.manual_places.get(j).copied().flatten().is_none();
                        if i != j && both_default && a.start_s < b.end_s && b.start_s < a.end_s {
                            set.insert((CapLane::Manual as u8, i));
                        }
                    }
                }
            }
            set
        };
        // The header column names the lanes now; flags ride into the loop.
        // Blocks tint by their track's accent (gold = the pipeline's, green
        // = the operator's — matching the header cards, ADR 0066 round 2).
        for (lane, y0, flags) in [
            (CapLane::Auto, cap_y0, self.trk_auto),
            (CapLane::Manual, man_y0, self.trk_manual),
        ] {
            let accent = match lane {
                CapLane::Auto => theme::GOLD,
                CapLane::Manual => theme::OK,
            };
            let units: &[CaptionUnit] = match lane {
                CapLane::Auto => {
                    self.transcript.as_ref().map(|t| t.units.as_slice()).unwrap_or(&[])
                }
                CapLane::Manual => &self.manual_units,
            };
            let selected_idx =
                match lane { CapLane::Auto => self.sel_unit, CapLane::Manual => self.sel_manual };
            for (i, u) in units.iter().enumerate() {
                let r = Rect::from_min_max(
                    egui::pos2(t_to_x(u.start_s), y0 + 2.0),
                    egui::pos2(t_to_x(u.end_s).max(t_to_x(u.start_s) + 3.0), y0 + 20.0),
                );
                if r.min.x > lanes.right() || r.max.x < lanes.left() {
                    continue; // scrolled out of the viewport window
                }
                let dragging_this =
                    drag_targets.as_ref().is_some_and(|t| t.contains(&(lane, i)));
                let selected = selected_idx == Some(i);
                let is_hidden = hidden.contains(&(lane as u8, i));
                // Edge trim zones only when the block is wide enough for
                // three distinct targets; tiny blocks stay move-only (the
                // panel's timestamps cover fine trims). A locked track has
                // no trim zones at all (ADR 0066).
                let edges = r.width() >= 24.0 && !flags.lock;
                let body =
                    (if edges { r.shrink2(egui::vec2(5.0, 0.0)) } else { r }).intersect(lanes);
                let mut tip = format!(
                    "{} – {}  ·  {}\n{}",
                    fmt_mmss_cc(u.start_s),
                    fmt_mmss_cc(u.end_s),
                    u.text,
                    if flags.lock {
                        "Track locked — unlock in its header to edit (click still selects)"
                    } else {
                        "Drag to move · edges trim · click to edit · right-click to delete"
                    }
                );
                if is_hidden {
                    tip.push_str(match lane {
                        CapLane::Auto => {
                            "\n⚠ WON'T SHOW at burn: the next caption starts at/before this one — move one of them"
                        }
                        CapLane::Manual => {
                            "\n⚠ OVERPRINTS: two of your captions share time and space — stagger their times"
                        }
                    });
                }
                // Registered after the strip's scrub response, so blocks win
                // the pointer; a drag on a block moves the CAPTION, not the
                // playhead (scrub anywhere else on the strip).
                let resp = ui
                    .interact(
                        body,
                        ui.id().with(("cap-unit", lane as u8, i)),
                        Sense::click_and_drag(),
                    )
                    .on_hover_text(tip);
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(if flags.lock {
                        egui::CursorIcon::NotAllowed
                    } else {
                        egui::CursorIcon::Grab
                    });
                }
                if resp.clicked() {
                    clicked_unit = Some((lane, i));
                }
                resp.context_menu(|ui| {
                    if ui
                        .add_enabled(!flags.lock, egui::Button::new("🗑 Delete caption"))
                        .clicked()
                    {
                        delete_unit = Some((lane, i));
                        ui.close();
                    }
                });
                if resp.drag_started_by(egui::PointerButton::Primary)
                    && self.drag.is_none()
                    && !flags.lock
                {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        begin_drag = Some(TimelineDrag::CapMove {
                            targets: vec![(lane, i)],
                            orig: vec![(u.start_s, u.end_s)],
                            grab_t: x_to_t(pos.x),
                        });
                    }
                }
                let mut edge_hot = (false, false);
                if edges {
                    for side in 0..2 {
                        let er = if side == 0 {
                            Rect::from_min_max(r.left_top(), egui::pos2(r.left() + 5.0, r.bottom()))
                        } else {
                            Rect::from_min_max(egui::pos2(r.right() - 5.0, r.top()), r.right_bottom())
                        };
                        let eresp = ui.interact(
                            er,
                            ui.id().with(("cap-edge", lane as u8, i, side)),
                            Sense::drag(),
                        );
                        let hot = eresp.hovered() || eresp.dragged();
                        if side == 0 {
                            edge_hot.0 = hot;
                        } else {
                            edge_hot.1 = hot;
                        }
                        if hot {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                        }
                        if eresp.drag_started_by(egui::PointerButton::Primary)
                            && self.drag.is_none()
                        {
                            if let Some(pos) = eresp.interact_pointer_pos() {
                                begin_drag = Some(if side == 0 {
                                    TimelineDrag::CapTrimStart {
                                        lane,
                                        unit: i,
                                        orig_start: u.start_s,
                                        grab_t: x_to_t(pos.x),
                                    }
                                } else {
                                    TimelineDrag::CapTrimEnd {
                                        lane,
                                        unit: i,
                                        orig_end: u.end_s,
                                        grab_t: x_to_t(pos.x),
                                    }
                                });
                            }
                        }
                    }
                }
                let hot = resp.hovered() || dragging_this || selected;
                // Eye-off tracks stay visible and editable, dimmed (output
                // visibility is not timeline visibility — ADR 0066).
                let dim = if flags.eye { 1.0 } else { 0.45 };
                p.rect_filled(
                    r,
                    CornerRadius::same(4),
                    Color32::from_rgba_unmultiplied(
                        accent.r(),
                        accent.g(),
                        accent.b(),
                        ((if hot { 54 } else { 30 }) as f32 * dim) as u8,
                    ),
                );
                p.rect_stroke(
                    r,
                    CornerRadius::same(4),
                    Stroke::new(
                        if dragging_this || selected || is_hidden { 1.5 } else { 1.0 },
                        (if is_hidden {
                            theme::ERR
                        } else if hot {
                            theme::GOLD
                        } else {
                            accent.gamma_multiply(0.5)
                        })
                        .gamma_multiply(dim),
                    ),
                    StrokeKind::Inside,
                );
                // Trim affordance: an accent tick on the hovered edge.
                for (on, x) in [(edge_hot.0, r.left() + 1.5), (edge_hot.1, r.right() - 1.5)] {
                    if on {
                        p.line_segment(
                            [egui::pos2(x, r.top() + 2.0), egui::pos2(x, r.bottom() - 2.0)],
                            Stroke::new(3.0, theme::GOLD),
                        );
                    }
                }
                if r.width() > 24.0 {
                    p.text(
                        r.left_center() + egui::vec2(4.0, 0.0),
                        Align2::LEFT_CENTER,
                        ellipsize(&u.text, (r.width() / 7.0) as usize),
                        FontId::proportional(10.0),
                        Color32::from_gray(200).gamma_multiply(dim),
                    );
                }
            }
        }
        // The burn-line rail: how the AUTO captions group into on-screen
        // lines (grouping genres only — a huge-word line IS its unit; the
        // operator's stream doesn't group, it shows each caption verbatim).
        // Dragging a rail moves the whole line's units together. Cumulative
        // word counts ARE the unit ranges (`preview_lines` builds each line
        // from consecutive units, one word per unit). An eye-off auto track
        // burns no lines, so the rail vanishes with it (ADR 0066).
        if self.style.genre != CaptionGenre::HugeWord && self.trk_auto.eye {
            let n_units = self.transcript.as_ref().map(|t| t.units.len()).unwrap_or(0);
            let mut base = 0usize;
            for (li, l) in self.lines.iter().enumerate() {
                let range = (base, base + l.words.len());
                base = range.1;
                if range.1 > n_units {
                    continue; // stale lines mid-frame; sync_lines heals next frame
                }
                let targets: Vec<(CapLane, usize)> =
                    (range.0..range.1).map(|i| (CapLane::Auto, i)).collect();
                let rr = Rect::from_min_max(
                    egui::pos2(t_to_x(l.start_s), rail_y),
                    egui::pos2(t_to_x(l.end_s).max(t_to_x(l.start_s) + 2.0), rail_y + 4.0),
                );
                if rr.min.x > lanes.right() || rr.max.x < lanes.left() {
                    continue; // scrolled out of the viewport window
                }
                let rresp = ui
                    .interact(
                        rr.expand2(egui::vec2(0.0, 1.5)).intersect(lanes),
                        ui.id().with(("cap-rail", li)),
                        Sense::drag(),
                    )
                    .on_hover_text(if self.trk_auto.lock {
                        "One on-screen line — track locked (unlock in its header)"
                    } else {
                        "One on-screen line — drag to move all its captions together"
                    });
                let hot = rresp.hovered()
                    || drag_targets.as_ref().is_some_and(|t| *t == targets);
                if rresp.hovered() && !self.trk_auto.lock {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
                if rresp.drag_started_by(egui::PointerButton::Primary)
                    && self.drag.is_none()
                    && !self.trk_auto.lock
                {
                    if let (Some(pos), Some(t)) =
                        (rresp.interact_pointer_pos(), &self.transcript)
                    {
                        begin_drag = Some(TimelineDrag::CapMove {
                            targets: targets.clone(),
                            orig: t.units[range.0..range.1]
                                .iter()
                                .map(|u| (u.start_s, u.end_s))
                                .collect(),
                            grab_t: x_to_t(pos.x),
                        });
                    }
                }
                p.rect_filled(
                    rr,
                    CornerRadius::same(2),
                    if hot {
                        theme::GOLD.gamma_multiply(0.8)
                    } else {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 60)
                    },
                );
            }
        }
        if let Some(d) = begin_drag {
            self.drag = Some(d);
        }
        if let Some((lane, i)) = delete_unit {
            match lane {
                CapLane::Auto => {
                    if let Some(t) = &mut self.transcript {
                        if i < t.units.len() {
                            t.units.remove(i);
                            self.sel_unit = None;
                            self.transcript_dirty = true;
                            self.lines_dirty = true;
                        }
                    }
                }
                CapLane::Manual => self.remove_manual_caption(i),
            }
        }
        if let Some((lane, i)) = clicked_unit {
            let start = match lane {
                CapLane::Auto => {
                    self.transcript.as_ref().and_then(|t| t.units.get(i)).map(|u| u.start_s)
                }
                CapLane::Manual => self.manual_units.get(i).map(|u| u.start_s),
            };
            if let Some(start) = start {
                self.playhead_s = start.clamp(0.0, dur);
                self.viewport.ensure_visible(self.playhead_s, dur);
                match lane {
                    CapLane::Auto => {
                        self.sel_unit = Some(i);
                        self.scroll_to_sel = true;
                    }
                    CapLane::Manual => self.sel_manual = Some(i),
                }
                // Same contract as the scrub: playing audio + video restart
                // at the jump.
                if self.playing.is_some() {
                    self.playing = Some((Instant::now(), self.playhead_s));
                    self.start_video();
                    action = Some(EditorAction::Play(self.play_range_from(self.playhead_s)));
                }
            }
        }

        // Speaker lanes — analysis visualization; the zone's eye hides it
        // (view-only: nothing here ever burns).
        let mut lane_y = rail_y + 6.0;
        if let Some(a) = self.speakers.as_ref().filter(|_| self.speakers_eye) {
            for tr in &a.tracks {
                let color = theme::track_color(tr.id);
                let mut i = 0usize;
                while i < a.speaking.len() {
                    if a.speaking[i] == Some(tr.id) {
                        let t0 = i as f64 * a.bin_s;
                        let mut j = i;
                        while j < a.speaking.len() && a.speaking[j] == Some(tr.id) {
                            j += 1;
                        }
                        let t1 = j as f64 * a.bin_s;
                        p.rect_filled(
                            Rect::from_min_max(
                                egui::pos2(t_to_x(t0), lane_y),
                                egui::pos2(t_to_x(t1), lane_y + lane_h - 4.0),
                            ),
                            CornerRadius::same(2),
                            color.gamma_multiply(0.75),
                        );
                        i = j;
                    } else {
                        i += 1;
                    }
                }
                lane_y += lane_h;
            }
            // The evidence row: the audio lanes themselves, beside the fused
            // seat lanes above — the voice lane (ADR 0042) as spans where the
            // voice claims a seat, in that seat's colour (dimmer: it is
            // evidence, not the camera), KNOWN off-screen voice in red, and
            // the shared-reaction mask (ADR 0046) in gold. The operator can
            // see WHY a split or a rescued cut happened.
            if a.voice.is_some() || a.reaction.is_some() {
                let span = |n: usize, sel: &dyn Fn(usize) -> Option<Color32>| {
                    let mut i = 0usize;
                    while i < n {
                        let Some(color) = sel(i) else {
                            i += 1;
                            continue;
                        };
                        let mut j = i;
                        while j < n && sel(j) == Some(color) {
                            j += 1;
                        }
                        p.rect_filled(
                            Rect::from_min_max(
                                egui::pos2(t_to_x(i as f64 * a.bin_s), lane_y + 1.0),
                                egui::pos2(t_to_x(j as f64 * a.bin_s), lane_y + lane_h - 5.0),
                            ),
                            CornerRadius::same(2),
                            color,
                        );
                        i = j;
                    }
                };
                if let Some(v) = &a.voice {
                    span(v.seat.len(), &|b: usize| {
                        v.seat[b].map(|s| theme::track_color(s).gamma_multiply(0.45))
                    });
                    span(v.offscreen.len(), &|b: usize| {
                        v.offscreen[b].then(|| theme::ERR.gamma_multiply(0.8))
                    });
                }
                if let Some(r) = &a.reaction {
                    span(r.len(), &|b: usize| {
                        (r[b] >= yc_frame::speaker::REACTION_TAU)
                            .then(|| theme::GOLD.gamma_multiply(0.6))
                    });
                }
            }
        }

        // Camera cut markers (Active Speaker) — draggable cut points (feature
        // plan #3): drag moves the boundary between the two shots (frame-
        // snapped), right-click deletes the cut (the earlier shot's framing
        // carries across the merged span), ✂ Split in the transport adds one.
        if self.camera_mode == CameraMode::ActiveSpeaker {
            let mut begin_cut: Option<TimelineDrag> = None;
            let mut delete_cut: Option<usize> = None;
            if let Some(plan) = &self.plan {
                for (i, s) in plan.shots.iter().enumerate().skip(1) {
                    let x = t_to_x(s.start_s);
                    if x < lanes.left() - 6.0 || x > lanes.right() + 6.0 {
                        continue; // scrolled out of the viewport window
                    }
                    let handle = Rect::from_min_max(
                        egui::pos2(x - 5.0, rect.top() + 14.0),
                        egui::pos2(x + 5.0, content_bottom - 2.0),
                    );
                    let resp = ui.interact(handle, ui.id().with(("cut", i)), Sense::click_and_drag());
                    let hot = resp.hovered()
                        || matches!(&self.drag, Some(TimelineDrag::Cut { boundary, .. }) if *boundary == i);
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                    if resp.drag_started_by(egui::PointerButton::Primary) && self.drag.is_none() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            begin_cut = Some(TimelineDrag::Cut {
                                boundary: i,
                                orig_t: s.start_s,
                                grab_t: x_to_t(pos.x),
                            });
                        }
                    }
                    resp.on_hover_text(format!(
                        "Camera cut at {} — the framing changes here (no time removed)\n\
                         Drag to move · right-click to delete",
                        fmt_mmss_cc(s.start_s)
                    ))
                    .context_menu(|ui| {
                        if ui.button("🗑 Delete camera cut").clicked() {
                            delete_cut = Some(i);
                            ui.close();
                        }
                    });
                    p.line_segment(
                        [egui::pos2(x, rect.top() + 14.0), egui::pos2(x, content_bottom - 2.0)],
                        Stroke::new(
                            if hot { 2.0 } else { 1.0 },
                            theme::GOLD.gamma_multiply(if hot { 1.0 } else { 0.6 }),
                        ),
                    );
                    // The ◆ badge, painted (no font in the stack carries ◆).
                    let dc = egui::pos2(x, content_bottom - 8.0);
                    p.add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(dc.x, dc.y - 4.0),
                            egui::pos2(dc.x + 3.5, dc.y),
                            egui::pos2(dc.x, dc.y + 4.0),
                            egui::pos2(dc.x - 3.5, dc.y),
                        ],
                        theme::GOLD.gamma_multiply(if hot { 1.0 } else { 0.8 }),
                        Stroke::NONE,
                    ));
                }
            }
            if let Some(d) = begin_cut {
                self.drag = Some(d);
            }
            if let Some(i) = delete_cut {
                self.delete_cut(i);
            }
        }

        // Timeline razor (feature plan #3, ADR 0065): removed segments veiled
        // (they are cut from the export — video, audio, captions), white ✂
        // markers on the cut boundaries, draggable; segment removal toggles
        // live in the strip's right-click menu.
        {
            let segs = self.razor.segments(dur);
            for (i, (a, b)) in segs.iter().enumerate() {
                if !*self.razor.removed.get(i).unwrap_or(&false) {
                    continue;
                }
                let r = Rect::from_min_max(
                    egui::pos2(t_to_x(*a), rect.top() + 14.0),
                    egui::pos2(t_to_x(*b), content_bottom - 2.0),
                );
                let vis = r.intersect(lanes);
                if vis.width() <= 0.0 {
                    continue; // scrolled out of the viewport window
                }
                p.rect_filled(r, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(0, 0, 0, 150));
                if vis.width() > 46.0 {
                    p.text(
                        vis.center(),
                        Align2::CENTER_CENTER,
                        "cut out",
                        FontId::proportional(10.0),
                        Color32::from_gray(160),
                    );
                }
            }
            let mut begin_razor: Option<TimelineDrag> = None;
            let mut delete_razor: Option<usize> = None;
            for (k, c) in self.razor.cuts.iter().enumerate() {
                let x = t_to_x(*c);
                if x < lanes.left() - 6.0 || x > lanes.right() + 6.0 {
                    continue; // scrolled out of the viewport window
                }
                let handle = Rect::from_min_max(
                    egui::pos2(x - 5.0, rect.top() + 2.0),
                    egui::pos2(x + 5.0, content_bottom - 2.0),
                );
                let resp = ui.interact(handle, ui.id().with(("razor", k)), Sense::click_and_drag());
                let hot = resp.hovered()
                    || matches!(&self.drag, Some(TimelineDrag::Razor { idx, .. }) if *idx == k);
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if resp.drag_started_by(egui::PointerButton::Primary) && self.drag.is_none() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        begin_razor =
                            Some(TimelineDrag::Razor { idx: k, orig_t: *c, grab_t: x_to_t(pos.x) });
                    }
                }
                resp.on_hover_text(format!(
                    "Clip cut at {}\nDrag to move · right-click to delete (merges the segments)",
                    fmt_mmss_cc(*c)
                ))
                .context_menu(|ui| {
                    if ui.button("🗑 Delete clip cut").clicked() {
                        delete_razor = Some(k);
                        ui.close();
                    }
                });
                let col = if hot { Color32::WHITE } else { Color32::from_gray(200) };
                p.line_segment(
                    [egui::pos2(x, rect.top() + 2.0), egui::pos2(x, content_bottom - 2.0)],
                    Stroke::new(if hot { 2.0 } else { 1.2 }, col),
                );
                p.text(
                    egui::pos2(x + 2.0, rect.top() + 2.0),
                    Align2::LEFT_TOP,
                    "✂",
                    FontId::proportional(9.0),
                    col,
                );
            }
            if let Some(d) = begin_razor {
                self.drag = Some(d);
            }
            if let Some(k) = delete_razor {
                self.razor.delete_cut(k);
            }
        }

        // The ▼ cut marker: the ✂←/→✂ reference point. Draggable; right-click
        // clears it. Culled (not cleared) when scrolled out of the window.
        if let Some((m, x)) = self
            .cut_marker
            .map(|m| (m, t_to_x(m)))
            .filter(|(_, x)| *x >= lanes.left() - 7.0 && *x <= lanes.right() + 7.0)
        {
            let handle = Rect::from_min_max(
                egui::pos2(x - 6.0, rect.top() + 2.0),
                egui::pos2(x + 6.0, content_bottom - 2.0),
            );
            let resp = ui.interact(handle, ui.id().with("cut-marker"), Sense::click_and_drag());
            let hot = resp.hovered() || matches!(self.drag, Some(TimelineDrag::Marker { .. }));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if resp.drag_started_by(egui::PointerButton::Primary) && self.drag.is_none() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    self.drag = Some(TimelineDrag::Marker { orig_t: m, grab_t: x_to_t(pos.x) });
                }
            }
            let mut clear_marker = false;
            resp.on_hover_text(format!(
                "Cut marker at {} — ✂← / →✂ cut from the playhead to here\n\
                 Drag to move · right-click to clear",
                fmt_mmss_cc(m)
            ))
            .context_menu(|ui| {
                if ui.button("🗑 Clear marker").clicked() {
                    clear_marker = true;
                    ui.close();
                }
            });
            if clear_marker {
                self.cut_marker = None;
            } else {
                let col = theme::INFO.gamma_multiply(if hot { 1.0 } else { 0.8 });
                p.line_segment(
                    [egui::pos2(x, rect.top() + 2.0), egui::pos2(x, content_bottom - 2.0)],
                    Stroke::new(if hot { 2.0 } else { 1.4 }, col),
                );
                // The ▼ head, painted (no font in the stack carries ▼).
                p.add(egui::Shape::convex_polygon(
                    vec![
                        egui::pos2(x - 5.0, rect.top() + 2.0),
                        egui::pos2(x + 5.0, rect.top() + 2.0),
                        egui::pos2(x, rect.top() + 10.0),
                    ],
                    col,
                    Stroke::NONE,
                ));
            }
        }

        // The strip's right-click menu: razor verbs at the click position
        // (recorded when the menu opened — the pointer moves once it's open).
        if resp.secondary_clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                self.strip_menu_t = Some(x_to_t(pos.x));
            }
        }
        resp.context_menu(|ui| {
            let at = self.strip_menu_t.unwrap_or(self.playhead_s);
            if ui.button(format!("✂ Cut clip here ({})", fmt_mmss(at))).clicked() {
                let t = if self.src_fps > 0.0 {
                    (at * self.src_fps).round() / self.src_fps
                } else {
                    at
                };
                self.razor.add_cut(t, dur);
                ui.close();
            }
            if ui.button("⏷ Place cut marker here").clicked() {
                self.cut_marker = Some(at);
                ui.close();
            }
            let seg = self.razor.seg_index_at(at);
            let removed = *self.razor.removed.get(seg).unwrap_or(&false);
            let label = if removed { "Restore this segment" } else { "Remove this segment from the export" };
            if ui.button(label).clicked() {
                if !self.razor.toggle_segment(seg) && !removed {
                    // The refusal case: this is the last kept segment.
                    tracing::info!("razor: refused to remove the last kept segment");
                }
                ui.close();
            }
            if self.camera_mode == CameraMode::ActiveSpeaker
                && self.plan.is_some()
                && ui.button("Camera cut here (framing change)").clicked()
            {
                let keep_playhead = self.playhead_s;
                self.playhead_s = at;
                self.split_shot_at_playhead();
                self.playhead_s = keep_playhead;
                ui.close();
            }
        });

        // Playhead — with its own grab handle at the head, registered AFTER
        // every cut/marker handle so scrubbing by the head can never grab a
        // marker underneath (operator ask 2026-07-13: the playhead parked on
        // a razor cut dragged the cut instead). Markers stay grabbable on
        // their lines below the head's zone. The handle exists only while
        // the playhead sits inside the viewport window.
        let px = t_to_x(self.playhead_s);
        let mut ph_hot = false;
        if px >= lanes.left() - 8.0 && px <= lanes.right() + 8.0 {
            let phandle = Rect::from_min_max(
                egui::pos2(px - 8.0, rect.top()),
                egui::pos2(px + 8.0, rect.top() + 16.0),
            );
            let presp =
                ui.interact(phandle, ui.id().with("playhead-grab"), Sense::click_and_drag());
            if presp.hovered() || presp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if presp.dragged() {
                if let Some(pos) = presp.interact_pointer_pos() {
                    // Same contract as the strip scrub: visuals track the
                    // pointer; the audio commits on release (a per-frame
                    // sink restart is a re-seek storm).
                    self.playhead_s = x_to_t(pos.x);
                    if self.playing.is_some() {
                        self.playing = Some((Instant::now(), self.playhead_s));
                        self.stop_video();
                    }
                }
            }
            if presp.drag_stopped() && self.playing.is_some() {
                self.playing = Some((Instant::now(), self.playhead_s));
                self.start_video();
                action = Some(EditorAction::Play(self.play_range_from(self.playhead_s)));
            }
            ph_hot = presp.hovered() || presp.dragged();
        }
        let px = t_to_x(self.playhead_s);
        p.line_segment(
            [egui::pos2(px, rect.top()), egui::pos2(px, content_bottom)],
            Stroke::new(2.0, theme::GOLD),
        );
        p.circle_filled(
            egui::pos2(px, rect.top() + 4.0),
            if ph_hot { 5.5 } else { 4.0 },
            theme::GOLD,
        );

        // Gesture feedback, above everything: the magnet guide (the anchor a
        // drag snapped onto) and the m:ss.cc chip at the manipulated time.
        if let Some(x) = snap_x {
            p.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, content_bottom)],
                Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.7)),
            );
        }
        if let Some(t) = drag_chip_t.filter(|_| self.drag.is_some()) {
            chip(
                &p,
                egui::pos2(
                    (t_to_x(t) + 8.0).clamp(lanes.left() + 2.0, rect.right() - 64.0),
                    rect.top() + 16.0,
                ),
                &fmt_mmss_cc(t),
                theme::GOLD,
            );
        }

        // Scrub: click or drag anywhere on the strip. While dragging, only the
        // visuals track the pointer (restarting the sink every frame is a
        // re-seek storm); the audio commits at the NEW time on click and on
        // drag RELEASE. The release must be its own check: on that frame
        // `dragged()` is already false, so a release branch nested under it
        // can never run — the bug where dragging mid-playback moved the
        // playhead while the audio kept playing from the old position.
        // A caption/cut gesture owns the pointer instead — no scrub then.
        if self.drag.is_none() && (resp.clicked() || resp.dragged()) {
            if let Some(pos) = resp.interact_pointer_pos() {
                self.playhead_s = x_to_t(pos.x);
                if self.playing.is_some() {
                    self.playing = Some((Instant::now(), self.playhead_s));
                    if resp.clicked() {
                        self.start_video();
                        action = Some(EditorAction::Play(self.play_range_from(self.playhead_s)));
                    } else {
                        // Mid-drag: the filmstrip tracks the pointer (the live
                        // stream would show the OLD position until release).
                        self.stop_video();
                    }
                }
            }
        }
        if self.drag.is_none() && resp.drag_stopped() && self.playing.is_some() {
            self.playing = Some((Instant::now(), self.playhead_s));
            self.start_video();
            action = Some(EditorAction::Play(self.play_range_from(self.playhead_s)));
        }

        // ---- Header column (plan #13, ADR 0066): names + painted toggles,
        // painted LAST so lane content can never cover it. Only toggles whose
        // OFF state is real today appear (per-kind honesty): Video 1 has no
        // eye (hiding the only video would export nothing) and no mute (that
        // arrives with the audio-mixer arc); the caption tracks carry
        // eye + lock; Speakers is an eye-only analysis zone.
        let hdr = Rect::from_min_max(rect.min, egui::pos2(lanes.left(), rect.bottom()));
        ph.rect_filled(
            hdr,
            CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 },
            Color32::from_rgb(0x16, 0x19, 0x1F),
        );
        ph.line_segment(
            [hdr.right_top(), hdr.right_bottom()],
            Stroke::new(1.0, Color32::from_gray(50)),
        );
        let name_font = FontId::proportional(9.5);
        let name_col = Color32::from_gray(200);
        // Corner: zoom verbs — the discoverable fallback for ctrl+wheel.
        let center_t = vp.left_t + vp.span(dur) * 0.5;
        let btn_r = |x0: f32, w: f32| {
            Rect::from_min_max(
                egui::pos2(hdr.left() + x0, hdr.top() + 2.0),
                egui::pos2(hdr.left() + x0 + w, hdr.top() + 15.0),
            )
        };
        if mini_btn(ui, &ph, btn_r(6.0, 16.0), "-", "Zoom out (or ctrl+wheel on the timeline)") {
            self.viewport.zoom_about(center_t, 0.5, 1.0 / 1.5, dur);
        }
        if mini_btn(ui, &ph, btn_r(24.0, 16.0), "+", "Zoom in (ctrl+wheel zooms about the pointer)")
        {
            self.viewport.zoom_about(center_t, 0.5, 1.5, dur);
        }
        if mini_btn(ui, &ph, btn_r(42.0, 24.0), "Fit", "Show the whole clip") {
            self.viewport = Viewport::default();
        }
        if vp.zoom > 1.001 {
            ph.text(
                egui::pos2(hdr.right() - 5.0, hdr.top() + 8.5),
                Align2::RIGHT_CENTER,
                format!("{:.1}×", vp.zoom),
                FontId::proportional(8.5),
                Color32::from_gray(140),
            );
        }
        // Every header is a CARD spanning exactly its track's lane band
        // ("same size between title and actual track" — the operator's
        // round-2 verdict), with a kind-colored accent bar that dims when
        // the track's eye is off. Blue = video, gold = the pipeline's
        // captions, green = the operator's own, gray = analysis.
        // Video 1 · Main (the filmstrip).
        let card = track_card(&ph, hdr, film_y0, film_y1, theme::INFO, true);
        ph.text(
            egui::pos2(card.left() + 10.0, card.center().y),
            Align2::LEFT_CENTER,
            "Video 1 · Main",
            name_font.clone(),
            name_col,
        );
        // The two caption tracks: eye + lock. The AI/operator separation is
        // architectural (operator ruling 2026-07-13) — these never merge.
        for (y0, label, is_auto) in
            [(cap_y0, "Captions · auto", true), (man_y0, "Captions · yours", false)]
        {
            let mut f = if is_auto { self.trk_auto } else { self.trk_manual };
            let accent = if is_auto { theme::GOLD } else { theme::OK };
            let card = track_card(&ph, hdr, y0, y0 + 22.0, accent, f.eye);
            let cy = card.center().y;
            let eye_tip = if is_auto {
                "Show the auto captions. OFF keeps the whole stream OUT of the export \
                 (and the preview) — the blocks stay on the timeline, dimmed, editable."
            } else {
                "Show your captions. OFF keeps your stream OUT of the export (and the \
                 preview) — the blocks stay on the timeline, dimmed, editable."
            };
            if track_toggle(
                ui,
                &ph,
                egui::pos2(card.left() + 13.0, cy),
                TrackIcon::Eye,
                f.eye,
                (label, 0),
                eye_tip,
            ) {
                f.eye = !f.eye;
            }
            if track_toggle(
                ui,
                &ph,
                egui::pos2(card.left() + 29.0, cy),
                TrackIcon::Lock,
                f.lock,
                (label, 1),
                "Lock this track — timeline drags, trims, and deletes are ignored \
                 (the caption panel still edits)",
            ) {
                f.lock = !f.lock;
            }
            if is_auto {
                self.trk_auto = f;
            } else {
                self.trk_manual = f;
            }
            ph.text(
                egui::pos2(card.left() + 40.0, cy),
                Align2::LEFT_CENTER,
                label,
                name_font.clone(),
                if f.eye { name_col } else { Color32::from_gray(130) },
            );
        }
        // Speakers analysis zone (eye only — nothing here ever burns). Its
        // card spans the whole flex zone; content sits at the top like any
        // tall NLE track header.
        if self.speakers.is_some() {
            let card = track_card(
                &ph,
                hdr,
                rail_y + 6.0,
                content_bottom - 20.0,
                Color32::from_gray(120),
                self.speakers_eye,
            );
            let cy = if card.height() < 20.0 { card.center().y } else { card.top() + 10.0 };
            if track_toggle(
                ui,
                &ph,
                egui::pos2(card.left() + 13.0, cy),
                TrackIcon::Eye,
                self.speakers_eye,
                ("speakers", 0),
                "Show the speaker analysis lanes (who's on camera / voice evidence). \
                 Analysis only — never part of the export.",
            ) {
                self.speakers_eye = !self.speakers_eye;
            }
            ph.text(
                egui::pos2(card.left() + 25.0, cy),
                Align2::LEFT_CENTER,
                "Speakers",
                name_font.clone(),
                if self.speakers_eye { name_col } else { Color32::from_gray(130) },
            );
        }
        // The camera badge gutter's card (Active Speaker only).
        if self.camera_mode == CameraMode::ActiveSpeaker && self.plan.is_some() {
            let card = track_card(
                &ph,
                hdr,
                content_bottom - 20.0,
                content_bottom - 2.0,
                theme::GOLD.gamma_multiply(0.7),
                true,
            );
            ph.text(
                egui::pos2(card.left() + 10.0, card.center().y),
                Align2::LEFT_CENTER,
                "Camera",
                FontId::proportional(8.5),
                Color32::from_gray(160),
            );
        }

        // ---- Scrollbar row (ADR 0066): the viewport window into the clip.
        let sb = Rect::from_min_max(
            egui::pos2(lanes.left() + 1.0, rect.bottom() - SCROLL_H + 1.0),
            egui::pos2(lanes.right() - 1.0, rect.bottom() - 1.0),
        );
        if vp.zoom > 1.001 {
            let sresp = ui.interact(sb, ui.id().with("tl-scrollbar"), Sense::click_and_drag());
            if sresp.clicked() || sresp.dragged() {
                if let Some(pos) = sresp.interact_pointer_pos() {
                    let frac_w = (vp.span(dur) / dur).clamp(0.0, 1.0) as f32;
                    let thumb_w = (sb.width() * frac_w).clamp(24.0_f32.min(sb.width()), sb.width());
                    let f = ((pos.x - sb.left() - thumb_w * 0.5)
                        / (sb.width() - thumb_w).max(1.0))
                    .clamp(0.0, 1.0);
                    self.viewport.left_t = f as f64 * (dur - vp.span(dur)).max(0.0);
                    self.viewport.clamp(dur);
                }
            }
            let hot = sresp.hovered() || sresp.dragged();
            p.rect_filled(sb, CornerRadius::same(3), Color32::from_gray(28));
            // Thumb geometry from the post-drag viewport so the drag is 1:1.
            let cur = self.viewport;
            let frac_w = (cur.span(dur) / dur).clamp(0.0, 1.0) as f32;
            let thumb_w = (sb.width() * frac_w).clamp(24.0_f32.min(sb.width()), sb.width());
            let denom = (dur - cur.span(dur)).max(1e-9);
            let tf = (cur.left_t / denom).clamp(0.0, 1.0) as f32;
            let tx = sb.left() + tf * (sb.width() - thumb_w);
            p.rect_filled(
                Rect::from_min_max(egui::pos2(tx, sb.top()), egui::pos2(tx + thumb_w, sb.bottom())),
                CornerRadius::same(3),
                if hot { Color32::from_gray(115) } else { Color32::from_gray(78) },
            );
        } else {
            // Fit: the whole clip is the window — a quiet full-width thumb.
            p.rect_filled(sb, CornerRadius::same(3), Color32::from_gray(28));
            p.rect_filled(sb.shrink(1.0), CornerRadius::same(3), Color32::from_gray(42));
        }
        action
    }

    // ----------------------------------------------------- transcript panel --

    /// The left panel: every caption, editable (focus task 2) — text, times,
    /// add / delete / split / merge / censor; click seeks; edits update the
    /// preview immediately and the render burns them verbatim.
    fn ui_transcript_panel(&mut self, ui: &mut egui::Ui, enabled: bool) -> Option<EditorAction> {
        let mut action = None;
        theme::section(ui, "Captions");
        let Some(transcript) = &mut self.transcript else {
            ui.add_space(6.0);
            // Name the engine actually running (ADR 0035) — "whisper" here
            // while the status bar said "Ensemble captions" read as two
            // different mysteries (operator feedback).
            match self.engine {
                CaptionEngine::Whisper => {
                    ui.weak("Transcribing with Whisper…");
                    ui.weak("Usually well under a minute.");
                }
                CaptionEngine::QwenEnsemble => {
                    ui.weak("Transcribing with the Qwen ensemble…");
                    ui.weak(
                        "Whisper decodes first, then five Qwen3-ASR passes vote on the words — \
                         more accurate, adds ~60–90 s. The status bar up top counts each decode \
                         (1/5 … 5/5). (Engine is set per Creator on the import panel.)",
                    );
                }
            }
            ui.weak("You can frame, scrub, and play while it runs; captions appear here when it finishes.");
            return action;
        };
        if transcript.units.is_empty() {
            ui.weak("No speech transcribed in this clip.");
        }
        if self.transcript_dirty {
            ui.label(
                egui::RichText::new("Edited - the render burns your text.")
                    .color(theme::GOLD)
                    .size(11.5),
            );
        }
        if !self.manual_units.is_empty() {
            ui.weak(
                "Your captions burn as their own line ABOVE the auto captions — they can \
                 share time with them.",
            );
        }
        ui.add_space(4.0);

        let mut dirty = false;
        let mut manual_dirty = false;
        // A time widget being edited RIGHT NOW: sorting must wait for it (a
        // mid-edit resort swaps the row under the operator's cursor — the
        // "editing replaced the other caption" bug, 2026-07-12).
        let mut time_edit_active = false;
        let mut seek: Option<f64> = None;
        let mut delete: Option<usize> = None;
        let mut split: Option<usize> = None;
        let mut merge: Option<usize> = None;
        let mut censor: Option<usize> = None;
        let mut delete_manual: Option<usize> = None;
        let playhead = self.playhead_s;
        let n = transcript.units.len();
        let dur = self.range.duration_s();

        egui::ScrollArea::vertical().id_salt("caption-rows").auto_shrink([false, true]).show(ui, |ui| {
            for i in 0..n {
                let active = {
                    let u = &transcript.units[i];
                    u.start_s <= playhead && playhead < u.end_s.max(u.start_s + 0.2)
                };
                let row_bg = if self.sel_unit == Some(i) {
                    theme::GOLD.gamma_multiply(0.12)
                } else if active {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 10)
                } else {
                    Color32::TRANSPARENT
                };
                let row_resp = egui::Frame::new()
                    .fill(row_bg)
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(egui::Margin::symmetric(4, 3))
                    .show(ui, |ui| {
                        // Two-line rows: times + actions up top, the text on
                        // its own FULL-WIDTH line below — a narrow panel must
                        // never squeeze the text field into uneditability
                        // (operator report 2026-07-12).
                        ui.horizontal(|ui| {
                            // Timestamps: editable mm:ss.cc (drag or type).
                            // Start MOVES the caption (duration kept); end
                            // sets how long it stays. The burn still clears a
                            // line at the next line's onset.
                            let u = &mut transcript.units[i];
                            let mut start = u.start_s;
                            let resp = ui.add_enabled(
                                enabled,
                                egui::DragValue::new(&mut start)
                                    .speed(0.02)
                                    .range(0.0..=dur)
                                    .custom_formatter(|v, _| fmt_mmss_cc(v))
                                    .custom_parser(parse_mmss_cc),
                            );
                            if resp.changed() {
                                let d = u.end_s - u.start_s;
                                u.start_s = start;
                                u.end_s = start + d.max(0.05);
                                dirty = true;
                            }
                            if resp.clicked() || resp.gained_focus() {
                                self.sel_unit = Some(i);
                            }
                            let mut end = u.end_s;
                            let eresp = ui
                                .add_enabled(
                                    enabled,
                                    egui::DragValue::new(&mut end)
                                        .speed(0.02)
                                        .range(0.0..=dur)
                                        .custom_formatter(|v, _| fmt_mmss_cc(v))
                                        .custom_parser(parse_mmss_cc),
                                )
                                .on_hover_text(
                                    "When this caption leaves the screen (the next caption's \
                                     start still wins at burn time)",
                                );
                            if eresp.changed() {
                                u.end_s = end.max(u.start_s + CAP_MIN_S);
                                dirty = true;
                            }
                            if eresp.clicked() || eresp.gained_focus() {
                                self.sel_unit = Some(i);
                            }
                            time_edit_active |= resp.dragged()
                                || resp.has_focus()
                                || eresp.dragged()
                                || eresp.has_focus();
                            // Row actions, right-aligned and compact.
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_enabled_ui(enabled, |ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    ui.spacing_mut().button_padding = egui::vec2(4.0, 2.0);
                                    if ui.small_button("🗑").on_hover_text("Delete this caption").clicked() {
                                        delete = Some(i);
                                    }
                                    if ui.small_button("＊").on_hover_text("Censor this word (d***)").clicked() {
                                        censor = Some(i);
                                    }
                                    if i + 1 < n {
                                        if ui.small_button("⇓").on_hover_text("Merge with the next caption").clicked() {
                                            merge = Some(i);
                                        }
                                    } else {
                                        ui.add_enabled(false, egui::Button::new("⇓").small());
                                    }
                                    if ui.small_button("✂").on_hover_text("Split this caption").clicked() {
                                        split = Some(i);
                                    }
                                });
                            });
                        });
                        let u = &mut transcript.units[i];
                        let text_resp = ui.add_enabled(
                            enabled,
                            egui::TextEdit::singleline(&mut u.text)
                                .id(egui::Id::new(("cap-text", i)))
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Body),
                        );
                        if self.focus_caption == Some((CapLane::Auto, i)) {
                            // Double-clicked on the preview: edit here.
                            text_resp.request_focus();
                            self.focus_caption = None;
                        }
                        if text_resp.changed() {
                            dirty = true;
                        }
                        if text_resp.gained_focus() {
                            self.sel_unit = Some(i);
                            seek = Some(u.start_s);
                        }
                    });
                // A timeline caption-block click selects this row: bring it
                // into view (one-shot).
                if self.scroll_to_sel && self.sel_unit == Some(i) {
                    row_resp.response.scroll_to_me(Some(egui::Align::Center));
                }
            }

            // The operator's OWN captions (the dedicated lane): same editing
            // grammar, minus split/merge/censor (type what you mean).
            if !self.manual_units.is_empty() {
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Your captions").strong().size(12.5));
                for i in 0..self.manual_units.len() {
                    let row_bg = if self.sel_manual == Some(i) {
                        theme::GOLD.gamma_multiply(0.12)
                    } else {
                        Color32::TRANSPARENT
                    };
                    // The preview's double-click handed focus here: bring the
                    // row into view (checked before the closure clears it).
                    let focus_this = self.focus_caption == Some((CapLane::Manual, i));
                    let row_resp = egui::Frame::new()
                        .fill(row_bg)
                        .corner_radius(CornerRadius::same(4))
                        .inner_margin(egui::Margin::symmetric(4, 3))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let u = &mut self.manual_units[i];
                                let mut start = u.start_s;
                                let resp = ui.add_enabled(
                                    enabled,
                                    egui::DragValue::new(&mut start)
                                        .speed(0.02)
                                        .range(0.0..=dur)
                                        .custom_formatter(|v, _| fmt_mmss_cc(v))
                                        .custom_parser(parse_mmss_cc),
                                );
                                if resp.changed() {
                                    let d = u.end_s - u.start_s;
                                    u.start_s = start;
                                    u.end_s = start + d.max(0.05);
                                    manual_dirty = true;
                                }
                                let mut end = u.end_s;
                                let eresp = ui.add_enabled(
                                    enabled,
                                    egui::DragValue::new(&mut end)
                                        .speed(0.02)
                                        .range(0.0..=dur)
                                        .custom_formatter(|v, _| fmt_mmss_cc(v))
                                        .custom_parser(parse_mmss_cc),
                                );
                                if eresp.changed() {
                                    u.end_s = end.max(u.start_s + CAP_MIN_S);
                                    manual_dirty = true;
                                }
                                if resp.clicked()
                                    || resp.gained_focus()
                                    || eresp.clicked()
                                    || eresp.gained_focus()
                                {
                                    self.sel_manual = Some(i);
                                }
                                time_edit_active |= resp.dragged()
                                    || resp.has_focus()
                                    || eresp.dragged()
                                    || eresp.has_focus();
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.add_enabled_ui(enabled, |ui| {
                                            ui.spacing_mut().button_padding = egui::vec2(4.0, 2.0);
                                            if ui
                                                .small_button("🗑")
                                                .on_hover_text("Delete this caption")
                                                .clicked()
                                            {
                                                delete_manual = Some(i);
                                            }
                                        });
                                    },
                                );
                            });
                            let u = &mut self.manual_units[i];
                            let text_resp = ui.add_enabled(
                                enabled,
                                egui::TextEdit::singleline(&mut u.text)
                                    .id(egui::Id::new(("man-text", i)))
                                    .desired_width(f32::INFINITY)
                                    .font(egui::TextStyle::Body),
                            );
                            if self.focus_caption == Some((CapLane::Manual, i)) {
                                // Double-clicked on the preview: edit here.
                                text_resp.request_focus();
                                self.focus_caption = None;
                            }
                            if text_resp.changed() {
                                manual_dirty = true;
                            }
                            if text_resp.gained_focus() {
                                self.sel_manual = Some(i);
                                seek = Some(u.start_s);
                            }
                        });
                    if focus_this {
                        row_resp.response.scroll_to_me(Some(egui::Align::Center));
                    }
                }
            }
        });
        self.scroll_to_sel = false;

        ui.add_space(6.0);
        // Deferred: `add_caption_at_playhead` needs `&mut self`, still
        // borrowed as `transcript` here (the timeline's ＋ Caption button is
        // the same verb).
        let mut add_at_playhead = false;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(enabled, egui::Button::new("＋ Add caption at playhead"))
                .clicked()
            {
                add_at_playhead = true;
            }
        });

        // Apply the row actions after the loop (indices stay valid).
        if let Some(i) = censor {
            let u = &mut transcript.units[i];
            u.text = censor_text(&u.text);
            dirty = true;
        }
        if let Some(i) = split {
            let u = transcript.units[i].clone();
            if let Some((a, b)) = split_unit(&u) {
                transcript.units[i] = a;
                transcript.units.insert(i + 1, b);
                dirty = true;
            }
        }
        if let Some(i) = merge {
            if i + 1 < transcript.units.len() {
                let next = transcript.units.remove(i + 1);
                let u = &mut transcript.units[i];
                u.text = format!("{} {}", u.text.trim_end(), next.text.trim_start());
                u.end_s = next.end_s.max(u.end_s);
                dirty = true;
            }
        }
        if let Some(i) = delete {
            transcript.units.remove(i);
            self.sel_unit = None;
            dirty = true;
        }
        if let Some(i) = delete_manual {
            if i < self.manual_units.len() {
                // Inline while `transcript` is still borrowed (disjoint
                // fields): same pairing contract as remove_manual_caption.
                self.manual_units.remove(i);
                self.manual_places.remove(i);
                self.sel_manual = None;
                self.lines_dirty = true;
                manual_dirty = true;
            }
        }
        if dirty {
            self.transcript_dirty = true;
        }
        if dirty || manual_dirty {
            self.lines_dirty = true;
            self.panel_sort_pending = true;
        }
        // Keep units start-ordered so grouping/preview stay sane — but only
        // once no time widget is active: sorting mid-edit swaps the row under
        // the operator's cursor and the edit lands on the WRONG caption.
        if self.panel_sort_pending && !time_edit_active {
            transcript.units.sort_by(|a, b| {
                a.start_s.partial_cmp(&b.start_s).unwrap_or(std::cmp::Ordering::Equal)
            });
            self.sort_manual_units(); // placements ride along
            self.panel_sort_pending = false;
        }
        if add_at_playhead {
            self.add_caption_at_playhead();
        }
        if let Some(t) = seek {
            self.playhead_s = t.clamp(0.0, self.range.duration_s());
            self.viewport.ensure_visible(self.playhead_s, self.range.duration_s());
            // Seeking during playback restarts audio + video at the row's
            // time — same contract as the timeline scrub.
            if self.playing.is_some() {
                self.playing = Some((Instant::now(), self.playhead_s));
                self.start_video();
                action = Some(EditorAction::Play(self.play_range_from(self.playhead_s)));
            }
        }
        action
    }

    // ---------------------------------------------------------- properties --

    fn ui_properties(
        &mut self,
        ui: &mut egui::Ui,
        busy: bool,
        prefs: &mut crate::settings::AppSettings,
    ) -> Option<EditorAction> {
        let mut action = None;

        // --- Camera ---
        theme::section(ui, "Camera");
        theme::card().show(ui, |ui| {
            for mode in [
                CameraMode::Manual,
                CameraMode::Center,
                CameraMode::AutoFace,
                CameraMode::ActiveSpeaker,
                CameraMode::Group,
            ] {
                let selected = self.camera_mode == mode;
                let label = match mode {
                    CameraMode::ActiveSpeaker => {
                        format!("{}  ·  best for podcasts", camera_mode_label(mode))
                    }
                    _ => camera_mode_label(mode).to_string(),
                };
                // Full-width rows: every mode the same size, nothing shifts.
                let mut resp = theme::wide_button(ui, theme::chip(selected, &label));
                if matches!(mode, CameraMode::ActiveSpeaker | CameraMode::Group) {
                    // One control, not two: picking an AI mode IS the speaker
                    // detection (the old separate "Detect speakers" button ran
                    // the identical job and read as a different feature).
                    resp = resp
                        .on_hover_text("First use runs the speaker analysis (CPU, ~10-30 s)");
                }
                if resp.clicked() {
                    self.camera_mode = mode;
                    if matches!(mode, CameraMode::ActiveSpeaker | CameraMode::Group)
                        && self.speakers.is_none()
                        && self.speaker_job == SpeakerJob::NotRun
                    {
                        self.speaker_job = SpeakerJob::Running;
                        action = Some(EditorAction::AnalyzeSpeakers);
                    }
                }
            }
            ui.add_space(4.0);
            match &self.speaker_job {
                SpeakerJob::NotRun => {
                    ui.weak("Active Speaker / Group analyze speakers on first use.");
                }
                SpeakerJob::Running => {
                    ui.weak("Analyzing speakers…");
                }
                SpeakerJob::Ready => {
                    if let Some(a) = &self.speakers {
                        for t in &a.tracks {
                            ui.label(
                                egui::RichText::new(format!(
                                    "■ {} · visible {:.0}%",
                                    track_label(t.id),
                                    t.presence * 100.0
                                ))
                                .color(theme::track_color(t.id))
                                .size(12.0),
                            );
                        }
                        if let Some(plan) = &self.plan {
                            ui.weak(format!("{} camera cuts planned", plan.shots.len().saturating_sub(1)));
                        }
                        // Voice lane status (ADR 0042/0044): what the
                        // diarization evidence contributed — or why a lane is
                        // off/degraded (one note line per lane).
                        if let Some(v) = &a.voice {
                            let bin_s = a.bin_s.max(1e-9);
                            let claimed = v.seat.iter().flatten().count() as f64 * bin_s;
                            let off = v.offscreen.iter().filter(|o| **o).count() as f64 * bin_s;
                            let over = v.overridden.iter().filter(|o| **o).count() as f64 * bin_s;
                            let mut line = format!("Voice lane: {claimed:.1}s attributed");
                            if over > 0.0 {
                                line.push_str(&format!(" · {over:.1}s corrected"));
                            }
                            if off > 0.0 {
                                line.push_str(&format!(" · {off:.1}s off-screen"));
                            }
                            ui.weak(line);
                        }
                        if let Some(note) = &self.voice_note {
                            for l in note.lines() {
                                ui.weak(l);
                            }
                        }
                        // Camera audit: jitter-class defects caught BEFORE the
                        // export (a crop moving without subject cause).
                        for w in &self.camera_audit {
                            ui.label(
                                egui::RichText::new(format!("⚠ {w}"))
                                    .color(theme::GOLD)
                                    .size(12.0),
                            );
                        }
                        ui.weak("Click a face in Original view to override a shot.");
                    }
                }
                SpeakerJob::Failed(e) => {
                    ui.colored_label(theme::ERR, "Speaker analysis failed").on_hover_text(e);
                    if ui.add_enabled(!busy, egui::Button::new("Retry")).clicked() {
                        self.speaker_job = SpeakerJob::Running;
                        action = Some(EditorAction::AnalyzeSpeakers);
                    }
                }
            }
        });

        // --- Framing (manual) ---
        theme::section(ui, "Framing");
        theme::card().show(ui, |ui| {
            ui.add_enabled_ui(self.camera_mode == CameraMode::Manual, |ui| {
                let kinds = [LayoutKind::Stacked, LayoutKind::FullCam, LayoutKind::FullGameplay];
                if let Some(i) = theme::chip_row(
                    ui,
                    &[
                        (self.kind == LayoutKind::Stacked, "Stacked"),
                        (self.kind == LayoutKind::FullCam, "Full cam"),
                        (self.kind == LayoutKind::FullGameplay, "Wide"),
                    ],
                ) {
                    self.kind = kinds[i];
                }
                if self.kind == LayoutKind::Stacked {
                    ui.horizontal(|ui| {
                        ui.label("Seam");
                        let resp = ui.add(
                            egui::Slider::new(&mut self.seam, 0.2..=0.85).show_value(false),
                        );
                        if resp.changed() {
                            let (ga, fa) = yc_frame::stacked_panel_aspects(self.seam);
                            self.gameplay = yc_frame::reaspect_keep_center(
                                self.gameplay, ga, self.src_w, self.src_h,
                            );
                            self.facecam = yc_frame::reaspect_keep_center(
                                self.facecam, fa, self.src_w, self.src_h,
                            );
                        }
                    });
                }
                // Motion presets (feature plan #9): one-click camera moves
                // rendered as a glide/cut of the manual crop through the same
                // per-shot machinery as the Active Speaker plan.
                if self.kind != LayoutKind::Stacked {
                    ui.label("Motion");
                    let opts: Vec<(bool, &str)> = std::iter::once((self.motion.is_none(), "None"))
                        .chain(MOTION_PRESETS.iter().map(|(m, l)| (self.motion == Some(*m), *l)))
                        .collect();
                    let mut pick: Option<usize> = None;
                    for (ri, row) in opts.chunks(3).enumerate() {
                        if let Some(j) = theme::chip_row(ui, row) {
                            pick = Some(ri * 3 + j);
                        }
                    }
                    if let Some(i) = pick {
                        self.motion = if i == 0 { None } else { Some(MOTION_PRESETS[i - 1].0) };
                    }
                    if self.motion.is_some() {
                        ui.weak("Renders as a glide/cut of your framing — the crop tools edit the start frame.");
                    }
                }
                if theme::wide_button(ui, egui::Button::new("Reset to auto framing")).clicked() {
                    self.reset_to_auto();
                }
                ui.weak("Original view: drag the box, corners resize, scroll zooms, arrows nudge (Shift = big steps), 0 resets.");
            });
            if self.camera_mode != CameraMode::Manual {
                ui.weak(format!(
                    "{} mode frames automatically - switch to Manual (or drag in Original view) to take over.",
                    camera_mode_label(self.camera_mode)
                ));
            }
        });

        // --- Caption style ---
        theme::section(ui, "Caption presets");
        theme::card().show(ui, |ui| {
            // Uniform 3-per-row chips through the shared chip_row grid: equal
            // boxes, columns aligned with every other choice row.
            let presets = caption_presets();
            for (row_idx, row) in presets.chunks(3).enumerate() {
                let opts: Vec<(bool, &str)> = row
                    .iter()
                    .enumerate()
                    .map(|(j, p)| (self.preset == Some(row_idx * 3 + j), p.name.as_str()))
                    .collect();
                if let Some(j) = theme::chip_row(ui, &opts) {
                    let i = row_idx * 3 + j;
                    self.style = presets[i].clone();
                    self.preset = Some(i);
                    self.lines_dirty = true;
                    self.overlay_cache = None;
                }
            }
            // The operator's own saved looks (feature plan #12), persisted in
            // settings.json. Right-click deletes.
            if !prefs.caption_presets.is_empty() {
                ui.add_space(2.0);
                ui.weak("Your presets:");
                let mut apply: Option<usize> = None;
                let mut delete: Option<usize> = None;
                for (ri, row) in prefs.caption_presets.chunks(3).enumerate() {
                    ui.horizontal(|ui| {
                        for (j, up) in row.iter().enumerate() {
                            let i = ri * 3 + j;
                            let selected = self.style == up.style;
                            let resp = ui
                                .add(theme::chip(selected, &up.name))
                                .on_hover_text("Apply this saved look · right-click to delete");
                            if resp.clicked() {
                                apply = Some(i);
                            }
                            resp.context_menu(|ui| {
                                if ui.button("🗑 Delete preset").clicked() {
                                    delete = Some(i);
                                    ui.close();
                                }
                            });
                        }
                    });
                }
                if let Some(i) = apply {
                    self.style = prefs.caption_presets[i].style.clone();
                    self.preset = None;
                    self.lines_dirty = true;
                    self.overlay_cache = None;
                }
                if let Some(i) = delete {
                    prefs.caption_presets.remove(i);
                }
            }
            ui.weak("Pick a starting look - everything below stays editable.");
        });

        // Customization is its own section so the preset cards above and the
        // knobs below can't be mistaken for one another (operator feedback).
        theme::section(ui, "Customize captions");
        theme::card().show(ui, |ui| {
            {
                let before = self.style.clone();
                // Label on its own line so the genre chips join the SAME
                // column grid as the preset/framing rows (inline labels made
                // this one row narrower — the asymmetry the operator flagged).
                ui.label("Animation");
                let genres =
                    [CaptionGenre::HugeWord, CaptionGenre::RollingPop, CaptionGenre::KaraokeFill];
                if let Some(i) = theme::chip_row(
                    ui,
                    &[
                        (self.style.genre == CaptionGenre::HugeWord, "Huge word"),
                        (self.style.genre == CaptionGenre::RollingPop, "Rolling"),
                        (self.style.genre == CaptionGenre::KaraokeFill, "Karaoke"),
                    ],
                ) {
                    self.style.genre = genres[i];
                }
                ui.horizontal(|ui| {
                    ui.label("Size");
                    ui.add(egui::Slider::new(&mut self.style.font_size, 40..=220).suffix(" px"));
                    ui.checkbox(&mut self.style.bold, "Bold");
                });
                ui.horizontal(|ui| {
                    ui.label("Text");
                    color_swatch(ui, &mut self.style.primary_color);
                    ui.label("Accent");
                    color_swatch(ui, &mut self.style.accent_color);
                    ui.label("Outline");
                    color_swatch(ui, &mut self.style.outline_color);
                });
                ui.horizontal(|ui| {
                    ui.label("Outline");
                    ui.add(egui::Slider::new(&mut self.style.outline, 0.0..=12.0).step_by(0.5));
                });
                ui.horizontal(|ui| {
                    ui.label("Shadow");
                    ui.add(egui::Slider::new(&mut self.style.shadow, 0.0..=10.0).step_by(0.5));
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.style.back_box, "Background box");
                    if self.style.back_box {
                        color_swatch(ui, &mut self.style.back_color);
                    }
                });
                if self.style != before {
                    self.preset = None; // customized: no chip is "the" preset
                    self.lines_dirty = self.style.genre != before.genre || self.lines_dirty;
                    self.overlay_cache = None;
                }
                ui.add_space(4.0);
                match self.placement {
                    Some(p) => {
                        ui.horizontal(|ui| {
                            ui.weak(format!(
                                "Placement x {:.0}% · y {:.0}% · scale {:.0}%",
                                p.x_frac * 100.0,
                                p.y_frac * 100.0,
                                p.scale * 100.0
                            ));
                            if ui.small_button("Reset").clicked() {
                                self.placement = None;
                            }
                        });
                    }
                    None => {
                        ui.weak("Drag the caption on the preview to place it; scroll on it to resize.");
                    }
                }
                // Save the current look as a named preset (feature plan #12);
                // same name = update in place. Persists across sessions.
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.preset_name)
                            .hint_text("Preset name")
                            .desired_width(130.0),
                    );
                    if ui
                        .button("Save preset")
                        .on_hover_text("Save this exact caption look for every future clip")
                        .clicked()
                    {
                        let name = self.preset_name.trim().to_string();
                        let name = if name.is_empty() {
                            format!("Preset {}", prefs.caption_presets.len() + 1)
                        } else {
                            name
                        };
                        let entry = crate::settings::UserCaptionPreset {
                            name: name.clone(),
                            style: self.style.clone(),
                        };
                        match prefs.caption_presets.iter_mut().find(|p| p.name == name) {
                            Some(p) => *p = entry,
                            None => prefs.caption_presets.push(entry),
                        }
                        self.preset_name.clear();
                    }
                });
            }
        });

        action
    }

    // ------------------------------------------------------- export summary --

    /// The pre-render summary modal (focus: "Before rendering, show a summary").
    /// `rendering` gates only the Render button — reviewing the summary while
    /// the caption pre-pass runs is fine (the render would queue behind it).
    fn ui_export_modal(&mut self, ctx: &egui::Context, rendering: bool) -> Option<EditorAction> {
        let mut action = None;
        // Dim the page behind the modal.
        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::new("export-dim"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter().rect_filled(
                    screen,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 140),
                );
                // Swallow clicks behind the modal.
                ui.allocate_rect(screen, Sense::click());
            });
        let mut open = true;
        egui::Window::new("Export summary")
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(340.0);
                let dur = self.range.duration_s();
                let spec_camera = self.camera_mode == CameraMode::ActiveSpeaker
                    && self.plan.as_ref().map(|p| !p.shots.is_empty()).unwrap_or(false);
                let cached = self.transcript.is_some();
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(k).weak());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(v);
                        });
                    });
                };
                row(ui, "Clip length", format!("{}  ({dur:.1}s)", fmt_mmss_cc(dur)));
                row(ui, "Resolution", "1080 × 1920 (9:16)".into());
                row(
                    ui,
                    "Captions",
                    match &self.transcript {
                        None => "on - transcribed during render".into(),
                        Some(t) => {
                            let mut s =
                                format!("on - {} lines", preview_lines(t, self.style.genre).len());
                            if !self.manual_units.is_empty() {
                                s.push_str(&format!(" + {} yours", self.manual_units.len()));
                            }
                            if self.transcript_dirty {
                                s.push_str(" (edited)");
                            }
                            s
                        }
                    },
                );
                row(ui, "Caption style", format!("{} · {}px", self.style.name, self.style.font_size));
                row(
                    ui,
                    "Camera",
                    if spec_camera {
                        // Off-screen splits are deliberate grammar (ADR 0042),
                        // so the summary names them — the operator should never
                        // be surprised by a split in the render.
                        let cuts = self
                            .plan
                            .as_ref()
                            .map(|p| p.shots.len().saturating_sub(1))
                            .unwrap_or(0);
                        let splits = match (&self.speakers, &self.plan) {
                            (Some(a), Some(p)) if a.voice.is_some() => p
                                .shots
                                .iter()
                                .filter(|s| {
                                    s.track.is_none()
                                        && a.voice.as_ref().is_some_and(|v| {
                                            let b = (s.start_s / a.bin_s.max(1e-9)) as usize;
                                            let e = ((s.end_s / a.bin_s.max(1e-9)) as usize)
                                                .min(v.offscreen.len());
                                            (b..e).any(|i| v.offscreen[i])
                                        })
                                })
                                .count(),
                            _ => 0,
                        };
                        if splits > 0 {
                            format!("Active Speaker · {cuts} cuts · {splits} off-screen splits")
                        } else {
                            format!("Active Speaker · {cuts} cuts")
                        }
                    } else if self.camera_mode == CameraMode::Manual && self.motion_camera().is_some()
                    {
                        format!("Motion · {}", self.motion.map(motion_label).unwrap_or("preset"))
                    } else {
                        camera_mode_label(self.camera_mode).to_string()
                    },
                );
                // Timeline razor cuts (ADR 0065): the export drops these spans.
                if let Some(keep) = self.razor.kept_spans(dur) {
                    let kept: f64 = keep.iter().map(|k| k.duration_s()).sum();
                    let n_removed = self.razor.removed.iter().filter(|r| **r).count();
                    row(
                        ui,
                        "Timeline cuts",
                        format!(
                            "{n_removed} segment{} removed · final {}",
                            if n_removed == 1 { "" } else { "s" },
                            fmt_mmss_cc(kept)
                        ),
                    );
                }
                if spec_camera && !self.camera_audit.is_empty() {
                    for w in &self.camera_audit {
                        ui.label(
                            egui::RichText::new(format!("⚠ {w}")).color(theme::GOLD).size(12.0),
                        );
                    }
                }
                row(
                    ui,
                    "Speaker tracking",
                    if spec_camera { "enabled".into() } else { "off".into() },
                );
                let est = estimate_render_s(dur, cached);
                row(ui, "Estimated render time", format!("~{}", fmt_mmss(est)));
                if self.transcript.is_none() {
                    ui.add_space(4.0);
                    ui.weak("Captions are still transcribing - rendering now simply waits for them.");
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!rendering, |ui| {
                        if theme::primary_button(ui, "Render Short").clicked() {
                            self.show_export = false;
                            // The app stops the audio for the render; stop our
                            // side of playback too so they never desync.
                            self.playing = None;
                            self.stop_video();
                            action = Some(EditorAction::Render(Box::new(self.render_spec())));
                        }
                    });
                    if ui.button("Back").clicked() {
                        self.show_export = false;
                    }
                });
            });
        if !open {
            self.show_export = false;
        }
        action
    }

    // ------------------------------------------------------ caption overlay --

    /// ASS Fontsize -> egui font size, so the preview draws captions at the
    /// burn's TRUE size (ADR 0036: the burn-in is ground truth). libass sizes
    /// a face by its OS/2 win cell (usWinAscent+usWinDescent - the VSFilter-
    /// compat FreeType REAL_DIM request); egui 0.34 (skrifa) sizes by the em
    /// square - the same nominal size drew Anton 1.73x bigger in the preview
    /// than in the export (the operator's "exported captions much smaller"
    /// bug). MEASURED 2026-07-03 on the production ffmpeg+libass (one-glyph
    /// ASS burn, ink-row count): Fontsize 150 -> 75 px cap height, exactly
    /// the win-cell prediction 74.4 (hhea-span would be 85.6, em 128.9);
    /// linear at 96 -> 48. Anton: upem 2048, winAsc 2876 + winDesc 674 =
    /// 3550. The unit test pins this against the shipped TTF's own tables -
    /// if the caption face ever changes, re-measure with a one-glyph burn.
    const ASS_TO_EGUI_FONT: f32 = 2048.0 / 3550.0;

    /// Draw the caption line active at the playhead over the composite and run
    /// its drag/resize interaction (ADR 0036). Grouping/timing/colours come from
    /// the render's own line model; egui rasterizes the glyphs (approximate
    /// look, exact geometry — the burn-in stays ground truth). In a speech gap
    /// the next line ghosts at low alpha so placement stays visible and
    /// draggable anywhere on the timeline.
    fn caption_overlay(&mut self, ui: &mut egui::Ui, canvas_rect: Rect, enabled: bool) {
        if self.transcript.is_none() {
            return;
        }
        self.sync_lines();
        let p = self.playhead_s;
        // The auto line under (or nearest) the playhead — picked only when
        // the auto track's eye is on: the preview shows what burns
        // (ADR 0066), and an eye-off stream burns nothing.
        let line_pick: Option<(usize, bool)> = if self.trk_auto.eye {
            match self.lines.iter().position(|l| l.start_s <= p && p < l.end_s) {
                Some(i) => Some((i, false)),
                None => self
                    .lines
                    .iter()
                    .position(|l| l.start_s >= p)
                    .or(self.lines.len().checked_sub(1))
                    .map(|i| (i, true)),
            }
        } else {
            None
        };
        let ghost = line_pick.is_some_and(|(_, g)| g);

        let style = &self.style;
        let place = self.placement.unwrap_or_default();
        // The SAME resolution the burn-in uses (anchor + font size in ASS
        // PlayRes pixels, clamps included — ADR 0036's geometry sharing), then
        // one factor converts PlayRes pixels to canvas points.
        let (ass_x, ass_y, ass_font) = resolve_placement(self.placement, style.font_size);
        let px = canvas_rect.width() / CANVAS_W as f32;
        let font_px = (ass_font as f32 * px * Self::ASS_TO_EGUI_FONT).max(4.0);
        let font_id = FontId::new(font_px, theme::display_family());
        let mul = if ghost { 0.35 } else { 1.0 };
        let tint = |c: [u8; 4]| {
            Color32::from_rgba_unmultiplied(c[0], c[1], c[2], (c[3] as f32 * mul) as u8)
        };
        let primary = tint(style.primary_color);
        let accent = tint(style.accent_color);
        let outline_c = tint(style.outline_color);
        let shadow_c = Color32::from_rgba_unmultiplied(0, 0, 0, (200.0 * mul) as u8);

        let painter = ui.painter_at(canvas_rect);

        // The auto stream: one line at a time (the burn's grammar). Skipped
        // entirely when its track's eye is off — the manual stream below is
        // independent (simultaneity is the point, ADR 0065 Amendment 3).
        let mut auto_box: Option<(egui::Pos2, egui::Vec2, usize)> = None;
        if let Some((li, _)) = line_pick {
            let line = &self.lines[li];
            let states = word_states(style.genre, line, p);

            let key = OverlayKey {
                line_start_bits: line.start_s.to_bits(),
                n_words: line.words.len(),
                states: states.clone(),
                ghost,
                font_bits: font_px.to_bits(),
            };
            let (galley, shadow) = match &self.overlay_cache {
                Some((k, g, s)) if *k == key => (g.clone(), s.clone()),
                _ => {
                    let mut job = egui::text::LayoutJob::default();
                    job.wrap.max_width = f32::INFINITY;
                    let mut shadow_job = egui::text::LayoutJob::default();
                    shadow_job.wrap.max_width = f32::INFINITY;
                    for (i, w) in line.words.iter().enumerate() {
                        let color = match if ghost { WordState::Base } else { states[i] } {
                            WordState::Hidden => Color32::TRANSPARENT, // reserves its space
                            WordState::Base => primary,
                            WordState::Sung => accent,
                        };
                        let text = if i + 1 < line.words.len() {
                            format!("{} ", w.text)
                        } else {
                            w.text.clone()
                        };
                        let fmt = |c| egui::TextFormat {
                            font_id: font_id.clone(),
                            color: c,
                            ..Default::default()
                        };
                        job.append(&text, 0.0, fmt(color));
                        let sc = if color == Color32::TRANSPARENT { color } else { outline_c };
                        shadow_job.append(&text, 0.0, fmt(sc));
                    }
                    let g = painter.layout_job(job);
                    let s = painter.layout_job(shadow_job);
                    self.overlay_cache = Some((key, g.clone(), s.clone()));
                    (g, s)
                }
            };
            let center = egui::pos2(
                canvas_rect.left() + ass_x as f32 * px,
                canvas_rect.top() + ass_y as f32 * px,
            );
            let size = galley.size();
            let top_left = center - size / 2.0; // \an5: centered both axes
            // Background box (BorderStyle 3 approximation).
            if style.back_box {
                painter.rect_filled(
                    Rect::from_center_size(center, size + egui::vec2(18.0 * px, 12.0 * px)),
                    CornerRadius::same(3),
                    tint(style.back_color),
                );
            }
            // Faux outline: four offset passes in the outline colour stand in
            // for libass's outline (ADR 0036's fidelity boundary: look
            // approximate, position/size/timing exact). Width follows the
            // style's outline px.
            if style.outline > 0.0 {
                let o = (style.outline * px * place.scale).clamp(1.0, 6.0);
                for d in [
                    egui::vec2(-o, 0.0),
                    egui::vec2(o, 0.0),
                    egui::vec2(0.0, -o),
                    egui::vec2(0.0, o),
                ] {
                    painter.galley(top_left + d, shadow.clone(), outline_c);
                }
            }
            if style.shadow > 0.0 {
                let s = (style.shadow * px).clamp(1.0, 8.0);
                painter.galley(top_left + egui::vec2(s, s), shadow.clone(), shadow_c);
            }
            painter.galley(top_left, galley, primary);
            auto_box = Some((center, size, li));
        }

        // The operator's OWN caption stream (ADR 0065): every caption active
        // at the playhead draws WITH the auto line — simultaneity is the
        // point. Each sits at ITS OWN anchor (their drag, else the default
        // block above the auto captions — the same geometry the ASS emits,
        // so preview and export agree), and each is directly draggable:
        // drag moves it anywhere on the canvas, scroll resizes it,
        // right-click resets it to the default anchor.
        // An eye-off manual track draws (and burns) nothing — the empty set
        // skips the loop without touching the operator's data (ADR 0066).
        let active_manual: Vec<usize> = if self.trk_manual.eye {
            self.manual_units
                .iter()
                .enumerate()
                .filter(|(_, m)| m.start_s <= p && p < m.end_s.max(m.start_s + 0.1))
                .map(|(i, _)| i)
                .collect()
        } else {
            Vec::new()
        };
        let mut set_place: Option<(usize, Option<CaptionPlacement>)> = None;
        for i in active_manual {
            let m = &self.manual_units[i];
            let own = self.manual_places.get(i).copied().flatten();
            let (mx, my, msize) = match own {
                Some(pl) => resolve_placement(Some(pl), style.font_size),
                None => resolve_manual_placement(self.placement, style.font_size),
            };
            let center = egui::pos2(
                canvas_rect.left() + mx as f32 * px,
                canvas_rect.top() + my as f32 * px,
            );
            let mfont = FontId::new(
                (msize as f32 * px * Self::ASS_TO_EGUI_FONT).max(4.0),
                theme::display_family(),
            );
            let galley = painter.layout_no_wrap(m.text.to_uppercase(), mfont.clone(), primary);
            let sgalley = painter.layout_no_wrap(m.text.to_uppercase(), mfont, outline_c);
            let gsize = galley.size();
            let tl = center - gsize / 2.0;
            if style.back_box {
                painter.rect_filled(
                    Rect::from_center_size(center, gsize + egui::vec2(18.0 * px, 12.0 * px)),
                    CornerRadius::same(3),
                    tint(style.back_color),
                );
            }
            if style.outline > 0.0 {
                let o = (style.outline * px).clamp(1.0, 6.0);
                for d in [
                    egui::vec2(-o, 0.0),
                    egui::vec2(o, 0.0),
                    egui::vec2(0.0, -o),
                    egui::vec2(0.0, o),
                ] {
                    painter.galley(tl + d, sgalley.clone(), outline_c);
                }
            }
            painter.galley(tl, galley, primary);

            if !enabled {
                continue;
            }
            let box_rect = Rect::from_center_size(center, gsize.max(egui::vec2(24.0, 16.0)));
            let resp = ui
                .interact(
                    box_rect.expand(6.0),
                    ui.id().with(("man-cap", i)),
                    Sense::click_and_drag(),
                )
                .on_hover_text(
                    "Your caption — drag to place it anywhere, scroll to resize, \
                     double-click to edit the text, right-click to reset position",
                );
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
                painter.rect_stroke(
                    box_rect.expand(4.0),
                    CornerRadius::same(3),
                    Stroke::new(1.0, theme::INFO),
                    StrokeKind::Outside,
                );
            }
            if resp.double_clicked() {
                self.sel_manual = Some(i);
                self.focus_caption = Some((CapLane::Manual, i));
            }
            resp.context_menu(|ui| {
                if ui.button("Reset position").clicked() {
                    set_place = Some((i, None));
                    ui.close();
                }
            });
            if resp.dragged() {
                // Anchor fracs from the CURRENT resolved spot, so the first
                // drag starts from where the caption already is.
                let cur = own.unwrap_or(CaptionPlacement {
                    x_frac: mx as f32 / CANVAS_W as f32,
                    y_frac: my as f32 / CANVAS_H as f32,
                    scale: 1.0,
                });
                let d = resp.drag_delta();
                let mut x = cur.x_frac + d.x / canvas_rect.width();
                let y = cur.y_frac + d.y / canvas_rect.height();
                if (x - 0.5).abs() < 0.02 {
                    x = 0.5; // snap to the canvas centre, like the main block
                }
                set_place = Some((
                    i,
                    Some(CaptionPlacement {
                        x_frac: x.clamp(0.05, 0.95),
                        y_frac: y.clamp(0.03, 0.97),
                        scale: cur.scale,
                    }),
                ));
            } else if resp.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll.abs() > 0.0 {
                    let cur = own.unwrap_or(CaptionPlacement {
                        x_frac: mx as f32 / CANVAS_W as f32,
                        y_frac: my as f32 / CANVAS_H as f32,
                        scale: 1.0,
                    });
                    let factor = (scroll * 0.0015).exp();
                    set_place = Some((
                        i,
                        Some(CaptionPlacement {
                            scale: (cur.scale * factor)
                                .clamp(CaptionPlacement::SCALE_MIN, CaptionPlacement::SCALE_MAX),
                            ..cur
                        }),
                    ));
                    ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                }
            }
        }
        if let Some((i, pl)) = set_place {
            if let Some(slot) = self.manual_places.get_mut(i) {
                *slot = pl;
            }
        }

        if !enabled {
            return;
        }
        let Some((center, size, li)) = auto_box else {
            return; // auto track hidden (or no lines): no auto block to grab
        };
        let line = &self.lines[li];
        // Drag to move, scroll on it to resize — the Crop verbs, applied to
        // the caption block. Double-click = edit what you see: focuses this
        // caption's text field in the panel (operator ask 2026-07-13).
        let box_rect = Rect::from_center_size(center, size.max(egui::vec2(24.0, 16.0)));
        let resp =
            ui.interact(box_rect.expand(6.0), ui.id().with("caption-box"), Sense::click_and_drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
            painter.rect_stroke(
                box_rect.expand(4.0),
                CornerRadius::same(3),
                Stroke::new(1.0, theme::GOLD),
                StrokeKind::Outside,
            );
        }
        if resp.double_clicked() {
            // The unit under the playhead within this line (the word being
            // spoken), else the line's first unit. Cumulative word counts map
            // lines onto transcript units (one word per unit).
            let base: usize = self.lines[..li].iter().map(|l| l.words.len()).sum();
            let off = line.words.iter().rposition(|w| w.start_s <= p).unwrap_or(0);
            let unit = base + off;
            if self.transcript.as_ref().is_some_and(|t| unit < t.units.len()) {
                self.sel_unit = Some(unit);
                self.scroll_to_sel = true;
                self.focus_caption = Some((CapLane::Auto, unit));
            }
        }
        if resp.dragged() {
            let d = resp.drag_delta();
            let mut x = place.x_frac + d.x / canvas_rect.width();
            let y = place.y_frac + d.y / canvas_rect.height();
            // Snap X to the canvas centre (the burn-in's default) within 2%.
            if (x - 0.5).abs() < 0.02 {
                x = 0.5;
                painter.line_segment(
                    [
                        egui::pos2(canvas_rect.center().x, canvas_rect.top()),
                        egui::pos2(canvas_rect.center().x, canvas_rect.bottom()),
                    ],
                    Stroke::new(1.0, theme::GOLD.gamma_multiply(0.4)),
                );
            }
            self.placement = Some(CaptionPlacement {
                x_frac: x.clamp(0.05, 0.95),
                y_frac: y.clamp(0.03, 0.97),
                scale: place.scale,
            });
        } else if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                let factor = (scroll * 0.0015).exp();
                self.placement = Some(CaptionPlacement {
                    scale: (place.scale * factor)
                        .clamp(CaptionPlacement::SCALE_MIN, CaptionPlacement::SCALE_MAX),
                    ..place
                });
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            }
        }
    }

    /// Drag the Seam: changing each stacked Panel's aspect re-fits its
    /// aspect-locked Crop (keep center) so the composite stays faithful.
    fn drag_seam(&mut self, ui: &egui::Ui, canvas_rect: Rect, canvas_h: f32) {
        let seam_y = canvas_rect.top() + self.seam * canvas_h;
        let handle = Rect::from_min_max(
            egui::pos2(canvas_rect.left(), seam_y - 4.0),
            egui::pos2(canvas_rect.right(), seam_y + 4.0),
        );
        let resp = ui.interact(handle, ui.id().with("seam"), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if resp.dragged() {
            let new_seam = (self.seam + resp.drag_delta().y / canvas_h).clamp(0.2, 0.85);
            if (new_seam - self.seam).abs() > f32::EPSILON {
                let (ga, fa) = yc_frame::stacked_panel_aspects(new_seam);
                self.gameplay =
                    yc_frame::reaspect_keep_center(self.gameplay, ga, self.src_w, self.src_h);
                self.facecam =
                    yc_frame::reaspect_keep_center(self.facecam, fa, self.src_w, self.src_h);
                self.seam = new_seam;
            }
        }
    }
}

/// Drag body to move + corner handles to resize + scroll to zoom, for one
/// crop box in Source view — over ANY crop (a Manual field, or a camera-plan
/// shot's crop for the operator's own per-shot framing). Returns true when
/// the crop changed.
fn crop_box_interaction(
    ui: &mut egui::Ui,
    frame_rect: Rect,
    screen: Rect,
    id: &str,
    crop: &mut Crop,
    sw: f32,
    sh: f32,
) -> bool {
    let px = sw / frame_rect.width().max(1.0);
    let mut changed = false;
    // Corner handles first (they win the pointer over the body).
    let hs = 7.0;
    let corners = [
        (screen.left_top(), -1.0, -1.0),
        (screen.right_top(), 1.0, -1.0),
        (screen.left_bottom(), -1.0, 1.0),
        (screen.right_bottom(), 1.0, 1.0),
    ];
    let mut resized = false;
    for (i, (pos, sx, sy)) in corners.iter().enumerate() {
        let hrect = Rect::from_center_size(*pos, egui::vec2(hs * 2.0, hs * 2.0));
        let resp = ui.interact(hrect, ui.id().with((id, "corner", i)), Sense::drag());
        ui.painter().rect_filled(
            Rect::from_center_size(*pos, egui::vec2(hs, hs)),
            CornerRadius::same(2),
            if resp.hovered() || resp.dragged() { theme::GOLD } else { Color32::WHITE },
        );
        if resp.dragged() {
            resized = true;
            changed = true;
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
            let d = resp.drag_delta();
            // Dragging a corner outward grows the box: project the delta on
            // the outward diagonal, aspect-locked via zoom about the
            // opposite corner.
            let grow = (d.x * sx + d.y * sy) * 0.5 * px;
            let factor = ((crop.w + 2.0 * grow) / crop.w).clamp(0.25, 4.0);
            let (ax, ay) = (
                crop.x + crop.w * (0.5 - sx * 0.5), // opposite corner stays put
                crop.y + crop.h * (0.5 - sy * 0.5),
            );
            *crop = yc_frame::zoom_crop(*crop, sw, sh, factor, ax, ay);
        }
    }
    if resized {
        return changed;
    }
    // Body: move.
    let resp = ui.interact(screen, ui.id().with((id, "body")), Sense::click_and_drag());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
    }
    if resp.dragged() {
        let d = resp.drag_delta();
        if d != egui::Vec2::ZERO {
            changed = true;
        }
        *crop = yc_frame::pan_crop(*crop, sw, sh, d.x * px, d.y * px);
    }
    // Scroll on the box: zoom about its center.
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            changed = true;
            let factor = (-scroll * 0.0015).exp();
            let (ax, ay) = (crop.x + crop.w * 0.5, crop.y + crop.h * 0.5);
            *crop = yc_frame::zoom_crop(*crop, sw, sh, factor, ax, ay);
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
    }
    changed
}

/// Everything the overlay's shaped galleys depend on (ADR 0036).
#[derive(PartialEq)]
struct OverlayKey {
    line_start_bits: u64,
    n_words: usize,
    states: Vec<WordState>,
    ghost: bool,
    font_bits: u32,
}

/// Derive the six editable fields from an auto-detected seed Layout. A
/// `FullFrame` seed is cam-vs-gameplay-ambiguous (the variant does not record
/// which), so it defaults the kind to Full gameplay but seeds *both* full Crops
/// from the auto crop, so flipping to Full cam keeps it.
fn seed_fields(
    auto: &Layout,
    src_w: f32,
    src_h: f32,
) -> (LayoutKind, f32, Crop, Crop, Crop, Crop) {
    let (ga, fa) = yc_frame::stacked_panel_aspects(yc_frame::SEAM_DEFAULT);
    let full = Crop { x: 0.0, y: 0.0, w: src_w, h: src_h };
    let gameplay_default = full.fit_to_aspect(ga);
    let facecam_default = yc_frame::default_facecam_crop(src_w, src_h, fa);
    let fullgameplay_default = yc_frame::fullframe_gameplay_crop(src_w, src_h);
    let fullcam_default = yc_frame::centered_fullcam_crop(src_w, src_h);
    match *auto {
        Layout::Stacked { seam, gameplay, facecam } => (
            LayoutKind::Stacked,
            seam,
            gameplay,
            facecam,
            fullcam_default,
            fullgameplay_default,
        ),
        Layout::FullFrame { crop } => (
            LayoutKind::FullGameplay,
            yc_frame::SEAM_DEFAULT,
            gameplay_default,
            facecam_default,
            crop,
            crop,
        ),
    }
}

/// Draw one Panel: the source-frame texture sampled at `crop`'s normalized
/// region, filling `rect` (Crop aspect == Panel aspect, so no distortion), plus
/// a faint label.
fn draw_panel(
    painter: &egui::Painter,
    rect: Rect,
    tex: egui::TextureId,
    crop: &Crop,
    src_w: f32,
    src_h: f32,
    label: &str,
) {
    let uv = Rect::from_min_max(
        egui::pos2(crop.x / src_w, crop.y / src_h),
        egui::pos2((crop.x + crop.w) / src_w, (crop.y + crop.h) / src_h),
    );
    painter.image(tex, rect, uv, Color32::WHITE);
    if !label.is_empty() {
        painter.text(
            rect.left_top() + egui::vec2(4.0, 2.0),
            Align2::LEFT_TOP,
            label,
            FontId::proportional(11.0),
            Color32::from_rgba_unmultiplied(255, 255, 255, 160),
        );
    }
}

/// Pan (drag) and zoom (scroll) a Crop within the source frame, in source
/// pixels, via the pure `yc_frame` ops. Dragging the image moves content under
/// the cursor (the Crop window moves opposite); scrolling zooms about the
/// cursor, aspect-locked.
fn pan_zoom(ui: &egui::Ui, rect: Rect, id: &str, crop: &mut Crop, src_w: f32, src_h: f32) {
    let resp = ui.interact(rect, ui.id().with(id), Sense::click_and_drag());
    if resp.dragged() {
        let d = resp.drag_delta();
        let sx = crop.w / rect.width().max(1.0);
        let sy = crop.h / rect.height().max(1.0);
        *crop = yc_frame::pan_crop(*crop, src_w, src_h, -d.x * sx, -d.y * sy);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            if let Some(p) = resp.hover_pos() {
                // Scroll up (positive) zooms in (smaller Crop).
                let factor = (-scroll * 0.0015).exp();
                let ax = crop.x + ((p.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0) * crop.w;
                let ay = crop.y + ((p.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0) * crop.h;
                *crop = yc_frame::zoom_crop(*crop, src_w, src_h, factor, ax, ay);
            }
        }
    }
}

/// Dim the parts of `frame` outside every rect in `keep` (the crop-tool focus
/// effect). Approximate: for one crop this is exact (4 side bands); for two it
/// dims rows/columns not covered by either (cheap and close enough).
fn dim_outside(painter: &egui::Painter, frame: Rect, keep: &[Rect]) {
    let dim = Color32::from_rgba_unmultiplied(0, 0, 0, 110);
    if keep.is_empty() {
        return;
    }
    if keep.len() == 1 {
        let k = keep[0];
        let bands = [
            Rect::from_min_max(frame.min, egui::pos2(frame.right(), k.top())),
            Rect::from_min_max(egui::pos2(frame.left(), k.bottom()), frame.max),
            Rect::from_min_max(egui::pos2(frame.left(), k.top()), egui::pos2(k.left(), k.bottom())),
            Rect::from_min_max(egui::pos2(k.right(), k.top()), egui::pos2(frame.right(), k.bottom())),
        ];
        for b in bands {
            if b.width() > 0.5 && b.height() > 0.5 {
                painter.rect_filled(b.intersect(frame), CornerRadius::ZERO, dim);
            }
        }
    } else {
        // Multiple keeps: a light whole-frame veil, then re-brighten is not
        // possible with plain painting — draw thin outlines instead of a veil.
        // (Two-panel stacked layouts already read clearly from the strokes.)
    }
}

/// Rule-of-thirds guides inside a crop rect.
fn thirds_grid(painter: &egui::Painter, r: Rect, color: Color32) {
    let c = color.gamma_multiply(0.35);
    for f in [1.0 / 3.0, 2.0 / 3.0] {
        painter.line_segment(
            [
                egui::pos2(r.left() + r.width() * f, r.top()),
                egui::pos2(r.left() + r.width() * f, r.bottom()),
            ],
            Stroke::new(1.0, c),
        );
        painter.line_segment(
            [
                egui::pos2(r.left(), r.top() + r.height() * f),
                egui::pos2(r.right(), r.top() + r.height() * f),
            ],
            Stroke::new(1.0, c),
        );
    }
}

/// The Shorts/TikTok safe-area guide: darken the UI-covered zones (top bar,
/// bottom title/actions, right-side buttons) and stroke the safe rect.
fn draw_safe_area(painter: &egui::Painter, canvas: Rect) {
    let dim = Color32::from_rgba_unmultiplied(255, 60, 60, 34);
    let top = canvas.height() * 0.06;
    let bottom = canvas.height() * 0.17;
    let right = canvas.width() * 0.14;
    painter.rect_filled(
        Rect::from_min_size(canvas.min, egui::vec2(canvas.width(), top)),
        CornerRadius::ZERO,
        dim,
    );
    painter.rect_filled(
        Rect::from_min_max(egui::pos2(canvas.left(), canvas.bottom() - bottom), canvas.max),
        CornerRadius::ZERO,
        dim,
    );
    painter.rect_filled(
        Rect::from_min_max(
            egui::pos2(canvas.right() - right, canvas.top() + top),
            egui::pos2(canvas.right(), canvas.bottom() - bottom),
        ),
        CornerRadius::ZERO,
        dim,
    );
    let safe = Rect::from_min_max(
        egui::pos2(canvas.left() + canvas.width() * 0.04, canvas.top() + top),
        egui::pos2(canvas.right() - right, canvas.bottom() - bottom),
    );
    painter.rect_stroke(
        safe,
        CornerRadius::same(6),
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 120)),
        StrokeKind::Inside,
    );
    painter.text(
        safe.left_bottom() + egui::vec2(6.0, -14.0),
        Align2::LEFT_TOP,
        "safe area",
        FontId::proportional(10.0),
        Color32::from_rgba_unmultiplied(255, 255, 255, 150),
    );
}

/// A small filled label chip painted directly on a canvas.
fn chip(painter: &egui::Painter, pos: egui::Pos2, text: &str, color: Color32) {
    let font = FontId::proportional(11.0);
    let galley = painter.layout_no_wrap(text.to_string(), font, Color32::WHITE);
    let r = Rect::from_min_size(pos, galley.size() + egui::vec2(10.0, 6.0));
    painter.rect_filled(r, CornerRadius::same(4), Color32::from_rgba_unmultiplied(10, 12, 16, 210));
    painter.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, color), StrokeKind::Inside);
    painter.galley(pos + egui::vec2(5.0, 3.0), galley, Color32::WHITE);
}

/// egui color picker over an RGBA byte array (the CaptionStyle color type).
fn color_swatch(ui: &mut egui::Ui, rgba: &mut [u8; 4]) {
    let mut c = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
    if egui::color_picker::color_edit_button_srgba(
        ui,
        &mut c,
        egui::color_picker::Alpha::OnlyBlend,
    )
    .changed()
    {
        *rgba = [c.r(), c.g(), c.b(), c.a()];
    }
}

pub(crate) fn camera_mode_label(mode: CameraMode) -> &'static str {
    match mode {
        CameraMode::Manual => "Manual",
        CameraMode::Center => "Center",
        CameraMode::AutoFace => "Auto face",
        CameraMode::ActiveSpeaker => "Active Speaker",
        CameraMode::Group => "Group",
    }
}

/// `m:ss` for ruler ticks and rough durations.
fn fmt_mmss(t: f64) -> String {
    let s = t.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// `m:ss.cc` — the transcript editor's timestamp format (focus task 2).
fn fmt_mmss_cc(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}.{:02}", cs / 6000, (cs / 100) % 60, cs % 100)
}

/// Parse `m:ss.cc` (also accepts `ss.cc` or plain seconds) back to seconds.
fn parse_mmss_cc(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some((m, rest)) = s.split_once(':') {
        let mins: f64 = m.trim().parse().ok()?;
        let secs: f64 = rest.trim().parse().ok()?;
        Some(mins * 60.0 + secs)
    } else {
        s.parse().ok()
    }
}

/// Censor a word for the burn-in: keep each word's first character, star the
/// rest ("damn" -> "d***"). Multi-word units censor each word.
fn censor_text(text: &str) -> String {
    text.split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => {
                    let rest = chars.count();
                    format!("{first}{}", "*".repeat(rest.max(1)))
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split one caption unit into two: at the space nearest the text midpoint
/// when there is one, else at the character midpoint; times split
/// proportionally to the text split. `None` when the unit is too short.
fn split_unit(u: &yc_core::CaptionUnit) -> Option<(yc_core::CaptionUnit, yc_core::CaptionUnit)> {
    let text = u.text.trim();
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 2 {
        return None;
    }
    let mid = chars.len() / 2;
    // Prefer the space nearest the midpoint.
    let spaces: Vec<usize> =
        chars.iter().enumerate().filter(|(_, c)| c.is_whitespace()).map(|(i, _)| i).collect();
    let (a_text, b_text, frac) = match spaces.iter().min_by_key(|i| i.abs_diff(mid)) {
        Some(&i) => {
            let a: String = chars[..i].iter().collect();
            let b: String = chars[i + 1..].iter().collect();
            let frac = (i as f64 / chars.len() as f64).clamp(0.1, 0.9);
            (a.trim().to_string(), b.trim().to_string(), frac)
        }
        None => {
            let a: String = chars[..mid].iter().collect();
            let b: String = chars[mid..].iter().collect();
            (a, b, 0.5)
        }
    };
    if a_text.is_empty() || b_text.is_empty() {
        return None;
    }
    let split_t = u.start_s + (u.end_s - u.start_s).max(0.1) * frac;
    Some((
        yc_core::CaptionUnit { text: a_text, start_s: u.start_s, end_s: split_t },
        yc_core::CaptionUnit { text: b_text, start_s: split_t, end_s: u.end_s },
    ))
}

/// Split the plan's shot covering `t` in two at `t` (feature plan #3's manual
/// cut point). Both halves keep the shot's subject and framing — the operator
/// retargets either by clicking a face. A glide is split so the motion stays
/// identical: the first half lands where the pan had reached at `t`, the
/// second continues from there to the original target. Refuses a split that
/// would leave a sliver shot (< [`MIN_SHOT_S`]).
fn split_plan_at(plan: &mut CameraPlan, t: f64) -> bool {
    let Some(idx) = plan.shots.iter().position(|s| s.start_s <= t && t < s.end_s) else {
        return false;
    };
    let shot = plan.shots[idx].clone();
    if t - shot.start_s < MIN_SHOT_S || shot.end_s - t < MIN_SHOT_S {
        return false;
    }
    let mid = shot.layout_at(t);
    let mut first = shot.clone();
    let mut second = shot;
    first.end_s = t;
    second.start_s = t;
    if first.pan_to.is_some() {
        if let Layout::FullFrame { crop } = mid {
            first.pan_to = Some(crop);
            second.layout = Layout::FullFrame { crop };
        }
    }
    plan.shots[idx] = first;
    plan.shots.insert(idx + 1, second);
    true
}

/// Remove the cut between shots `boundary-1` and `boundary`: the earlier
/// shot's framing carries across the merged span (the framing the viewer was
/// already watching leading into the cut; a kept glide just spans longer).
fn delete_plan_cut(plan: &mut CameraPlan, boundary: usize) -> bool {
    if boundary == 0 || boundary >= plan.shots.len() {
        return false;
    }
    let removed = plan.shots.remove(boundary);
    plan.shots[boundary - 1].end_s = removed.end_s;
    true
}

/// The nearest anchor within `tol` of `t`, if any — the timeline's magnetic
/// snap (feature plan #3: dragged times snap to nearby anchors).
fn magnet(t: f64, anchors: &[f64], tol: f64) -> Option<f64> {
    anchors
        .iter()
        .copied()
        .filter(|a| (a - t).abs() <= tol)
        .min_by(|a, b| {
            (a - t).abs().partial_cmp(&(b - t).abs()).unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Quantize to centiseconds — the transcript panel's own granularity
/// (`m:ss.cc`), so a drag never bakes in precision the panel can't show.
fn quantize_cs(t: f64) -> f64 {
    (t * 100.0).round() / 100.0
}

/// A rough wall-clock estimate for the export (the summary row): NVENC runs
/// ~2.5x realtime on the 1080x1920 encode, whisper ~0.9x when the transcript
/// is not already cached, plus fixed process overhead.
fn estimate_render_s(dur_s: f64, transcript_cached: bool) -> f64 {
    let encode = dur_s * 0.4 + 8.0;
    let whisper = if transcript_cached { 0.0 } else { dur_s * 0.9 + 10.0 };
    encode + whisper
}

/// Truncate to at most `max` characters with an ellipsis.
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max.max(1) {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yc_core::CaptionUnit;

    #[test]
    fn ass_to_egui_font_factor_matches_the_shipped_caption_face() {
        // The calibration constant IS the shipped Anton's upem / OS/2 win
        // cell: libass sizes a face by its win cell (VSFilter compat), egui
        // 0.34/skrifa by the em square. Measured 2026-07-03 on the production
        // ffmpeg+libass: Fontsize 150 burned a 75 px cap height (win-cell
        // predicts 74.4; the em mapping would draw 128.9). If this fails the
        // caption face changed - re-measure with a one-glyph burn and update
        // ASS_TO_EGUI_FONT.
        let ttf = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fonts/Anton-Regular.ttf"),
        )
        .expect("shipped caption face");
        let u16be = |o: usize| u16::from_be_bytes([ttf[o], ttf[o + 1]]);
        let u32be = |o: usize| u32::from_be_bytes([ttf[o], ttf[o + 1], ttf[o + 2], ttf[o + 3]]);
        let num_tables = u16be(4) as usize;
        let table = |tag: &[u8; 4]| {
            (0..num_tables)
                .map(|i| 12 + 16 * i)
                .find(|&rec| &ttf[rec..rec + 4] == tag)
                .map(|rec| u32be(rec + 8) as usize)
                .expect("metric table present")
        };
        let upem = u16be(table(b"head") + 18) as f32;
        let os2 = table(b"OS/2");
        let win_cell = (u16be(os2 + 74) + u16be(os2 + 76)) as f32;
        assert_eq!((upem, win_cell), (2048.0, 3550.0), "Anton's tables moved");
        assert!(
            (EditorState::ASS_TO_EGUI_FONT - upem / win_cell).abs() < 1e-6,
            "ASS_TO_EGUI_FONT must equal the shipped face's upem/winCell"
        );
    }

    #[test]
    fn mmss_cc_formats_and_parses_round_trip() {
        assert_eq!(fmt_mmss_cc(0.0), "0:00.00");
        assert_eq!(fmt_mmss_cc(4.25), "0:04.25");
        assert_eq!(fmt_mmss_cc(83.7), "1:23.70");
        assert_eq!(parse_mmss_cc("1:23.70"), Some(83.7));
        assert_eq!(parse_mmss_cc("0:04.25"), Some(4.25));
        assert_eq!(parse_mmss_cc("12.5"), Some(12.5));
        assert_eq!(parse_mmss_cc("garbage"), None);
    }

    #[test]
    fn censor_keeps_first_letter_and_stars_the_rest() {
        assert_eq!(censor_text("damn"), "d***");
        assert_eq!(censor_text("two words"), "t** w****");
        assert_eq!(censor_text("a"), "a*"); // even a 1-char word masks something
    }

    #[test]
    fn split_unit_prefers_the_space_and_splits_times_proportionally() {
        let u = CaptionUnit { text: "hello world".into(), start_s: 1.0, end_s: 2.0 };
        let (a, b) = split_unit(&u).expect("splittable");
        assert_eq!(a.text, "hello");
        assert_eq!(b.text, "world");
        assert!(a.start_s == 1.0 && b.end_s == 2.0);
        assert!((a.end_s - b.start_s).abs() < 1e-9, "contiguous");
        assert!(a.end_s > 1.2 && a.end_s < 1.8, "roughly proportional: {}", a.end_s);
        // Single word splits at the char midpoint.
        let w = CaptionUnit { text: "okay".into(), start_s: 0.0, end_s: 1.0 };
        let (a, b) = split_unit(&w).expect("splittable");
        assert_eq!((a.text.as_str(), b.text.as_str()), ("ok", "ay"));
        // A 1-char unit does not split.
        assert!(split_unit(&CaptionUnit { text: "a".into(), start_s: 0.0, end_s: 1.0 }).is_none());
    }

    #[test]
    fn render_estimate_charges_whisper_only_when_uncached() {
        let cached = estimate_render_s(60.0, true);
        let fresh = estimate_render_s(60.0, false);
        assert!(fresh > cached + 30.0, "{fresh} vs {cached}");
    }

    #[test]
    fn nice_steps_are_round() {
        assert_eq!(nice_step(3.4), 5.0);
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(nice_step(0.8), 1.0);
        assert_eq!(nice_step(20.0), 30.0);
        // The zoomed-in ruler's sub-second rungs (ADR 0066).
        assert_eq!(nice_step(0.09), 0.1);
        assert_eq!(nice_step(0.3), 0.5);
    }

    #[test]
    fn viewport_round_trips_and_clamps() {
        let dur = 60.0;
        let mut vp = Viewport::default();
        vp.zoom_about(30.0, 0.5, 4.0, dur); // 4× about the middle
        assert!((vp.zoom - 4.0).abs() < 1e-9);
        assert!((vp.span(dur) - 15.0).abs() < 1e-9);
        // x ↔ t round-trips through the window.
        let (left, width) = (100.0_f32, 800.0_f32);
        let t = 31.7;
        let x = vp.t_to_x(t, left, width, dur);
        assert!((vp.x_to_t(x, left, width, dur) - t).abs() < 1e-4);
        // Scroll clamps at the clip edges.
        vp.scroll_px(-1e9, width, dur);
        assert_eq!(vp.left_t, 0.0);
        vp.scroll_px(1e9, width, dur);
        assert!((vp.left_t - (dur - vp.span(dur))).abs() < 1e-9);
        // Fit can't zoom out below 1 and never scrolls.
        let mut fit = Viewport::default();
        fit.zoom_about(10.0, 0.3, 0.25, dur);
        assert_eq!(fit.zoom, 1.0);
        assert_eq!(fit.left_t, 0.0);
        // Max zoom keeps at least MIN_SPAN_S visible.
        let mut deep = Viewport::default();
        deep.zoom_about(30.0, 0.5, 1e9, dur);
        assert!(deep.span(dur) >= Viewport::MIN_SPAN_S - 1e-9);
    }

    #[test]
    fn viewport_zoom_about_pointer_keeps_the_anchor_time() {
        let dur = 120.0;
        let mut vp = Viewport::default();
        // Pointer parked at 25% of the lanes, over t = 30.
        let (left, width) = (0.0_f32, 1000.0_f32);
        let anchor = vp.x_to_t(250.0, left, width, dur);
        assert!((anchor - 30.0).abs() < 1e-4);
        vp.zoom_about(anchor, 0.25, 3.0, dur);
        let x_after = vp.t_to_x(30.0, left, width, dur);
        assert!((x_after - 250.0).abs() < 0.5, "anchor drifted to {x_after}");
        // Zooming far back out re-clamps to fit.
        vp.zoom_about(anchor, 0.25, 1.0 / 100.0, dur);
        assert_eq!(vp.zoom, 1.0);
        assert_eq!(vp.left_t, 0.0);
    }

    #[test]
    fn viewport_follow_and_ensure_visible_page_the_window() {
        let dur = 100.0;
        let mut vp = Viewport { zoom: 10.0, left_t: 0.0 }; // span 10
        vp.follow(5.0, dur);
        assert_eq!(vp.left_t, 0.0, "inside the window: no move");
        vp.follow(11.0, dur);
        assert!((vp.left_t - 10.5).abs() < 1e-9, "pages past the right edge");
        vp.ensure_visible(50.0, dur);
        assert!((vp.left_t - 47.0).abs() < 1e-9, "outside jump parks at 30%");
        let before = vp.left_t;
        vp.ensure_visible(50.5, dur);
        assert_eq!(vp.left_t, before, "inside jump: untouched");
    }

    fn editor_state() -> EditorState {
        EditorState::from_seed(
            Layout::FullFrame { crop: Crop { x: 0.0, y: 0.0, w: 100.0, h: 200.0 } },
            1920.0,
            1080.0,
            yc_core::TimeRange { start_s: 0.0, end_s: 60.0 },
            None,
            Vec::new(),
            4.0,
            yc_core::CaptionGenre::RollingPop,
            yc_core::CaptionEngine::Whisper,
            Vec::new(),
            std::path::PathBuf::new(),
            std::path::PathBuf::new(),
            0.0,
            30.0,
        )
    }

    fn cap(t0: f64, t1: f64) -> CaptionUnit {
        CaptionUnit { text: "hi".into(), start_s: t0, end_s: t1 }
    }

    #[test]
    fn track_eyes_gate_the_render_spec() {
        let mut ed = editor_state();
        ed.transcript = Some(yc_core::Transcript {
            language: yc_core::Language::En,
            units: vec![cap(1.0, 2.0)],
        });
        ed.manual_units = vec![cap(3.0, 4.0)];
        ed.manual_places = vec![None];
        // Eyes on, transcript untouched: no override, the manual stream rides.
        let spec = ed.render_spec();
        assert!(spec.transcript_override.is_none());
        assert_eq!(spec.manual_captions.len(), 1);
        // Auto eye off: the override says "burn none" — the eye wins even
        // over an edited (dirty) transcript.
        ed.trk_auto.eye = false;
        ed.transcript_dirty = true;
        let spec = ed.render_spec();
        assert_eq!(spec.transcript_override.as_ref().map(|t| t.units.len()), Some(0));
        // Manual eye off: the stream leaves the spec; the editor's data stays.
        ed.trk_manual.eye = false;
        let spec = ed.render_spec();
        assert!(spec.manual_captions.is_empty());
        assert_eq!(ed.manual_units.len(), 1);
        // Eyes back on + dirty: the edited transcript burns verbatim again.
        ed.trk_auto.eye = true;
        ed.trk_manual.eye = true;
        let spec = ed.render_spec();
        assert_eq!(spec.transcript_override.as_ref().map(|t| t.units.len()), Some(1));
        assert_eq!(spec.manual_captions.len(), 1);
    }

    fn plan(bounds: &[f64]) -> yc_core::CameraPlan {
        let shots = bounds
            .windows(2)
            .enumerate()
            .map(|(i, w)| yc_core::Shot {
                start_s: w[0],
                end_s: w[1],
                track: Some(i),
                layout: Layout::FullFrame {
                    crop: Crop { x: i as f32 * 10.0, y: 0.0, w: 100.0, h: 200.0 },
                },
                pan_to: None,
            })
            .collect();
        yc_core::CameraPlan { shots }
    }

    #[test]
    fn split_plan_at_keeps_coverage_and_splits_a_glide_without_a_jump() {
        // Static shot: both halves inherit the framing; spans stay contiguous.
        let mut p = plan(&[0.0, 4.0, 8.0]);
        assert!(split_plan_at(&mut p, 2.0));
        assert_eq!(p.shots.len(), 3);
        assert_eq!((p.shots[0].start_s, p.shots[0].end_s), (0.0, 2.0));
        assert_eq!((p.shots[1].start_s, p.shots[1].end_s), (2.0, 4.0));
        assert_eq!(p.shots[0].layout, p.shots[1].layout);
        assert_eq!(p.shots[0].track, p.shots[1].track);
        // A sliver split is refused outright.
        assert!(!split_plan_at(&mut p, 2.0 + MIN_SHOT_S / 2.0));
        assert!(!split_plan_at(&mut p, 99.0), "outside every shot");
        // A glide splits into two glides that meet exactly at the cut: the
        // motion on screen is unchanged.
        let mut p = plan(&[0.0, 4.0]);
        p.shots[0].pan_to = Some(Crop { x: 40.0, y: 0.0, w: 100.0, h: 200.0 });
        let before = p.shots[0].layout_at(2.0);
        assert!(split_plan_at(&mut p, 2.0));
        assert_eq!(p.shots[0].layout_at(2.0 - 1e-9), p.shots[1].layout_at(2.0));
        assert_eq!(p.shots[1].layout_at(2.0), before);
        assert_eq!(
            p.shots[1].pan_to,
            Some(Crop { x: 40.0, y: 0.0, w: 100.0, h: 200.0 }),
            "the second half still glides to the original target"
        );
    }

    #[test]
    fn delete_plan_cut_merges_into_the_earlier_shot() {
        let mut p = plan(&[0.0, 4.0, 8.0, 10.0]);
        let earlier = p.shots[0].layout.clone();
        assert!(delete_plan_cut(&mut p, 1));
        assert_eq!(p.shots.len(), 2);
        assert_eq!((p.shots[0].start_s, p.shots[0].end_s), (0.0, 8.0));
        assert_eq!(p.shots[0].layout, earlier, "the framing the viewer was already watching");
        assert_eq!(p.shots[1].start_s, 8.0, "later shots untouched");
        // Boundary 0 is the clip start, not a cut; out-of-range is a no-op.
        assert!(!delete_plan_cut(&mut p, 0));
        assert!(!delete_plan_cut(&mut p, 5));
    }

    #[test]
    fn razor_add_toggle_delete_and_kept_spans() {
        let mut r = RazorState::default();
        assert!(r.kept_spans(10.0).is_none(), "no cuts, no removals");
        assert!(r.add_cut(4.0, 10.0));
        assert!(!r.add_cut(4.05, 10.0), "too near an existing cut");
        assert!(!r.add_cut(0.05, 10.0), "too near the clip start");
        assert!(r.add_cut(7.0, 10.0));
        assert_eq!(r.segments(10.0), vec![(0.0, 4.0), (4.0, 7.0), (7.0, 10.0)]);
        // Cuts alone are markers: the export is unchanged.
        assert!(r.kept_spans(10.0).is_none());
        // Remove the middle segment.
        assert!(r.toggle_segment(1));
        let keep = r.kept_spans(10.0).unwrap();
        assert_eq!(keep.len(), 2);
        assert_eq!((keep[0].start_s, keep[0].end_s), (0.0, 4.0));
        assert_eq!((keep[1].start_s, keep[1].end_s), (7.0, 10.0));
        // Removing every segment is refused: one kept segment survives.
        assert!(r.toggle_segment(0));
        assert!(!r.toggle_segment(2), "the last kept segment is protected");
        // Deleting the cut between two REMOVED segments keeps them removed...
        assert!(r.delete_cut(0));
        assert_eq!(r.segments(10.0).len(), 2);
        assert!(r.removed[0], "both halves removed -> merged removed");
        // ...but merging removed+kept lands KEPT (the safe default).
        assert!(r.delete_cut(0));
        assert!(!r.removed[0]);
        assert!(r.kept_spans(10.0).is_none());
    }

    #[test]
    fn razor_marker_scissors_cut_left_right_and_remove_span() {
        // ✂← with no marker: removes from the clip start to the playhead.
        let mut r = RazorState::default();
        assert!(r.cut_left(4.0, None, 10.0));
        let keep = r.kept_spans(10.0).unwrap();
        assert_eq!((keep[0].start_s, keep[0].end_s), (4.0, 10.0));
        // →✂ with a marker right of the playhead stops AT the marker.
        let mut r = RazorState::default();
        assert!(r.cut_right(2.0, Some(6.0), 10.0));
        let keep = r.kept_spans(10.0).unwrap();
        assert_eq!(keep.len(), 2);
        assert_eq!((keep[0].start_s, keep[0].end_s), (0.0, 2.0));
        assert_eq!((keep[1].start_s, keep[1].end_s), (6.0, 10.0));
        // The operator's middle-cut flow: marker at 3, playhead moved PAST it
        // to 6, ✂← removes exactly [3, 6] — "cut left until it touches the
        // marker, then stop".
        let mut r = RazorState::default();
        assert!(r.cut_left(6.0, Some(3.0), 10.0));
        let keep = r.kept_spans(10.0).unwrap();
        assert_eq!(keep.len(), 2);
        assert_eq!((keep[0].start_s, keep[0].end_s), (0.0, 3.0));
        assert_eq!((keep[1].start_s, keep[1].end_s), (6.0, 10.0));
        // A marker on the WRONG side falls back to the clip edge.
        let mut r = RazorState::default();
        assert!(r.cut_left(4.0, Some(7.0), 10.0), "marker right of playhead: cut to start");
        assert_eq!(r.kept_spans(10.0).unwrap()[0].start_s, 4.0);
        // Refusals: a sub-segment span; removing the last kept segment.
        let mut r = RazorState::default();
        assert!(!r.cut_left(0.05, None, 10.0));
        assert!(r.cut_left(9.0, None, 10.0));
        assert!(!r.cut_right(9.0, None, 10.0), "would remove the last kept segment");
        // remove_span reuses a razor cut within the min-segment radius
        // instead of stacking a second cut beside it.
        let mut r = RazorState::default();
        r.add_cut(5.0, 20.0);
        assert!(r.remove_span(5.04, 12.0, 20.0));
        assert_eq!(r.cuts.len(), 2, "reused the existing cut at 5.0");
        // Delete key: the segment under the playhead.
        let mut r = RazorState::default();
        r.add_cut(3.0, 10.0);
        r.add_cut(6.0, 10.0);
        assert!(r.remove_segment_at(4.5));
        assert!(!r.remove_segment_at(4.5), "already removed");
    }

    #[test]
    fn razor_playback_skip_and_adjacent_kept_merge() {
        let mut r = RazorState::default();
        r.add_cut(2.0, 10.0);
        r.add_cut(5.0, 10.0);
        r.toggle_segment(1);
        assert_eq!(r.removed_span_at(3.0, 10.0), Some((2.0, 5.0)));
        assert_eq!(r.removed_span_at(1.0, 10.0), None);
        // A cut between two KEPT segments is just a marker: the kept spans
        // merge across it, so the export sees one contiguous trim.
        r.add_cut(7.0, 10.0);
        let keep = r.kept_spans(10.0).unwrap();
        assert_eq!(keep.len(), 2, "adjacent kept segments merge");
        assert_eq!((keep[1].start_s, keep[1].end_s), (5.0, 10.0));
    }

    #[test]
    fn motion_plans_cover_the_clip_and_move_as_named() {
        let base = Crop { x: 200.0, y: 100.0, w: 450.0, h: 800.0 };
        let (sw, sh) = (1920.0, 1080.0);
        for (m, _) in MOTION_PRESETS {
            let plan = motion_plan(base, sw, sh, 20.0, m);
            assert!(!plan.shots.is_empty());
            assert_eq!(plan.shots.first().unwrap().start_s, 0.0);
            assert_eq!(plan.shots.last().unwrap().end_s, 20.0);
            for w in plan.shots.windows(2) {
                assert_eq!(w[0].end_s, w[1].start_s, "contiguous shots");
            }
        }
        // Zoom in: ends tighter than it starts.
        let plan = motion_plan(base, sw, sh, 20.0, MotionPreset::ZoomIn);
        let Layout::FullFrame { crop } = plan.shots[0].layout.clone() else { panic!() };
        assert!(plan.shots[0].pan_to.unwrap().w < crop.w);
        // Pan left: same size, traveling left.
        let plan = motion_plan(base, sw, sh, 20.0, MotionPreset::PanLeft);
        let Layout::FullFrame { crop } = plan.shots[0].layout.clone() else { panic!() };
        let to = plan.shots[0].pan_to.unwrap();
        assert!(to.x < crop.x, "pan-left travels left: {} -> {}", crop.x, to.x);
        assert_eq!(to.w, crop.w, "a pan never zooms");
        // Punch in: two static shots, the second tighter — a hard cut.
        let plan = motion_plan(base, sw, sh, 20.0, MotionPreset::PunchIn);
        assert_eq!(plan.shots.len(), 2);
        assert!(plan.shots.iter().all(|s| s.pan_to.is_none()));
        let Layout::FullFrame { crop: c0 } = plan.shots[0].layout.clone() else { panic!() };
        let Layout::FullFrame { crop: c1 } = plan.shots[1].layout.clone() else { panic!() };
        assert!(c1.w < c0.w);
    }

    #[test]
    fn magnet_snaps_to_the_nearest_anchor_within_tolerance_only() {
        assert_eq!(magnet(5.1, &[5.0, 6.0], 0.2), Some(5.0));
        assert_eq!(magnet(5.6, &[5.0, 6.0], 0.5), Some(6.0));
        assert_eq!(magnet(5.5, &[5.0, 7.0], 0.2), None);
        assert_eq!(magnet(5.0, &[], 1.0), None);
    }

    #[test]
    fn quantize_cs_rounds_to_centiseconds() {
        assert!((quantize_cs(1.23456) - 1.23).abs() < 1e-9);
        assert!((quantize_cs(1.235) - 1.24).abs() < 1e-9);
    }

    #[test]
    fn preview_crop_time_binds_to_the_shown_frame_not_the_playhead() {
        // The editor's "blank at a cut": parked at 13.31 s — just past the
        // ANTITESA source cut at 13.302833 — the ~1.74 fps filmstrip is still
        // SHOWING the frame extracted at ~13.23 s (index 23, outgoing shot).
        // Picking the crop by the playhead would paint the INCOMING shot's
        // crop over that outgoing picture; picking by the shown frame's time
        // keeps crop and pixels on the same side of the cut.
        let strip_fps = 120.0 / 69.0; // this clip's real adaptive strip rate
        let idx = ((13.31f64 * strip_fps).round() as usize).min(119); // = strip_idx()
        let t = strip_frame_time_s(idx, strip_fps);
        assert_eq!(idx, 23);
        assert!(t < 13.302833, "shown frame predates the cut: {t}");
        // Live playback decodes on the SOURCE grid (fps=src_fps): each output
        // tick shows the last source frame with pts <= the tick, so with the
        // real ANTITESA seek phase (frames at k*delta + 39.6 ms) the cut frame
        // (pts 13.302833, source k=318) first APPEARS at tick 319 (13.305).
        // The shown frame's midpoint must pick the matching side of the cut
        // in both states: tick 318 still shows the outgoing scene, tick 319
        // shows the incoming one.
        let fps = 24000.0 / 1001.0;
        let showing_outgoing = crate::player::frame_mid_s(319, fps); // ticks 0..=318
        assert!(
            showing_outgoing < 13.302833,
            "old scene on screen keeps the old crop: {showing_outgoing}"
        );
        let showing_cut = crate::player::frame_mid_s(320, fps); // ticks 0..=319
        assert!(
            showing_cut > 13.302833,
            "the tick that shows the cut frame selects the incoming shot: {showing_cut}"
        );
    }
}

/// Clip time of filmstrip frame `idx` — the strip is extracted at
/// `strip_fps` from the clip start, so frame k's content sits at ~k/fps.
/// Camera shots / overlays for a strip frame must be picked at THIS time,
/// not the playhead's (see [`EditorState::display_time`]).
fn strip_frame_time_s(idx: usize, strip_fps: f64) -> f64 {
    idx as f64 / strip_fps.max(1e-6)
}

/// One track-header CARD (the operator's round-2 verdict, ADR 0066): a
/// rounded box spanning EXACTLY the track's lane band — header and lane
/// read as one bar — with a kind-colored accent edge that dims when the
/// track's eye is off. Returns the card rect so callers place content in it.
fn track_card(
    p: &egui::Painter,
    hdr: Rect,
    y0: f32,
    y1: f32,
    accent: Color32,
    live: bool,
) -> Rect {
    let card = Rect::from_min_max(
        egui::pos2(hdr.left() + 4.0, y0 + 1.5),
        egui::pos2(hdr.right() - 6.0, y1 - 1.5),
    );
    p.rect_filled(card, CornerRadius::same(4), Color32::from_rgb(0x1E, 0x22, 0x2B));
    p.rect_stroke(
        card,
        CornerRadius::same(4),
        Stroke::new(1.0, Color32::from_rgb(0x2A, 0x2F, 0x38)),
        StrokeKind::Inside,
    );
    p.rect_filled(
        Rect::from_min_max(card.left_top(), egui::pos2(card.left() + 3.0, card.bottom())),
        CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 },
        if live { accent.gamma_multiply(0.9) } else { Color32::from_gray(70) },
    );
    card
}

/// Which painted track-header toggle to draw.
#[derive(Clone, Copy, PartialEq)]
enum TrackIcon {
    Eye,
    Lock,
}

/// A 15-px track-header toggle, PAINTED — no font in the stack reliably
/// carries 👁/🔒 (the round-6 tofu lesson, ADR 0065 Amendment 5).
fn track_toggle(
    ui: &mut egui::Ui,
    p: &egui::Painter,
    center: egui::Pos2,
    icon: TrackIcon,
    on: bool,
    id: (&str, u8),
    tip: &str,
) -> bool {
    let r = Rect::from_center_size(center, egui::vec2(15.0, 15.0));
    let resp = ui.interact(r, ui.id().with(("trk-toggle", id.0, id.1)), Sense::click());
    if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(3), Color32::from_gray(46));
    }
    match icon {
        TrackIcon::Eye => draw_eye(p, center, on),
        TrackIcon::Lock => draw_lock(p, center, on),
    }
    resp.on_hover_text(tip).clicked()
}

/// An almond-shaped eye + pupil; slashed when off. Sampled quadratics —
/// crisp at 15 px and font-proof.
fn draw_eye(p: &egui::Painter, c: egui::Pos2, on: bool) {
    let col = if on { Color32::from_gray(208) } else { Color32::from_gray(105) };
    let (a, b) = (c + egui::vec2(-4.6, 0.0), c + egui::vec2(4.6, 0.0));
    let mut pts = Vec::with_capacity(18);
    for k in 0..=8 {
        pts.push(quad_point(a, c + egui::vec2(0.0, -5.8), b, k as f32 / 8.0));
    }
    for k in 0..=8 {
        pts.push(quad_point(b, c + egui::vec2(0.0, 5.8), a, k as f32 / 8.0));
    }
    p.add(egui::Shape::line(pts, Stroke::new(1.1, col)));
    p.circle_filled(c, 1.7, col);
    if !on {
        p.line_segment(
            [c + egui::vec2(-4.6, 4.6), c + egui::vec2(4.6, -4.6)],
            Stroke::new(1.3, Color32::from_gray(150)),
        );
    }
}

/// A padlock: gold-filled body when locked, quiet outline when open.
fn draw_lock(p: &egui::Painter, c: egui::Pos2, locked: bool) {
    let col = if locked { theme::GOLD } else { Color32::from_gray(105) };
    let body = Rect::from_center_size(c + egui::vec2(0.0, 2.0), egui::vec2(7.6, 6.2));
    if locked {
        p.rect_filled(body, CornerRadius::same(1), col);
    } else {
        p.rect_stroke(body, CornerRadius::same(1), Stroke::new(1.1, col), StrokeKind::Inside);
    }
    let top = egui::pos2(body.center().x, body.top());
    let mut pts = Vec::with_capacity(9);
    for k in 0..=8 {
        let ang = std::f32::consts::PI * (1.0 - k as f32 / 8.0);
        pts.push(egui::pos2(top.x + 2.6 * ang.cos(), top.y - 2.6 * ang.sin()));
    }
    p.add(egui::Shape::line(pts, Stroke::new(1.1, col)));
}

/// A point on the quadratic Bézier (a, ctrl, b) at parameter `t`.
fn quad_point(a: egui::Pos2, ctrl: egui::Pos2, b: egui::Pos2, t: f32) -> egui::Pos2 {
    let u = 1.0 - t;
    egui::pos2(
        u * u * a.x + 2.0 * u * t * ctrl.x + t * t * b.x,
        u * u * a.y + 2.0 * u * t * ctrl.y + t * t * b.y,
    )
}

/// A tiny painted button (the zoom corner) — painted like the toggles so it
/// lives on the strip's own painter, above the lane content.
fn mini_btn(ui: &mut egui::Ui, p: &egui::Painter, r: Rect, label: &str, tip: &str) -> bool {
    let resp = ui.interact(r, ui.id().with(("tl-mini", label)), Sense::click());
    p.rect_filled(
        r,
        CornerRadius::same(4),
        if resp.hovered() {
            Color32::from_rgb(0x2A, 0x2F, 0x3A)
        } else {
            Color32::from_rgb(0x1E, 0x22, 0x2B)
        },
    );
    p.rect_stroke(
        r,
        CornerRadius::same(4),
        Stroke::new(1.0, Color32::from_rgb(0x2A, 0x2F, 0x38)),
        StrokeKind::Inside,
    );
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(9.5),
        Color32::from_gray(210),
    );
    resp.on_hover_text(tip).clicked()
}

/// A round ruler step (0.1/0.2/0.5/1/2/5/10/15/30/60s ladder) at least `raw`
/// long — the sub-second rungs exist for the zoomed-in ruler (ADR 0066).
fn nice_step(raw: f64) -> f64 {
    for s in [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0] {
        if raw <= s {
            return s;
        }
    }
    600.0
}
