# Session prompt — Fix #3: re-anchor the mis-onset words dragged early onto laughter

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read the
context below, then run /grill-with-docs BEFORE any code. Fix #1 (reaction-phantom
suppression) shipped last session (ADR 0050); this slice does fix #3 of the ADR
0049 roadmap — **re-anchor the mis-onset words** the operator confirmed appear
~1.5 s EARLY, onto the laughter, so the real speech feels un-captioned.

## First: confirm the slice with the operator (a real branch)

The ADR 0049 approved order puts **fix #2 (too-fast grouping)** next, not #3. But
#2 **bends the huge-word look** in dense stretches (the operator flagged this as a
tradeoff to weigh AT its gate) — a look decision only the operator can approve —
whereas **#3 is look-preserving** and directly completes fix #1: the "siapa tau"
and "gue" words that SURVIVED suppression last session are exactly the words #3
re-places at their real post-laughter onset. Open the grill by asking which the
operator wants next (recommend #3: look-preserving, operator-confirmed defect,
coupled to what just shipped). If they pick #2, re-scope to too-fast grouping.

## Why #3 exists (the evidence)

ADR 0049 measured 5 mis-onset cues on clip 3, mean lead **-1.47 s (all EARLY)**,
4/5 on contested/reaction bins. The operator confirmed the mechanism against their
eye: the cue is anchored ~1.5 s early onto the laughter; in huge-word that reads
as LAG (the early cue clears before the real word lands). Fix #1 removed the
hallucinated PILES; the mis-onset REAL words (e.g. "siapa tau" belongs ~5 s,
"gue" belongs ~28 s) still sit early and now need re-placing.

## The slice (grill the seam + the false-positive bar FIRST)

1. **Stage attribution is the OPENING question (UN-RUN).** Is the -1.5 s lead
   whisper's raw DTW onset, or the ensemble fusion (`fuse_onto_timing`) placing the
   word early onto a laughter onset? Run the existing `caption_diag` on clip 3 and
   compare whisper's raw onsets to the shipped `clip.ass` times BEFORE deciding
   where the re-anchor lives. This decides everything downstream.
2. **Pin the re-anchor seam in the grill**: does the word move in the ensemble
   fusion (re-place onto the real post-laughter onset instead of the laughter one),
   or in a refine-stage pass? The reaction mask + speech onsets are the inputs; the
   mask is now recomputable at caption time (ADR 0050's `suppress_caption_phantoms`
   pattern — reuse it). How does the mask reach the re-anchor stage?
3. **Pre-declare the gate BARS** (ADR 0045 discipline): the 5 mis-onset cues move to
   within tol of their heard onset (instrument's mis-onset |lead| drops < 0.50 s);
   **no correctly-placed word is moved** (the false-positive killer); turn-taking
   controls unchanged; fixtures green.
4. **Wire it, gate with the instrument on the operator's eyes.** Ship only if bars pass.

## Read first

1. `handoffs/2026-07-08-caption-reaction-phantom-suppression.md` (state + traps —
   esp. the mis-onset/phantom entanglement and the recomputable mask) and ADR 0050.
2. `docs/adr/0049-caption-overlap-instrument-gate.md` (fix roadmap, the -1.47 s
   measurement, the mis-onset stage-attribution open thread).
3. `crates/transcribe/src/ensemble.rs` (`fuse_onto_timing`, `apply_store_positional`
   — the existing onset re-placement + `at_s` pin machinery this can build on) and
   `crates/app/examples/caption_diag.rs` (the whisper-DTW-vs-shipped attribution twin).

## Hard rules

- Measure with the instrument on the PRODUCTION path against operator ground truth
  BEFORE claiming the fix works (the standing discipline).
- **A correctly-placed word must never move** — the gate MUST prove re-anchoring
  only touches the mis-onset words, not the good ones.
- Turn-taking captions must not regress; fixtures stay green (ANTITESA fg SHA-pin,
  Deddy person-join + laughter + shared-reaction, audits; suites both ways — 356 of
  record). AnalyzeSpeakers stays editor-only; the harness twin is speaker_diag.
- Operator prefers best accuracy over speed (standing directive) — reuse the real
  recomputed mask, don't approximate.

## Ritual

/grill-with-docs first; /verify before committing; finish with /handoff +
whatwedone.md entry; commit as Evoyn with the model trailer (-F file);
`git push origin main` has standing permission. PS 5.1 quirks per the standing
memory notes. If the operator picks #2 instead, re-scope the grill to too-fast
grouping (readability-gated fallback from one-word cues to a compact 2-3 word line;
weigh the huge-word-look tradeoff at the gate).
