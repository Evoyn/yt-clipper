# Session prompt — the editor suite arc (operator's own plan, queued 2026-07-12; slices 1+2 SHIPPED same day)

The operator wrote `feature-implementation-plan.md` (repo root, untracked —
THEIR file, do not reformat it) and said "i want to move to the next new
feature". That plan is the arc: 16 editor features plus a title-generation
improvement pipeline.

## Arc state (2026-07-12, after ADR 0064 + 0065 — read both first)

**DONE and through the operator's build** (slice 1) **or awaiting their eye**
(slice 2): #2's timeline half (caption blocks = units, rail for burn lines,
drag/trim/insert/delete, end-time field), #3 in BOTH senses (the timeline
razor — segments removable from the export with cut audio + remapped
captions — and draggable camera cuts), #7 (master volume, persisted), #9
(motion presets via CameraPlan glides), #12 (saved caption presets in
`workspace/settings.json`), plus per-shot manual framing in Active Speaker
(drag the picture to reframe the shot) — the operator's "my own camera
frame" ask. Vocabulary rule from the verdict: **Camera cut** (framing) vs
**Clip cut** (razor); never "split".

**REMAINING — all render-arc, none egui-only** (the reason they weren't
one-shotted): #4/#10 transitions (fade/dip/dissolve — xfade+afade at razor
joins and clip edges), #5 thumbnail intro (image segment + concat), #6
music track (amix + a music lane on the timeline), #1 keyframe transforms +
#8 auto-zoom (beyond what motion presets already cover — true keyframes
need animated filter expressions; rotation needs new machinery), #11
inspector extras (speed/opacity/per-clip volume — setpts/atempo), #13/#14
multi-track + B-roll (architecture: overlay compositing, project.json
schema), #16 speed ramp. #15 crop presets: refused as designed — the canvas
is fixed 9:16 (Shorts tool); the panel crops are the crop tool.

## Sharp edges for the next slice (verified in code)

- The razor render path (`build_camera_filtergraph(_, _, cut_audio=true)`)
  is the natural HOME for join transitions (#4): each kept piece is already
  its own trimmed v+a pair — a dip-to-black is a fade-out/in per piece
  boundary. Start there, not from scratch.
- Music (#6) needs a second input (`-i music`) + `amix` — the export
  invocation today is single-input; `export_args_inner` grows a second
  input path + the graph gains an audio mix stage. The timeline needs a
  music lane (the lane-height math in `ui_timeline` reserves chrome rows —
  extend `n_lanes`).
- True keyframes (#1): the per-shot `pan_to` glide is position+size at shot
  granularity; arbitrary-time keyframes mean generalizing `Shot` or a new
  keyframe track on the Clip — a schema decision (project.json) the
  operator should rule on BEFORE code.
- Standing rules hold: quality over runtime, gate on the burn (a razored
  export's A/V/caption sync at the joins is the FIRST eye test), PS 5.1
  quoting, `git commit -F`, captions never corrected via store JSON.

## The ritual

One implementation per session. Open with /grill-with-docs UNLESS the
operator says "automatic" (they did for slices 1+2). Finish: handoff +
whatwedone.md + a fresh nextprompt, ending with the starter line. The
operator's queued NEXT session is `nextprompt-title-gen.md` (non-egui, the
plan's title pipeline) — this file is the arc's continuation AFTER that.
