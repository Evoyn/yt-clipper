# Whisper-engine forced alignment: measured, and REFUSED at its own bar — without a vote, the aligner amplifies whisper's token-list errors (lane 1's prerequisite is lane 2)

The operator's directive (2026-07-12): stop polishing single clips — fix
captions so the fix applies to **all future videos**, with the most accurate
timing available. The wav2vec2-CTC forced aligner is the production default
timing source on the **ensemble** path (ADR 0054/0055, eye-approved on the
burn), but the **whisper** engine — Helmy Yahya Bicara, "local", and every
NEW Creator's default — still times captions from whisper's own DTW onsets
(ADR 0013/0018/0019/0021). ADR 0051's stage attribution proved the mis-onset
defect class is whisper's DTW onset and reproduces on the whisper engine, so
the queued lane 1 (nextprompt-caption-general.md) was: wire the same shipped
aligner into the whisper-only caption path.

This ADR records the measure-first outcome: **the wiring was built, measured
against pre-registered bars on the cross-clip fixture corpus, FAILED its
defect-clip bar, and was NOT shipped.** The instrument and the production
entry point stay in-tree as measurement fuel; every render is byte-identical
to before. Same discipline as ADR 0051's refuted auto-re-anchor and ADR
0049's drop bar: a plausible plan killed by measurement, kept as the record.

Session discipline: the operator delegated ("do this automatically"); the
recorded rubric picked lane 1. The bars below were committed to this file
BEFORE the first fixture run (the ADR 0049 pattern).

## What was built (in-tree, production-inert)

- **`ensemble::forced_align_retime`** — the whisper-path entry point: same
  texts, one unit per input unit (harvest `unit_index`es stay valid), times
  from the CTC aligner via the ensemble's own `forced_align_fusion` +
  `fuse_onto_alignment`. Gates, any miss ⇒ `None` ⇒ the caller's DTW spans
  stand: the shared `YC_FORCED_ALIGN` knob (read inside, ADR 0033),
  non-empty units, `Language::Id` (the pinned model is `w2v2-align-id`),
  then the fusion's own guards (`align` feature, model present, session,
  feasibility, ≥ half aligned). `forced_align_fusion` now takes the model
  dir instead of the whole `EnsembleConfig`, so both engines' entry points
  share it verbatim.
- **`whisper_align_diag`** (crates/app example, `align`-gated) — ONE
  production whisper decode per fixture (layered stores in production
  order), then both timing arms through the production downstream (`at_s`
  pins; `refine_caption_timing` with the silence-drop ARMED — whisper words
  are one decoder's unverified guess, the `keep_verified` rationale does
  not transfer): DTW arm = whisper's spans verbatim; ALIGN arm =
  `forced_align_retime`. Prints the gt table, the cross-arm shift
  distribution with every > 0.5 s mover NAMED, the silence-drop delta word
  by word, dwell stats; writes `clip_wdtw.ass` / `clip_walign.ass` beside
  the wav.
- The planned pipeline placement (recorded for a future wiring): in
  `ensure_transcript` after the LLM correct pass, before the harvest filter
  (outcomes must reflect the timing that renders), before pins (which
  override the aligner, ADR 0051/0055) and refine. A `NOT wired` comment
  sits at that exact spot in pipeline.rs.

## The PRE-REGISTERED bars (committed before the first run)

Fixtures — the ADR 0049/0055 cross-clip corpus ON the whisper engine, plus
the one whisper-native Creator:

```
clip                       range           regime               gt
VIOR fans Fadhil (4p)      3592.0-3653.0   overlap (defect)     8 by-ear onsets
Deddy Tretan/Coki (3p)     1559.6-1629.7   overlap (densest)    -
guru gembul Eps1034 (solo) 368.1-427.9     turn-taking karaoke  -
ANTITESA (2p)              790.0-859.0     turn-taking          -
Helmy Yahya (whisper-native Creator) 1369.0-1413.0 turn-taking  -
```

- **W1 (defect fix, clip 3 gt)**: ALIGN mis-onset count (|onset−heard| >
  0.5 s) ≤ DTW's, and the catastrophic class (> 1.5 s early — the hand-pin
  class) → 0 among words the aligner places.
- **W2 (no-regression, turn-taking controls)**: cross-arm onset shift
  median ≤ 0.15 s; movers in family with ADR 0055 (p90 0.28–0.62 s); dwell
  %< 0.40 s within ±5 points (word-unit basis).
- **W3 (structural, every clip)**: texts byte-identical 1:1 pre-refine;
  aligned coverage ≥ 90 %.
- **W4 (hallucination guard)**: silence-drop delta = 0 on turn-taking
  controls; every overlap-clip delta listed and judged against the gt — no
  gt-named phantom rescued.

## Measured (2026-07-12, release build, idle GPU, one production decode per clip)

