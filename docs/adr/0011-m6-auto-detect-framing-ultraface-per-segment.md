# M6 auto-detect framing: Ultraface face detection per Segment, three-way auto-layout, no manual editor first

ROADMAP M6 was a manual framing editor (ADR 0005: draggable Crops, Seam,
debounced true-preview). The operator reframed it (2026-06-19) to
**automation-first** — the LLM judges which Moments are clip-worthy (ADR 0010),
so vision-ML should decide their *framing* too, not a drag editor. M6 becomes
**auto-detect the Facecam and choose the Layout**, with the manual nudge as a
later increment. The hardcoded `pipeline.rs::layout_for` scaffolding (a fixed
Stacked Layout with the facecam pinned bottom-right 20%×30%) is what this
replaces; `Crop`/`Layout`/`Panel`/`Seam` already exist in `core` and CONTEXT.md.

This is feasible because the `ort` runtime is already in the tree (M4 arousal,
ADR 0008) — a tiny face model is the same `Session` pattern with an image tensor
instead of audio.

## Considered options

- **When/where the frames come from.** Import is audio-only (ADR 0001: never
  download the whole VOD video); video only arrives when a Moment is promoted
  (the padded Segment). Chosen: **detect at Promote, on the Segment we already
  download** — sample frames within it, decide the Layout, render. Rejected
  sampling low-res keyframes across the whole VOD at import: it adds a video
  fetch the audio-only import deliberately avoids, *and* it is wrong in principle
  here — the Facecam is **not static across a VOD**. A streamer runs a small
  corner-cam during gameplay but may switch to a full-screen cam during a
  talking session, so whole-VOD sampling would blend two different cam layouts
  into one confused cluster. The Segment captures the cam *as it is during that
  specific Moment*, which is exactly what the Clip should frame. Framing is a
  Clip concern (CONTEXT.md: a Clip gets framing, a Moment gets nothing), and
  Promote is where video first exists — so framing belongs there.
- **Per-VOD vs per-Creator detection.** The reframe said "detect once per
  Creator (fixed webcam)," but Creators are not persisted yet (no
  `creators.json`, no picker; `project.json` is keyed by `video_id`,
  `Vod.creator` is a bare string). Chosen: **detect per Segment (per Clip), no
  cache** — re-running detection on each promote. Two reasons: (1) the Facecam
  moves between gameplay and talking segments, so a crop cached from one Clip
  would be wrong for another of the same VOD; (2) it keeps M6 on the vision-ML
  core and out of a Creator-management feature. Detection is cheap enough to
  repeat (Ultraface on a few dozen frames is tens of milliseconds, negligible
  beside the whisper + NVENC already in promote). Per-Creator caching lifts
  cleanly later, once `creators.json` + a picker exist.
- **Face model — Ultraface RFB-320 (not YuNet).** The ROADMAP tentatively named
  YuNet but flagged "not OpenCV": YuNet's ONNX is normally decoded inside
  `cv::FaceDetectorYN`, and with no OpenCV in the tree its raw decode (3 outputs,
  prior/stride reconstruction + NMS) must be reimplemented by hand. Chosen:
  **Ultraface RFB-320** (~1 MB, WIDER FACE), whose raw output is the simplest of
  any candidate — two arrays, boxes (normalized LTRB) + scores → confidence
  threshold → NMS, no anchor/stride reconstruction — with direct Rust+`ort`
  precedents (`olalium/face-prediction-rs`, `sgasse/infercam_onnx`). Our task is
  a **large, clear, static webcam face**, not tiny crowd faces, so YuNet's
  accuracy edge on small/occluded faces and its 5 landmarks buy nothing here.
  Rejected the `rust-faces` crate (batteries-included decode) because it risks
  pulling its own onnxruntime version conflicting with our pinned
  `ort 2.0.0-rc.12`. Model-agnostic in spirit (the detector sits behind a
  `faces-in-frame → boxes` seam); this is the bundled default.
- **Auto-layout — three-way by face size/position.** Because the cam can be a
  corner inset *or* a full-screen talking cam, "cam found → Stacked, else
  full-frame" is too coarse. Chosen: the detected face's size and position imply
  the moment type. A **small, off-center, persistent** face (corner-cam) →
  **Stacked** (gameplay Panel + facecam Panel). A **large, roughly-centered**
  face dominating the frame (talking session) → **FullFrame on the face** (the
  streamer is the content; no gameplay Panel). **No persistent face** →
  **FullFrame gameplay**. Ambiguous-middle defaults to Stacked (the safer, more
  recoverable framing). This is why CONTEXT.md's `Layout` glossary was
  generalized: FullFrame is no longer gameplay-only.
