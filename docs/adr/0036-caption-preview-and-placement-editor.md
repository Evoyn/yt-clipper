# In-editor caption preview + placement editor: filmstrip playback, shared line model, spatial-only drag

The operator asked for the deferred ADR 0012 item grown into a feature: *see* the
captions (including the error ones) over a video preview inside the editor, *add*
missing captions, and *drag captions to move and resize* them. This ADR records the
design. Decided in an operator-AFK grill: every fork below was resolved to the
option recommended against CONTEXT.md + the ADRs; the operator can veto any of it,
and the slice-2 items are direction, not shipped code.

The headline split: **manual caption control is spatial only** (where the block
sits, how big the text draws — the thing with zero controls today, hard-coded at
`CAPTION_Y_FRAC = 0.46`, centered), while **timing stays fully automated** (the
DTW + onset-clamp + gap-fill machinery of ADRs 0013/0019/0021 is measured against
real failure modes; a manual retime would fight it on every re-render) and **text
fixes stay curation** (slice 2 routes them into the dialect store, not a parallel
edit layer).

## Considered options

- **Preview mechanism — filmstrip playback with egui-drawn captions.** Chosen: the
  editor preloads a dense, low-res frame strip (~480p at a budgeted density, ~120
  frames max — replacing the 5–9 @720p scrub frames entirely: one frame source,
  one timeline), plays the Clip's real audio by slicing the whole-VOD analysis wav
  through the existing rodio path (the Moment-review player), and draws the active
  caption line in egui synced to an `Instant`-driven playhead. Audio and captions —
  the things being QA'd — are exactly timed; the video updates a few frames per
  second and is context. Rejected **smooth streaming decode** (ffmpeg pipe → texture
  ring buffer at full fps): true video playback, but a decode thread + seek +
  buffering infrastructure up front for fidelity the QA task doesn't need — it is
  the natural later upgrade, not v1. Rejected **scrub-only + captions**: cannot
  watch/listen for a wrong word, which is the operator's stated goal. Rejected the
  ADR-0005-literal **real-filtergraph preview** per edit for the same reasons ADR
  0012 did: ffmpeg per nudge, GPU contention, debounce.
