//! The nudge editor (M6 increment, ADR 0012): a manual escape hatch over the
//! auto-detected framing. The operator drags/zooms the facecam (and gameplay)
//! Crops, drags the Seam, and overrides the Layout type, watching a **live egui
//! UV-composite** — each Panel is drawn as a UV sub-rectangle of one extracted
//! source frame, which is pixel-exact for the render's crop/scale/vstack
//! geometry (the only gap, subtitles, is immaterial to framing and appears on
//! Render). This supersedes ADR 0005's ffmpeg-filtergraph preview for the
//! framing editor: no ffmpeg per nudge, no GPU contention, 60 fps.
//!
//! All geometry is the pure, unit-tested `yc_frame` layer; this module is only
//! the egui surface and the per-kind Crop bookkeeping.

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind};
use yc_core::{Crop, Layout, TimeRange, CANVAS_H, CANVAS_W};

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

/// What `show` reports back to the app each frame.
pub enum EditorAction {
    /// Nothing to do this frame.
    None,
    /// The operator hit Render: composite this (nudged) Layout.
    Render(Layout),
    /// The operator dismissed the editor without rendering.
    Cancel,
}

/// The editable framing state. All four Crops stay resident so switching Layout
/// kind never discards a nudge: e.g. toggling Stacked -> Full gameplay -> Stacked
/// preserves the facecam the operator placed.
pub struct EditorState {
    src_w: f32,
    src_h: f32,
    range: TimeRange,
    /// Preview frames sampled across the clip range (textures), for the scrub
    /// slider. Always non-empty (Prepare fails otherwise).
    frames: Vec<egui::TextureHandle>,
    frame_idx: usize,
    /// The auto-detected seed, kept for "Reset to auto".
    auto_layout: Layout,
    kind: LayoutKind,
    seam: f32,
    gameplay: Crop,
    facecam: Crop,
    fullcam: Crop,
    fullgameplay: Crop,
}

impl EditorState {
    /// Seed the editor from the auto-detected Layout (ADR 0012). Crops the auto
    /// pick does not provide are seeded with sensible defaults so a Layout-type
    /// override has something to start from.
    pub fn from_seed(
        auto_layout: Layout,
        src_w: f32,
        src_h: f32,
        range: TimeRange,
        frames: Vec<egui::TextureHandle>,
    ) -> Self {
        let frame_idx = frames.len() / 2; // a representative middle frame
        let (kind, seam, gameplay, facecam, fullcam, fullgameplay) =
            seed_fields(&auto_layout, src_w, src_h);
        Self {
            src_w,
            src_h,
            range,
            frames,
            frame_idx,
            auto_layout,
            kind,
            seam,
            gameplay,
            facecam,
            fullcam,
            fullgameplay,
        }
    }

