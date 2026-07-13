# CONSUMED 2026-07-13, same day — the operator tested it live

The music track shipped and the operator drove it; their verdict, in
their own words: **"okay it work."** No findings. ADR 0068's amendment
records it, plus what stays deliberately unexamined (a razor-cut export
WITH music end-to-end, a multi-clip mix, mono/5.1 decode, the menu gain
slider's feel, the decode-at-pick stall) — those surface during normal
use, not by inference. The queue moves to `nextprompt-undo.md` (operator
pick, jumping title-gen again).

---

# (original prompt below, kept for the record)

# Session prompt — background music: the operator's burn verdict (plan #6 gate; queued 2026-07-13)

The Music track shipped (ADR 0068, handoff
`handoffs/2026-07-13-music-track.md` — READ BOTH FIRST; the ADR holds the
one-amix-wrap decision, the two-clocks conversion, the selection-routed cut
grammar, and the rejected alternatives; the handoff ends with the sharp
edges this session exists to check). 166 tests green, zero new clippy
warnings, `face,align,ser` release build compiled — but **no human has
HEARD a music'd export**. This is the verdict round (gate on the burn, not
the preview — the operator's EAR this time, not just their eye).

## What to do

1. Confirm the release build is current (`scripts\build-release.bat`,
   FOREGROUND — background shells get reaped) and have the operator drive
   a real clip: promote → `+ Music` (a real song file — their actual
   music; mp3 first, then one flac/ogg if they have it) → drag the block,
   trim its edges, split it (right-click → Split here), set a volume
   (~60–80%), then Export. The pre-registered burn gate (ADR 0068):
   - music ENTERS exactly where the strip shows it — same content under
     the entrance in preview and export;
   - trims/splits land (the split is seamless in the burn: two clips,
     source-continuous — no gap, no repeat);
   - the gain is audible as set; the VOICE level is unchanged from a
     no-music export (`normalize=0` — the whole point);
   - speech stays legible under the music;
   - the output ends WITH the video — music never extends the Short.
2. The selected-clip cut grammar, live (their own question — "how do i
   cut the music if my razor cannot cut it?"): click a block → ✂⏴ / ⏵✂
   trim it to the playhead (tooltips flip to say so), Delete removes it,
   Esc / empty-strip click deselects, and with nothing selected the verbs
   razor the MAIN timeline exactly as before. Verify the main razor never
   moves a music block.
3. The two-clocks conversion on a real export: add a razor cut, REMOVE a
   segment that sits BEFORE a music clip, export — the music must still
   enter against the same content the strip shows it over (the
   export_clock conversion, ADR 0068 decision 3). This is the subtlest
   shipped decision; one real listen closes it.
4. Music over the intro: place a clip at output 0 with a thumbnail intro
   set — preview plays music over the held image (voice silent); the
   export does the same.
5. Feel gates while they drive: block drag/trim clamps (no overlap, no
   spill past the strip), the volume slider in the right-click menu
   (stays open while sliding?), mute (blocks dim; export modal says
   "muted — not in the export"; preview silent), lock, scrub ACROSS a
   music entrance during playback (one restart, no double-start, no
   stutter), pause/resume mid-music, and the ~0.5–1.5 s decode-at-pick
   stall on a long mp3 (a finding only if it reads as a hang — a status
   line is the likely fix).
6. Every finding: timestamp + what the ear/eye caught (publish-bar
   discipline); fix in THIS session if it's polish on the shipped
   machinery, pre-register as the next slice if it's new machinery.
   Amend ADR 0068 with the verdict (the 0067 amendment pattern).
7. Consumed verdict → mark this file CONSUMED (the
   `nextprompt-thumbnail-verdict.md` pattern) and the queue moves to
   **`nextprompt-title-gen.md`** (re-queued behind this arc by the
   operator's jump).

## Sharp edges to hand the operator's senses (from the handoff)

- A razor-cut export WITH music — the export_clock conversion has never
  been heard end-to-end (unit-tested only).
- A multi-clip mix (2–3 clips, one gain lowered): entrances and the
  amix's `duration=first` tail.
- A mono or 5.1 music file: the decode keeps the native layout and the
  amix aformats to stereo — listen for channel weirdness.
- The gain slider inside an egui context menu: interaction feel is
  unverified (menus close on outside clicks — does a slider drag hold?).
- The music block label at narrow widths and far zoom (ellipsized name +
  gain suffix).

## Standing rules

Quality over runtime. AI and operator artifacts never share a track. No
manual entries in dialect stores. PS 5.1 quoting (`"--features"
"face,align,ser"`); commit via `git commit -F <file>`; push to main is
authorized. Validate: cargo test -p yt-clipper -p yc-core -p yc-render,
clippy on touched files, feature check, release build FOREGROUND + the
operator's ear on the burn. Finish: handoff + whatwedone.md + fresh
nextprompt + the starter line. (`nextprompt-title-gen.md` stays queued
behind this verdict.)
