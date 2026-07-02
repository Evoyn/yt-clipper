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
/// Public: the app bar and primary-button text sit directly on/of it.
pub const INK: Color32 = Color32::from_rgb(0x14, 0x16, 0x1B);
/// Panels / window fill — one step up from the ink. Public: page-level panel
/// frames pick between SURFACE (chrome/sidebar) and WELL (content bed).
pub const SURFACE: Color32 = Color32::from_rgb(0x1B, 0x1E, 0x26);
/// A faint striped/alternate fill.
const SURFACE_ALT: Color32 = Color32::from_rgb(0x20, 0x24, 0x2E);
/// Resting widget fill (buttons, combos).
const RAISED: Color32 = Color32::from_rgb(0x24, 0x28, 0x33);
/// Hovered widget fill.
const RAISED_HI: Color32 = Color32::from_rgb(0x2E, 0x33, 0x41);
/// Hairline borders / separators.
const HAIRLINE: Color32 = Color32::from_rgb(0x33, 0x39, 0x47);
/// Primary text — just off pure white (pure white is reserved for `strong`).
const TEXT: Color32 = Color32::from_rgb(0xEE, 0xF0, 0xF6);
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
///
/// The midnight studio is the app's identity, not a preference: egui 0.34
/// follows the OS light/dark setting by default and keeps SEPARATE dark/light
/// styles — on a light-mode Windows the resolved theme flipped to Light and
/// rendered the stock light style (our custom style had only landed in the
/// dark slot). So: pin the theme preference to Dark, write our style into
/// BOTH slots (no resolution path can ever reach stock light), and ask the OS
/// for a dark window title bar to match.
pub fn apply(ctx: &egui::Context, font_path: &Path) {
    let has_display = install_display_font(ctx, font_path);

    let mut style = (*ctx.global_style()).clone();
    style.visuals = visuals();

    // Spacing: roomier than the default, so the dense pipeline UI breathes.
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 7.0);
    style.spacing.window_margin = Margin::same(14);
    style.spacing.menu_margin = Margin::same(8);
    style.spacing.indent = 18.0;
    style.spacing.interact_size.y = 28.0;

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

    ctx.set_theme(egui::ThemePreference::Dark);
    let style = std::sync::Arc::new(style);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    // The native title bar follows the app, not the OS (Windows: the
    // immersive dark-mode chrome).
    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
}

/// The dark "midnight studio" [`egui::Visuals`].
fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    let r = CornerRadius::same(8);

    v.dark_mode = true;
    // No global text-color override: each widget's fg_stroke sets its colour, so the
    // brand heading can be gold (the signature) while body stays soft white.
    v.panel_fill = SURFACE;
    v.window_fill = SURFACE;
    v.window_stroke = Stroke::new(1.0, HAIRLINE);
    v.window_corner_radius = CornerRadius::same(12);
    v.window_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };
    v.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.extreme_bg_color = INK; // text-edit / scroll-area background
    v.faint_bg_color = SURFACE_ALT;
    v.code_bg_color = INK;
    v.hyperlink_color = GOLD;
    v.warn_fg_color = GOLD;
    v.error_fg_color = ERR;
    // Secondary text must stay READABLE on the dark surfaces: egui's derived
    // weak colour (text tinted toward the fill) sank too close to the panels
    // (operator: "some of darker text cannot be seen"), so pin an explicit
    // light slate — clearly quieter than body, never grey-on-grey; likewise
    // raise the disabled fade above the default.
    v.weak_text_color = Some(Color32::from_rgb(0xB0, 0xB8, 0xC6));
    v.disabled_alpha = 0.65;
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

    // Hovered: lift the fill, hint the gold on the edge, brighten the label.
    v.widgets.hovered.bg_fill = RAISED_HI;
    v.widgets.hovered.weak_bg_fill = RAISED_HI;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, GOLD.gamma_multiply(0.55));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::from_rgb(0xF4, 0xF6, 0xFB));
    v.widgets.hovered.corner_radius = r;

    // Active: pressed / on — the gold fill moment. fg is PURE WHITE, not ink:
    // egui derives `strong_text_color()` from THIS state's text colour
    // (style.rs), so an ink fg here silently rendered every `strong` label —
    // VOD titles, "Transcript", the Studio toolbar title — near-black on the
    // dark panels (operator's unreadable-text screenshots). White makes
    // strong text pop above the soft-white body; the pressed-gold instant
    // shows white-on-gold, and the primary CTA keeps its own explicit ink
    // label (`primary_button`).
    v.widgets.active.bg_fill = GOLD;
    v.widgets.active.weak_bg_fill = GOLD_DEEP;
    v.widgets.active.bg_stroke = Stroke::new(1.0, GOLD);
    v.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
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

/// A choice chip: a selectable button that ALWAYS draws its resting frame.
/// egui's `selectable_label` hides the frame until hover, so an unselected
/// option looks like plain text and a full-height pill pops in around it on
/// hover — which reads as the row expanding (operator bug report). With the
/// resting frame permanent, hover only re-tints. Size chips with
/// `ui.add_sized([w, chip_h(ui)], chip(..))` so heights always match the
/// theme's button minimum (a smaller size makes the pill overflow the row).
pub fn chip<'a>(selected: bool, text: &'a str) -> egui::Button<'a> {
    egui::Button::selectable(selected, text).frame_when_inactive(true)
}

/// The exact height every chip must be sized to: the theme's minimum
/// interactive height. Anything less and the button paints past the slot.
pub fn chip_h(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y
}

/// One row of equal-width chips spanning the full available width — THE
/// layout primitive for every choice row, so columns line up across sections
/// (operator: hand-computed widths made the rows asymmetric). Chip `i` of `n`
/// occupies the same column in every row of the same container. Returns the
/// clicked index.
///
/// Sized via `Button::min_size`, NOT `add_sized`: the atom-layout button
/// paints its frame at its own `frame_size` (content `.at_least(min_size)`),
/// while `add_sized`'s justified wrapper distorts it — measured: the first
/// chip rendered ~11 px wider than its siblings, none at the computed width.
pub fn chip_row(ui: &mut egui::Ui, options: &[(bool, &str)]) -> Option<usize> {
    let mut clicked = None;
    let n = options.len().max(1) as f32;
    let gap = ui.spacing().item_spacing.x;
    ui.horizontal(|ui| {
        // Explicit gaps, zero automatic spacing: egui slipped a leading
        // item_spacing before the first chip (measured: the whole row sat
        // ~11 px right of the card's other content), so the row owns its
        // geometry outright.
        ui.spacing_mut().item_spacing.x = 0.0;
        let w = ((ui.available_width() - gap * (n - 1.0)) / n).floor();
        for (i, (selected, label)) in options.iter().enumerate() {
            if i > 0 {
                ui.add_space(gap);
            }
            if ui.add(chip(*selected, label).min_size(egui::vec2(w, chip_h(ui)))).clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// A full-width single button/chip row (camera modes, reset actions) — same
/// `min_size` sizing as [`chip_row`] so it shares the grid's outer edges.
pub fn wide_button(ui: &mut egui::Ui, button: egui::Button<'_>) -> egui::Response {
    let w = ui.available_width();
    ui.add(button.min_size(egui::vec2(w, chip_h(ui))))
}

/// A segmented-control row over an enum-ish set: draws `options` as connected
/// selectable segments, returns the clicked index.
pub fn segmented(ui: &mut egui::Ui, selected: usize, options: &[&str]) -> Option<usize> {
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, label) in options.iter().enumerate() {
            if ui.add(chip(selected == i, label)).clicked() {
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