- **What the preview draws — the render's own line model, exported.** The grouping
  + line-timing logic (char-budget lines, gap splits, holds, per-word onsets) moves
  into a pure `preview_lines(transcript, genre)` model in `yc-render` that **both**
  the three ASS emitters and the egui overlay consume. ADR 0004's "the ASS
  generator is the single place caption animation logic lives" survives — the
  preview cannot drift from the render because they are the same code. What is
  exact: grouping, timing, position, size, karaoke sung/unsung state. What is
  approximate: glyph shaping/outline (egui's rasterizer with the same bundled TTF,
  not libass) and the pop animation. The burned render stays ground truth — this
  extends ADR 0012's geometry-exact argument from framing to caption *layout*,
  explicitly not caption *look*. Rejected re-implementing grouping UI-side (the
  drift is the bug class this repo keeps paying for — see caption_diag's history).
- **Preview truth — the refined transcript flows to the UI.** `do_render` already
  caches the transcript **post-correction, post-refine** (exactly what `generate_ass`
  consumes); a new `Progress::Captions { transcript }` ships it to the editor right
  after refine — before NVENC even finishes — so the preview shows the render's
  actual units, drops included. Captions appear in the editor after the first
  Render of a Clip (whisper stays out of Prepare, ADR 0012); a Transcribe-without-
  render job is slice 2, where it pairs with text editing.
- **Manual control — per-Clip placement + scale, timing untouchable.** Chosen: drag
  the caption block anywhere on the canvas (x snaps to center), scroll to scale,
  persisted as `Clip.caption_placement: Option<CaptionPlacement { x_frac, y_frac,
  scale }>` (serde-defaulted — old project.json loads; absent means today's
  constants, so headless/batch output stays byte-identical, the ADR 0033 spirit).
  Presentation data on the Clip, never the dialect store (a placement says nothing
  about words). Rejected temporal drag (fights the measured refine pass). Rejected
  per-word placement (ASS could, via per-event `\pos`, but the genres share one
  anchor and no failure mode demands it). A per-Creator placement *default* (ADR
  0016 pattern) is deferred until the operator actually repeats a placement.
- **Slice 2 direction (decided, not built): editing text IS curating.** Fixing a
  wrong word in the editor writes a `wrong → right` correction, `at_s`-pinned (ADR
  0034 — `at_s` is part of a correction's identity), into the **per-clip** store,
  riding the existing auto-promote/clip_only machinery (ADR 0031) — the review
  queue and the editor stay one system with one truth source. **Adding a missing
  caption is a new insertion entry kind: inherently clip-scoped and never
  auto-promoted**, because its `at_s` is VOD-absolute — "insert this word at
  12:03" is meaningless in any other VOD. Harvested garbles get highlighted in the
  preview so the error captions are visible in context. Rejected a per-clip
  transcript-override file: it forks the truth, bypasses curation, and an engine
  switch (ADR 0033) silently invalidates it.

## Consequences

- `yc-core` gains `CaptionPlacement`; `Clip` gains the optional field. `Job::Render`
  and `EditorAction::Render` carry it; headless/batch pass `None`.
- `yc-render` exports the preview line model; the ASS emitters are refactored over
  it (existing emitter tests pin the behavior through the refactor). `generate_ass`
  takes the optional placement; `None` reproduces today's output exactly.
- `do_prepare` extracts the dense strip instead of the 7-frame scrub (same
  `extract_frames_rgb`, new params); `Progress::Prepared` carries the strip fps.
  ~120 frames @480p ≈ ≤200 MB of textures — sized for the 8 GB card with whisper
  loaded; drop resolution first if it ever contends. Facecam crops preview slightly
  softer than at 720p; geometry (the thing being edited) is unaffected.
- The editor stays open through renders (ADR 0012 lifetime); the caption overlay
  has an eye-toggle so panel pan/zoom under the caption stays reachable.
- The app registers the bundled caption TTF into egui's fonts for the overlay.
- CONTEXT.md gains **Caption placement**.

## Outcome

**Build (2026-07-02, Fable 5):** the whole slice landed in one session —
`CaptionPlacement` (yc-core), the shared `preview_lines` model + placement-aware
`generate_ass` (yc-render), the filmstrip Prepare + `Progress::Captions` +
placement threading (pipeline), and the editor's playback + caption overlay +
drag/resize. Fast suite **200 green** (11 core + 36 ingest + 18 detect + 16
frame + 33 render + 70 transcribe + 16 app), including the new golden guard
(`None == Some(default)` byte-for-byte, all three genres) and the
preview-matches-Dialogues consistency tests. Release build green on
`--features correct,face` (CUDA).

**Verified on the production path:** headless render of the Deddy control
fixture (`nyYuwbQPzxY` 1800–1860, whisper default path, no placement) ran
end-to-end through the new strip extraction and produced `clip-30-00 (7).mp4`;
its `data/clip.ass` carries the unchanged anchor `\pos(540,883)` and Style line
(`Anton,150,...`) — the pre-editor output, byte-identical where it matters.

**Review pass (8-angle, same session):** confirmed findings all fixed — the
geometry/clamp sharing got real teeth (`resolve_placement` is now the overlay's
actual source, scale bounds are shared `CaptionPlacement::SCALE_MIN/MAX`
consts), the per-genre word reveal/sung semantics moved into
`yc_render::word_states` (tested — never re-derived UI-side), the overlay's
galleys are cached (shaping ran twice per frame at the 30 fps tick), scrubbing
while playing no longer restarts the audio sink per drag-frame, the caption
resize consumes its wheel delta, and headless/batch Prepare skips the
filmstrip entirely (`Job::Prepare.preview`) instead of decoding 120 frames to
drop them. Known and accepted: a Clip's persisted placement is not yet
rehydrated on re-promote (exact parity with Layout nudges — ADR 0012's
deferred clip-reopen slice owns both).

**Not yet exercised:** the live drag/resize/playback interaction itself —
inherent (needs the GUI + a human), the same gap ADR 0012 recorded for the
nudge editor's interactions. The egui hover z-order assumption (caption
interact registered after panel pan/zoom wins the pointer) is the specific
thing the operator's interactive pass should confirm, along with audio↔caption
sync feel. Slice 2 (text fixes as curation + insertions + harvest highlighting
+ a Transcribe-without-render job) is queued.

## Calibration (2026-07-03): ASS Fontsize ≠ egui font size

The operator caught exported captions rendering **much smaller** than the
preview. Root cause: the overlay converted ASS `Fontsize` to egui points by
the canvas factor alone, but the two rasterizers disagree about what the
number means — **libass sizes a face by its OS/2 win cell** (usWinAscent +
usWinDescent, the VSFilter-compat FreeType `REAL_DIM` request), **egui 0.34
(skrifa) by the em square**. For Anton (upem 2048, win cell 3550) the same
nominal size draws 1.73× bigger in egui. Measured on the production
ffmpeg+libass with a one-glyph ASS burn: Fontsize 150 → 75 px cap height,
matching the win-cell prediction (74.4) and ruling out the hhea-span (85.6)
and em (128.9) mappings; linear at 96 → 48.

Per this ADR's own contract the burn-in is ground truth, so the **preview**
was corrected: the overlay multiplies by `ASS_TO_EGUI_FONT = upem / win cell`
(editor.rs, unit-tested against the shipped TTF's actual tables so a font
swap fails loudly). Placement scale composes on top unchanged — after the
factor, the operator's size lever is the existing scroll-to-resize. Known
residual approximation: libass and egui still center slightly different line
boxes on the anchor (win-cell vs hhea line heights), a few PlayRes pixels of
vertical offset at caption sizes — spatial-drag territory, not a size bug.
