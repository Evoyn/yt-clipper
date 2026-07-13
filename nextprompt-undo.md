# Session prompt — editor undo/redo (operator pick 2026-07-13; jumps title-gen again)

The operator closed the music burn gate ("okay it work", ADR 0068
amendment) and picked UNDO as the next arc, in their own words: **"next we
will work on undo since it doesnt have it right now, do we need to put max
number for the undo to save resources?"** Note: undo is NOT in
`feature-implementation-plan.md` — this is their own ask, queued ahead of
title-gen (again). Their question was answered in-session (2026-07-13) and
the answer is pre-agreed: **yes, cap it — a ring buffer of 100 steps.**
Snapshots hold only the DOCUMENT (captions, music clips, razor, camera
plan, framing, style — a few KB to ~100 KB, dominated by caption text);
the heavy things (decoded music PCM ~40 MB/song, filmstrip textures, the
intro texture) are derived caches keyed by path and are NEVER cloned —
they heal after restore. 100 steps ≈ ≤10 MB worst case; the cap exists so
a thousand-edit session can never creep, not because snapshots are big
(Premiere ships 32, Photoshop ~50). Redo clears on a new edit.

## What EXISTS today (verified in code 2026-07-13 — do not re-guess)

- **The document surface** (`EditorState`, editor.rs): `transcript` +
  `transcript_dirty`, `manual_units` + `manual_places` (1:1, maintained
  by shared helpers), `razor: RazorState { cuts, removed }`, `cut_marker`,
  `plan: Option<CameraPlan>`, the framing set (`kind`, `seam`, `gameplay`,
  `facecam`, `fullcam`, `fullgameplay`), `style` + `preset`, `placement`,
  `camera_mode`, `motion`, `intro: Option<IntroState{path,duration_s,tex}>`
  (tex = derived), `music: Vec<MusicClip>` + `trk_music`, the track flags
  (`trk_auto`, `trk_manual`, `speakers_eye`). NOT document: `viewport`,
  `playhead_s`/`playing`/`live`, selections (`sel_unit`/`sel_manual`/
  `sel_music`), `frames`, `music_pcm`, `lines`/`overlay_cache` (derived),
  `speakers`/`faces`/`voice_note` (worker artifacts).
- **Gesture commit boundaries**: every timeline gesture is a
  `TimelineDrag` applied per-frame by `apply_timeline_drag` and landed by
  `finish_timeline_drag` — so a snapshot belongs at drag START (the
  None→Some transition of `self.drag`), one step per gesture, never per
  frame. Assignment sites: the consolidated `begin_drag` in `ui_timeline`
  (captions, rails, music blocks/edges), `begin_cut`, `begin_razor`, the
  marker, and the intro edge — a tiny `begin_drag(&mut self, d)` helper
  that snapshots-then-sets centralizes all of them.
- **Canvas drags mutate per-frame WITHOUT `TimelineDrag`**: `pan_zoom`
  (crops), `drag_seam`, the AS-shot reframe in `draw_output`, the caption
  overlay drag/scroll-resize, manual-caption placement drags. Their
  `Response`s expose `drag_started()` — the snapshot hook. Same coalescing
  problem in the panels: `TextEdit`/`DragValue` fire `changed()` per
  keystroke/tick; snapshot on `gained_focus()` / `drag_started()` so one
  editing session = one undo step.
- **Verb sites** (each = one step, snapshot before mutating): `+ Caption`,
  `+ Thumbnail`/replace/remove, `+ Music` (`add_music`), the routed
  scissors (`razor.cut_left/right` vs `trim_music_*`), ⏷ Mark, Delete
  (routed), strip/block menus (razor cut/toggle/delete, camera cut/delete,
  caption delete, music split/gain/delete), panel row verbs
  (delete/split/merge/censor), preset picks + style knobs, motion preset,
  camera-mode switch, reset-to-auto, track toggles, placement right-click
  reset. The music gain slider coalesces like a DragValue.
