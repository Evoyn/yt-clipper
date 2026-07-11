# Forced alignment becomes the ensemble's DEFAULT timing skeleton — `YC_FORCED_ALIGN=0` is the off-switch; the dead `suppress_nst` decode is skipped when the aligner runs

ADR 0054 shipped the native wav2vec2-CTC forced aligner opt-in and the
operator's eye passed the burn gate the same morning ("timing for rust port
is right", 2026-07-11). This ADR records Phase 2: the default flip, the
GPU-decode saving that falls out of it, the cross-clip widening evidence,
and the pin-over-aligner check.

## The ordering decision (taken under delegated authority)

The nextprompt posed flip-now-recall-next vs recall-first. The operator
delegated the session ("do this automatically"); the RECOMMENDED order was
taken: **flip now, recall lane next**. Rationale: the siapa/SDC drift class
— the one the operator hand-pinned every clip — fixes on every ensemble
render immediately; the garble-float residual (ADR 0054 §garble-float) is
rare, existed under DTW too (hidden inside the token hole), and its
structural fix is the recall lane regardless of ordering; the text drops
exist on both engines regardless. Nothing about flipping first makes the
recall work harder. If the operator disagrees, `YC_FORCED_ALIGN=0` restores
the previous behavior exactly, render-for-render, while the code is
re-ordered.

## What changed

- **The knob polarity inverted** (`ensemble::forced_align_from`): unset =
  ON; only an explicit `0`/`false`/`off` disables. The pre-flip opt-in
  spellings (`1`/`true`/`on`) still read ON, so any operator script keeps
  working. The knob is still read INSIDE `ensemble::apply` (ADR 0033
  discipline — a render and a diag cannot diverge). The truth table is
  unit-pinned (`forced_align_flag_defaults_on_with_an_off_switch`); unset
  parses identically to `1`, so the emit-arm gate runs (which pin `1` so an
  operator-set `0` can't blank a burn) exercise byte-for-byte the `apply`
  branch a knob-less render takes.
- **The byte-identity contract MOVED, by design**: whisper-engine renders
  are untouched (the flip lives entirely in the ensemble block);
  ensemble-engine renders on an `align` build with the fetched model now
  time by forced alignment — that change is the POINT, and it is what the
  operator's eye approved on the burn. Ensemble renders on a build WITHOUT
  the `align` feature, or without the model, keep the DTW fusion
  byte-identical to pre-flip (measured contract, see the matrix below).
- **The `suppress_nst` second whisper decode is SKIPPED when alignment will
  run** (`pipeline.rs`): under the aligner, that decode fed nothing — a
  whole GPU decode per ensemble render, pure waste. The new
  `ensemble::forced_align_active(align_model)` predicate (knob + `align`
  feature + `model.onnx` present — exactly the preconditions
  `forced_align_fusion` checks before touching the session) gates it, so
  the old DTW behavior is preserved bit-for-bit whenever DTW will actually
  be the skeleton.
- `caption_align_diag` grew optional extra dialect-store layers (production
  order: creator store, then per-clip store) so `at_s` pins apply in the
  instrument exactly as on a render, and its `YC_ALIGN_EMIT=1` arm now
  hands `apply` the same `timing_extra = None` production does when the
  aligner is active.

## The state matrix (what runs, what the captions are)

```
build      model    YC_FORCED_ALIGN   suppress_nst decode   timing skeleton
align      present  unset (default)   SKIPPED               forced alignment
align      present  0                 runs                  DTW (pre-flip, exact)
align      missing  unset             runs                  DTW (pre-flip, exact) + warn
no-align   -        unset             runs                  DTW (pre-flip, exact) + warn
align      present  unset, RUNTIME    skipped (already)     DTW on the default whisper
                    aligner failure                         skeleton alone - degraded,
                                                            rare (spike: 0/111 infeasible;
                                                            gate: 117/117, 184/184 aligned),
                                                            captions never missing
```

The one deliberate trade: a RUNTIME aligner failure (session error,
infeasible path, <50% aligned) lands on a DTW fusion without the
`suppress_nst` skeleton, because that decode was skipped before `apply` ran.
Predicting a runtime failure would mean always paying the decode, which is
exactly the waste the flip removes; the failure class never fired across
the gate corpus and fails soft (warn + captions on the default skeleton).

## Cross-clip widening (the gate instrument, production decodes, 2026-07-11)

Fresh `caption_align_diag` runs, one production decode set per clip, both
fusions on the SAME voted words. With ADR 0054's ANTITESA control the
corpus now spans solo / 2-person / 3-person / 4-person and both caption
genres (huge-word + karaoke_fill Creators):

```
clip                          regime            aligned    shift med/p90/max      dwell
VIOR fans Fadhil (4p)         overlap (defect)  117/117    0.11 / 0.62 / 4.86 s   60->63%
Deddy Tretan/Coki (3p)        overlap (densest) 211/211    0.08 / 0.48 / 2.54 s   80->80%
guru gembul (solo, karaoke)   turn-taking       164/164    0.09 / 0.28 / 0.66 s   71->73%
ANTITESA (2p) [ADR 0054]      turn-taking       184/184    0.08 / 0.35 / 2.95 s   77->74%
```

The signature is consistent everywhere: the bulk of words barely move
(median <=0.11 s on every clip), and the movers concentrate where DTW
drifts — the overlap clips carry the big re-anchors (max 2.5-4.9 s, the
SIAPA/SDC class), while the turn-taking clips stay put (guru gembul max
mover 0.66 s: the aligner agrees wherever timing was already right — the
no-regression bar). Dwell (word-unit basis) never regresses beyond noise.
Clip 3's gt table reproduced the ADR 0054 gate exactly (siapa +0.08, sdc
−0.07; mis-onset 4/8 → 2/8 with the pin; the residual pinguin/jalanannya
rows are the recall lane on both engines). `clip_dtw.ass`/`clip_align.ass` sit
beside each clip's analysis.wav for the operator's eye whenever they want
a look.

