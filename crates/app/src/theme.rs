//! The app's visual theme (ADR 0024): a "midnight studio" dark palette with the
//! caption brand gold as the one bold accent, applied once at startup.
//!
//! Design intent (frontend-design principles): a *named* palette tied to the
//! subject (a gaming-VOD → Shorts creator tool) instead of default egui gray; the
//! caption gold `#FFD100` as the single restrained accent ("spend boldness in one
//! place"); the caption display face (Anton) for headings, so the UI wears the
//! same identity as the Shorts it makes; and disciplined spacing + a clear type
//! scale so everything around the accent stays quiet.

use std::path::Path;
use std::sync::Arc;

use egui::{Color32, CornerRadius, FontId, Margin, Stroke};

// ---- the palette (4-6 values, named) ---------------------------------------

/// Deepest background — a cool near-black, not pure black, not generic gray.
const INK: Color32 = Color32::from_rgb(0x14, 0x16, 0x1B);
/// Panels / window fill — one step up from the ink.
const SURFACE: Color32 = Color32::from_rgb(0x1B, 0x1E, 0x26);
/// A faint striped/alternate fill.
const SURFACE_ALT: Color32 = Color32::from_rgb(0x20, 0x24, 0x2E);
/// Resting widget fill (buttons, combos).
const RAISED: Color32 = Color32::from_rgb(0x24, 0x28, 0x33);
/// Hovered widget fill.
const RAISED_HI: Color32 = Color32::from_rgb(0x2E, 0x33, 0x41);
/// Hairline borders / separators.
const HAIRLINE: Color32 = Color32::from_rgb(0x33, 0x39, 0x47);
/// Primary text — a soft white, never pure white.
const TEXT: Color32 = Color32::from_rgb(0xE6, 0xE9, 0xF2);
/// The one bold accent — the caption brand gold. Public so the UI can paint the
/// brand heading with it (the signature element).
pub const GOLD: Color32 = Color32::from_rgb(0xFF, 0xD1, 0x00);
/// Pressed-accent / trailing fill — a deeper gold.
const GOLD_DEEP: Color32 = Color32::from_rgb(0xC9, 0xA4, 0x00);
/// Success (preflight ok) — softened from pure green.
pub const OK: Color32 = Color32::from_rgb(0x5B, 0xD1, 0x7A);
/// Error / missing — softened from pure red.
pub const ERR: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x6B);
/// Informational accent (links to nowhere, speaker B, chat overlays) — a calm
/// sky blue that never competes with the gold.
pub const INFO: Color32 = Color32::from_rgb(0x6C, 0xB2, 0xFF);
/// Deep panel background one step *below* SURFACE — the editor's preview well
/// and timeline bed, so the video reads as the brightest thing on screen.
pub const WELL: Color32 = Color32::from_rgb(0x0F, 0x11, 0x15);

/// Speaker-track colours (Person A, B, C, D) for face overlays and the speaker
/// timeline: distinct at a glance, all softened to sit on the dark surface.
pub const TRACKS: [Color32; 4] = [
    GOLD,
    Color32::from_rgb(0x6C, 0xB2, 0xFF), // B: sky
    Color32::from_rgb(0x5B, 0xD1, 0x7A), // C: green
    Color32::from_rgb(0xE8, 0x8B, 0xD0), // D: orchid
];

/// The colour for speaker track `i` (wraps past [`TRACKS`]).
pub fn track_color(i: usize) -> Color32 {
    TRACKS[i % TRACKS.len()]
}

/// The heading font family name, installed from the caption font when present.
const HEADING_FAMILY: &str = "display";

/// The display/caption egui font family. Always a valid family (when the caption
/// TTF is absent it is aliased to the proportional stack), so callers — headings,
/// the editor's caption overlay (ADR 0036) — can use it unconditionally.
pub fn display_family() -> egui::FontFamily {
    egui::FontFamily::Name(HEADING_FAMILY.into())
}

