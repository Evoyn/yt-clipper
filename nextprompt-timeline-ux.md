# Session prompt — Studio timeline: make the voice row visible (resizable strip)

You are giving yt-clipper's Studio timeline a usable height
(F:\yt-clipper, pure-Rust egui app). Fresh session: read the context
below, then run /grill-with-docs BEFORE any code. One implementation this
session: **the timeline strip resizable by the operator** — so the voice
row (and the seat lanes under it) can actually be seen. This is the
operator's explicit request from their 2026-07-06 fresh-binary first look:
"all looks good but because timeline is so small it doesnt visible there,
maybe make a timeline size dragable or have a scroll? so it can get big or
scrollable".

## Open the grill with
1. Drag-to-resize (an egui drag handle on the strip's top edge, height
   persisted) vs a scrollable fixed-height strip vs both — recommend ONE
   primary mechanism and defend it (egui panels already scroll; the
   complaint is the ROW HEIGHT, so resize is likely the real fix).
2. Where the height persists (per-session egui memory vs a settings file)
   and its default/min/max.
3. Whether row heights scale with the strip (voice row + seat lanes +
   caption row share the new space proportionally, or fixed-px rows that
   just stop clipping).
4. Confirm the alternative slice was considered and deferred: the
   laughter-class instrument spike (ADR 0044's named path to win the
   Deddy splits back). If the operator would rather have that, THIS
   session becomes that spike instead — ask before any UI code.

## Read first (in this order)
1. handoffs/2026-07-06-person-join.md — gate state + traps (the ear
   overturn, the protected renders, the numbers of record).
2. docs/adr/0044-person-scoped-voice-join.md — the shipped join the
   timeline row surfaces.
3. docs/adr/0039-studio-editor-page.md — the Studio layout the strip
   lives in.
4. crates/app/src/editor.rs — the timeline strip painting (voice row,
   seat lanes, cut markers, playhead) and the panel layout around it.

## Acceptance
- The operator can make the timeline tall enough that the voice row's
  spans (colored by joined seat, off-screen spans distinct) are plainly
  visible on the ANTITESA clip in the Studio — their eyes are the gate.
- Zero analysis/render behavior change: this is UI-only. Suites green
  both ways (327 today). The ANTITESA fg byte-pin `9e07d81f…` and the
  Deddy person-join bars (occupant map 4+2, one VALID edge V2 x cam0 ->
  P1, off-screen 1.5 s) stay untouched — run both harness fixtures before
  committing anyway (.claude/skills/verify/SKILL.md has the bars).

## Fixtures + harness (same as ever)
- Deddy: `workspace/Deddy Corbuzier/BGN B NYA…. 😂 SEREM BGT NIH PODCAST
  ASUU‼️ Tretan, Coki, Adriano/data`, range `1559.6094450950623
  1629.7294450950621`.
- ANTITESA: `workspace/Leon Hartono/ANTITESA Cacing Cacing Naga Naga! -
  Ft. Andrew Susanto/data`, range `790.0 859.0`.
- `cargo run --release -p yt-clipper --example speaker_diag --features
  face -- "<data dir>" <start> <end>`.
- GUI drive: PrintWindow capture, never CopyFromScreen while the operator
  works (recipe in .claude/skills/verify/SKILL.md).

## Ritual
/grill-with-docs first; /verify before committing; finish with /handoff to
`handoffs/<date>-<slug>.md` + a dated entry prepended to whatwedone.md;
commit as Evoyn with the model's Co-Authored-By trailer (message via -F
file); `git push origin main` has standing permission. Windows PS 5.1
quirks per the standing memory notes.