```
clip                       units  aligned  align-CPU  shift med/p90/max   >0.5s  drops D->A  dwell D->A
VIOR fans Fadhil (4p)      101    101/101  28.0s      0.06 / 1.45 / 3.79   21    0 -> 0      54% -> 54%
Deddy Tretan/Coki (3p)     204    204/204  34.8s      0.04 / 0.13 / 1.23    6    0 -> 0      76% -> 77%
guru gembul (solo)         148    148/148  19.6s      0.03 / 0.09 / 0.57    2    0 -> 0      66% -> 66%
ANTITESA (2p)              178    178/178  25.3s      0.03 / 0.11 / 3.19    3    0 -> 0      70% -> 67%
Helmy Yahya (whisper)       83     83/83   17.5s      0.04 / 0.13 / 1.24    2    0 -> 0      47% -> 49%
```

- **W2 PASSED.** Turn-taking medians 0.03–0.04 s; every mover named and
  explained: ANTITESA's `TAPI 38.92→35.73` is bit-identical to the ADR 0054
  ensemble mover the operator's eye already passed at the flip gate; Helmy's
  `kita 9.58→10.82` and guru gembul's two `jadi`s are the DTW-smear
  un-smears; dwell moves ≤ 3 points.
- **W3 PASSED.** 714/714 words aligned across the corpus; texts byte-equal
  in every arm pair.
- **W4 PASSED.** Silence-drop deltas are ZERO on every clip (no rescue, no
  new drop anywhere); the `ya`@12 phantom class is untouched in both arms.
- **W1 FAILED.** Clip 3 gt: mis-onset 6/8 → 6/8 (clause 1 ties), and the
  catastrophic-early class does NOT zero — `sdc` −2.22 → **−2.51**:

```
word         heard    DTW-onset       ALIGN-onset
siapa          5.0     3.66 (-1.34)    6.46 (+1.46)
sdc           23.0    20.78 (-2.22)   20.49 (-2.51)   <- catastrophic stays
gue           28.0    29.86 (+1.86)   29.91 (+1.91)
fadil         32.0    31.32 (-0.68)   32.78 (+0.78)
pinguin       52.0    50.58 (-1.42)   50.60 (-1.40)   <- recall-lane row, both engines
jalanannya    54.0    53.48 (-0.52)   52.76 (-1.24)   <- known aligner residual (ADR 0056: ear wants 54.4)
otot/kreatin           MISSING         MISSING        <- never transcribed (recall lane)
```

## The mechanism (why the ensemble's win does not transfer)

Where whisper's tokens MATCH the vote's, the whisper-engine aligner lands
**bit-identical** onsets to the eye-approved ensemble runs — fadil 32.77/8,
pinguin 50.60, jalanannya 52.76 reproduce ADR 0054/0055/0056's values
exactly. The aligner is deterministic on the same audio; the difference is
entirely the WORD LIST.

And whisper's own word list on the defect clip is pathological in a way the
vote's is not: whisper transcribed the brand **twice** (`SDC`@20.8 AND
`Susu Deddy Corp`@24–25) and doubled the opening phrase. CTC alignment is
monotonic over the token order, so the duplicated brand forces the whole
stretch one slot early — `Susu Deddy Corp` dragged to 21.35–23.79 (onto the
real SDC's audio), `SDC` itself pushed onto the dropped "siapa tau" at
20.49, the opening pile re-ordered (`coba aku bawa` onto ~5 s where the ear
wants `siapa`). ADR 0054's §garble-float named this class at single-word
scale ("the aligner is faithful to its token list; the token list is
wrong"); on a vote-less engine it operates at PHRASE scale, on the headline
words. Deddy 3p — denser overlap but token-clean — is immaculate (median
0.04 s), which pins the cause to token pathology, not the overlap regime
per se.

**The ensemble's aligner win is a SYSTEM property: the vote cleans the
words, THEN alignment can trust them.** Wiring the aligner under whisper's
unvoted words ships the amplifier without the cleaner.

## Consequences

- **Nothing ships to production**: whisper renders keep whisper's own DTW
  spans, byte-identical; the ensemble path is untouched (its
  `forced_align_fusion` merely takes `Option<&Path>` now). Suites green
  (360 workspace default + 30 face,align app + 98 transcribe both flavors).
- `forced_align_retime` + `whisper_align_diag` stay in-tree: the measured
  entry point and its instrument, re-runnable in minutes (17–35 s CPU
  alignment per clip, release) when either unlock below lands.
- **The two unlocks, for the operator to order** (recorded in the handoff):
  1. **Lane 2 (whisper-engine recall/witness/vote)** — any mechanism that
     cleans whisper's token list makes this slice's wiring safe to
     re-measure; the pre-registered bars and instrument are ready.
  2. **The engine-default policy call** — defaulting NEW Creators to the
     ensemble sidesteps the whole class (cost: 5 sidecar decodes + models
     per clip; ADR 0033/0035 made switching deliberate, so this is the
     operator's call, not a session's).
- The turn-taking population (today's actual whisper Creators) measured
  neutral-to-better under alignment — the un-smear class is real (Helmy's
  `kita`, ANTITESA's eye-passed `TAPI`). If the operator wants that small
  win before an unlock lands, it is a one-line wire behind the existing
  gates — but it ships the amplifier for any future overlap import, which
  is exactly what W1 measured. Their call, on the record.
- The gt file's `pinguin` row measures the recall lane on both engines
  (the ~50.6 cue is a different, correctly-placed utterance; the @52 one
  was never transcribed) — carried unchanged from ADR 0055's footnote.
