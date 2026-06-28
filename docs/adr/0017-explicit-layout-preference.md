# Explicit Layout preference: a global operator override of M6 auto-detect

The operator reported their preferred **stacked** framing (gameplay Panel on
top, Facecam Panel below) "stopped appearing." This is not a regression bug — it
is a **deliberate default change in M6 (ADR 0011)**, surfacing now that the
operator's main workflow is the headless `--batch` render.

M1 hard-coded a stacked Layout (facecam pinned bottom-right). M6 (ADR 0011)
replaced that with auto-detect, whose **uncertain / no-face fallback is
`FullFrame` gameplay** — chosen explicitly as "a safe default that never
misplaces a facecam, unlike the old hardcoded Stacked." So the operator now gets
full-frame gameplay whenever the corner cam is not *confidently* classified as a
small off-center face:

- a `--batch` / `--headless` binary built **without `--features face`** → the
  detector never runs, so `build_layout` always returns the no-face fallback
  (full-frame gameplay), every clip;
- a **detection miss** (cluster persistence < `MIN_PERSISTENCE` 0.5, or every
  box below `MIN_CONF` 0.7);
- a **large / roughly-centered** cam tripping the talking-session branch
  (`FULLCAM_FACE_W_FRAC` 0.28) → `FullFrame` on the face, not Stacked.

And the nudge editor's existing three-way Layout picker (ADR 0012) cannot help
the operator's real workflow, because **`--batch` never opens the editor** — it
renders each Moment straight off the auto-detected Layout.

## Decision

Add a **global Layout preference** the operator sets explicitly; it overrides
(or, for `Auto`, defers to) the M6 auto-detect.

- A new `core::LayoutPref` enum — `Auto | Stacked | FullCam | FullGameplay`,
  default `Auto` — mirroring `CaptionGenre`'s shape (a small `Copy` choice the UI
  and the CLI both set).
- It travels on `Job::Prepare { range, title, layout_pref }`, so every promote
  path (GUI single-promote, GUI batch, headless, `--batch`) carries it to the
  worker.
- `build_layout` is restructured to **always run face detection first** (cheap —
  Ultraface on a few dozen frames is tens of ms) to obtain the optional Facecam
  cluster, **then apply the preference** through a new *pure* function
  `yc_frame::decide_layout_with_pref(pref, face, src_w, src_h, seam)`:
  - `Auto` → the unchanged ADR 0011 three-way `decide_layout` (no regression);
  - `Stacked` → always a stacked Layout, using the **detected** Facecam Crop when
    a face was found, else the bottom-right `default_facecam_crop` seed (so forced
    Stacked still works with no `face` feature and no detected cam);
  - `FullCam` → `FullFrame` centered on the detected face, else a centered 9:16
    column;
  - `FullGameplay` → `FullFrame` gameplay.
- Surfaced as a **GUI top-bar combo** (next to the Caption-style picker) and an
  optional **CLI token** on `--batch` / `--headless` (`auto` | `stacked` | `cam`
  | `gameplay`). The nudge editor is unchanged: it seeds from the prepared
  Layout, which now already reflects the preference, and the operator can still
  nudge or flip kinds from there.

## Considered options

- **Editor-only picker (just default it to Stacked / make it prominent).**
  Rejected: the editor never opens in `--batch`, the operator's primary path, so
  it cannot set the layout for a batch render. The picker stays (per-Clip nudge),
  but the *selectable default* must live above it.
- **Re-tune the auto-detect fallback back to Stacked.** Rejected: it throws away
  M6's deliberate "no-face → safe full-frame" default and the talking-session
  full-cam branch, and it still is not *selectable* — the operator explicitly
  asked to pick the layout, not to swap one fixed guess for another. If a real
  clip shows the operator's corner cam mis-classified (e.g. a wide cam ≥ 28% of
  frame width read as full-cam), that is a separate threshold tune, justified
  only against the failing clip — not a blanket fallback change.
- **Default the global preference to Stacked.** Rejected: forcing Stacked
  globally mis-frames genuine talking-session clips (where FullCam is correct),
  undoing M6's value. `Auto` default + a prominent one-click menu balances both;
  the operator reaches Stacked in a single selection.
- **Persist the preference (per-Creator, like the Caption Style — ADR 0016).**
  Deferred. ADR 0016 already reserves per-Creator seam/crop defaults for a later
  "Creator-aware framing" slice; a persisted "always Stacked for this Creator"
  belongs there. This ADR is the tracer-bullet: make the layout *selectable*
  everywhere first. Today the preference is in-memory (GUI session) or per-invocation
  (CLI arg).

## Consequences

- `LayoutPref` lives in `core` (both `yc-frame` and the app see it). The forcing
  logic is **pure** and unit-tested in `yc-frame` with no `ort` dependency, like
  the rest of `decide_layout`; the stacked-construction is factored into one
  helper that both `decide_layout` (Auto) and the forced path reuse (DRY).
- Detection now runs on **every** promote (it previously short-circuited when no
  `face` feature / model), because a forced `Stacked`/`FullCam` still wants the
  detected Facecam position when one is available. Cost is negligible beside the
  whisper + NVENC already in render; with no `face` feature the detector is a
  no-op returning `None` and the forced seeds apply.
- The CLI grows one optional positional: `--batch <vod> [lang] [genre] [k]
  [layout]` and `--headless <file> <start> <end> [lang] [genre] [layout]`.
- Extends ADR 0011 (the auto-detect three-way; `Auto` is exactly it) and ADR 0012
  (the nudge editor; unchanged, seeds from the now-preference-aware Layout).
  CONTEXT.md gains a **Layout preference** term and the **Layout** term notes the
  explicit override.

## Outcome

**Built + verified (2026-06-28).** `yc-frame` `decide_layout_with_pref` is pure
and unit-tested (4 new tests: `Auto` is byte-for-byte `decide_layout`; forced
Stacked overrides even a large centered face; forced Stacked with no face uses
the bottom-right seed; forced FullCam/FullGameplay force full-frame). The app
builds clean both `--features face` (25 s) and default (the `#[cfg(not(face))]`
`detect_facecam` no-op). All 100 fast tests green (frame 12 → 16).

**Live (a real 1920×1080 Ino Gemink horror Segment, range 6–24 s, headless):**
- `stacked` → the operator's preferred framing: gameplay (the horror room) on
  top, Ino's facecam below, Seam ≈ 0.62. Confirmed by eyeballing a burned frame.
- `auto` → on this clip auto-detect **correctly** found the corner cam
  (`cam=true persistence=1.0`) and chose Stacked, identical to the forced result.
  So the "stacked is gone" report is **not** a general auto-detect break — it is
  the no-`face`-feature batch build / a detection-miss / full-cam-misclassify
  cases, exactly the cases the explicit preference now covers deterministically.
- `gameplay` → a single full-frame panel (no facecam), visually distinct from the
  other two — proving the preference actually drives the rendered Layout.

The menu/CLI gives the operator selectable framing everywhere, including
`--batch` (which never opens the nudge editor). Per-Creator persistence of the
preference remains the next increment (ADR 0016's reserved Creator-framing slice).
