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
//!
//! Since ADR 0036 the editor also carries the **caption preview + placement
//! editor**: the frame scrub grew into a filmstrip playhead (with the Clip's
//! real audio, played by the app on request), and once a Render has shipped its
//! refined transcript (`Progress::Captions`) the active caption line draws over
//! the composite — same grouping/timing as the burn-in (`yc_render`'s shared
//! line model), egui-rasterized (approximate glyphs, exact layout). Dragging the
//! caption moves it, scrolling over it resizes — the per-Clip Caption placement.

use std::time::Instant;

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind};
use yc_core::{
    CaptionGenre, CaptionPlacement, Crop, Layout, TimeRange, Transcript, CANVAS_H, CANVAS_W,
};
use yc_render::{preview_lines, resolve_placement, word_states, PreviewLine, WordState};

use crate::pipeline::caption_style;
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

/// What `show` reports back to the app each frame.
pub enum EditorAction {
    /// Nothing to do this frame.
    None,
    /// The operator hit Render: composite this (nudged) Layout with this
    /// per-Clip Caption Style genre (M7 — the editor overrides the global pick)
    /// and this Caption placement (ADR 0036; `None` = never dragged = the
    /// built-in anchor).
    Render(Layout, CaptionGenre, Option<CaptionPlacement>),
    /// The operator dismissed the editor without rendering.
    Cancel,
    /// Start clip-audio playback over this absolute VOD range (Play pressed, or
    /// a scrub while playing). The app owns the audio sink (ADR 0036).
    Play(TimeRange),
    /// Stop clip audio: Pause pressed, playback reached the clip end, or a
    /// render started (playback pauses so the repaint throttle that protects
    /// whisper from the wgpu loop stays in force).
    StopAudio,
}

