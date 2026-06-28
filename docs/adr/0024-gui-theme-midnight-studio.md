# GUI theme: "midnight studio" — a branded dark palette, not default egui

The app wore egui's default visuals — generic gray, neutral, the "AI-slop" look
the operator flagged. The fix is a deliberate theme set once at startup, following
the frontend-design principles: a *named* palette tied to the subject, one bold
accent used with restraint, a signature element, and disciplined spacing/type so
everything around the accent stays quiet.

## Decision

A dark **"midnight studio"** theme in a new `theme.rs`, applied in the eframe
creation hook (`theme::apply(&cc.egui_ctx, &paths.font())`).

- **Palette** (named, 4–6 values tied to a gaming-VOD → Shorts creator tool): a
  cool near-black `INK` and one-step `SURFACE`/`RAISED` slates, a `HAIRLINE`
  border, soft-white `TEXT` (never pure white), and the **caption brand gold**
  `#FFD100` as the single accent — the same gold the captions burn.
- **One bold accent, with restraint.** Gold appears only where it means something:
  the brand heading, the hover edge, the pressed/active fill (dark text on gold),
  and the selection highlight. Everything else is quiet slate.
- **Signature element.** The `yt-clipper` heading is painted in gold **in the
  caption display font (Anton)**, installed under a `display` family — so the UI
  wears the same identity as the Shorts it makes. "Spend your boldness in one
  place"; the rest stays disciplined.
- **Type scale + spacing.** A clear hierarchy (heading 30 / body 15 / small 12),
  roomier `item_spacing`/`button_padding`/margins so the dense pipeline UI
  breathes, and consistent 6 px widget corners.
- **Copy.** Active, specific (the heading + a one-line tagline "Turn long gaming
  VODs into vertical Shorts - on your machine."; the old dev label
  "M3 detection" removed), preflight `ok`/`MISSING` softened off pure green/red.

## Considered options / variations

- **Midnight slate + caption gold (chosen).** Ties the theme to the product's own
  output (the gold captions) and to the creator-tool context; the gold heading in
  the caption font is a free, meaningful signature.
- **A lighter "studio paper" or a neutral mid-gray dark.** Rejected — generic, and
  a light theme fights the video-tool context (dark UIs sit better beside footage
  and reduce glare during long edits).
- **A second display font for the body.** Deferred — Anton (display) for the
  heading paired with egui's neutral, highly-legible proportional body is a sound
  pairing; bundling a second body face is extra weight for little gain on a dense
  tool UI.
- **`override_text_color` for uniform text.** Tried, rejected — it flattens the
  heading to the body colour, killing the signature. Colour is set per-widget via
  `fg_stroke` instead, and the gold heading is painted explicitly; `ui.weak()`
  carries the secondary hints.

## Consequences

- New `crates/app/src/theme.rs` (`apply`, the `visuals()` palette, best-effort
  display-font install). `main.rs` calls it once and paints the gold brand
  heading; `theme::{GOLD, OK, ERR}` are shared with the UI. No behavioural change —
  purely visual, and structure/layout are untouched (the theme is the foundation;
  a deeper per-panel redesign can build on it later).
- Font install is best-effort: a missing Anton falls back to the proportional
  heading (still gold), so the theme never fails to load.

## Outcome

**Shipped + verified (2026-06-28).** Builds clean (`--features face`), no warnings.
Launched the real GUI and **screenshotted the window** (OS capture via window
enumeration): the heading renders in **gold Anton** (zoom-confirmed — the condensed
caption letterforms), the tagline dims below it, the body is readable soft-white on
the slate, the combos carry subtle hairline borders, and `ok` is a soft green — a
cohesive, branded dark look, distinctly not the default-egui gray. The structure
and every existing interaction are unchanged.
