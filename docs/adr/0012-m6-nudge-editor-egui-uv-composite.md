# M6 nudge editor: live egui UV-composite preview, Prepare/Render split, nudge + layout override

M6 shipped auto-detect framing (ADR 0011) and deferred the manual editor as the
next increment: "drag the facecam rect + a Seam slider over the true-preview."
This ADR records that editor's design. It is the operator's **escape hatch when
auto-framing mis-crops** — and the headline decision **supersedes ADR 0005's
preview consequence** (the framing preview need no longer *be* the real ffmpeg
filtergraph), because for framing *geometry* egui can reproduce the composite
exactly and live.

ADR 0005 chose pure-Rust egui and committed the framing editor's preview to the
real ffmpeg+libass filtergraph at reduced resolution piped to a texture — "exact
WYSIWYG, no simulation gap" — with a cheap egui approximation only during the
live drag. That was the right instinct for a *general* preview, but the nudge
editor edits geometry, and geometry is the one thing egui already composites
exactly.

## Considered options

- **Preview mechanism — egui UV-composite, not the ffmpeg filtergraph
  (supersedes ADR 0005).** A `Crop` is a sub-rectangle of the source; in egui
  each Panel is that sub-rectangle drawn as a **UV window** of one extracted
  source-frame texture into the Panel's on-screen rect. Panning the Crop moves
  the UV window; zooming scales it; the Stacked Seam splits the canvas. This is
  not an *approximation* of the filtergraph's `crop,scale,vstack` — it is the
  same geometry, pixel-exact, at 60 fps with no debounce and **no GPU
  contention** (the 8 GB card's recurring constraint — the detect-hang scar). The
  only things egui's UV-composite cannot reproduce are the libass subtitle
  burn-in and the NVENC/yuv420p color path — **neither matters for placing a
  face**, and captions are M7. Chosen: **egui UV-composite for the editor;
  captions appear on Render** (the real filtergraph, unchanged). Rejected the
  ADR-0005-literal ffmpeg-per-debounce preview: it re-runs ffmpeg on every nudge
  to reproduce a composite that is geometrically identical to the egui one, adds
  frame-decode + debounce plumbing, and re-introduces per-edit ffmpeg work. The
  "no simulation gap" principle is preserved for the thing being edited
  (geometry); the residual gap (subtitles, encoder color) is immaterial to it.

- **The promote flow — split atomic Promote into Prepare + Render.** The editor
  must interject between "we have the Segment and the auto Layout" and "render."
  Today `do_promote` is one atomic worker job (download -> transcribe -> ASS ->
  autoframe -> NVENC -> Done); the Layout is computed deep in the worker and
  consumed immediately, never reaching the UI. Chosen: **two jobs with the worker
  holding a `PreparedClip` between them**, mirroring the existing stateful worker
  pattern (it already holds a `Session` between Import and Detect/Promote).
  `Prepare { range }` downloads the Segment, probes it, auto-detects the **seed**
  Layout, and extracts the preview frames; `Render { layout }` renders the
  operator's (possibly nudged) Layout. Rejected blocking the worker mid-job
  waiting on UI input (couples worker to a UI request/response and muddies
  Cancel). **Whisper is deferred to Render**: it must stay out of the nudge
  window regardless (GPU vs the wgpu loop), and deferring keeps Prepare pure
  CPU/network so the editor opens fast and a reframed-then-abandoned clip is never
  transcribed. Headless preserves today's behavior by **auto-forwarding `Render`
  with the unmodified auto Layout** on `Prepared`.

- **Scope — nudge + layout-type override, not nudge-only.** ADR 0011's deferred
  wording ("drag the facecam rect + a Seam slider") assumes auto picked the right
  Layout. But the worst auto failure is the **wrong three-way pick** (a missed
  Facecam -> FullFrame-gameplay that should be Stacked; a false-positive game face
  -> Stacked with a garbage facecam). Chosen: a **3-way layout selector**
  (Stacked / FullFrame-cam / FullFrame-gameplay) seeded from the auto pick but
  switchable, then drag rect + Seam — bounded to the three existing Layouts (no
  new shapes). Rejected nudge-only: a hatch that cannot switch Layout leaves the
  worst failure unfixable. Switching re-seeds Crops from defaults when there is no
  detection to seed from (a default corner facecam rect; a centered 9:16 column).

- **Interaction — single-canvas, edit-on-output.** Chosen: the 9:16 composite
  *is* the canvas; **drag inside a Panel pans** its Crop, **scroll zooms**
  (aspect-locked, cursor-centered, clamped), the **Seam is a draggable divider**,
  the **layout type a segmented control**. Both Panels editable (the gameplay
  action is not always centered). This matches CONTEXT.md's `Crop` semantics
  ("resizing zooms, dragging pans") and the deferred-increment wording ("drag
  over the true-preview"), and is the least code. Rejected the two-view editor
  (full source frame with draggable overlay rectangles beside a composite): it
  shows out-of-frame context but needs two canvases and fiddlier aspect-locked
  rect dragging; direct-manipulation panning brings new pixels into view as you
  drag, so the lost context costs little.

- **Preview frames — scrub a few, not a single still.** M6's documented
  limitation is that a *static* crop sits high/low on a *moving* face, and this
  editor is its named interim lever. A single composited still would let the
  operator frame the face at one instant with no signal about drift elsewhere —
  false confidence. Chosen: **extract ~5-9 frames evenly across the clip range
  and add a scrub slider** (all frames are textures; UV-recompositing whichever
  is selected is trivial), opening on the frame nearest the detected-face median.
  Rejected the single frame: cheaper, but blind to the very motion the lever
  exists to expose.

- **Lifetime — persist the editor across re-renders within the session.** Chosen:
  the editor opens on Prepare and **stays open after Render** (showing Done ->
  path); adjust + Render again reuses the cached Segment (no re-download) and the
  transcript cached after the first Render (NVENC-only, fast), giving a tight
  nudge->render->nudge loop. Switching Moments rebuilds from a fresh auto-seed.
  Rejected strict one-shot (re-Promote to redo, discarding nudges — clumsy
  iteration). Reopening previously-rendered Clips from a Clip list needs that list
  UI and is deferred to M8 hardening.

## Consequences

- **The editor is not behind the `face` feature.** It is pure egui + extracted
  preview frames; only the auto-*seed* quality depends on `face`/Ultraface. In a
  default build, Prepare seeds FullFrame-gameplay and the operator overrides to
  Stacked and places the cam by hand — so the manual lever works **exactly when
  auto is absent or wrong**, and default builds gain a usable framing editor.
- The worker gains `Job::Prepare`/`Job::Render` (replacing `Job::Promote`), a
  `PreparedClip` held between them (render source, seek offset, source
  dimensions, and — after the first Render — the cached transcript), and a
  `Progress::Prepared { layout, src_w, src_h, frames, range }`. The UI uploads the
  frames to textures and drives the editor.
- Preview-frame extraction (~720p, ~5-9 frames across the clip sub-range
  `[seek_s, seek_s + duration]`) is added in `yc-ingest`, reusing the
  `extract_frames_rgb` pattern (separate from the 320x240 detection pass, which
  stays low-res whole-Segment).
- A new egui editor module in `crates/app`: the UV-composite canvas (one
  `Painter::image` per Panel with a normalized-Crop UV rect), pan/zoom/seam-drag
  interaction in source-pixel space, the layout segmented control, the scrub
  slider, and Render/Cancel. Moving the Seam re-fits both Crops to the new Panel
  aspects (keep centers; `Crop::fit_to_aspect`) — inherent to aspect-locked
  Stacked.
- The auto Layout now flows back to the UI as an editable seed; the rendered
  `Clip.layout` persists the operator's final choice (already supported by
  `persist_clip`).
- Captions are **not** shown in the editor preview (framing-only). An in-editor
  caption preview and a caption-safe-zone guide are future work that pairs with
  M7 Caption Styles.
- Extends ADR 0005 (the pure-Rust UI remains the home of framing preview/edit;
  only the preview *mechanism* changes, and only for geometry), ADR 0001 (video
  still arrives only at promote), and ADR 0011 (the auto-detected Layout is the
  editor's seed).
- CONTEXT.md is unchanged: the glossary already carries `Crop` ("resizing zooms,
  dragging pans"), a per-Clip `Seam`, and the generalized `Layout`/`Facecam`/
  `Panel` — it anticipated this editor.

## Outcome

**Build (2026-06-19):** the editor (`crates/app/src/editor.rs`), the pure editor
geometry (`yc-frame`: `pan_crop` / `zoom_crop` / `reaspect_keep_center` + the
override seeds), and the Prepare/Render worker split (`pipeline.rs`) all compile;
`check -p yt-clipper` is green on **both** default and `--features face`. The
egui 0.34 UV-composite needed exactly two APIs, both confirmed against the
vendored source first: `Painter::image(tex, rect, uv, tint)` and
`ColorImage::from_rgb` (rgb24 straight to texture, no RGBA copy). The editor is
**not** behind the `face` feature, so default builds get the manual lever too.

**Verified (2026-06-19):** `yc-frame` is **12/12** (8 prior + 4 new geometry
tests: pan-clamp, aspect-locked anchored zoom, seam re-fit, override seeds).
Headless end-to-end on the real 1080p Segment `ZSegfmsrYmE` (`--headless`,
`--features face`): the split auto-forwarded `Render` with the auto Layout and
produced a correct **1080x1920** clip via the M1 NVENC path; the persisted Clip
records the Ultraface-detected **Stacked** Layout (facecam `x~1557, y~791` - the
streamer's real bottom-right cam, matching M6's `~1545,789`, seam 0.62). So the
Prepare/Render split **preserves M6 auto-framing exactly**; the editor is purely
additive over it.

**Not yet exercised:** the live drag/zoom/seam/layout-switch/scrub interaction
itself - inherent (needs the GUI + a human; headless cannot drive it), the same
gap M1/M3 noted for their GUI work. It rests on the unit-tested `yc-frame`
geometry and the compiled egui wiring. The operator's interactive pass is the
remaining verification. **Nudge editor done; interactive GUI verification is the
follow-up.**
