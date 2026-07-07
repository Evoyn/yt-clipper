# Session prompt — Fix #1: suppress phantom captions on shared-reaction stretches

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read the
context below, then run /grill-with-docs BEFORE any code — the measure-first
step is already DONE (ADR 0049 built the instrument and passed the gate), so
this slice WIRES the first fix and gates it. The grill's job here is to pin
WHERE the suppression lives and HOW the overlap signal reaches the caption
stage (it does not today), and to pin the gate that proves it removes phantoms
WITHOUT deleting real words — before touching the production caption path.

## Why this exists

ADR 0049 measured the shipped 4-person VIOR captions and found four
overlap-specific failure classes. The cheapest, highest-confidence, look-
preserving fix is **#1: phantom suppression** — drop a caption cue whose onset
sits on a **reaction-masked bin with no clean attributed speaker**. On clip 3
the instrument auto-detected the signature: a pile "YA SIAPA TAU MAU"
(1.64–2.36 s) of too-fast cues on a `laugh` 0.25–0.60 stretch, plus "gue"@26 on
`laugh` 0.76 — words the operator confirmed are laughter, not speech.

## What we already know (the evidence + the seam)

- The **Shared-reaction mask** (ADR 0045/0046, `yc_frame::reaction`) already
  marks group laughter per bin, and the **Voice lane** (ADR 0042/0044) already
  attributes a speaker per bin (`speaker == -1` / low `conf` = nobody cleanly
  speaking). The camera lanes consume both. **Captions do not** — `do_render` →
  whisper/ensemble → `refine_caption_timing*` runs on the raw mixed audio with
  no access to either. The whole fix is: plumb that per-bin signal into the
  caption stage, then gate a cue on it.
- The instrument `caption_overlap_diag` (ADR 0049, `crates/render/examples/`) is
  the gate tool: re-run it on the fixed clip's `clip.ass` and require the phantom
  pile GONE while the turn-taking control stays 0–2% sub-floor.

## The slice (wire + gate — a real production-path change)

1. **Grill the seam FIRST**: does suppression belong in the ensemble fusion
   (`ensemble.rs::fuse_onto_timing`, where words place onto the skeleton) or in
   `refine_caption_timing*` (ass.rs, where cues get final timing)? How does the
   per-bin reaction mask + voice attribution get from the Speaker analysis to
   whichever stage — passed on the transcript job, recomputed, or threaded
   through `do_render`? Pin this before code.
2. **Pre-declare the gate BARS** (the ADR 0045 discipline) before reading any
   table: phantom cues removed on clip 3 (the "YA SIAPA TAU MAU" pile + "gue"@26
   gone); **no real word suppressed** (the false-positive killer — a real word
   spoken *during* laughter must survive); the turn-taking control clips
   (guru gembul solo, Helmy, ANTITESA) stay 0–2% sub-floor (no regression); and
   the fixture bars stay green.
3. **Wire it**, then judge at the gate with the instrument on the operator's
   eyes. Ship only if the bars pass.

## Read first

1. `docs/adr/0049-caption-overlap-instrument-gate.md` (the gate + roadmap) and
   `handoffs/2026-07-08-caption-overlap-instrument.md` (traps: the signal never
   reaches captioning; drops aren't measurable here; the cross-clip control).
2. `crates/transcribe/src/ensemble.rs` (`fuse_onto_timing`, `place_run`) and
   `crates/render/src/ass.rs` (`refine_caption_timing*`) — the two candidate
   homes for the gate.
3. `crates/frame/src/reaction.rs` + the `voice`/`reaction` fields on the Speaker
   analysis — the mask + attribution to plumb. CONTEXT.md: **Shared reaction**,
   **Voice lane**, **Caption engine**.

## Hard rules

- Measure with the instrument on the PRODUCTION path against operator ground
  truth BEFORE claiming the fix works (the standing discipline).
- **A real word spoken during laughter must survive** — suppression keyed on the
  mask alone will eat real speech; the gate MUST prove real words aren't dropped.
- Turn-taking captions must not regress (the control clips stay clean).
- Fixture bars stay green (Deddy person-join + laughter, ANTITESA fg byte-pin,
  audits clean, suites both ways). AnalyzeSpeakers stays editor-only; the harness
  twin is speaker_diag.
- When this fix wires, add the crystallized caption-overlap term to CONTEXT.md
  (deferred from ADR 0049, the way Solo presence got its term at wiring).

## Ritual

/grill-with-docs first; /verify before committing; finish with /handoff +
whatwedone.md entry; commit as Evoyn with the model trailer (-F file);
`git push origin main` has standing permission. PS 5.1 quirks per the standing
memory notes.
