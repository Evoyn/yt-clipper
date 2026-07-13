# CONSUMED 2026-07-13, same session — the operator tested it live

The intro shipped and the operator drove it in the same session; their
verdict, in their own words: **"okay its good, i just test it."** No
findings. ADR 0067's amendment records it, plus what stays deliberately
unexamined (a real .webp pick, the boundary frame on a 23.976 source,
boundary audio under the aformat-48k concat, eye-off track + intro in
one export) — those surface during normal use, not by inference. The
queue moves to `nextprompt-title-gen.md`.

---

# (original prompt below, kept for the record)

# Session prompt — thumbnail intro: the operator's burn verdict (plan #5 gate; queued 2026-07-13)

The thumbnail intro shipped (ADR 0067, handoff
`handoffs/2026-07-13-thumbnail-intro.md` — READ BOTH FIRST; the ADR holds
the prepend-around-the-stream decision, the output-time timeline, and the
rejected alternatives; the handoff ends with the sharp edges this session
exists to check). 123 tests green, clippy clean, `face,align,ser` release
build compiled — but **no human has seen an intro'd export**. This is the
verdict round (gate on the burn, not the preview — and not the label).

## What to do

1. Confirm the release build is current (`scripts\build-release.bat`,
   FOREGROUND — background shells get reaped) and have the operator drive
   a real clip: promote → `+ Thumbnail` (a real cover image; try their
   actual thumbnail file) → trim the block's edge → add a razor cut and,
   on a podcast clip, keep a camera cut → Export. The pre-registered burn
   gate (ADR 0067):
   - the exported mp4 opens on the image, aspect-fit with black bars,
     for exactly the trimmed 1–2 s, with SILENCE under it;
   - the FIRST caption lands exactly on its word (not a caption's-width
     early/late — the whole point of the prepend architecture);
   - a camera cut and a razor cut land where the strip showed them (the
     strip's output clock = the export's clock);
   - listen at the intro→clip boundary and overall: the main audio now
     passes through `aformat` 48k for the concat — any artifact is a
     finding (expected inaudible).
2. Feel gates in the editor while they drive: the block's edge trim
   (clamp 0.5–2.0), click-to-park + the canvas showing the image, Play
   from inside the intro (hold → audio+video start together at the
   crossing; the ~0.1–0.5 s align stall at the crossing is the standard
   decoder-spawn behavior — a finding only if it reads as a hitch), scrub
   ACROSS the boundary during playback (audio must restart in sync, the
   today contract), the razor buttons disabling inside the intro, panel
   timestamps matching the ruler (+D display shift), and a `.webp` pick
   (the `-loop 1` webp_pipe path is untested on a real file).
3. Every finding: timestamp + what the eye saw (publish-bar discipline);
   fix in THIS session if it's polish on the shipped machinery,
   pre-register as the next slice if it's new machinery. Amend ADR 0067
   with the verdict (the 0065/0066 amendment pattern).
4. Consumed verdict → mark this file CONSUMED (the
   `nextprompt-multitrack-verdict.md` pattern) and the queue moves to
   **`nextprompt-title-gen.md`** (re-queued behind this arc by the
   operator's jump).

## Sharp edges to hand the operator's eye (from the handoff)

- Intro→clip boundary on a 23.976 source: the still re-grids 25→source
  fps — look for a duplicated/odd frame at the crossing.
- The intro block at far zoom-in: its right-edge trim under high zoom
  (the mapping breathes with D mid-drag — converges by design; verify it
  FEELS anchored).
- Remove-thumbnail while playing inside the intro (playhead converts −D
  and playback restarts — should be seamless).
- An eye-off caption track + intro together in one export (two features'
  first meeting: ADR 0066's eye gates the stream, ADR 0067 prepends
  around whatever burned).

## Standing rules

Quality over runtime. AI and operator artifacts never share a track. No
manual entries in dialect stores. PS 5.1 quoting (`"--features"
"face,align,ser"`); commit via `git commit -F <file>`; push to main is
authorized. Validate: cargo test -p yt-clipper -p yc-core -p yc-render,
clippy on touched files, feature check, release build FOREGROUND + the
operator's eye on the burn. Finish: handoff + whatwedone.md + fresh
nextprompt + the starter line. (`nextprompt-title-gen.md` stays queued
behind this verdict.)
