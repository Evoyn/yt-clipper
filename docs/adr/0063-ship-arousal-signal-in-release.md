# Ship the arousal Signal in release builds (wire `ser`; fail-soft a corrupt model like the llm judge)

`scripts/build-release.bat` builds `--features face,align` — no `ser`. The ser-gated refine pass (app pipeline.rs ~1036) is therefore compiled out of every production binary, degrading two Signals at once: the default `Weights` reserve 0.15 for arousal (yc-detect lib.rs:109) and renormalize it away, so Moments rank loud-vs-flat blind — the exact false-positive class ADR 0008 exists to kill — and the LLM judge's prompt corroborates against `arousal_z` (yc-detect llm.rs), which a ser-less build has fed `None` the whole time. The operator's saved ECA table shows the fingerprint: all 25 Moments carry `arousal: null`. The mixed-audio gate PASSED 2026-06-18 (ADR 0008 Outcome, real 66-min VOD); this slice validates-and-ships that gate-passed Signal into the release binary. It does not re-open the gate, does not resurrect discovery-arousal (deferred in ADR 0008 pending vocal separation), and does not retune weights.

## Considered options

- **Wire `ser` and keep the hard-fail on a present-but-corrupt model.** `Ser::load(&paths.ser_model)?` fails the whole detect job. Rejected: by the time the arousal pass runs, discovery + the full whisper refine (GPU) are already paid for, and the codebase's own convention for a refine-Signal failing mid-run is warn-and-omit — the llm judge arm does exactly that (`Err(e) if cancel.is_cancelled() => return Err(e)`, else `warn!` + omit), and the pipeline comment already reads "exactly like a missing SER model". The Downloads heal flow can't even produce a corrupt install (streamed to `.part`, pinned SHA-256 verified, then staged into place), so the hard-fail only ever punishes disk rot or a hand-copied file with a dead detect job.
- **Fail-soft per-Moment (Option per candidate).** Rejected: `arousal::apply` z-scores across the candidate set; scoring a subset would z-score over survivors and quietly skew the Signal. Whole-batch warn-and-omit keeps the semantics binary: the Signal is either present for every candidate or absent for all (renormalize), the same contract a missing model already has.
- **Soften the missing-model path further / add UI surfacing.** Not needed: missing model is already structurally soft (`is_file()` guard), the Diagnostics ▸ Downloads row heals it, and the GUI shows the per-Moment arousal column (a dead Signal is visible as an empty column, same visibility the llm Signal gets).

## Decision

1. `scripts/build-release.bat` builds `--features face,align,ser`; its comment block documents why, in the file's existing per-feature convention.
2. The arousal pass is extracted to a fallible `arousal_refine(...)` helper; the call site matches the llm-judge arm: cancellation propagates, any other error (corrupt model, unreadable wav) logs `warn!` and omits the Signal, and `combined_score` renormalizes as designed. Unit-covered with a present-but-garbage `model.onnx`.
3. Scoring itself changes by zero words: `combined_score`, `Weights`, and the 0.15 default are untouched.

## Pre-registered bars (written and committed BEFORE the first A/B run)

A/B protocol: same VOD (the ECA podcast, `LeR59VmXiSc`, 4801 s, no chat replay — loudness-led discovery), same params (defaults, top_n 25), same cached `analysis.wav`, greedy/temp-0 llm judge (reproducible, so any llm delta is the `arousal_z` prompt line, i.e. the Signal's designed corroboration). A = today's ser-less release exe (archived aside), B = the `face,align,ser` build. The operator's saved `project.json` + `review.json` are byte-backed-up (SHA-256 recorded) and restored after; detect overwrites both.

The wire is sane iff:

1. **Coverage**: every refined Moment in the B table carries `Some(arousal)` — 25/25, no holes.
2. **Explainability**: every rank move A→B is explainable by the arousal column (directly through the 0.15 weight, or second-order through the judge reading `arousal_z`) — the known classes move the known way: emotionally activated reactions up, loud-but-flat down. A move the arousal column cannot explain (e.g. changed bounds, lexicon shifts) is investigated before the wire ships, not explained away.
3. **A-side integrity**: the A table carries `arousal: null` on every row (proves the archived exe is the ser-less binary, not a stale build).
4. **Fail-soft**: the corrupt-model unit test passes — `arousal_refine` returns `Err` on garbage bytes and the call site's warn+skip keeps detect alive.
5. **Cost of record**: per-window CPU wall and the refine-batch wall re-measured today, recorded beside ADR 0008's numbers (~0.35–0.46 s / 4 s window, ~163 s / 25-candidate batch on 2026-06-18 hardware).
6. **Restore**: the operator's `project.json` + `review.json` back in place, SHA-256 byte-identical to the backup.

Boot check after the wire: release GUI opens, Diagnostics SER row green, a detect surfaces the "Refining moments (arousal, CPU)" stage.

The re-ranked detect is a behavior change, so the before/after top-N table with per-Signal columns plus the measured cost line goes to the operator's eye (ADR 0050/0057 lesson); their eye rules the final gate.

## Outcome

Pending — filled in after the A/B and boot check.