- **Restore invariants** (what a naive restore breaks): set `drag = None`,
  `lines_dirty = true`, `overlay_cache = None`, `panel_sort_pending =
  false`; clear/clamp the three selections; `refresh_camera_audit()`;
  `transcript_dirty` IS document state (rides the snapshot). Derived
  heals: `IntroState.tex = None` is already safe to DRAW (`draw_intro_frame`
  guards it) but should re-decode via the `set_intro_image` path;
  `music_cues` already SKIPS clips whose PCM is missing (handoff
  watch-out) — after undoing a music delete the PCM may be evicted
  (`remove_music` evicts on last reference): re-probe+decode lazily or at
  restore (the `add_music` path exists to reuse).
- **Keys**: `handle_keys` runs only when NO widget has focus (egui's own
  text-field undo stays intact inside fields) — Ctrl+Z / Ctrl+Shift+Z /
  Ctrl+Y land there. `set_captions`/`set_speakers` (worker arrivals) are
  not operator ops — decide explicitly whether they push (recommend: no;
  set_captions already refuses over a dirty transcript).

## The architecture the ask implies (decide honestly)

1. **Snapshot/memento, not command-inverses**: one `Snapshot` struct =
   clones of the document fields above; `fn snapshot(&self)` +
   `fn restore(&mut self, s)` on EditorState (no giant field-move
   refactor); `undo: VecDeque<Snapshot>` capped at **100** (drop-front),
   `redo: Vec<Snapshot>` cleared on every new push. Undo = push current
   to redo, pop+restore; redo mirrors. Command-pattern inverses across
   ~40 scattered mutation sites is the rejected alternative (miss one
   site and undo corrupts silently).
2. **One choke point per family**: `push_undo()` called from the
   `begin_drag` helper, the canvas `drag_started()`s, the
   focus-gain/drag-start of panel widgets, and each verb — with a
   coalescing guard so a no-op (drag started but never moved, focus
   gained but never typed) doesn't stack a duplicate (compare-with-top or
   drop-on-equal; snapshots are cheap to compare by the few hot fields —
   or just accept benign duplicates and let equality-with-top skip them).
3. **UI**: Ctrl+Z / Ctrl+Shift+Z (+ Ctrl+Y) in `handle_keys`; ↶/↷
   transport buttons (painted or text — tofu rule, ADR 0065 Am. 5)
   with tooltips + disabled states showing depth; operator eye decides
   the look.
4. **Pre-register ADR 0069** with: the document/derived split, the 100
   cap + the resource numbers, the coalescing rules, what is NOT undone
   (view state, selections, worker arrivals), and the rejected
   alternatives (command inverses; persistent structural sharing — im/Arc
   — overkill at these sizes; unbounded history).

## Bars (pre-register in ADR 0069 BEFORE building)

- Every mutating family verified: one gesture = one step (a 60-frame
  drag lands ONE snapshot); one text-edit session = one step; every verb
  = one step. A representative test per family (razor verb, caption
  drag, panel edit, music verb, style knob, intro trim).
- Undo→redo round-trips byte-equal on the document fields (serde or
  PartialEq pins); restore resets the derived/selection invariants
  (drag None, lines_dirty, audit refreshed).
- The cap: pushing 150 steps holds 100, oldest dropped, no panic;
  a new edit after undo clears redo.
- Music-delete undo: the clip returns AND plays again (PCM heal), spec
  carries it — the handoff watch-out closed.
- Byte-identity: with zero undo activity, render_spec and every export
  path are untouched (undo is editor-session state, never persisted —
  the ADR 0066 decision-6 scope).
- The feel gate (operator): Ctrl+Z after each of their real edits does
  what CapCut taught their hands; nothing "undoes" a scroll/zoom/park.

## The ritual

/grill-with-docs UNLESS the operator says "automatic" — then decide by
this file, pre-register ADR 0069, build, keep the ADR 0066 shell grammar
+ ADR 0067 output-time contract + ADR 0068 music grammar intact. Release
builds FOREGROUND. Validate: cargo test -p yt-clipper -p yc-core -p
yc-render, clippy on touched files, `"--features" "face,align,ser"`
check, `scripts\build-release.bat`, the operator's hands on the feel
gate. Standing rules: quality over runtime, PS 5.1 quoting, commit via
`git commit -F <file>`, captions never corrected via store JSON, AI and
operator artifacts never share a track. Finish: handoff + whatwedone.md
+ fresh nextprompt + the starter line.
(`nextprompt-title-gen.md` stays queued behind this arc.)
