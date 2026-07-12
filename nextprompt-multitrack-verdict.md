# Session prompt — multi-track shell: the operator's verdict round (plan #13 slice 2, queued 2026-07-13)

Slice 1 of plan #13 shipped (ADR 0066, handoff
`handoffs/2026-07-13-multitrack-timeline-shell.md` — READ BOTH FIRST; the
ADR holds the track model + viewport decisions and the list of things
deliberately NOT built). The timeline now has the Premiere/CapCut shell:
header column (painted eye/lock per track), zoom about the pointer
(ctrl+wheel/pinch, `- + Fit` corner buttons), wheel scroll + scrollbar
row, sub-second ruler, viewport-culled lanes. 115 tests green, clippy
clean, `face,align,ser` release build compiled — but **no human has seen
it**. This session is the verdict round (the ADR 0065 pattern: ship →
operator drives → findings become the next slice).

## What to do

1. Confirm the release build is current (`scripts\build-release.bat`,
   FOREGROUND — background shells get reaped) and have the operator drive
   a real podcast clip in the Studio. The open feel gates, from ADR 0066's
   validation section:
   - zoom about the pointer (does the time under the cursor stay put?),
     wheel-scroll direction, pinch if they use a trackpad;
   - drag-under-zoom: caption block moves/trims and razor drags at 10×+,
     including scroll/zoom MID-DRAG (should keep tracking — drags live in
     time);
   - header cards (round-2 boxed style: card = exactly the track's band,
     accent edge dims with the eye) — legibility, icon read, block tints;
   - the eye's honesty ON THE BURN: eye-off the auto track, export, play
     the mp4 — the auto stream must be absent, the manual stream intact
     (and vice versa). Gate on the burn, not the preview;
   - lock feel (blocks refuse drags, panel still edits, `+ Caption`
     greys out for a locked yours-track);
   - scrollbar drag, playhead follow while playing, `Fit` recovery.
2. Every finding gets recorded with a timestamp + what the eye saw
   (publish-bar discipline), then fixed in THIS session if it's shell
   polish, or pre-registered as the next slice if it's new machinery.
3. Amend ADR 0066 with the verdict (the 0065 amendment pattern).

## Sharp edges to watch (the code's own suspicions)

- Wheel sign: wheel-up scrolls EARLIER (left), matching ScrollArea — if
  the operator expects CapCut's opposite, it's a one-line flip in the
  `scroll_px` call (editor.rs, viewport-input block).
- The zoom corner buttons zoom about the view CENTER (only ctrl+wheel
  zooms about the pointer) — deliberate; verify it doesn't feel wrong.
- Caption lanes grew 15→22 px and blocks to 18 px (round-2 boxed style);
  chrome is 125 incl. the 8 px scrollbar row. Check nothing feels
  cramped or fat at the minimum panel height.
- Eye-off blocks dim to 0.45 — enough contrast against the veils?
- `MIN_SPAN_S = 1.0` (max zoom = 1 s across the strip). If the operator
  wants frame-level zoom, that constant is the lever; keep points-based
  snapping honest at that scale.
- If the operator asks to drag a caption PAST the window edge while
  zoomed: autoscroll-on-drag is deliberately unbuilt (wheel works
  mid-drag) — their call whether it becomes the next slice.

## Standing rules

AI and operator artifacts NEVER share a track (their ruling — no silent
cross-lane drags, ever). Tracks arrive WITH their render arcs (no dead
Music/Images/B-roll UI; mute waits for the mixer arc). No manual entries
in dialect stores. Quality over runtime. PS 5.1 quoting (`"--features"
"face,align,ser"`); commit via `git commit -F <file>`; push to main is
authorized. Validate: cargo test -p yt-clipper -p yc-core -p yc-render,
clippy on touched files, feature check, release build FOREGROUND + the
operator's eye. Finish: handoff + whatwedone.md + fresh nextprompt + the
starter line. (`nextprompt-title-gen.md` stays queued behind this arc.)