- **Manual adjust — deferred past M6.** ADR 0005's full drag editor + debounced
  true-preview is exactly what the reframe replaces. Chosen: **auto-only in M6**,
  verified headless (the way the whole project is verified — `--headless`,
  `--detect` — sidestepping the 8 GB card's GUI/VRAM contention). The "optional
  adjust" (drag the facecam rect + a Seam slider over the true-preview) is the
  immediate next increment, kept out of the tracer bullet so detection is proven
  cleanly first.

## Consequences

- A new **`yc-frame`** crate, split like `yc-detect`'s arousal: the **pure
  geometry** — `FaceBox` (source pixels), `cluster_static_face` (persistence +
  center-variance clustering), `decide_layout` (the three-way decision +
  box→`Crop` expansion) — is always compiled and unit-tested with no `ort` dep;
  the **Ultraface inference** lives behind a `face` cargo feature
  (`dep:ort`), so default builds and tests need neither the ONNX Runtime binary
  nor the model. One crate per concern (ingest / transcribe / detect / render /
  llm-judge / frame).
- In `do_promote`, after the Segment is fetched and probed for `src_w/src_h`:
  when built `--features face` with the model present, extract ~1–2 fps of frames
  from the Segment (ffmpeg `scale=320:240`, rgb24), run Ultraface per frame,
  cluster the static Facecam, and `decide_layout` builds the `Layout` — replacing
  `layout_for`. When the feature or model is absent, or detection finds no
  persistent face, the Layout falls back to **FullFrame gameplay** (a safe,
  never-misframed default — unlike the old hardcoded Stacked, which assumed a
  bottom-right cam).
- The detection grid stretches the 16:9 Segment to the model's 320×240 (4:3) and
  unmaps boxes with independent x/y scales back to source pixels. Acceptable: the
  webcam face is large and clear, robust to mild aspect distortion; revisit with
  letterboxing if recall ever disappoints.
- The thresholds (sampling fps + cap, detection confidence, cluster persistence +
  variance, the size/position cutoffs, the box-expansion factors) are **tunable
  constants** seeded with defaults, like the detection weights and the caption
  timing knobs — retunable without re-architecture.
- `Layout::FullFrame { gameplay: Crop }`'s field is renamed to `crop` (it can now
  show the Facecam), and the CONTEXT.md `Layout` term is generalized; a
  **Facecam** term is added (the source region showing the webcam overlay,
  detected per Clip).
- The Ultraface ONNX (~1 MB) is fetched from a credible source (the
  Linzaer/ONNX-zoo release), like the other models.
- Extends ADR 0005 (the pure-Rust UI; framing preview/edit still its home, just
  auto-seeded now), ADR 0001 (audio-first; video only at promote), and ADR 0008
  (the `ort` runtime and the pure/neural split pattern).

## Outcome

**Build (2026-06-19):** the `yc-frame` Ultraface inference compiled against
`ort 2.0.0-rc.12` with **no signature fixups** (unlike the SER caveat predicted),
and the app builds both default and `--features face`. The model is
`models/version-RFB-320.onnx` (1.27 MB, Linzaer release); it loads with input
`input` and outputs `scores`/`boxes` exactly as assumed — the dynamic name
discovery matched, and the normalized-box decode (threshold + NMS) needed no
per-export decode. `ort` emits harmless "initializer appears in graph inputs"
warnings on load (the model keeps weights as graph inputs); detection is
unaffected.

**Verified (2026-06-19):** two real 1080p gaming Segments (Windah Basudara)
auto-framed end-to-end headless — Ultraface found the static Facecam at
**persistence 0.95 and 1.0**, the three-way decision chose **Stacked**, and the
facecam Crop tracked the streamer's actual bottom-right cam (≈`x 1545, y 789`),
aspect-fit to the panel. Both rendered correct 1080×1920 clips (gameplay Panel
above the streamer's face) via the M1 NVENC path. The full-cam (talking) and
no-face fallback branches are unit-tested in `decide_layout` (and the `FullFrame`
render path in `yc-render`); Stacked is the e2e-verified branch. A static crop on
a moving face can sit high/low frame-to-frame (the wide-short facecam panel shows
a horizontal band) — the deferred nudge editor and the future dynamic-framing
work (ROADMAP) address fine-tuning. Also fixed a pre-existing latent bug: a
relative local-VOD path broke the export (which runs ffmpeg in the clip folder);
`import_local` now absolutizes it. **M6 (auto-framing) done; the nudge editor is
the next increment.**
