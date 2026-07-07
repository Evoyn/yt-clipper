# Session prompt — Solo-shot presence gate (measure first)

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session:
read the context below, then run /grill-with-docs BEFORE any code — the
grill's job is to pin the measurement's bars and the fallback grammar
BEFORE the instrument exists (the ADR 0045 pattern: instrument spike →
pre-declared bars → gate → wiring).

## Why this exists

The 2026-07-07 ship-shakedown (first fresh-material production run,
VIOR VOD o1SBOz5UK2Q; ledger:
`handoffs/2026-07-07-ship-shakedown-ledger.md`) shipped 2 of 3 clips and
converged on ONE worst defect, twin-confirmed across all three clips and
BOTH regimes (9 publisher-visible hits):

**A solo Shot never verifies that what it frames is the attributed
person's real, currently-visible face.** Two sub-classes:

- **A — no measurement in the shot**: subject undetected (turned away,
  leaned out, detection gap) → the previous/elsewhere crop persists →
  set dressing or a sliced head ships → the camera audit is BLIND (no
  measurement, no finding). Hits: Clip 1 shot #1 early window (guitar
  wall, 5 s), Clip 2 #3/#7 (EMPTY frames — held the clip), Clip 3 #7/#8
  (blank at 13.09).
- **B — false or angle-stale measurement**: in a new camera angle the
  seat's position lands on set dressing, the track latches onto a
  face-like blob there (guitar body at Clip 3 22.19; pegboard/arm at
  28.77), the audit sees "subject in crop" and PASSES, and the real
  speaker sits center-frame labeled as a different seat in this angle's
  geometry.

Operator's causal read (verified): "the camera doesn't reframe or change
after previous person" — no/false measurement means no dead-zone
trigger, so the stale crop persists.

## The slice (one implementation, spike-shaped)

1. **Instrument first**: extend speaker_diag with a per-solo-shot
   presence/quality table — for every solo shot: (a) subject-detected
   bins / shot bins, (b) whether detections sit inside the crop, (c) a
   face-quality/landmark gate on what the track matched in the current
   angle (the occupant map's YuNet/SFace path already embeds faces —
   reuse, don't invent). Run it on the three VIOR clips (data dir:
   `workspace/Deddy Corbuzier/MENUJU INDONESIA CEMAS BERSAMA VIOR‼️🤣 -  VINCENT, VADI, NINO, CT/data`,
   ranges 86.4–160.6 / 4737.789188985825–4784.469188985825 /
   3592.0–3653.0 — per-clip segment.mp4 must be re-fetched; production
   args in youtube.rs segment_args; the padded fetches are deterministic)
   AND both fixtures (Deddy person-join, ANTITESA byte-pin — the gate
   plans must score CLEAN or the bar is wrong).
2. **Pre-declare the bars** with the operator: which of the 9 known hits
   must the metric flag; which fixture shots must it NOT flag.
3. **Grammar rule only after the gate**: fallback (group/wide/hold-prior
   -angle crop) for a solo shot failing the presence/quality bar, and
   the audit extension (flag unmeasured + low-quality solo windows).
   The occupant map already knows persons-per-angle for the B-class.

## Read first

1. `handoffs/2026-07-07-ship-shakedown.md` — session state + traps.
2. `handoffs/2026-07-07-ship-shakedown-ledger.md` — all 9 findings, twin
   verdicts (F5–F9), and the infrastructure findings (F1 ser-less build,
   F2 segment-fetch retry vs YouTube's flapping windows, F3 silent
   Failed chip).
3. verify SKILL.md — fixture bars that stay green through ANY change.
4. Twin outputs: session scratchpad twin_clip1/2/3.txt (regenerate via
   speaker_diag if gone — commands in the handoff).

## Hard rules

- Fixture bars stay green (Deddy person-join + laughter bars, ANTITESA
  fg pin 9e07d81f…, audits clean, suites 337/337 both ways).
- The four gate artifacts in the BGN B NYA workspace dir are never
  re-rendered.
- The VIOR renders + ledger are the shakedown record — never overwrite
  `Nego Nutrisi…`, `Diskusi…`, `Aku fans….mp4`.
- AnalyzeSpeakers stays editor-only; the harness twin is speaker_diag.

## Open threads (not this slice, keep visible)

- Voice-join scrap fragmentation on rapid banter (Clip 2's 0.0 s lane —
  ADR 0044's texture generalizing; off-screen splits unavailable exactly
  when banter needs them).
- F2/F3: segment-fetch retry schedule spanning minutes + a Retry
  affordance + error surfacing on the Failed chip (M8 error-state polish
  candidates, now with measured evidence).
- F1: fold `ser` into the production build line? (operator decision —
  re-detect A/B on the VIOR 25 candidates would measure the ranking
  shift.)
- int8 tagger measurement (ADR 0046 follow-up, hash a6254a4c…);
  applause/cheering class fixture material.

## Ritual

/grill-with-docs first; /verify before committing; finish with /handoff
+ whatwedone.md entry; commit as Evoyn with the model trailer (-F file);
`git push origin main` has standing permission. PS 5.1 quirks per the
standing memory notes.
