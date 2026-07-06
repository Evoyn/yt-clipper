# Laughter-class instrument spike: an AudioSet tagger through ort — bars declared first; FAIL on the stale annotation, PASS on the ear-corrected truth (gate PASSED)

ADR 0044 shipped the honest person join and named this spike's job: the two
good-looking Deddy splits (14.2–22.1, 27.0–30.2) reverted to solos because
their off-screen fuel was proven false, and the three speech-domain
replacement discriminators were measured dead (contested share 74–100% on
EVERY segment; absence ≤35%; cluster composition inverted). The splits
return only via a real acoustic instrument for the **Shared reaction**
class (CONTEXT.md). This ADR records that instrument's spike: choice,
pre-declared bars, conventions pinned by selftest, and the measured
verdict. Harness-only — zero production behavior change; the split grammar
is untouched.

## The instrument (settled in the session grill)

- **Chosen: a pinned AudioSet-class audio-event tagger through the existing
  `ort` stack** — the only candidate that tags laughter *directly*
  ("Laughter" + Baby laughter / Giggle / Snicker / Belly laugh /
  Chuckle-chortle are literal AudioSet classes) instead of deriving it from
  speech-domain evidence, the path ADR 0044 measured dead. Every shipped
  instrument here is a learned model behind a measured frontend from a
  pinned source (whisper, Silero, CAM++, YuNet+SFace, w2v2 SER); this one
  follows.
- **Model**: icefall Zipformer-M audio tagger (527 classes, Apache-2.0)
  from the sherpa-onnx `audio-tagging-models` release — the CAM++ sourcing
  pattern. Picked over the same release's CED exports because its input is
  `(x [N,T,80] Kaldi fbank, x_lens)`: the EXISTING byte-validated `Fbank`
  frontend feeds it directly (CED wants a 64-mel torchaudio-style frontend
  — new matching work not paid speculatively). Asset
  `sherpa-onnx-zipformer-audio-tagging-2024-04-09.tar.bz2`, SHA-256
  `6c89b86c3d4812520e6937316d9aff944458871ec037dd47cf33ae2034b5eb54` (the
  live asset, matching the GitHub API digest; **the release's checksum.txt
  lists a stale pre-re-upload hash** `8d786db8…` — k2-fsa re-uploads into
  living releases). Installed `model.onnx` SHA-256 `a8f11014…`, labels CSV
  `cdd10498…`. Spike-only install: NO Diagnostics registry row, NO
  download_specs entry — ADR 0041 wiring happens only if the class ships.
- **Rejected: whisper non-speech tokens** — "(laughs)" is a learned
  subtitle artifact, unreliable and default-suppressed, with no honest
  0.25 s timing, and whisper doesn't run at analyze time.
- **Rejected: hand-rolled spectral heuristics** (bout periodicity,
  voiced/unvoiced alternation) — thresholds hand-tuned on the same fixture
  that gates them prove nothing; tune-to-answer by construction.
- **Not a candidate: the w2v2 arousal model** already in the registry —
  Arousal deliberately ignores *which* emotion; it cannot tell shared
  laughter from excited storytelling.

## Conventions pinned by selftest, not assumed (Bar 0)

`speaker_diag tagselftest` scored the release's own 13 test wavs under both
fbank sample scales and printed the raw output ranges:

- **`SampleScale::Unit`** reproduces the published reference outputs
  rank-perfect on all 13 wavs (4.wav: **Laughter 0.86** with the family
  beneath; Cat 0.95 vs published 0.939; Water/Stream; Oink; Siren; Meow),
  while `Int16` audibly breaks them (the laughter wav tags Music/Singing,
  the stream tags Engine) — the icefall/lhotse [-1, 1] convention, and a
  strong negative control: the frontend convention genuinely matters.
- **The export ends in a sigmoid**: raw outputs live in exactly [0, 1]
  with hard zeros on absent classes and match the published probabilities
  nearly digit-for-digit. `TagOutput::Probs` is pinned so the lane never
  double-sigmoids (the first cut did; the selftest's raw-range printout
  caught it — that printout exists for exactly this).
- No CMN: an event tagger's evidence includes absolute level, which CMN
  would erase. **Bar 0: PASS.**

## The pre-declared bars (pinned in the grill, before any code)

One laughter-family score (max over the six classes) per 0.25 s step, each
step scored from the 2.0 s window centered on it, over the SAME analysis.wav
samples the voice lane embeds, VAD-independent. At ONE tau from the
pre-declared grid {0.1, 0.2, 0.3, 0.4, 0.5}:

- **Bar A (hit)**: ≥ 50% of steps in the known laughter stretch
  (annotated 22.1–27.0 from the ADR 0044 session record) score ≥ tau.
- **Bar B (clean)**: ≤ 10% of steps in EVERY monologue segment (inter-cut
  segments outside the target and ex-split spans, ≥ 1 s voiced) at the
  SAME tau; a breach gets an operator ear-check — real laughter there is
  corroboration, not a false positive.
- **Bar C (gap)**: target mass ≥ 5× the worst monologue segment's.

## Measured on the Deddy fixture (thr grid verbatim from the harness)

```
tau 0.1: target 25% (5/20) A[FAIL] | worst mono 0% B[pass] | gap inf C[pass]
tau 0.2: target 20% (4/20) A[FAIL] | worst mono 0% B[pass] | gap inf C[pass]
tau 0.3: target 15% (3/20) A[FAIL] | worst mono 0% B[pass] | gap inf C[pass]
tau 0.4: target 15% (3/20) A[FAIL] | worst mono 0% B[pass] | gap inf C[pass]
tau 0.5: target 10% (2/20) A[FAIL] | worst mono 0% B[pass] | gap inf C[pass]
BARS: FAIL — no tau on the declared grid meets A(>=50%) B(<=10%) C(gap >=5x)
```

