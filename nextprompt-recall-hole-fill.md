# Session prompt — Recall lane: fill the vote's token holes (ADR 0052 follow-through; kills the garble-float's room)

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read the
context below, then run /grill-with-docs BEFORE any code.

## Where you are (read this first)

Forced-alignment timing is the ensemble DEFAULT now (ADR 0055, committed):
`YC_FORCED_ALIGN=0` is the off-switch, the suppress_nst second decode is
skipped when the aligner runs, cross-clip gates measured (numbers in ADR
0055). The mis-onset lane is CLOSED for the drift class; `at_s` pins remain
the override for the duplicated-common-word residual (verified end-to-end,
the gue@3620 pin in clip 3's per-clip store).

What remains on clip 3 is TEXT, not timing — the operator named both on the
burn that passed the gate:

1. **The 51-56 s token hole**: today's vote DROPS `pinguin`@52,
   `jalanannya`@54, and garbles ~51.2 s `gemes` -> `gemoy` — a 5 s hole over
   speechful audio. The stray GEMOY the operator saw floating to 56.06 is
   the CTC spelling that orphan into the hole (ADR 0054 §garble-float).
   Fill the hole and the float has no room; then the store can fix the
   spelling in place.
2. **Version drift between votes**: the 2026-07-08 instrument run measured
   the SAME words surviving the vote (ADR 0052's verdict table:
   pinguin 6/6 decoders, in merged); the 2026-07-11 fresh decode dropped
   them. The decode is deterministic per ADR 0052 — so either the qwen
   sidecar decodes drifted across days (llama.cpp nondeterminism? prompt
   cache? GPU state?) or the vote is knife-edge on these tokens. MEASURE
   WHICH FIRST (`caption_recall_diag` on the same range, twice, diff the
   per-variant word lists) before designing any admission rule.

## Read first

1. `docs/adr/0052-caption-recall-lane-loss-localization.md` — the
   pre-committed branches AND the verdict that refuted them for the 07-08
   decode (a vote-admission rule was a NO-OP then: the words survived).
   Today's drops may make it live again — but only the instrument says.
2. `docs/adr/0054-forced-alignment-rust-port.md` §garble-float —
   confidence-gates and store-pins are MEASURED DEAD ENDS for the float;
   do not rebuild them.
3. `docs/adr/0055-forced-alignment-default-flip.md` — the flip semantics,
   the state matrix, the cross-clip numbers.
4. `handoffs/2026-07-11-forced-align-default-flip.md` — operational state.
5. `crates/app/examples/caption_recall_diag.rs` (the loss-localization
   instrument) + `ensemble::vote_merge` (the strict-majority insert rule).

## The shape of the slice (grill to sharpen)

1. Re-run `caption_recall_diag` on clip 3's 51-56 s rows (pinguin,
   jalanannya, gemes) against TODAY'S decodes: DECODE-loss or VOTE-loss?
   Run twice to measure decode variance directly.
2. If VOTE-loss: the ADR 0052 pre-registered admission rule (distinctive +
   >=K denoised-variant agreement + absent-from-merged) is the candidate —
   its gate bars are already pre-declared in ADR 0052 (zero words added on
   turn-taking controls; operator's eye on the re-burn).
3. If DECODE-loss (today's variants genuinely don't hear them): the
   escalation ladder (extra decode configs on the masked span) before any
   heavy per-speaker machinery.
4. Re-measure the garble-float after the hole fills: the GEMOY cue must
   land ~51.2 (or be renamed by the store once its position is stable).
5. Re-burn clip 3 via segment_seek + swapped clip.ass; operator's eye gates.

## Hard rules

- Measure BEFORE building (the ADR 0052 discipline that already refuted one
  pre-committed fix). Gate on the operator's eye on the burn (ADR 0050).
- Zero words added on clean turn-taking controls (the pre-declared bar).
- The vote stays text-only/time-blind at admission (ADR 0050's lesson).
- Best accuracy over speed (standing directive).

## Ritual

/grill-with-docs first; finish with /handoff + whatwedone.md entry; commit
as Evoyn with the model trailer (-F file); `git push origin main` has
standing permission. PS 5.1 quirks per the standing memory notes.
