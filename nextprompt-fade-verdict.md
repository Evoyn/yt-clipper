# Session prompt — edge fades: the operator's burn gate (ADR 0070; queued 2026-07-13)

Edge fades shipped (ADR 0070, handoff
`handoffs/2026-07-13-edge-fades.md` — READ BOTH FIRST; the ADR holds
the scope cuts, the wrap order, the `-t` trap, the preview-parity
decision, and the accepted trade-offs; the handoff ends with the sharp
edges this session exists to check). 21+67+66 tests green (9 new
pinning the pre-registered bars), clippy clean, `"--features"
"face,align,ser"` check, release build compiled (exe 2026-07-13
15:41) — but **no faded export has met a human eye or ear**. This is
the burn gate (ADR 0070's last bar), and the standing rule applies:
gate on the burn, not the label — the preview agreeing with the math
is NOT the operator's eye on the burned clip.

## What to do

1. Confirm the release build is current (`scripts\build-release.bat`,
   FOREGROUND — background shells get reaped) and have the operator
   set a fade (the Fade section, right panel below Framing) and BURN
   real exports — their eye and ear on each:
   - **the plain case**: fade in 0.5 s + fade out 1.5 s, black — the
     video dims exactly as the preview showed; the voice AND the music
     die TOGETHER; no hard cut at the very end (the `-t` tail was the
     named risk);
   - **a razor-cut clip**: remove segments, fade out — the burn fades
     over the final seconds of what REMAINS (the two-clocks trap; the
     preview ramp must have shown the same);
   - **with the thumbnail intro**: fade-in over the image's first
     moments reads right (the wrap is outside the intro concat);
   - **the white variant** — video only goes white (audio has no
     color, obviously — but the eye check is the c=white burn);
   - **music placed at the very end**: a clip whose tail crosses the
     fade-out window — its sound must die inside the envelope too
     (the one-envelope promise, the operator's literal ask);
   - **a no-fade export**: byte-identical to yesterday's output path
     (sliders at 0 — if in doubt, diff the ffmpeg args / camera.fg).
2. **Preview parity while they drive** (ADR 0036): scrub through the
   fade regions — the canvas ramp and the preview loudness must match
   what the burns then showed. A scrub INTO the fade-out region lands
   the gain within a frame (~16 ms) — a finding only if their ear
   catches a pop.
3. Feel gates: the two sliders ride smooth and Ctrl+Z after a ride is
   ONE step; the Black/White chips are one step each; undo/redo of
   fades during playback re-tunes the preview gain correctly; parked
   inside a razor-removed TAIL the canvas shows the fade's end state
   (recorded as honest in the ADR — a finding if it reads as haunted
   to them).
4. Every finding: timestamp + what the eye/ear caught; fix in THIS
   session if it's polish on the shipped machinery (a curve feel, a
   tooltip, the section placement); pre-register as the next slice if
   it's new machinery (cut-boundary dips, per-clip music fades, curve
   shapes). Amend ADR 0070 with the verdict (the 0069 amendment
   pattern).
5. Consumed verdict → mark this file CONSUMED (the
   `nextprompt-undo-verdict.md` pattern) and the queue moves to
   **`nextprompt-title-gen.md`** (queued behind this arc by the
   operator's jumps) — unless the operator's own words pick the next
   arc first (they usually do).

## Sharp edges to hand the operator (from the handoff)

- The fade-out `st` anchors on `intro + kept_total`, never `-t` — if
  ANY burn fades late/early or not at all, that anchor (or a path that
  bypassed `fade_edges`) is the suspect; the graph is in `camera.fg`
  next to the export.
- The no-graph-audio path (`[0:a]afade…[aout]` + the arg-mapping flip)
  runs exactly when there's no razor/intro/music — the plain-clip
  fade case exercises it; a silent export would mean the mapping flip
  regressed.
- Overlapping edges on a very short clip (in+out > length) never reach
  full brightness mid-clip — CapCut-same, recorded; a finding only if
  it surprises them.
- The preview ramp uses the razor's kept spans; the burn the cut
  plan's (sub-frame sliver difference, <50 ms) — do NOT chase a tiny
  "drift" there.

## Standing rules

Quality over runtime. Gate on the burn, not the label. AI and operator
artifacts never share a track. No manual entries in dialect stores.
PS 5.1 quoting (`"--features" "face,align,ser"`); commit via
`git commit -F <file>`; push to main is authorized. Validate: cargo
test -p yt-clipper -p yc-core -p yc-render, clippy on touched files,
feature check, release build FOREGROUND + the operator's eye AND ear
on the burns. Finish: handoff + whatwedone.md + fresh nextprompt + the
starter line. (`nextprompt-title-gen.md` stays queued behind this
verdict.)
