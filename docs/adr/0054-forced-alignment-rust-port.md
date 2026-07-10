# Forced-alignment timing ported to native Rust (ONNX + ort) — parity-exact with the validated spike, shipped opt-in behind `YC_FORCED_ALIGN=1`

ADR 0053's spike proved wav2vec2-CTC forced alignment reproduces the
operator's hand-pinned caption onsets from the audio alone (Python,
one-off). This ADR records the Phase-1 port: the same pipeline as **native
Rust in the shipped app** — no Python, offline, self-contained — wired as an
opt-in timing skeleton for the ensemble caption path, plus the measured
parity + gate evidence. Default renders are byte-identical (flag unset =
the whisper-DTW fusion exactly as before); the operator-eye burn gate on
the default flip is the NEXT slice.

## What shipped

- **`models/w2v2-align-id/`** — `cahya/wav2vec2-large-xlsr-indonesian`
  (rev `fe66c9f1`, the spike's exact weights) exported to ONNX: fp32,
  opset 17, dynamic batch/samples axes, `input_values -> logits`.
  ~1.3 GB. No hosted ONNX exists for this model, so the pin is
  source-weights + a deterministic local export:
  `scripts/export-align-onnx.py`, invoked by fetch-models.ps1 via `uv`
  (dev-time only — the shipped app never runs Python).
- **`yc-transcribe::align`** — the aligner. Pure, always-compiled, unit-tested:
  `AlignVocab::parse` (vocab.json: 28 char tokens, `|`=15 delim,
  `[PAD]`=27 blank), `tokenize_words` (spike tokenization verbatim: OOV
  chars skipped, all-OOV word -> lone delimiter placeholder),
  `log_softmax_rows`, `forced_align_spans` (CTC Viterbi, torchaudio
  `forced_align`'s topology: interleaved blanks, repeat rule, f64
  accumulators over the ~3000-frame log-prob sums), `word_frame_spans`,
  `align_words_on_emission` (seconds = `frame * dur/T`, the spike's clock).
  Only the ONNX session (`Aligner`, ort 2.0.0-rc.12, CPU EP) sits behind the
  `align` cargo feature — the pattern `detect::arousal` set.
- **`ensemble::fuse_onto_alignment`** — units straight from per-word spans:
  an aligned word IS its span (no anchor adoption, no DP, no respread — the
  audio said so); a run of unaligned words (all-OOV: digits) lays onto RMS
  onsets inside the gap between aligned neighbors via the existing
  `place_run`. The `at_s` positional store pass runs after it unchanged —
  operator pins stay the override for the duplicated-common-word residual.
- **The knob**: `YC_FORCED_ALIGN=1` (parsed like the other env knobs, read
  INSIDE `ensemble::apply` so a render and any diag cannot diverge — the
  ADR 0033 discipline). Double-gated: the `align` feature compiles it (now
  in build-release.bat: `--features face,align`), the env arms it. ANY
  aligner failure — model missing, session error, infeasible path, under
  half the words aligned — warns and falls back to the DTW fusion; captions
  never go missing because the aligner did. `EnsembleConfig.align_model`
  carries the model dir (pipeline: `models/w2v2-align-id`).
- **Harnesses**: `align_parity` (Viterbi-vs-torchaudio span equality on the
  dumped reference emission + full Rust-chain onset parity on the spike's
  inputs) and `caption_align_diag` (the gate instrument: one production
  decode set, both fusions side by side — GT onsets, cross-method shift
  distribution, post-refine dwell; `YC_ALIGN_EMIT=1` re-runs the REAL
  `ensemble::apply` with the flag for the burn artifact).

## The input contract is RAW samples (measured, deliberate)

The model card's processor says `do_normalize: true`; **the spike fed raw
samples** — and the operator approved the spike's burn. Measured both on
clip 3 (torch, 111 words, 2026-07-11):

```
                    torch-raw (spike)   torch-normalized
SIAPA  (gt 5.0)     5.08                5.08
SDC    (gt 23.0)    22.93               22.93
GUE    (gt 28.0)    29.91 (the miss)    28.29 (fixed)
PINGUIN(gt 52.0)    52.76               50.60 (NEW miss - dup grab)
max onset delta vs raw: 3.32 s, mean 0.13 s
```

Normalization trades one duplicated-common-word miss for another — no
dominance, so parity with the operator-approved burn wins (the ADR 0050
lesson: the eye gated RAW). The export script and the module doc both pin
this so nobody "fixes" it to match the model card.

## Parity (the port is exact, not approximate)

- ONNX-vs-torch logits on clip 3's full 61 s emission: max abs diff
  5.1e-4, mean 1.7e-5 (fp32 kernel noise).
- **Rust Viterbi vs torchaudio `forced_align`+`merge_tokens` on the same
  emission: 650/650 target spans IDENTICAL.**
- **Full Rust chain (ort session -> log-softmax -> Viterbi) vs the Python
  reference: worst per-word onset delta 0.0000 s across all 111 words**
  — the spike's operator-approved numbers reproduce bit-exactly through
  the shipped code path (SIAPA 5.08, SDC 22.93, OTOT 6.80, KREATIN 8.82,
  FADIL 32.77, PINGUIN 52.76, GUE 29.91, JALANANNYA 53.68).

## The gate (production decode, both fusions on the SAME voted words)

`caption_align_diag` decodes a clip ONCE on the production path (whisper +
suppress_nst + the 5 qwen variants + the real vote) and fuses the same
merged words both ways — the timing swap isolated from every other lane.

**Clip 3** (VIOR fans Fadhil, 4-person overlap — the defect clip; fresh
production decode, 117 merged words; the operator's by-ear onsets as truth):

```
word         heard    DTW skeleton     forced-align
siapa          5.0     3.46 (-1.54)     5.08 (+0.08)
otot           6.5     6.81 (+0.31)     6.80 (+0.30)
kreatin        8.5     8.56 (+0.06)     8.82 (+0.32)
sdc           23.0    20.78 (-2.22)    22.93 (-0.07)
gue           28.0    27.50 (-0.50)    28.55 (+0.55)
fadil         32.0    31.32 (-0.68)    32.77 (+0.77)
pinguin       52.0    50.56 (-1.44)*   50.60 (-1.40)*
jalanannya    54.0      MISSING*         MISSING*
```

\* text lane, identical both sides: TODAY'S vote kept only the ~50.5 s
`pinguin` (which belongs there) and dropped `jalanannya` — the recall/
version-drift lane (ADR 0052); timing cannot invent words. On the words the
vote kept: mis-onset (>0.5 s) 4/8 -> 3/8, and **the catastrophic class
(>1.5 s early, the one the operator hand-pinned every clip) goes to
±0.1 s automatically**. 117/117 words aligned (25.3 s CPU, debug build).
Cross-method shift over all 117 words: median 0.11 s, p90 0.79 s — most
words barely move; the movers are the drift class. Post-refine dwell 60% ->
64% sub-0.40 s (word-unit basis; the too-fast lane is ADR 0049 roadmap #2,
untouched by this slice). The `YC_ALIGN_EMIT=1` re-run through the REAL
`ensemble::apply` came out **dialogue-identical** to the instrument's
fusion (decode determinism held) — the burn artifact is production output:
`_forced-align clip3 (RUST PORT - production).mp4`, judged against
`_recall-lane clip3 (fresh current-code ensemble).mp4` (same words, DTW
timing). **Operator-eye gate: PENDING (they were asleep; first thing next
session).**

**ANTITESA "rebalance investasi"** (2-person turn-taking control, ADR 0049's
"good" regime; 184 merged words): median |shift| **0.08 s**, p90 **0.35 s**,
>0.5 s movers 9/184 (4.9 %), max 2.95 s; 184/184 aligned (31.0 s). Dwell
77 % -> 74 % sub-0.40 s (word-unit basis — NOT comparable to ADR 0049's
0-2 % which counted the shipped clip's karaoke LINE cues; the control here
is the relative change, which does not regress). The turn-taking timing
signature is preserved; the named movers for the ear at the flip gate:
`INI BIKIN ORANG IRI TERUS` (17-20 s, pulled 1-2 s earlier), `TAPI`
38.7->35.7, and `TADI`/`NANTI` +0.5-1.0 s (the soft-onset late-bias class
ADR 0053 predicted).

## Drawbacks / notes

- **The GUE class stands** (duplicated common word grabs a neighbor
  occurrence, +1.9 s in the spike) — `at_s` pins remain the override;
  they apply after the fusion on both engines (ADR 0051 parity).
- **Text lane unchanged**: alignment times the words the vote KEPT.
  A word the vote dropped (the recall lane, ADR 0052 — today's decode
  lacks `jalanannya` and the second `pinguin` the shipped clip's vote had)
  cannot be timed into existence. Version drift between ensemble runs is
  the same standing issue the recall ADR recorded.
- **CPU emission cost** per ~60 s clip: ~38 s debug / measured release in
  the gate table below — next to the ensemble's five sidecar decodes it is
  the cheap half, and the operator's standing directive is best accuracy
  over speed. CUDA EP is a later option (the ort runtime already ships).
- The ~1.3 GB model raises the models/ footprint; fetch is operator-initiated
  (Offline constraint holds).

## Consequences

- With the flag on, whisper's timing authority on the ensemble path ends:
  DTW t0/t1 feed nothing (whisper still contributes WORDS to the vote and
  remains the whole whisper-engine path). The suppress_nst second decode
  becomes dead weight under the flag — removing it is part of the
  default-flip slice, not this one (byte-identical-off discipline).
- The mis-onset lane (ADR 0051 hand-pins) and the recall lane's mis-timing
  residual (ADR 0052) collapse into one automatic mechanism if the default
  flips — hand-pinning becomes the exception (exactly ADR 0053's thesis).
- The default flip is gated on the operator's EYE over the re-burn
  (`_forced-align clip3 (RUST PORT - production).mp4`) + more clips,
  per the standing gate-on-the-burn rule.
