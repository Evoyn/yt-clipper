# Session prompt — The DROP / recall lane: caption the words whisper never hears on masked overlap (ADR 0049 fix #4)

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read the
context below, then run /grill-with-docs BEFORE any code.

## What happened last session (read this first)

ADR 0051 shipped the mis-onset re-anchor (fix #3): the operator's `at_s` time-pin now
snaps a word onto its real speech onset on BOTH caption engines (was ensemble-only).
Automatic re-anchoring was REFUTED by measurement (no acoustic signal separates a
mis-placed word from a correct-before-a-pause word — only the operator's ear does).
The operator ruled the timing FIXED on a clip-3 re-burn.

BUT the operator's publish-bar findings on that burn are almost all DROPS — words that
were SPOKEN but whisper never transcribed at all (so there is no cue to re-time or
re-anchor; the mis-onset fix structurally cannot reach them). This slice is the recall
lane: get those words captioned.

## The operator's drop findings (clip 3, the must-fix list)

- "siapa tau" (~20s) — another speaker, just before the real word "SDC"; whisper's
  20.78-23.28 "SDC" span swallowed it.
- "yang itu isinya otot kayaknya" (~6.5s) — only "yang" captioned, the rest dropped.
- "yang keluar kreatin-kreatin" (~8.5s) — "yang" + "kreatin-kreatin" not carried.
- "pinguin" (~52s) — spoken, never captioned.

All on masked / overlapping stretches (the regime ADR 0049 measured). The benchmark
`benchmarks/vior-fans-fadhil.captions.groundtruth.txt` carries them as `drop` rows.

## The slice (grill the instrument + the bars FIRST)

ADR 0049 called this the HEAVY one — "per-speaker decode or denoise for masked overlap
(ADR 0014/0029 reopened); its own spike -> gate -> wire" — and warned the DROP class
"cannot be measured from the shipped file," so it needs its OWN instrument.

1. **Build the drop instrument FIRST (measure-first).** A heard-word (operator ground
   truth) vs carried-word (shipped clip.ass) alignment that flags a spoken word with NO
   nearby cue — tolerant of mis-transcription (a mis-heard neighbour a fraction of a
   second away reads as "carried," ADR 0049's own finding). Pin the tolerance in the grill.
2. **Pin the recall approach in the grill.** ADR 0014 REJECTED htdemucs for captions (a
   music separator, it drops real speech). ADR 0029 (enh / DeepFilterNet gentle denoise)
   is already the caption input. Candidates: per-speaker decode (using the speaker
   analysis / voice lane the camera path computes), a stronger denoise on the masked
   spans only, or extra decode variants voted in (the ensemble is already a vote). Grill
   which reaches the drops WITHOUT re-garbling the clean speech.
3. **Pre-declare the gate BARS (ADR 0045 discipline):** the dropped words get captioned
   (heard on the re-burn, operator's eye); NO clean speech regresses (the false-positive
   killer — a recall pass that re-garbles clean captions is a net loss); turn-taking
   controls unchanged; fixtures green; gate on the operator's eye on a re-burn
   (`segment_seek` + re-burn, the ADR 0050/0051 cheap path — no GPU camera re-decode).

## Read first

1. `handoffs/2026-07-08-caption-misonset-reanchor.md` (the mis-onset arc + the reusable
   re-burn recipe) and `docs/adr/0049-caption-overlap-instrument-gate.md` (fix #4 is the
   recall lane; the "drop can't be measured from the shipped file" finding) and ADR
   0014/0029 (the vocal-stem rejection + the enh denoise contract).
2. `crates/app/examples/caption_reanchor_diag.rs` (the whisper + tagger + re-burn emitter
   pattern to fork the drop instrument from) and `caption_overlap_diag.rs`.
3. `crates/app/src/pipeline.rs` `ensure_transcript` (the caption path; where enh / the
   ensemble decode live) and `crates/transcribe/src/ensemble.rs` (the multi-decode vote).

## Hard rules

- Gate on the OPERATOR'S EYE on the burned clip (ADR 0050 lesson). Measure on the
  production path first (the `enh` overclaim was caught twice).
- No clean speech may regress — prove it on the fixtures + the clip.
- Turn-taking captions must not regress; fixtures stay green (356 of record). Operator
  prefers best accuracy over speed (standing directive).

## Ritual

/grill-with-docs first; the re-burn on the operator's eye before committing; finish with
/handoff + whatwedone.md entry; commit as Evoyn with the model trailer (-F file);
`git push origin main` has standing permission. PS 5.1 quirks per the standing memory notes.
