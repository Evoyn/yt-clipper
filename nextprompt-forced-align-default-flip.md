# Session prompt — Forced-alignment timing: operator burn gate, then flip the ensemble default (ADR 0054 Phase 2)

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read the
context below, then run /grill-with-docs BEFORE any code.

## What happened last session (read this first)

ADR 0053's forced-alignment timing was PORTED TO NATIVE RUST and shipped
opt-in (ADR 0054, committed to main): `cahya` wav2vec2-CTC exported to ONNX
(`models/w2v2-align-id/`, pinned via fetch-models.ps1 + scripts/export-align-
onnx.py), a torchaudio-parity CTC Viterbi in `yc-transcribe::align` (650/650
spans identical, onset parity 0.0000 s on the spike's 111 words), wired into
`ensemble::apply` behind `YC_FORCED_ALIGN=1` on an `align` build (now in
build-release.bat). Default renders are byte-identical (362 workspace tests
green). On the PRODUCTION decode of clip 3 the two catastrophic mis-onsets
fixed automatically: SIAPA -1.54s -> +0.08, SDC -2.22s -> -0.07. The
turn-taking control (ANTITESA) numbers are in ADR 0054's gate table. A
re-burn mp4 was left for the operator's eye (see below).

## The gate that MUST come first (operator, before any code)

Watch `workspace/Deddy Corbuzier/MENUJU INDONESIA CEMAS BERSAMA VIOR…/
_forced-align clip3 (RUST PORT - production).mp4` against
`_recall-lane clip3 (fresh current-code ensemble).mp4` (same words, DTW
timing) — the ONLY question is TIMING on the ear/eye (text drops are the
recall lane, ADR 0052, unchanged by this). If the operator rules it worse,
STOP — measure, don't flip.

## The slice (if the burn gate passes)

1. **Flip the ensemble default**: forced alignment becomes the ensemble
   path's timing skeleton without the env knob (knob becomes the off-switch,
   `YC_FORCED_ALIGN=0`). Byte-identity contract moves: whisper-engine
   renders stay untouched; ensemble renders change by DESIGN (the gate).
2. **Drop the dead weight**: under alignment the `suppress_nst` second
   whisper decode feeds nothing on the ensemble path — remove it there
   (a full GPU decode per render, pure waste once flipped).
3. **Cross-clip widening**: run `caption_align_diag` on the Deddy
   Tretan/Coki overlap clip + one guru gembul karaoke clip (different genre
   path!) before trusting the flip beyond huge-word. Karaoke/rolling builders
   consume the same units — verify nothing assumes DTW span shapes.
4. **at_s pins**: confirm a pin still wins over the aligner (the GUE class,
   +0.55s residual on clip 3) — `apply_store_positional` runs after fusion,
   test exists, but eyeball one real pin end-to-end.

## Read first

1. `docs/adr/0054-forced-alignment-rust-port.md` (the port, the parity, the
   gate numbers, the RAW-input contract — do NOT "fix" it to normalized) and
   `docs/adr/0053-forced-alignment-timing-whisperx-spike.md` (the decision).
2. `handoffs/2026-07-11-forced-align-rust-port.md` (operational state, the
   re-burn recipe, the traps — decode variance, the recall-lane rows).
3. `crates/transcribe/src/align.rs` + `ensemble.rs` (`forced_align_fusion`,
   `fuse_onto_alignment`) and `crates/app/examples/caption_align_diag.rs`.

## Hard rules

- Gate on the OPERATOR'S EYE on the burned clip (ADR 0050 lesson) BEFORE the
  flip. Measure on the production path (`caption_align_diag`).
- Turn-taking timing must not regress on ANY control clip.
- Native ONNX via `ort`, NO Python in the shipped app. Fixtures stay green.
- Best accuracy over speed (standing directive).

## Ritual

/grill-with-docs first (pin the flip semantics + which clips gate it);
finish with /handoff + whatwedone.md entry; commit as Evoyn with the model
trailer (-F file); `git push origin main` has standing permission. PS 5.1
quirks per the standing memory notes.
