# Session prompt — Port wav2vec2 forced alignment to native Rust as the caption timing skeleton (ADR 0053 Phase 1)

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read the
context below, then run /grill-with-docs BEFORE any code.

## What happened last session (read this first)

The caption desync the operator has been hand-fixing every clip (words shown early,
onto laughter; "slowly drifts out of sync") was traced to its root: caption TIMING
comes from `whisper.cpp` DTW token timestamps, which drift across non-speech
(laughter/pauses) because DTW is one global path. A measure-first **spike validated
the fix**: wav2vec2 **forced alignment** (the WhisperX approach) derives each word's
time from the audio itself and **reproduced the operator's hand-pinned onsets
automatically, with zero pins** on clip 3 (SIAPA 3.66→5.08, SDC 20.78→22.93 — the
two whisper botched worst — nailed; 7/8 within ~0.8s; 0 interpolation fallbacks).
Operator approved the direction. This session ports it to native Rust so it ships.

## The slice

Replace/supplement `whisper.cpp` DTW as the caption timing skeleton with a native
Rust wav2vec2-CTC forced-alignment pass. **Supplement, not replace** — the Qwen
ensemble's WORDS stay (Indonesian slang, ADR 0033/0034); only the TIMING changes.

1. **Export the alignment model to ONNX.** `cahya/wav2vec2-large-xlsr-indonesian`
   (cached from the spike at `~/.cache/huggingface/hub/models--cahya--…`). Export
   the CTC model to ONNX; load via `ort` (already a dep — `yc-frame` tagger/face-id
   use it). Pin it like the other models (fetch-models.ps1).
2. **Port the CTC forced-align.** ~100 lines: emission (log-softmax over the ONNX
   logits) + tokenize the ensemble's `merged` words to the model vocab + Viterbi
   `forced_align` + `merge_tokens` → per-word frame spans → `frame * dur/T` onsets.
   `crates/app/examples/.../spike_align.py` (scratchpad, and in the ADR) is the exact
   reference — vocab via the processor, `|` word delimiter, per-word grouping.
3. **Wire it in** as the timing source for the ensemble path (`fuse_onto_timing`
   already takes the skeleton as an arg), behind a Caption-engine/feature flag, OFF
   by default (ADR 0034 opt-in discipline; byte-identical default render).
4. **Gate** on the ADR 0049 cross-clip control (turn-taking timing must NOT regress)
   + the clip-3 re-burn + more overlap clips, on the operator's eye. Keep the `at_s`
   pin as the override for the duplicated-common-word residual (GUE, +1.9s in the spike).

## Read first

1. `docs/adr/0053-forced-alignment-timing-whisperx-spike.md` (the decision, the spike
   numbers, the Phase-1 plan, the drawbacks) and `handoffs/2026-07-08-caption-recall-and-
   forced-align.md` (operational state, the spike env, the re-burn recipe).
2. `crates/transcribe/src/ensemble.rs` `fuse_onto_timing` (the timing seam) + `rms_onsets`
   (the crude grid forced alignment replaces) and `crates/transcribe/src/lib.rs` (the
   whisper DTW `t0/t1` this supersedes).
3. `scratchpad/spike_align.py` + `scratchpad/spikeenv` (the validated reference; re-run
   with `spikeenv\Scripts\python.exe`). `uv` at `C:\Users\Nebu\.local\bin\uv.exe`.

## Hard rules

- Gate on the OPERATOR'S EYE on the burned clip (ADR 0050 lesson). Measure on the
  production path first (the `enh` overclaim was caught twice).
- No turn-taking timing may regress — prove it on the ADR 0049 cross-clip control.
- Keep the shipped app offline + self-contained: **native ONNX via `ort`, NO Python**.
  Fixtures stay green. Operator prefers best accuracy over speed (standing directive).

## Ritual

/grill-with-docs first (pin the ONNX export + the align algorithm + the gate BEFORE
code); the re-burn on the operator's eye before committing; finish with /handoff +
whatwedone.md entry; commit as Evoyn with the model trailer (-F file); `git push origin
main` has standing permission. PS 5.1 quirks per the standing memory notes.