## The pin still wins over the aligner (the GUE class)

`apply_store_positional` runs after the aligned fusion unchanged; the
existing unit test covers the mechanics, and an end-to-end check on clip 3
ran a REAL pin through the production fusion: a `gue -> gue` pure timing
pin at VOD 3620.0 (operator's ear: the word is at ~28.0 clip-relative; the
aligner's duplicated-common-word grab leaves it +0.55 s) now sits in the
clip's per-clip store (`clip_only: true` — it never promotes; delete it if
unwanted). Measured (2026-07-11, the instrument with the store layered in,
both fusion arms):

```
at_s pin "gue"@28.0s -> "gue" at 28.12s (was 27.50s)   [DTW arm]
at_s pin "gue"@28.0s -> "gue" at 28.12s (was 28.55s)   [ALIGN arm]
```

28.12 s is the real speech onset ADR 0051 measured; the pin snapped both
skeletons onto it, overriding the aligner's residual exactly as designed.
The gt table's `gue` row reads +0.12 on both sides with the pin in play
(vs −0.50 DTW / +0.55 ALIGN without it, the ADR 0054 gate table). The
`YC_ALIGN_EMIT` real-`apply` burn (fresh 5-decode run, 117 cues) came out
dialogue-identical to the instrument's ALIGN arm on 116/117 cues; the one
delta IS the pinned GUE at 28.14 vs 28.12 — production snaps the pin on
its cleaned-onset grid, the instrument arms on the mix grid, 20 ms (one
envelope hop) apart on the same speech onset. `clip_alignburn.ass` beside
clip 3's analysis.wav is current (pin included) if the operator wants the
GUE fix on glass.

## Karaoke / rolling builders (genre path)

Read against aligned span shapes: `group_lines` keys on inter-unit gaps
(honest gaps are what the aligner produces — line-splitting behavior
preserved), `karaoke_fill_events` dwells `\k` from onset to next onset with
a 1 cs floor (zero-width spans advance, gaps are dwelled across — the snap
lands ON the spoken moment), `rolling_pop_events` keys pops off
line-relative onsets. No builder assumes abutting DTW spans; aligned units
are strictly better-formed (non-overlapping, monotonic by CTC
construction). New unit test `karaoke_snap_handles_forced_alignment_span_shapes`
pins the gap-dwell + zero-width-floor behavior. The guru gembul diag run
above covers the karaoke-genre audio regime (solo lecture, ADR 0049's
"good" row).

## Consequences

- Hand-pinning mis-onsets stops being the workflow (ADR 0053's thesis lands
  for real renders); `at_s` pins remain the override for the duplicated
  common-word residual, applied after fusion on both engines (ADR 0051).
- Every ensemble render saves one whisper GPU decode; the aligner adds its
  emission cost (CPU EP; ADR 0054's numbers — the cheap half next to the
  five sidecar decodes; CUDA EP remains a later option).
- The garble-float (ADR 0054 §garble-float) and the pinguin/jalanannya
  drops ride along unchanged — they are the RECALL lane (ADR 0052), the
  next slice; re-measure the 51-56 s stretch of clip 3 when it lands.
- `caption_recall_diag` still reproduces the DTW placement for its
  loss-localization (word PRESENCE is fusion-independent); timing questions
  belong to `caption_align_diag`.
- The off-switch is the field escape hatch: `YC_FORCED_ALIGN=0` restores
  pre-flip ensemble renders exactly (the suppress_nst decode included).