    /// The Layout the operator's current edits describe.
    fn current_layout(&self) -> Layout {
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

    /// Draw the editor and return the operator's action. `enabled` is false while
    /// a worker job runs (a render in flight): the composite still draws, but
    /// interactions and the Render/Cancel buttons are inert.
    pub fn show(&mut self, ui: &mut egui::Ui, enabled: bool) -> EditorAction {
        let mut action = EditorAction::None;
        ui.separator();
        ui.strong(format!(
            "Frame the Clip  ({:.1}s)   - drag to pan, scroll to zoom",
            self.range.duration_s()
        ));

        // Layout-type override (the three-way; ADR 0012).
        ui.horizontal(|ui| {
            ui.label("Layout:");
            ui.add_enabled_ui(enabled, |ui| {
                if ui.selectable_label(self.kind == LayoutKind::Stacked, "Stacked").clicked() {
                    self.kind = LayoutKind::Stacked;
                }
                if ui.selectable_label(self.kind == LayoutKind::FullCam, "Full cam").clicked() {
                    self.kind = LayoutKind::FullCam;
                }
                if ui
                    .selectable_label(self.kind == LayoutKind::FullGameplay, "Full gameplay")
                    .clicked()
                {
                    self.kind = LayoutKind::FullGameplay;
                }
            });
        });

        // The composite canvas: a 9:16 rectangle, panels drawn as UV sub-rects.
        let canvas_w = ui.available_width().min(360.0).max(160.0);
        let canvas_h = canvas_w * CANVAS_H as f32 / CANVAS_W as f32;
        let (canvas_rect, _) =
            ui.allocate_exact_size(egui::vec2(canvas_w, canvas_h), Sense::hover());
        let tex = self.frames[self.frame_idx.min(self.frames.len().saturating_sub(1))].id();
        let painter = ui.painter_at(canvas_rect);
        painter.rect_filled(canvas_rect, CornerRadius::ZERO, Color32::BLACK);

        let (src_w, src_h) = (self.src_w, self.src_h);
        match self.kind {
            LayoutKind::Stacked => {
                let top_h = (self.seam * canvas_h).clamp(8.0, canvas_h - 8.0);
                let top = Rect::from_min_size(canvas_rect.min, egui::vec2(canvas_w, top_h));
                let bot = Rect::from_min_max(
                    egui::pos2(canvas_rect.left(), canvas_rect.top() + top_h),
                    canvas_rect.max,
                );
                draw_panel(&painter, top, tex, &self.gameplay, src_w, src_h, "gameplay");
                draw_panel(&painter, bot, tex, &self.facecam, src_w, src_h, "facecam");
                if enabled {
                    pan_zoom(ui, top, "gp", &mut self.gameplay, src_w, src_h);
                    pan_zoom(ui, bot, "fc", &mut self.facecam, src_w, src_h);
                    self.drag_seam(ui, canvas_rect, canvas_h);
                }
                // The Seam line.
                let seam_y = canvas_rect.top() + self.seam * canvas_h;
                painter.line_segment(
                    [egui::pos2(canvas_rect.left(), seam_y), egui::pos2(canvas_rect.right(), seam_y)],
                    Stroke::new(2.0, Color32::from_rgb(255, 209, 0)),
                );
            }
            LayoutKind::FullCam => {
                draw_panel(&painter, canvas_rect, tex, &self.fullcam, src_w, src_h, "cam");
                if enabled {
                    pan_zoom(ui, canvas_rect, "full", &mut self.fullcam, src_w, src_h);
                }
            }
            LayoutKind::FullGameplay => {
                draw_panel(&painter, canvas_rect, tex, &self.fullgameplay, src_w, src_h, "gameplay");
                if enabled {
                    pan_zoom(ui, canvas_rect, "full", &mut self.fullgameplay, src_w, src_h);
                }
            }
        }
        painter.rect_stroke(
            canvas_rect,
            CornerRadius::ZERO,
            Stroke::new(1.0, Color32::from_gray(90)),
            StrokeKind::Inside,
        );

        // Scrub across the preview frames to check the static crop across motion.
        if self.frames.len() > 1 {
            ui.horizontal(|ui| {
                ui.label("Preview frame");
                let last = self.frames.len() - 1;
                ui.add_enabled(
                    enabled,
                    egui::Slider::new(&mut self.frame_idx, 0..=last).show_value(false),
                );
                let t = self.range.duration_s() * self.frame_idx as f64 / last as f64;
                ui.label(format!("+{t:.1}s"));
            });
        }

        ui.horizontal(|ui| {
            ui.add_enabled_ui(enabled, |ui| {
                if ui.button("Render").clicked() {
                    action = EditorAction::Render(self.current_layout());
                }
                if ui.button("Cancel").clicked() {
                    action = EditorAction::Cancel;
                }
                if ui.button("Reset to auto").clicked() {
                    self.reset_to_auto();
                }
            });
        });
        action
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
    painter.text(
        rect.left_top() + egui::vec2(4.0, 2.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(11.0),
        Color32::from_rgba_unmultiplied(255, 255, 255, 160),
    );
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
