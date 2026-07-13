# Session prompt — undo/redo: the operator's feel gate (ADR 0069; queued 2026-07-13)

Undo/redo shipped (ADR 0069, handoff
`handoffs/2026-07-13-editor-undo.md` — READ BOTH FIRST; the ADR holds
the document/derived split, the coalescing rules, what is deliberately
NOT undone, and the accepted trade-offs; the handoff ends with the
sharp edges this session exists to check). 63 app tests green (9 new
pinning the pre-registered bars), clippy clean, `face,align,ser` check,
release build compiled — but **no human hand has pressed Ctrl+Z**. This
is the feel gate (ADR 0069's last bar): Ctrl+Z after each of their real
edits must do what CapCut taught their hands.

## What to do

1. Confirm the release build is current (`scripts\build-release.bat`,
   FOREGROUND — background shells get reaped) and have the operator
   drive a real clip, undoing after EACH kind of edit they actually
   make:
   - drag a caption block, Ctrl+Z — the whole drag reverts as ONE step
     (never a few pixels of it);
   - type in a caption field, click out, Ctrl+Z — the whole sentence
     reverts as one step (while the field still has FOCUS, Ctrl+Z is
     egui's own in-field undo — check that split reads as sane);
   - razor: ✂⏴/⏵✂, remove a segment, drag a cut, then Ctrl+Z each;
   - music: move/trim/split/volume, then **delete a clip and Ctrl+Z —
     it must PLAY again** (the PCM re-decode; a long mp3 pauses ~a
     second at undo time, same as at pick);
   - thumbnail: remove it, Ctrl+Z — the image block returns WITH its
     picture (production-only re-decode path);
   - crops/seam in both views, style knobs (one slider ride = one
     step), a preset pick, track eyes/locks/mute;
   - Ctrl+Shift+Z and Ctrl+Y both redo; a new edit after undo kills
     redo (the ↻ tooltip says so).
2. **The ↺/↻ glyphs — the tofu rule** (ADR 0065 Am. 5): nobody has SEEN
   U+21BA/U+21BB in this font stack. If they tofu, paint arrows (the
   `draw_eye`/`draw_lock` pattern in `theme.rs`) in THIS session.
3. Negative space (the "never undoes a park" bar): scroll/zoom the
   timeline, move the playhead, change selection, flip
   Preview/Original — Ctrl+Z after each must revert the last EDIT, not
   the view. Nothing may teleport their vantage point.
4. Feel gates while they drive: undo DURING playback (voice + music
   re-cue together — listen for double-starts); undo with a drag still
   held (the gesture lands first); selections clearing on undo (a
   finding if their CapCut hands expect survival); rapid
   Ctrl+Z-Z-Z-Z walking cleanly; the DragValue first-flick wart (ADR
   trade-off — a finding only if their hands notice); undo past the
   speaker-analysis arrival (the plan un-arrives, redo returns it — a
   finding if it reads as haunted).
5. Every finding: timestamp + what the hand/eye caught; fix in THIS
   session if it's polish on the shipped machinery (glyphs, tooltips,
   a missed verb site), pre-register as the next slice if it's new
   machinery (e.g. selection-preserving restore, arrival-crossing
   gates). Amend ADR 0069 with the verdict (the 0068 amendment
   pattern).
6. Consumed verdict → mark this file CONSUMED (the
   `nextprompt-music-verdict.md` pattern) and the queue moves to
   **`nextprompt-title-gen.md`** (re-queued behind this arc by the
   operator's two jumps).

## Sharp edges to hand the operator's hands (from the handoff)

- A missed verb site is the memento's one failure mode that MATTERS:
  if they find an edit Ctrl+Z does not revert, that is a P1 finding —
  name the widget, add the push, add the family test.
- The music-PCM heal stall on a long mp3 at Ctrl+Z time (a finding
  only if it reads as a hang — a status line is the likely fix).
- Two quick scroll-resize bursts under ~1 s merge into one step (no
  press boundary exists for scroll — accepted trade-off, recorded).
- 100-deep in a real session: edit past 100 steps and confirm the
  oldest quietly drop (no panic, no weirdness at the floor).

## Standing rules

Quality over runtime. AI and operator artifacts never share a track.
No manual entries in dialect stores. PS 5.1 quoting (`"--features"
"face,align,ser"`); commit via `git commit -F <file>`; push to main is
authorized. Validate: cargo test -p yt-clipper -p yc-core -p
yc-render, clippy on touched files, feature check, release build
FOREGROUND + the operator's hands on the feel gate. Finish: handoff +
whatwedone.md + fresh nextprompt + the starter line.
(`nextprompt-title-gen.md` stays queued behind this verdict.)
