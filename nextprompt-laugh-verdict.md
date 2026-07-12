# Operator verdict: laughter-aware hold trim (ADR 0062) — ~2 min of your eye

Two burns sit in the VIOR stream folder, identical except **seven cue ENDS**
(same words, same onsets, same everything else — the emit reproduced the
instrument value-for-value, decode-deterministic):

- **A**: `_laugh-hold clip3 (ADR 0062 - A raw production).mp4` — today's raw
  pipeline output (stores raw, no pins — the first burn of the true code
  path since your no-JSON ruling).
- **B**: `_laugh-hold clip3 (ADR 0062 - B trimmed holds).mp4` — the same
  emit with held words CLEARING at the laugh onset instead of lingering
  into it (never below the 0.40 s readability floor; nothing deleted,
  nothing moved).

Where to look (clip-relative — the only moments A and B differ):

```
 3.2-4.4   MAU         clears at 4.00 instead of riding the laugh 0.42s
 7.0-8.2   TUH         clears at 7.42 (was riding 0.80s)
 8.8-9.4   KERATIN     clears at 9.24
11.0-11.7  PROTEIN     clears at 11.50
14.1-14.7  MASALAH     clears at 14.52
46.8-48.0  NGELEDEKIN  clears at 47.50 (the mocking-laugh moment)
51.2-52.4  GEMOY       clears at 51.60 instead of floating 1.15s into the big laugh
```

## Why this is staged instead of shipped

The pre-registered bars (ADR 0062, committed before the first run) split:
every safety bar passed on all 5 corpus clips (nothing deleted/moved, every
trim named and on the laugh mask, controls clean, zero words +/-), and the
trim removed **every second the readability floor allows** — but bar R3's
letter said "residual ≤ 30%" and the floor physically protects 41% (words
in dense overlap pop < 0.40 s before the room erupts; those residues are
untouchable without minting flash-frames). Per the pre-registration,
nothing wired. The bar mis-measured the floor, not the trim — but a bar
re-pin + a caption look-change is YOUR call (the ADR 0050/0057 lesson).

## Say one of these

- **"B is better"** (or "trim it"): next session re-pins R3 clause 1 to its
  clause-2 form ("zero trimmable seconds left"), wires
  `yc_render::trim_reaction_holds` at the marked pipeline spot (the NOT-wired
  comment in ensure_transcript names the exact shape: post-refine, mixed
  analysis audio, `speaker::REACTION_TAU`, ADR 0050 fail-soft,
  `YC_LAUGH_TRIM=0` off-switch), reruns the suites, and the trim applies to
  ALL future videos on BOTH engines.
- **"A is better"** (or "leave holds alone"): ADR 0062 gets a REFUSED
  banner; the instrument + pure function stay in-tree as the measured
  record; captions stay byte-identical (they already are).
- **A timing nudge by ear**: name the word + what read wrong. Honest note:
  the 0.40 s floor is not a lever (ADR 0013/0049 — lowering it mints
  flashes), and tau 0.1 is the gated ADR 0045 operating point (re-sweeping
  it needs its own gate) — so a nudge likely routes to a different lane
  (mis-onset/pop-on-laugh is ADR 0051's, not this trim's).

Cleanup notes: `data\clip_laughtrim.ass` is the trimmed artifact (durable),
`data\clip_alignburn.raw0062.bak.ass` the raw backup; `clip_alignburn.ass`
on disk is the RAW production emit (restored after burn B). The 27
pop-on-laugh cues the instrument counted are the mis-onset class — a
different lane, deliberately untouched here.