- **The clean side is perfect**: every monologue segment carries 0%
  laughter mass at every tau — six segments, zero false steps. The control
  segment hears `Speech 1.00 | Narration, monologue 0.03`. On the second
  fixture (ANTITESA) the lane is equally quiet-and-localized, and the fg
  byte-pin `9e07d81f…` held (structurally guaranteed — the lane feeds
  nothing — and verified anyway).
- **The hit side fails on the ANNOTATED span** — and the mask says why:
  at tau 0.3 the laughter mass sits at `25.50–26.00, 26.75–27.25,
  28.00–28.75, 29.75–30.25` — the tail of the annotated stretch plus the
  first three seconds of seg5 (27.0–30.2), which carries **46%** mass
  against the annotated seg4's 15%. The whole-span aggregate hears
  `Speech 1.00 … Laughter 0.00`: 22.1–25.5 is acoustically SPEECH.
- **The annotation itself looks misplaced, by ADR 0044's own words**: it
  described "the laughter segment, whose voiced time fragments into
  V4/V8/V9 scraps" — and today's evidence table shows the V4/V8/V9
  segment is seg5 = **27.0–30.2** (V4 49% / V8 25% / V9 18%), not the
  22.1–27.0 span the session record carried forward. The instrument's mask
  agrees with the ADR's description against the recorded span. The ear
  rules (protocol: the 20.8 s precedent — annotations are "heard",
  instruments must be "proven"; ear-check wavs of both spans were cut for
  the ruling).
- Bars are judged only when the operator supplies the ear-truth spans
  (`YC_LAUGH_TARGET`, `YC_LAUGH_EXSPLIT`); the harness otherwise prints
  the mask and per-segment laugh means as plain forensics.
- **The ear-check ruled: the annotation was wrong, the instrument right.**
  The operator listened to both spans cut from the fixture's own
  analysis.wav and confirmed `25.5–30.25` is the group laughter and
  `22.1–25.5` is speech (someone talking through the grins — mouths move,
  which is why the CONTESTED share was high there; grinning is visual, not
  acoustic). Second annotation overturned by an instrument on this fixture
  (after the 20.8 s off-screen call, ADR 0044); the corrected target was
  re-declared (`YC_LAUGH_TARGET=25.5-30.25`, ex-split finding span
  `14.2-22.1`) and the SAME bars re-judged — no bar, tau, or model
  parameter moved, only the ear-truth.

## Measured again on the ear-corrected span (same bars, same grid, same model)

```
tau 0.1: target 89% (17/19) A[pass] | worst mono 0% B[pass] | gap inf C[pass]  << ALL BARS MET
tau 0.2: target 63% (12/19) A[pass] | worst mono 0% B[pass] | gap inf C[pass]  << ALL BARS MET
tau 0.3: target 47% (9/19)  A[FAIL] | worst mono 0% B[pass] | gap inf C[pass]
BARS: PASS at tau 0.1
```

- Mask at tau 0.1: `25.25–26.25, 26.75–30.50` — near-solid coverage of the
  confirmed stretch. The ex-split piece 14.2–22.1 carries **0% (0/31)**:
  that piece has NO laughter fuel — whatever wins it back, it is not this
  class, stated plainly.
- Calibration texture (load-bearing for the grammar): real overlapped
  podcast laughter scores 0.1–0.3 per 2 s step (the clean solo-laughter
  test wav scores 0.86) — bursts among speech, not a wall. The honest
  operating point is LOW tau over the zero-noise floor (B held 0% at
  every tau on both fixtures — the gap is infinite; discrimination is
  perfect while absolute probabilities are modest).
- Person-join bars held through every run of the session: occupant map
  4 + 2 at cut 0.40, exactly one VALID edge `V2 x cam0 -> P1`, off-screen
  1.5 s, camera audit clean, 15 shots with 0 differing from mouth-only.

## Operator verdict (2026-07-06, same session): gate PASSED — queue the wiring slice

The operator ear-checked both spans (annotation corrected on their ear, the
heard-vs-proven protocol), accepted the corrected-span re-judge, and ruled
the gate PASSED with the production wiring queued as the next slice: the
shared-reaction mask beside the Voice lane, split-grammar consumption
designed from the burst evidence above, the tagger's ADR 0041 registry row
+ DownloadSpec, and a fresh render-gate artifact on the operator's eyes.
Nothing ships this session.

## Consequences

- The `yc_frame::reaction` module exists (pure grid/labels/mass machinery
  unit-tested; `TagSession` behind the `voice` feature) but NOTHING in the
  production path calls it — the Studio, the planner, and both fixtures'
  plans are byte-identical to before (0 shots differ; ANTITESA fg pin
  `9e07d81f…` intact; suites green both ways, 334 face / 268 non-face).
- The Deddy laughter annotation of record is now **25.5–30.25 s**
  (clip-relative); the 22.1–27.0 span in the ADR 0044 session record is
  DEAD — it was a mis-transcription of the segment table (the V4/V8/V9
  fragmentation segment was always 27.0–30.2). Third instrument-over-ear
  correction on this fixture; do not tune toward the stale span.
- The wiring slice's grammar threshold must be derived from burst evidence
  (masked seconds / low-tau mass), not wall evidence — and must NOT expect
  the 14.2–22.1 piece to change via this class (0% measured).
- The stale-checksum finding is recorded: pin verification for k2-fsa
  living releases must use the live asset hash (GitHub API digest), not
  checksum.txt alone.
