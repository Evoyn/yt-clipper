# Session prompt — Captions under overlapping speech (measure the failure first)

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session:
read the context below, then run /grill-with-docs BEFORE any code — the
grill's job here is to pin WHAT "good enough" means for captions under
overlapping speech (a measurable bar) and to decide the spike shape BEFORE
touching the timing, because this is a quality regression with no
instrument yet (the ADR 0045 pattern: measure the defect first).

## Why this exists

The operator watched the shipped VIOR clips (a 4-person podcast) and found
the **captions are not good enough when several people talk at the same
time** (2026-07-07). Concretely, in overlapping-speech stretches:
- some captions are **delayed** (appear after the words),
- some words are **not picked up** at all (dropped),
- some appear **too fast** (flash by before they're readable).

For **turn-taking** speech — 2 OR 4 people, as long as they speak in turn —
the captions are good. So the defect is specific to the **simultaneous-
speech (overlap) regime**, not speaker count. This session's solo-presence
work touched ZERO caption/transcribe code (confirmed by git diff), so this
is a pre-existing quality gap, freshly noticed on real 4-person material.

## What we already know (the evidence + the latent signal)

- Captions come from the **Caption engine** (CONTEXT.md): Whisper single-
  decode, or the **Qwen ensemble** (five-variant vote, whisper a voter),
  timing "fused onto skeleton anchors and speech onsets". Deddy's Creator
  setting is `qwen_ensemble`; the shipped VIOR captions are the ensemble's.
- The **Voice lane** (ADR 0042, `yc_frame::voice`) already measures a
  **contested share per bin** — stretches where several mouths move / several
  voices overlap are exactly the "no clean evidence" bins. Overlapping
  speech ≈ high contested share. So the tool ALREADY has a per-bin signal
  for "where captioning is hard" — it just isn't fed to the caption timing.
- The **Shared-reaction mask** (ADR 0045/0046) marks group laughter — a
  sibling of overlap. Laughter stretches are where "too fast / dropped"
  captions likely cluster.

## The slice (measure-first, gate-shaped — do NOT jump to a fix)

1. **Reproduce + name the failure modes** on a real overlapping-speech clip
   (a VIOR segment with a known simultaneous-speech stretch; the cached
   segments + full `analysis.wav` are in the scratchpad, ranges in the
   handoff). Get the operator's **ground truth**: which words are delayed /
   dropped / too-fast, with clip-relative timestamps. This is the
   must-fix list the instrument must flag.
2. **Build the instrument** (diagnostics-only, additive — the ADR 0045
   pattern): measure per caption cue, against the overlap signals (voice-lane
   contested share, reaction mask), the timing error (onset vs spoken onset),
   the on-screen dwell (too-fast = below a readability floor), and the drop
   rate (words the operator heard but no cue carries). Pre-declare the BARS
   before reading the table.
3. **Decide the fix at the gate** (candidates to weigh in the grill, not to
   pre-commit): a readability **minimum dwell / hold** for too-fast cues; an
   **onset re-alignment** using the voice lane's attribution instead of a
   single global VAD; **per-speaker** decode gated by the voice lane in
   contested stretches; the `enh`/denoise path for overlap. The winning fix
   is wired only after its own gate, like every lane before it.

## Read first

1. `crates/transcribe/` (whisper decode + the ensemble) and wherever caption
   TIMING is fused onto onsets/skeleton anchors — the code that decides WHEN
   a cue shows and for how long.
2. CONTEXT.md terms: **Caption engine**, **Caption Style** (dwell/animation),
   **Cleaned voice** (`enh`), **Voice lane** (the contested-share signal),
   **Shared reaction**.
3. ADR 0034 (the ensemble recipe + its gate) and ADR 0045/0046 (the
   measure-first instrument→gate→wiring pattern to copy).
4. `handoffs/2026-07-07-solo-presence-wiring.md` — the cached VIOR segments,
   ranges, and how to drive the harness twin.

## Hard rules

- Measure the defect on the PRODUCTION path against operator ground truth
  BEFORE proposing a fix (the standing discipline — the enh overclaim was
  caught twice by skipping this).
- Fixture bars stay green (Deddy person-join + laughter, ANTITESA fg
  byte-pin, audits clean, suites both ways). A caption-timing change must
  not regress the turn-taking case (which is already good).
- AnalyzeSpeakers stays editor-only; the harness twin is speaker_diag.

## Ritual

/grill-with-docs first; /verify before committing; finish with /handoff +
whatwedone.md entry; commit as Evoyn with the model trailer (-F file);
`git push origin main` has standing permission. PS 5.1 quirks per the
standing memory notes.