/// The editable framing state. All four Crops stay resident so switching Layout
/// kind never discards a nudge: e.g. toggling Stacked -> Full gameplay -> Stacked
/// preserves the facecam the operator placed.
pub struct EditorState {
    src_w: f32,
    src_h: f32,
    range: TimeRange,
    /// The preview filmstrip (ADR 0036): frames sampled at `frame_fps` across
    /// the clip range, as textures. Always non-empty (Prepare fails otherwise);
    /// the playhead shows the nearest frame.
    frames: Vec<egui::TextureHandle>,
    frame_fps: f64,
    /// Clip-relative playhead (seconds) the filmstrip + caption preview draw at.
    playhead_s: f64,
    /// `Some((anchor, offset))` while playing: playhead = offset + since(anchor).
    /// Wall-clock-driven — the app's audio sink runs alongside; drift over a
    /// clip-length span is inaudible.
    playing: Option<(Instant, f64)>,
    /// The auto-detected seed, kept for "Reset to auto".
    auto_layout: Layout,
    kind: LayoutKind,
    seam: f32,
    gameplay: Crop,
    facecam: Crop,
    fullcam: Crop,
    fullgameplay: Crop,
    /// Per-Clip Caption Style genre (M7). Seeded from the app's current selection
    /// (which is the Creator-remembered default, ADR 0016) and overridable here.
    caption_genre: CaptionGenre,
    /// The refined transcript the last Render burned (`Progress::Captions`) —
    /// the render's truth. `None` until the first Render of this Clip.
    transcript: Option<Transcript>,
    /// `transcript` regrouped via the render's own line model
    /// (`yc_render::preview_lines`) for `lines_genre` — lazily recomputed by
    /// the overlay whenever the genre differs, so no mutation path can leave
    /// it stale.
    lines: Vec<PreviewLine>,
    lines_genre: CaptionGenre,
    /// The shaped caption galleys (text + shadow) for the overlay, keyed by
    /// what they depend on — glyph shaping is the expensive part of egui text
    /// and would otherwise run twice per frame at the 30 fps playback tick.
    overlay_cache: Option<(OverlayKey, std::sync::Arc<egui::Galley>, std::sync::Arc<egui::Galley>)>,
    /// Caption placement (ADR 0036): `None` until the operator drags/resizes —
    /// the built-in anchor, and the Clip record stays unstamped. "Reset
    /// placement" returns here.
    placement: Option<CaptionPlacement>,
    /// Eye toggle: hide the overlay to reach panel pan/zoom underneath it.
    show_captions: bool,
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
        frame_fps: f64,
        caption_genre: CaptionGenre,
    ) -> Self {
        let (kind, seam, gameplay, facecam, fullcam, fullgameplay) =
            seed_fields(&auto_layout, src_w, src_h);
        Self {
            src_w,
            src_h,
            range,
            frames,
            frame_fps: frame_fps.max(0.1),
            playhead_s: range.duration_s() * 0.5, // a representative middle frame
            playing: None,
            auto_layout,
            kind,
            seam,
            gameplay,
            facecam,
            fullcam,
            fullgameplay,
            caption_genre,
            transcript: None,
            lines: Vec::new(),
            lines_genre: caption_genre,
            overlay_cache: None,
            placement: None,
            show_captions: true,
        }
    }

    /// Receive the refined transcript a Render is burning (`Progress::Captions`,
    /// ADR 0036) — from now on the overlay previews exactly those units.
    pub fn set_captions(&mut self, transcript: Transcript) {
        self.lines = preview_lines(&transcript, self.caption_genre);
        self.lines_genre = self.caption_genre;
        self.overlay_cache = None;
        self.transcript = Some(transcript);
    }

    /// Keep `lines` in step with the current genre (grouping is per-genre).
    /// Lazy: called by the overlay each frame, so *any* path that changes the
    /// genre is covered without manual invalidation.
    fn sync_lines(&mut self) {
        if self.lines_genre != self.caption_genre {
            if let Some(t) = &self.transcript {
                self.lines = preview_lines(t, self.caption_genre);
            }
            self.lines_genre = self.caption_genre;
            self.overlay_cache = None;
        }
    }

    /// The absolute VOD range audio playback covers when started at a
    /// clip-relative offset: `offset` into the Clip, through its end.
    fn play_range_from(&self, offset_s: f64) -> TimeRange {
        TimeRange { start_s: self.range.start_s + offset_s, end_s: self.range.end_s }
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

        // Advance the playhead while playing. A render in flight (`!enabled`)
        // pauses playback: the 10 fps repaint throttle that protects whisper
        // from the wgpu loop must stay in force (the detect-hang scar).
        let dur = self.range.duration_s();
        if let Some((anchor, offset)) = self.playing {
            if !enabled {
                self.playing = None;
                action = EditorAction::StopAudio;
            } else {
                self.playhead_s = offset + anchor.elapsed().as_secs_f64();
                if self.playhead_s >= dur {
                    self.playhead_s = dur;
                    self.playing = None;
                    action = EditorAction::StopAudio;
                } else {
                    // ~30 fps visual tick; the filmstrip itself is coarser.
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
                }
            }
        }

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

        // Per-Clip Caption Style override (M7): the animation genre for this Clip,
        // seeded from the app's (Creator-remembered) pick and overridable here.
        ui.horizontal(|ui| {
            ui.label("Caption:");
            ui.add_enabled_ui(enabled, |ui| {
                for (genre, label) in [
                    (CaptionGenre::HugeWord, "Huge word"),
                    (CaptionGenre::RollingPop, "Rolling pop"),
                    (CaptionGenre::KaraokeFill, "Karaoke fill"),
                ] {
                    if ui.selectable_label(self.caption_genre == genre, label).clicked() {
                        self.caption_genre = genre; // overlay re-syncs lazily
                    }
                }
            });
        });

        // The composite canvas: a 9:16 rectangle, panels drawn as UV sub-rects.
        let canvas_w = ui.available_width().min(360.0).max(160.0);
        let canvas_h = canvas_w * CANVAS_H as f32 / CANVAS_W as f32;
        let (canvas_rect, _) =
            ui.allocate_exact_size(egui::vec2(canvas_w, canvas_h), Sense::hover());
        // The filmstrip frame nearest the playhead (frame i sits at i/fps).
        let frame_idx = ((self.playhead_s * self.frame_fps).round() as usize)
            .min(self.frames.len().saturating_sub(1));
        let tex = self.frames[frame_idx].id();
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

        // The caption preview + placement overlay (ADR 0036), registered after
        // the panel interactions so it wins the pointer where they overlap.
        if self.show_captions {
            self.caption_overlay(ui, canvas_rect, enabled);
        }

        // Playback: the timeline drives the filmstrip + caption preview; the
        // Clip's real audio is the app's sink, started/stopped via the action.
        ui.horizontal(|ui| {
            let label = if self.playing.is_some() { "Pause" } else { "Play" };
            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                if self.playing.is_some() {
                    self.playing = None;
                    action = EditorAction::StopAudio;
                } else {
                    if self.playhead_s >= dur {
                        self.playhead_s = 0.0; // replay from the top
                    }
                    self.playing = Some((Instant::now(), self.playhead_s));
                    action = EditorAction::Play(self.play_range_from(self.playhead_s));
                }
            }
            let resp = ui.add_enabled(
                enabled,
                egui::Slider::new(&mut self.playhead_s, 0.0..=dur).show_value(false),
            );
            if self.playing.is_some() {
                if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                    // Scrub settled (drag released / click-jump / keyboard):
                    // restart the audio at the new offset.
                    self.playing = Some((Instant::now(), self.playhead_s));
                    action = EditorAction::Play(self.play_range_from(self.playhead_s));
                } else if resp.changed() {
                    // Mid-drag: track the playhead visually only — restarting
                    // the sink every drag frame is a re-seek storm; the audio
                    // catches up on release.
                    self.playing = Some((Instant::now(), self.playhead_s));
                }
            }
            ui.label(format!("{:.1}s / {dur:.1}s", self.playhead_s));
        });

        // Caption overlay controls: nothing to preview before the first Render
        // ships its transcript; after it, the eye toggle + placement readout.
        ui.horizontal(|ui| {
            ui.label("Captions:");
            if self.transcript.is_none() {
                ui.weak("appear here after the first Render");
            } else {
                ui.add_enabled_ui(enabled, |ui| {
                    let eye = if self.show_captions { "Shown" } else { "Hidden" };
                    if ui.selectable_label(self.show_captions, eye).clicked() {
                        self.show_captions = !self.show_captions;
                    }
                    match self.placement {
                        Some(p) => {
                            ui.label(format!(
                                "x {:.0}%  y {:.0}%  size {:.0}%",
                                p.x_frac * 100.0,
                                p.y_frac * 100.0,
                                p.scale * 100.0
                            ));
                            if ui.button("Reset placement").clicked() {
                                self.placement = None;
                            }
                        }
                        None => {
                            ui.weak("drag caption to move, scroll on it to resize");
                        }
                    }
                });
            }
        });

        ui.horizontal(|ui| {
            ui.add_enabled_ui(enabled, |ui| {
                if ui.button("Render").clicked() {
                    action = EditorAction::Render(
                        self.current_layout(),
                        self.caption_genre,
                        self.placement,
                    );
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

        let style = caption_style(self.caption_genre);
        let place = self.placement.unwrap_or_default();
        // The SAME resolution the burn-in uses (anchor + font size in ASS
        // PlayRes pixels, clamps included — ADR 0036's geometry sharing), then
        // one factor converts PlayRes pixels to canvas points. The canvas is
        // aspect-true 9:16, so the x and y factors are identical.
        let (ass_x, ass_y, ass_font) = resolve_placement(self.placement, style.font_size);
        let px = canvas_rect.width() / CANVAS_W as f32;
        let font_px = (ass_font as f32 * px).max(4.0);
        let font_id = FontId::new(font_px, theme::display_family());
        let mul = if ghost { 0.35 } else { 1.0 };
        let tint = |c: [u8; 4]| {
            Color32::from_rgba_unmultiplied(c[0], c[1], c[2], (c[3] as f32 * mul) as u8)
        };
        let primary = tint(style.primary_color);
        let accent = tint(style.accent_color);
        let shadow_c = Color32::from_rgba_unmultiplied(0, 0, 0, (200.0 * mul) as u8);

        // Per-word visibility/colour comes from the render crate's own
        // word_states — the genre's ASS-tag semantics at this playhead (ADR
        // 0036: never re-derived UI-side). A ghost line shows every word in
        // the base colour (reveal/sung state would be meaningless in a gap).
        let states = word_states(self.caption_genre, line, p);

        // Shaping is the expensive part of egui text; rebuild the galleys only
        // when something they depend on changes (word states flip a few times
        // a second during playback — far below the 30 fps repaint tick).
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
                // One pass builds both jobs, so the shadow can never fall out
                // of lockstep with the glyphs it backs.
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
                    let sc = if color == Color32::TRANSPARENT { color } else { shadow_c };
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
        // Faux outline: four offset shadow passes stand in for libass's
        // outline+shadow (ADR 0036's fidelity boundary: look approximate,
        // position/size/timing exact).
        let o = (2.0 * px * place.scale).clamp(1.0, 4.0);
        for d in [
            egui::vec2(-o, 0.0),
            egui::vec2(o, 0.0),
            egui::vec2(0.0, -o),
            egui::vec2(0.0, o),
        ] {
            painter.galley(top_left + d, shadow.clone(), shadow_c);
        }
        painter.galley(top_left, galley, primary);

        if !enabled {
            return;
        }
        // Drag to move, scroll on it to resize — the Crop verbs, applied to the
        // caption block. Registered after the panel interactions, so the caption
        // wins the pointer where they overlap (hide it via the eye toggle to
        // reach the Panel underneath).
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
            // Snap X to the canvas centre (the burn-in's default) within 2%,
            // with a guide line while snapped.
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
                // Scroll up grows the text (direct manipulation), clamped to
                // the shared CaptionPlacement bounds — the same constants
                // resolve_placement clamps with, so preview == burn-in.
                let factor = (scroll * 0.0015).exp();
                self.placement = Some(CaptionPlacement {
                    scale: (place.scale * factor)
                        .clamp(CaptionPlacement::SCALE_MIN, CaptionPlacement::SCALE_MAX),
                    ..place
                });
                // Consume the wheel so the same gesture can't also scroll the
                // editor window / zoom a Panel underneath.
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

/// Everything the overlay's shaped galleys depend on (ADR 0036): the active
/// line's identity, its per-word states at the playhead, ghost dimming, and the
/// resolved font size. Equal key ⇒ the cached galleys are still exact.
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
