# Caption-overlap instrument + measured geography: four failure classes, all overlap-specific — bars declared first, decided on the cross-clip control (gate PASSED)

The operator watched the shipped VIOR clips (a 4-person podcast) and found the
captions **not good enough when several people talk at once** (2026-07-07):
some cues delayed/lead, some words dropped, some flash too fast. For
**turn-taking** speech — 2 OR 4 people, as long as they speak in turn — the
captions are good. So the defect is the **simultaneous-speech (overlap)
regime**, not speaker count. The solo-presence work (ADR 0047/0048) touched
zero caption/transcribe code — this is a pre-existing quality gap, freshly
noticed on real 4-person material. This ADR records the measure-first
instrument (the ADR 0045 pattern for captions), the pinned bars, the measured
verdict, the operator's ruling, and the fix roadmap. **Harness-only — zero
production behavior change; the caption path is byte-untouched.**

## The instrument (settled in the session grill)

`caption_overlap_diag` (`crates/render/examples/`) — a pure, GPU-free, fully
deterministic join over three artifacts that already exist:

1. **the shipped `clip.ass`** — the cues that actually burned (start / end /
   text). Measuring the SHIPPED output (not a re-decode) attributes the FINAL
   defect the operator saw, and the ensemble is deterministic (temp 0) so a
   re-decode would only reproduce it more slowly.
2. **a `speaker_diag` per-bin CSV** — the overlap signal already computed over
   the SAME `analysis.wav`: per-track mouth activity (→ **contested** = ≥2
   mouths over `speaker::MIN_ACTIVITY`, voice.rs:550) and the reaction/`laugh`
   mask (ADR 0045/0046). No re-run needed — the `Nact` + `laugh` columns of the
   existing dump carry it; the tool derives contested itself.
3. **the operator ground truth** —
   `benchmarks/vior-fans-fadhil.captions.groundtruth.txt`, the 19 by-ear items
   the operator named with clip-relative times (their ear is the bar).

- **Rejected: re-running whisper/the ensemble** to measure. It is slow
  (5 sidecar decodes, the ADR 0034 GPU-contention risk) and deterministic, so
  it measures the same cues the shipped `clip.ass` already holds. Re-running
  whisper alone (the existing `caption_diag`) is the STAGE-ATTRIBUTION tool for
  the mis-onset fix (whisper-DTW vs ensemble-fusion), not the gate.
- **Rejected: a Python throwaway.** The instrument is the DELIVERABLE (gated,
  ADR-referenced, re-run against each fix); it lives in-tree like `caption_diag`.

## The pre-declared bars (pinned in the grill, before any table)

- **Too-fast:** a cue on screen shorter than **`READ_FLOOR_S = 0.40`** (the
  ass.rs `MIN_READ_S` the "clamp to next onset LAST" rule silently defeats when
  onsets pile up).
- **Mis-onset:** |cue start − heard onset| > **`0.50 s`**, signed (report
  early-vs-late).
- **Contested:** ≥2 tracks over `MIN_ACTIVITY` (0.004). **Reaction:**
  `laugh ≥ REACTION_TAU` (0.1).
- **Drop:** pre-declared as "no cue within a window of the heard word" — and
  measuring it proved the bar WRONG: a mis-transcribed neighbour a fraction of a
  second away reads as "carried". A drop is a NEGATIVE the shipped file cannot
  confirm; only the operator's ear asserts it, and the instrument reports its
  overlap CONTEXT, not a confirmation. **That is a finding, recorded — not a bar
  quietly moved.**

## The gate (ADR 0045's hit / clean / gap shape)

- **A (catches):** every non-typo class is flagged on the ground truth.
- **B (clean):** turn-taking clips stay quiet — the instrument does not cry wolf
  over good captions.
- **C (gap):** the failures sit in the overlap regime at ≫ the turn-taking rate.

## Measured on clip 3 (fans Fadhil, huge-word, qwen_ensemble)

Four failure classes — **all overlap-specific**:

```
class      measured                                   mechanism
too-fast   53/101 cues (52%) < 0.40s                  overlap packs 2-3x the word-onsets/sec;
                                                       the next-onset clamp floors them sub-readably
mis-onset  5/5 confirmed, mean lead -1.47s (EARLY),   the word is anchored ~1.5s EARLY, onto the laughter
           4/5 on contested/reaction bins
phantom    auto-pile "YA SIAPA TAU MAU" (1.64-2.36),  words hallucinated onto a reaction stretch
           + "gue"@26 on laugh 0.76
drop       "otot kayaknya"@6.5, "pinguin"@52          masked, never transcribed (RECALL — un-re-timable)
```

**The decisive control is CROSS-CLIP, not within-clip.** The within-clip
contested/clean too-fast ratio was a weak **1.21×** — misleading, because clip 3
is fast THROUGHOUT (even its "clean" bins are in an all-overlap clip). The
strong control is the dwell distribution across the workspace's clips
(`clip.ass` only, no GPU — `benchmarks`/scratchpad `caption_toofast_crossclip`):

```
regime        clip                       cues/s  %<0.40s  median dwell
turn-taking   guru gembul (solo x3)      0.75-0.90   0-2%   0.96-1.18s
turn-taking   Helmy Yahya                0.65        0%     1.46s
turn-taking   ANTITESA (2p)              0.82        0%     0.96s
OVERLAP       VIOR fans Fadhil (4p)      1.66        54%    0.38s
OVERLAP       Deddy Tretan/Coki (3p)     2.92        78%    0.24s
```

Turn-taking **0–2%** sub-floor vs overlap **54–78%** — a ~30× gap. Too-fast is
NOT huge-word physics; it tracks overlap DENSITY. Bar B and Bar C both hold, and
the finding UNIFIES: all four classes are overlap-specific, and the operator's
"turn-taking is good" is confirmed by measurement.

**The root:** the caption stage is **blind to the overlap signal the analysis
already computes**. The per-bin voice-lane attribution + reaction mask (what the
CSV IS) drive the camera lanes (ADR 0044/0046) but never reach captioning —
whisper / the ensemble caption the raw mixed audio with no notion of who is
speaking or when it is a shared reaction. Feed that signal into the caption
stage and three of the four classes collapse.

## Operator ruling (2026-07-08): gate PASSED

The operator confirmed the **leading-not-lagging** mechanism against their eye
(the mis-placed cue appears ~1.5 s EARLY onto the laughter; in huge-word that
reads as lag — the early cue clears before the real word lands, so the speech
feels un-captioned), and approved the fix order. Nothing ships this session
beyond the instrument (the measure-first bargain: an unmeasured "fix" is the
`enh` overclaim, caught twice).

## Fix roadmap (candidates weighed; approved order — each its OWN gated slice)

1. **Phantom suppression** — gate a cue out when its onset sits on a
   reaction-masked bin with no clean attributed speaker. Cheapest,
   highest-confidence; reuses the ADR 0046 mask verbatim. Keeps the huge-word look.
2. **Too-fast grouping** — when onsets are denser than the floor allows, fall
   back from one-word-per-cue to a compact 2–3 word line (shared reading
   budget). Fixes the 52%. **Bends the huge-word look in dense stretches**
   (operator flagged + accepted as a tradeoff to weigh at that gate).
3. **Mis-onset re-anchor** — after a stage-attribution pass (`caption_diag`:
   whisper-DTW vs ensemble-fusion), re-place the early word at the real
   post-laughter onset using the mask.
4. **Drop / recall lane** — the heavy one: per-speaker decode or denoise for
   masked overlap (ADR 0014/0029 reopened). Its own spike → gate → wire.

## Consequences

- `caption_overlap_diag` exists in `crates/render/examples/` and is called by
  NOTHING in the production path; the render/caption code is byte-untouched
  (42 yc-render tests green; the diff is the example + the benchmark file only).
- The reusable measure loop for every fix above: re-run the instrument on the
  fix's `clip.ass` and require the turn-taking control to stay 0–2% sub-floor
  (no regression) while the overlap clip's flagged classes shrink.
- **Drops cannot be measured from the shipped file** — the instrument reports
  their context; the operator's ground truth is their record. The recall lane
  will need its own transcription-recall instrument (a heard-word vs
  carried-word alignment tolerant of mis-transcription), not this one.
- The huge-word genre is physically unable to show every word ≥ 0.40 s once
  speech exceeds ~2.5 words/s — the too-fast fix must change WHAT is shown
  (group, or thin by attribution), not merely the floor.
