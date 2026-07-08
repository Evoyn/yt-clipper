# Caption timing: wav2vec2 forced alignment replaces whisper DTW as the timing skeleton — spike-validated on clip 3 (supersedes hand-pinning for most cases)

The operator asked whether the caption desync — text correct, timestamps drift
out of sync especially around laughter/pauses — is a `whisper.cpp` DTW limitation,
and whether a **WhisperX + faster-whisper** pipeline (VAD → transcribe → wav2vec2
forced alignment → build cues from aligned word times) would fix it. This ADR
records the research, the architecture decision, and a **measured spike that
validated it** on clip 3. Same measure-first discipline as ADR 0049/0051.

## The desync is whisper.cpp DTW (confirmed in-code)

Every caption word's TIME comes from `whisper.cpp`'s DTW token timestamps
(`whisper_rs::{DtwMode, DtwParameters}`, `transcribe/src/lib.rs` — each token
carries `t0,t1`). The Qwen ensemble supplies the WORDS but fuses them onto this
DTW skeleton snapped to a crude RMS-energy grid (`fuse_onto_timing`, `rms_onsets`).
ADR 0051 already measured the mis-onset as whisper's DTW. DTW finds one **global**
alignment path across a segment; non-speech (laughter, pauses, filler) has no
matching tokens, so the path smears across it and **accumulates** offset — the
operator's "slowly drifts out of sync." Word-level DTW is a ±500 ms-class signal;
forced alignment (wav2vec2 CTC) is ±50 ms and re-anchors **each word
independently**, so error cannot accumulate (WhisperX paper; on Switchboard/AMI it
beats whisper's own timestamps 93.2/85.4 % and 84.1/78.9 % precision).

## Decision: SUPPLEMENT (align the ensemble's words), do NOT replace

The ensemble's WORDS are good (Indonesian slang, ADR 0033/0034); `faster-whisper`
would regress them. The valuable half of WhisperX is the **forced alignment**,
applied as a timing pass over the ensemble's existing words — swapping whisper DTW
as the timing source, leaving the vote untouched. Indonesian is supported
(`cahya/wav2vec2-large-xlsr-indonesian`, a HF alignment model).

## The spike (measured 2026-07-08 — validated)

`spike_align.py` (scratchpad, one-off Python — CPU torch + transformers + the
`cahya` XLSR model) force-aligned the operator-APPROVED word sequence (from the
surgical clip.ass, i.e. the correct text) to clip 3's 16 kHz **mixed** audio and
compared the derived onsets to the ground truth — text fixed, only TIMING tested:

```
word         whisper DTW (shipped)   forced-align    ground truth   operator hand-pin
SIAPA        3.66  (-1.3s, WRONG)    5.08  (+0.08)   5.0            5.06
SDC          20.78 (-2.2s, WRONG)    22.93 (-0.07)   23.0           22.78
OTOT         (dropped)               6.80  (+0.30)   6.5            6.64
KREATIN      (dropped)               8.82  (+0.32)   8.5            8.72
FADIL        31.32 (WRONG)           32.77 (+0.77)   32.0           32.26
PINGUIN      50.58                   52.76 (+0.76)   52.0           52.65
GUE          25.84                   29.91 (+1.9s)   28.0           28.12
JALANANNYA   53.48                   53.68 (-0.32)   54.0           54.00
```

**Forced alignment reproduced the operator's hand-tuning automatically, no pins.**
The two mis-onsets whisper botched worst (SIAPA, SDC) landed within 0.1 s; 7/8
within ~0.8 s; **0 of 111 words fell back to interpolation** (even the
laughter-masked ones aligned — better than the research warned). The one miss,
**GUE +1.9 s**, is the duplicated-common-word ambiguity (ADR 0051's exact caveat —
"gue" is said many times; the aligner grabbed a different one), which pins also
struggle with. Burn on the operator's eye: `_recall-lane clip3 (FORCED-ALIGN
spike - no pins).mp4` — _operator ruling: follow the recommendation (proceed)._

## Why this matters: timing becomes automatic (it GENERALIZES)

The operator's standing problem is that **hand-pins don't carry to other videos**
(a pin is "this word at this second"). Forced alignment derives timing **from the
audio itself**, so it is automatic on every clip. If it holds across clips it folds
**two lanes into one**: the mis-onset lane (ADR 0051, hand-pins) AND the recall
lane's mis-timing residual (ADR 0052) both collapse into a single automatic
mechanism — a far bigger win than either alone. The `at_s` pin stays only as the
**override** for the residual common-word cases (GUE).

## Architecture: native Rust + ONNX, not a Python sidecar

WhisperX/faster-whisper are Python (torch + CTranslate2 + wav2vec2); this app is
pure-Rust, offline, self-contained exe sidecars. The spike used Python to validate
fast, but shipping goes **native**: `cahya` wav2vec2-CTC exported to ONNX, run via
**`ort`** (already a dep — the tagger and face-id use it) + a ~100-line CTC
forced-align (Viterbi). No Python, stays self-contained. wav2vec2-CTC is light
(<1 s/clip on GPU); one model download (~1.2 GB XLSR-large, smaller variants exist);
CUDA 13.3 present satisfies any runtime need.

## Migration plan (Phase 1, minimal change)

The seam already exists: `fuse_onto_timing(merged, whisper, timing_extra, …)` takes
the timing skeleton as an argument. Forced alignment slots in there — no vote/word
changes.

1. Export `cahya/wav2vec2-large-xlsr-indonesian` to ONNX; load via `ort`.
2. A `forced_align(emission, tokens)` CTC pass (torchaudio's algorithm ported) →
   word onsets over the ensemble's `merged` words.
3. Wire it as the timing source into `fuse_onto_timing` (or replace the DTW skeleton
   for the ensemble path), behind a Caption-engine/feature flag, **off by default**.
4. Gate on the ADR 0049 cross-clip control (turn-taking timing must not regress) +
   more overlap clips, on the operator's eye. Flip the default only if it wins.

## Drawbacks / open questions

- **Duplicated common words** (GUE) still need the occasional `at_s` override.
- The spike fed the CORRECT text; production text is the ensemble's (garbles like
  `keratin` → aligned correctly but still spelled wrong; a separate text lane).
- OOV slang/names may align weakly (fall to interpolation) — measure per clip.
- Small +0.3–0.8 s bias on soft-onset words (the aligner marks confident-phoneme
  onset) — tunable with a fixed offset.
- Validated on ONE clip; the gate must confirm no turn-taking regression before it
  ships. Alternative noted but not chosen: **CrisperWhisper** (a Whisper variant with
  accurate verbatim timestamps) — a model swap, not a Rust fit.

## Consequences

- If Phase 1 gates green, whisper's role shrinks to a word/skeleton source and the
  timing authority moves to forced alignment; hand-pinning (ADR 0051) becomes the
  exception, not the workflow.
- New pinned model (`cahya` wav2vec2 ONNX) + an `ort` alignment pass; the offline,
  self-contained-sidecar contract is preserved (no Python in the shipped app).
- Research basis: WhisperX (arxiv 2303.00747), faster-whisper/CTranslate2 (SYSTRAN),
  MFA-vs-WhisperX accuracy (arxiv 2406.19363), CrisperWhisper (arxiv 2408.16589).
