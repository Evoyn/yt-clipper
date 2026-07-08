# Session prompt — Fix the REAL caption defect: mis-onset re-anchor (words on the wrong clock)

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read the
context below, then run /grill-with-docs BEFORE any code.

## What happened last session (read this first)

ADR 0049's roadmap called the opening "YA SIAPA TAU MAU COBA AKU" a phantom pile
and made "phantom suppression" fix #1. It shipped (ADR 0050), passed the
instrument — and the operator, watching the real BEFORE/AFTER **burn**, ruled it
WRONG: **"siapa tau mau coba ku bawa" is real speech**, spoken fast and placed too
early, not laughter. Deleting it made the clip worse than shipped. **It was
reverted the same day** (ADR 0050 is marked REVERSED; captions are byte-identical
to before). **Lesson, now baked into the benchmark file: an instrument agreeing
with a hand-labelled "phantom" is NOT the operator's eye on the burned clip. Gate
on the burn.**

So the reported defect was never phantoms. It is **mis-onset** (right word, wrong
time), plus **mis-transcription** (wrong text) and **dropped words**. This slice
does the biggest CODE win: **re-anchor the mis-onset words to when they are
actually said.**

## The operator's corrected ground truth (2026-07-08, clip 3)

`benchmarks/vior-fans-fadhil.captions.groundtruth.txt` now carries the 2026-07-08
correction. The **mis-onset** items (the target of this slice):
- opening "siapa tau (mau coba ku bawa)" shows ~1.6-3.7 s, real "siapa"/"tau" at ~5 s
- "SDC"/"siapa tau" shows ~21 s, belongs ~23 s
- "gue" shows ~26 s, belongs ~28 s
- "fadil" shows ~31 s, belongs ~32 s
- "jalannya" shows ~53 s, belongs ~54 s

Out of scope for THIS slice (note them, don't chase): **wrong-text** items are
dialect-store curation the operator does (lu→lucu, ya→yakan, SDC→siapa tau,
aslinya→ASI, pamu→Pak muh, gemot→gemes, duduk-duduk→dodo, proten→protein);
**drops** (otot kayaknya, kreatin-kreatin, pinguin) are the recall lane (ADR
0049 fix #4). One possible true **phantom**: "ya"@12 (maybe faint bg speech).

## The slice (grill the seam + the false-positive bar FIRST)

1. **Stage attribution is the OPENING question (UN-RUN).** Is the ~1.5 s lead
   whisper's raw DTW onset, or the ensemble fusion (`fuse_onto_timing`) placing the
   word early onto a laughter onset? Run `caption_diag` on clip 3 and compare
   whisper's raw onsets to the shipped `clip.ass` times BEFORE deciding where the
   re-anchor lives. This decides everything downstream.
2. **Pin the re-anchor seam in the grill**: the word moves in the ensemble fusion
   (place onto the real post-laughter speech onset instead of the laughter one), or
   a refine-stage re-place? The RMS speech onsets + the reaction mask are the
   inputs; `apply_store_positional` in ensemble.rs already re-places words onto
   onsets near an `at_s` moment — study it, it may be most of the machinery.
3. **Pre-declare the gate BARS** (ADR 0045 discipline): the 5 mis-onset cues land
   within tol of their heard onset; **no correctly-placed word moves** (the
   false-positive killer); turn-taking controls unchanged; fixtures green — AND
   the gate is the operator's eye on a re-burn (use `segment_seek` for the exact
   seek, re-burn the clip.ass over the segment, no GPU re-decode needed).

## Read first

1. `handoffs/2026-07-08-caption-reaction-phantom-suppression.md` (the full arc:
   what was tried, why it reversed, the seam) and `docs/adr/0050-...md` (REVERSED
   banner + lesson) and ADR 0049 (the -1.47 s mis-onset measurement).
2. `crates/transcribe/src/ensemble.rs` — `fuse_onto_timing` (where words place onto
   onsets), `apply_store_positional` (existing onset re-placement + `at_s` pins),
   `rms_onsets`. `crates/app/examples/caption_diag.rs` (whisper-DTW-vs-shipped twin).
3. `crates/ingest/examples/segment_seek.rs` — measures the render seek so you can
   re-burn a fixed clip.ass and eyeball it on the real surface (last session proved
   the re-burn is byte-identical to `do_render`).

## Hard rules

- Gate on the OPERATOR'S EYE on the burned clip, not on an instrument passing
  against a label (the ADR 0050 lesson). Measure on the production path first.
- **A correctly-placed word must never move** — the gate MUST prove re-anchoring
  only touches the mis-onset words, not the good ones. Never DELETE a word to fix
  timing (that was the ADR 0050 mistake).
- Turn-taking captions must not regress; fixtures stay green (ANTITESA fg SHA-pin,
  Deddy person-join + laughter + shared-reaction, audits; suites both ways — 356 of
  record). AnalyzeSpeakers stays editor-only; the harness twin is speaker_diag.
- Operator prefers best accuracy over speed (standing directive).

## Ritual

/grill-with-docs first; /verify before committing; finish with /handoff +
whatwedone.md entry; commit as Evoyn with the model trailer (-F file);
`git push origin main` has standing permission. PS 5.1 quirks per the standing
memory notes.
