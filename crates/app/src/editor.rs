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
//! ├─────────────── timeline: ruler · captions · speakers · cuts ───────┤
//! ```
//!
//! All framing geometry stays the pure `yc_frame` layer; the speaker analysis
//! and camera plan come from the worker (`Progress::Speakers`); the caption
//! line model stays `yc_render`'s (the preview cannot drift from the burn-in,
//! ADR 0036). This module is only the egui surface.

use std::time::Instant;

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind};
use yc_core::{
    CameraMode, CameraPlan, CaptionEngine, CaptionGenre, CaptionPlacement, CaptionStyle, Crop,
    Layout, TimeRange, Transcript, CANVAS_H, CANVAS_W,
};
use yc_frame::speaker::{track_label, SpeakerAnalysis};
use yc_frame::FaceCluster;
use yc_render::{preview_lines, resolve_placement, word_states, PreviewLine, WordState};

use crate::player::PreviewPlayer;
use crate::presets::caption_presets;
use crate::theme;

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
    /// The active-speaker cut plan — `Some` only in ActiveSpeaker mode.
    pub camera: Option<CameraPlan>,
    /// The operator's edited transcript — `Some` only when they touched it.
    pub transcript_override: Option<Transcript>,
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
    pub speaker_job: SpeakerJob,
    /// Selected caption row (click focuses + seeks).
    sel_unit: Option<usize>,
    /// One-shot: scroll the transcript panel to `sel_unit` this frame (set by
    /// clicking a caption block on the timeline).
    scroll_to_sel: bool,
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
            speaker_job: SpeakerJob::NotRun,
            sel_unit: None,
            scroll_to_sel: false,
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
        self.lines = preview_lines(&transcript, self.style.genre);
        self.lines_genre = self.style.genre;
        self.lines_dirty = false;
        self.overlay_cache = None;
        self.transcript = Some(transcript);
    }

    /// Receive the speaker analysis + camera plan (`Progress::Speakers`).
    pub fn set_speakers(&mut self, analysis: SpeakerAnalysis, plan: CameraPlan) {
        // Auto-arm Active Speaker when the analysis proves multi-person and
        // the operator hasn't chosen a mode deliberately (Manual = the seed).
        if analysis.tracks.len() >= 2 && self.camera_mode == CameraMode::Manual {
            self.camera_mode = CameraMode::ActiveSpeaker;
        }
        self.speakers = Some(analysis);
        self.plan = Some(plan);
        self.speaker_job = SpeakerJob::Ready;
        self.refresh_camera_audit();
    }

    /// Re-audit the current camera plan against the analysis (on receipt and
    /// after any operator override) so the Camera panel's warnings never go
    /// stale.
    fn refresh_camera_audit(&mut self) {
        self.camera_audit = match (&self.speakers, &self.plan) {
            (Some(a), Some(p)) => yc_frame::speaker::audit_camera_plan(a, p),
            _ => Vec::new(),
        };
    }

    /// Keep `lines` in step with the current genre and any operator edits.
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

    /// The Layout the preview shows at clip time `t` under the current camera
    /// mode — and what a static-mode render exports.
    fn effective_layout(&self, t: f64) -> Layout {
        match self.camera_mode {
            CameraMode::Manual => self.manual_layout(),
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
            _ => None,
        };
        RenderSpec {
            layout: self.effective_layout(self.playhead_s),
            style: self.style.clone(),
            placement: self.placement,
            camera,
            transcript_override: if self.transcript_dirty { self.transcript.clone() } else { None },
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
                        .on_hover_text("Show/hide the caption overlay")
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
        egui::Panel::bottom("studio-timeline")
            .exact_size(150.0)
            .show_inside(ui, |ui| {
                if let Some(a) = self.ui_timeline(ui, busy, rendering) {
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
                    if let Some(a) = self.ui_properties(ui, busy) {
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
        let (space, esc, left, right, up, down, plus, minus, zero, shift) = ui.input(|i| {
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
        if self.playing.is_some() {
            if let Some(mid) = self.live.as_ref().and_then(|l| l.shown_frame_mid_s()) {
                let (_, offset) = self.playing.expect("playing");
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
        if self.camera_mode == CameraMode::ActiveSpeaker {
            if let Some(a) = &self.speakers {
                let text = match self
                    .plan
                    .as_ref()
                    .and_then(|p| p.shot_at(shown_t))
                    .and_then(|s| s.track)
                {
                    Some(id) => format!(
                        "Tracking {} · {:.0}%",
                        track_label(id),
                        (a.confidence_at(shown_t) * 100.0).clamp(0.0, 100.0)
                    ),
                    None => "Group shot".to_string(),
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
                    self.crop_box_interaction(ui, frame_rect, to_screen(&g), "src-gp", CropTarget::Gameplay);
                    self.crop_box_interaction(ui, frame_rect, to_screen(&f), "src-fc", CropTarget::Facecam);
                }
                LayoutKind::FullCam => {
                    let c = self.fullcam;
                    self.crop_box_interaction(ui, frame_rect, to_screen(&c), "src-cam", CropTarget::FullCam);
                }
                LayoutKind::FullGameplay => {
                    let c = self.fullgameplay;
                    self.crop_box_interaction(ui, frame_rect, to_screen(&c), "src-gpl", CropTarget::FullGameplay);
                }
            }
        } else {
            // In an AI mode a drag on the frame flips to Manual (CapCut-style:
            // touching the framing takes control), seeding from the AI crop.
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

    /// Drag body to move + corner handles to resize + scroll to zoom, for one
    /// crop box in Source view.
    fn crop_box_interaction(
        &mut self,
        ui: &mut egui::Ui,
        frame_rect: Rect,
        screen: Rect,
        id: &str,
        target: CropTarget,
    ) {
        let (sw, sh) = (self.src_w, self.src_h);
        let px = sw / frame_rect.width().max(1.0);
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
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                let d = resp.drag_delta();
                // Dragging a corner outward grows the box: project the delta on
                // the outward diagonal, aspect-locked via zoom about the
                // opposite corner.
                let crop = self.crop_of_mut(target);
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
            return;
        }
        // Body: move.
        let resp = ui.interact(screen, ui.id().with((id, "body")), Sense::click_and_drag());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
        }
        if resp.dragged() {
            let d = resp.drag_delta();
            let crop = self.crop_of_mut(target);
            *crop = yc_frame::pan_crop(*crop, sw, sh, d.x * px, d.y * px);
        }
        // Scroll on the box: zoom about its center.
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                let factor = (-scroll * 0.0015).exp();
                let crop = self.crop_of_mut(target);
                let (ax, ay) = (crop.x + crop.w * 0.5, crop.y + crop.h * 0.5);
                *crop = yc_frame::zoom_crop(*crop, sw, sh, factor, ax, ay);
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            }
        }
    }

    fn crop_of_mut(&mut self, target: CropTarget) -> &mut Crop {
        match target {
            CropTarget::Gameplay => &mut self.gameplay,
            CropTarget::Facecam => &mut self.facecam,
            CropTarget::FullCam => &mut self.fullcam,
            CropTarget::FullGameplay => &mut self.fullgameplay,
        }
    }

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

    /// The bottom strip: transport, ruler + scrub, caption blocks, speaker
    /// lanes, cut markers.
    fn ui_timeline(&mut self, ui: &mut egui::Ui, busy: bool, rendering: bool) -> Option<EditorAction> {
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

        // The strip: ruler(16) + captions(22) + speakers(N*12) + cuts(10).
        let n_tracks = self.speakers.as_ref().map(|a| a.tracks.len()).unwrap_or(0);
        let strip_h = 16.0 + 24.0 + (n_tracks as f32 * 13.0) + 12.0 + 8.0;
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), strip_h.max(60.0)),
            Sense::click_and_drag(),
        );
        let p = ui.painter_at(rect);
        p.rect_filled(rect, CornerRadius::same(4), theme::WELL);
        let t_to_x = |t: f64| rect.left() + (t / dur) as f32 * rect.width();
        let x_to_t = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * dur;

        // Ruler ticks: a major every ~1/8 of the clip, rounded to a nice step.
        let step = nice_step(dur / 8.0);
        let mut t = 0.0;
        while t <= dur + 1e-9 {
            let x = t_to_x(t);
            p.line_segment(
                [egui::pos2(x, rect.top() + 2.0), egui::pos2(x, rect.top() + 12.0)],
                Stroke::new(1.0, Color32::from_gray(90)),
            );
            p.text(
                egui::pos2(x + 3.0, rect.top() + 1.0),
                Align2::LEFT_TOP,
                fmt_mmss(t),
                FontId::monospace(9.5),
                Color32::from_gray(175),
            );
            t += step;
        }

        // Caption blocks — clickable: jump the playhead to the line AND
        // select + scroll to its row in the transcript panel (the timeline is
        // the fastest way to LOCATE a caption; the panel is where you fix it).
        self.sync_lines();
        let cap_y0 = rect.top() + 18.0;
        let mut clicked_line: Option<usize> = None;
        for (li, l) in self.lines.iter().enumerate() {
            let r = Rect::from_min_max(
                egui::pos2(t_to_x(l.start_s), cap_y0),
                egui::pos2(t_to_x(l.end_s).max(t_to_x(l.start_s) + 2.0), cap_y0 + 20.0),
            );
            let text = l.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            // Registered after the strip's scrub response, so blocks win the
            // pointer for clicks; drags still fall through to the scrub.
            let resp = ui.interact(r, ui.id().with(("cap-block", li)), Sense::click());
            let hovered = resp.hovered();
            if hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.on_hover_text(format!("{}  ·  {text}", fmt_mmss_cc(l.start_s))).clicked() {
                clicked_line = Some(li);
            }
            p.rect_filled(
                r,
                CornerRadius::same(3),
                Color32::from_rgba_unmultiplied(255, 255, 255, if hovered { 48 } else { 26 }),
            );
            p.rect_stroke(
                r,
                CornerRadius::same(3),
                Stroke::new(1.0, if hovered { theme::GOLD } else { Color32::from_gray(80) }),
                StrokeKind::Inside,
            );
            if r.width() > 26.0 {
                p.text(
                    r.left_center() + egui::vec2(4.0, 0.0),
                    Align2::LEFT_CENTER,
                    ellipsize(&text, (r.width() / 7.0) as usize),
                    FontId::proportional(10.5),
                    Color32::from_gray(200),
                );
            }
        }
        if let Some(li) = clicked_line {
            let start = self.lines[li].start_s;
            self.playhead_s = start.clamp(0.0, dur);
            // Locate the line's first word in the transcript rows and scroll
            // the panel to it.
            if let Some(t) = &self.transcript {
                self.sel_unit = t
                    .units
                    .iter()
                    .position(|u| u.start_s >= start - 1e-6)
                    .or(Some(t.units.len().saturating_sub(1)));
                self.scroll_to_sel = true;
            }
            // Same contract as the scrub: playing audio + video restart at the jump.
            if self.playing.is_some() {
                self.playing = Some((Instant::now(), self.playhead_s));
                self.start_video();
                action = Some(EditorAction::Play(self.play_range_from(self.playhead_s)));
            }
        }

        // Speaker lanes.
        let mut lane_y = cap_y0 + 26.0;
        if let Some(a) = &self.speakers {
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
                                egui::pos2(t_to_x(t1), lane_y + 9.0),
                            ),
                            CornerRadius::same(2),
                            color.gamma_multiply(0.75),
                        );
                        i = j;
                    } else {
                        i += 1;
                    }
                }
                lane_y += 13.0;
            }
        }

        // Camera cut markers (Active Speaker).
        if self.camera_mode == CameraMode::ActiveSpeaker {
            if let Some(plan) = &self.plan {
                for s in plan.shots.iter().skip(1) {
                    let x = t_to_x(s.start_s);
                    p.line_segment(
                        [egui::pos2(x, rect.top() + 14.0), egui::pos2(x, rect.bottom() - 2.0)],
                        Stroke::new(1.0, theme::GOLD.gamma_multiply(0.6)),
                    );
                    p.text(
                        egui::pos2(x + 2.0, rect.bottom() - 12.0),
                        Align2::LEFT_TOP,
                        "✂",
                        FontId::proportional(9.0),
                        theme::GOLD.gamma_multiply(0.8),
                    );
                }
            }
        }

        // Playhead.
        let px = t_to_x(self.playhead_s);
        p.line_segment(
            [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
            Stroke::new(2.0, theme::GOLD),
        );
        p.circle_filled(egui::pos2(px, rect.top() + 3.0), 4.0, theme::GOLD);

        // Scrub: click or drag anywhere on the strip. While dragging, only the
        // visuals track the pointer (restarting the sink every frame is a
        // re-seek storm); the audio commits at the NEW time on click and on
        // drag RELEASE. The release must be its own check: on that frame
        // `dragged()` is already false, so a release branch nested under it
        // can never run — the bug where dragging mid-playback moved the
        // playhead while the audio kept playing from the old position.
        if resp.clicked() || resp.dragged() {
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
        if resp.drag_stopped() && self.playing.is_some() {
            self.playing = Some((Instant::now(), self.playhead_s));
            self.start_video();
            action = Some(EditorAction::Play(self.play_range_from(self.playhead_s)));
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
        ui.add_space(4.0);

        let mut dirty = false;
        let mut seek: Option<f64> = None;
        let mut delete: Option<usize> = None;
        let mut split: Option<usize> = None;
        let mut merge: Option<usize> = None;
        let mut censor: Option<usize> = None;
        let playhead = self.playhead_s;
        let n = transcript.units.len();

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
                        ui.horizontal(|ui| {
                            // Timestamp: editable mm:ss.cc (drag or type).
                            let u = &mut transcript.units[i];
                            let mut start = u.start_s;
                            let resp = ui.add_enabled(
                                enabled,
                                egui::DragValue::new(&mut start)
                                    .speed(0.02)
                                    .range(0.0..=self.range.duration_s())
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
                            // Text.
                            let text_resp = ui.add_enabled(
                                enabled,
                                egui::TextEdit::singleline(&mut u.text)
                                    .desired_width(ui.available_width() - 108.0)
                                    .font(egui::TextStyle::Body),
                            );
                            if text_resp.changed() {
                                dirty = true;
                            }
                            if text_resp.gained_focus() {
                                self.sel_unit = Some(i);
                                seek = Some(u.start_s);
                            }
                            // Row actions, right-aligned and compact.
                            ui.add_enabled_ui(enabled, |ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                ui.spacing_mut().button_padding = egui::vec2(4.0, 2.0);
                                if ui.small_button("✂").on_hover_text("Split this caption").clicked() {
                                    split = Some(i);
                                }
                                if i + 1 < n {
                                    if ui.small_button("⇓").on_hover_text("Merge with the next caption").clicked() {
                                        merge = Some(i);
                                    }
                                } else {
                                    ui.add_enabled(false, egui::Button::new("⇓").small());
                                }
                                if ui.small_button("＊").on_hover_text("Censor this word (d***)").clicked() {
                                    censor = Some(i);
                                }
                                if ui.small_button("🗑").on_hover_text("Delete this caption").clicked() {
                                    delete = Some(i);
                                }
                            });
                        });
                    });
                // A timeline caption-block click selects this row: bring it
                // into view (one-shot).
                if self.scroll_to_sel && self.sel_unit == Some(i) {
                    row_resp.response.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });
        self.scroll_to_sel = false;

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(enabled, egui::Button::new("＋ Add caption at playhead"))
                .clicked()
            {
                let at = self.playhead_s;
                let idx = transcript.units.iter().position(|u| u.start_s > at).unwrap_or(n);
                transcript.units.insert(
                    idx,
                    yc_core::CaptionUnit { text: "text".into(), start_s: at, end_s: at + 0.8 },
                );
                self.sel_unit = Some(idx);
                dirty = true;
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
        if dirty {
            // Keep units start-ordered so grouping/preview stay sane even if a
            // timestamp edit reordered them.
            transcript
                .units
                .sort_by(|a, b| a.start_s.partial_cmp(&b.start_s).unwrap_or(std::cmp::Ordering::Equal));
            self.transcript_dirty = true;
            self.lines_dirty = true;
        }
        if let Some(t) = seek {
            self.playhead_s = t.clamp(0.0, self.range.duration_s());
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

    fn ui_properties(&mut self, ui: &mut egui::Ui, busy: bool) -> Option<EditorAction> {
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
                    match (&self.transcript, self.transcript_dirty) {
                        (None, _) => "on - transcribed during render".into(),
                        (Some(t), true) => format!("on - {} lines (edited)", preview_lines(t, self.style.genre).len()),
                        (Some(t), false) => format!("on - {} lines", preview_lines(t, self.style.genre).len()),
                    },
                );
                row(ui, "Caption style", format!("{} · {}px", self.style.name, self.style.font_size));
                row(
                    ui,
                    "Camera",
                    if spec_camera {
                        format!(
                            "Active Speaker · {} cuts",
                            self.plan.as_ref().map(|p| p.shots.len().saturating_sub(1)).unwrap_or(0)
                        )
                    } else {
                        camera_mode_label(self.camera_mode).to_string()
                    },
                );
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
        let (line, ghost) = match self.lines.iter().find(|l| l.start_s <= p && p < l.end_s) {
            Some(l) => (l, false),
            None => match self.lines.iter().find(|l| l.start_s >= p).or(self.lines.last()) {
                Some(l) => (l, true),
                None => return,
            },
        };

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

        let states = word_states(style.genre, line, p);

        let key = OverlayKey {
            line_start_bits: line.start_s.to_bits(),
            n_words: line.words.len(),
            states: states.clone(),
            ghost,
            font_bits: font_px.to_bits(),
        };
        let painter = ui.painter_at(canvas_rect);
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
        // Faux outline: four offset passes in the outline colour stand in for
        // libass's outline (ADR 0036's fidelity boundary: look approximate,
        // position/size/timing exact). Width follows the style's outline px.
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

        if !enabled {
            return;
        }
        // Drag to move, scroll on it to resize — the Crop verbs, applied to the
        // caption block.
        let box_rect = Rect::from_center_size(center, size.max(egui::vec2(24.0, 16.0)));
        let resp = ui.interact(box_rect.expand(6.0), ui.id().with("caption-box"), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
            painter.rect_stroke(
                box_rect.expand(4.0),
                CornerRadius::same(3),
                Stroke::new(1.0, theme::GOLD),
                StrokeKind::Outside,
            );
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

/// Which crop a Source-view interaction targets.
#[derive(Clone, Copy)]
enum CropTarget {
    Gameplay,
    Facecam,
    FullCam,
    FullGameplay,
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

/// A round ruler step (1/2/5/10/15/30/60s ladder) at least `raw` long.
fn nice_step(raw: f64) -> f64 {
    for s in [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0] {
        if raw <= s {
            return s;
        }
    }
    600.0
}