/// Apply the theme to `ctx` once at startup: install the display font (best
/// effort), then set the palette, spacing, and type scale.
pub fn apply(ctx: &egui::Context, font_path: &Path) {
    let has_display = install_display_font(ctx, font_path);

    let mut style = (*ctx.global_style()).clone();
    style.visuals = visuals();

    // Spacing: roomier than the default, so the dense pipeline UI breathes.
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.window_margin = Margin::same(14);
    style.spacing.menu_margin = Margin::same(8);
    style.spacing.indent = 18.0;
    style.spacing.interact_size.y = 26.0;

    // Type scale: a clear hierarchy (heading >> body > small), body a touch larger
    // than egui's default for comfortable reading.
    use egui::{FontFamily, TextStyle};
    let heading_family = if has_display {
        FontFamily::Name(HEADING_FAMILY.into())
    } else {
        FontFamily::Proportional
    };
    style.text_styles.insert(TextStyle::Heading, FontId::new(30.0, heading_family));
    style.text_styles.insert(TextStyle::Body, FontId::new(15.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Button, FontId::new(15.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Small, FontId::new(12.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace));

    ctx.set_global_style(style);
}

/// The dark "midnight studio" [`egui::Visuals`].
fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    let r = CornerRadius::same(6);

    v.dark_mode = true;
    // No global text-color override: each widget's fg_stroke sets its colour, so the
    // brand heading can be gold (the signature) while body stays soft white.
    v.panel_fill = SURFACE;
    v.window_fill = SURFACE;
    v.window_stroke = Stroke::new(1.0, HAIRLINE);
    v.window_corner_radius = CornerRadius::same(10);
    v.extreme_bg_color = INK; // text-edit / scroll-area background
    v.faint_bg_color = SURFACE_ALT;
    v.code_bg_color = INK;
    v.hyperlink_color = GOLD;
    v.warn_fg_color = GOLD;
    v.error_fg_color = ERR;
    // Accent the selection with a translucent gold + a gold edge.
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(0xFF, 0xD1, 0x00, 48);
    v.selection.stroke = Stroke::new(1.0, GOLD);
    v.slider_trailing_fill = true;

    // Noninteractive: labels, separators, the panel chrome. Body text is soft
    // white and readable; genuinely-secondary text uses `ui.weak()` (a dimmed
    // derivative) for hierarchy, not a blanket dim.
    v.widgets.noninteractive.bg_fill = SURFACE;
    v.widgets.noninteractive.weak_bg_fill = SURFACE;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.corner_radius = r;

    // Inactive: buttons / combos at rest.
    v.widgets.inactive.bg_fill = RAISED;
    v.widgets.inactive.weak_bg_fill = RAISED;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.corner_radius = r;

    // Hovered: lift the fill, hint the gold on the edge.
    v.widgets.hovered.bg_fill = RAISED_HI;
    v.widgets.hovered.weak_bg_fill = RAISED_HI;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, GOLD.gamma_multiply(0.55));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.corner_radius = r;

    // Active: pressed / on — the gold moment, with dark text on it.
    v.widgets.active.bg_fill = GOLD;
    v.widgets.active.weak_bg_fill = GOLD_DEEP;
    v.widgets.active.bg_stroke = Stroke::new(1.0, GOLD);
    v.widgets.active.fg_stroke = Stroke::new(1.5, INK);
    v.widgets.active.corner_radius = r;

    // Open: an expanded combo / menu.
    v.widgets.open.bg_fill = RAISED;
    v.widgets.open.weak_bg_fill = RAISED;
    v.widgets.open.bg_stroke = Stroke::new(1.0, HAIRLINE);
    v.widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.open.corner_radius = r;

    v
}

// ---- widget helpers (one design system, used by every page) -----------------

/// The primary call-to-action: dark text on the brand gold. One per view — the
/// button the operator is *supposed* to press next.
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let label = egui::RichText::new(text).color(INK).strong();
    ui.add(egui::Button::new(label).fill(GOLD).corner_radius(CornerRadius::same(6)))
}

/// A section header: small-caps-feel kicker in gold over the quiet surface —
/// the one place outside the brand mark the gold text appears.
pub fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(egui::RichText::new(text.to_uppercase()).color(GOLD).size(11.5).strong());
    ui.add_space(2.0);
}

/// A raised card frame for grouping a form section on a panel.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE_ALT)
        .stroke(Stroke::new(1.0, HAIRLINE))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
}

/// A small status chip: coloured dot + label, for pipeline states.
pub fn status_chip(ui: &mut egui::Ui, color: Color32, text: &str) {
    egui::Frame::new()
        .fill(RAISED)
        .stroke(Stroke::new(1.0, HAIRLINE))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, color);
                ui.label(egui::RichText::new(text).size(12.5));
            });
        });
}

/// A segmented-control row over an enum-ish set: draws `options` as connected
/// selectable segments, returns the clicked index.
pub fn segmented(ui: &mut egui::Ui, selected: usize, options: &[&str]) -> Option<usize> {
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, label) in options.iter().enumerate() {
            if ui.selectable_label(selected == i, *label).clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// Install the caption display font (Anton) under [`HEADING_FAMILY`] so headings
/// (and the editor's caption overlay, ADR 0036) wear the same face as the burned
/// captions. Best-effort: returns `false` if the font file is absent — the family
/// is then aliased to the proportional stack, so [`display_family`] still
/// resolves and callers never need the fallback logic themselves.
fn install_display_font(ctx: &egui::Context, font_path: &Path) -> bool {
    let mut fonts = egui::FontDefinitions::default();
    let has_display = match std::fs::read(font_path) {
        Ok(bytes) => {
            fonts
                .font_data
                .insert(HEADING_FAMILY.to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
            fonts
                .families
                .entry(egui::FontFamily::Name(HEADING_FAMILY.into()))
                .or_default()
                .insert(0, HEADING_FAMILY.to_owned());
            true
        }
        Err(_) => {
            let proportional = fonts
                .families
                .get(&egui::FontFamily::Proportional)
                .cloned()
                .unwrap_or_default();
            fonts.families.insert(egui::FontFamily::Name(HEADING_FAMILY.into()), proportional);
            false
        }
    };
    ctx.set_fonts(fonts);
    has_display
}
