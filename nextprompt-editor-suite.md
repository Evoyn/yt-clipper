# Session prompt — the editor suite arc (operator's own plan, queued 2026-07-12)

The operator wrote `feature-implementation-plan.md` (repo root, untracked —
THEIR file, do not reformat it) and said "i want to move to the next new
feature". That plan is the arc: 16 editor features (keyframe transforms,
manual captions, timeline cut-point editing, transitions, thumbnail intro,
music track, volume controls, AI auto-zoom, motion presets, transition
library, clip inspector, font presets, multi-track timeline, B-roll track,
crop tool, speed ramp) plus a title-generation improvement pipeline.

## Why this MUST open with the grill, not code

- The plan describes a general-purpose multi-track NLE (Premiere/CapCut
  shapes). Today's editor is a **nudge editor over one promoted clip**
  (layout + captions + a render pipeline that is ffmpeg-filtergraph + ASS +
  NVENC, ADR 0005/0012). Several items (13 multi-track, 14 B-roll, 16 speed
  ramp) are architecture, not features — the grill must find what the
  operator actually cuts clips with versus what is aspiration.
- Order is undecided: the plan lists 1–16 but the operator's real pain
  ranking is unknown. Candidate tracer bullets the grill should test:
  **#1 keyframe transforms** (touches render math end-to-end, small
  surface), **#2 manual captions** (nearest to existing caption machinery,
  ADR 0036 preview + clip.ass), or the **title-gen pipeline** (pure
  yc-llm-judge prompt work, no UI — but the judge today reads only the
  candidate transcript; the plan wants whole-video context).
- Sharp edges to bring: keyframes/multi-track need a persistence schema
  (project.json is Moments+Clips today); transitions/music/speed touch the
  single-pass ffmpeg render; captions are pipeline-owned (the no-JSON rule
  — manual captions must become a FIRST-CLASS editor artifact, not store
  edits); "does not affect exports" master volume is egui playback only.

## The ritual

/grill-with-docs on the plan WITH the operator present (they authored it —
do not guess their priorities). Outcome of that session: the plan sliced
into tracer-bullet arcs (CONTEXT.md terms sharpened, ADRs for the first
slice pre-registered), a picked first implementation, and fresh
nextprompt-<slug>.md files per the session ritual. One implementation per
session after that. Standing rules hold: quality over runtime, gate on the
burn, PS 5.1 quoting, idle-gate GPU work, captions never corrected via
store JSON.
