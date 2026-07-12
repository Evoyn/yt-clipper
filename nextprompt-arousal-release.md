# Session prompt — the AROUSAL Signal has never shipped in a release build (operator pick 2026-07-12)

The operator closed the caption-general arc and picked this from the
derived open-threads list ("i want you to make based on your recommended").

## The defect (evidenced, 2026-07-07 ship-shakedown)

`scripts/build-release.bat` builds `--features face,align` — **no `ser`**.
The Arousal Signal (ADR 0008; CONTEXT.md **Arousal**: the emotional
*activation* of the streamer's voice — the one Signal that tells an
emotional reaction from merely-loud audio) is therefore compiled out of
every production binary: release detect has ranked Moments from
chat-rate / loudness / lexicon / LLM only, through the entire camera arc,
while the gate-passed model sits installed (`models/w2v2-emotion`) doing
nothing. The shakedown recorded it verbatim: "detect ranks without one
Signal."

## Ground truth the grill must verify BEFORE code

- ADR 0008 (what the Signal measures, the CPU-refine staging), ADR
  0002/0007 (where Signals enter Moment scoring; Signals stay SEPARATE —
  never blended away, CONTEXT.md **Signal**).
- The `ser` feature wiring: workspace Cargo.toml → crates/app (`ser =
  ["yc-detect/ser"]`) → yc-detect. What a ser build does when the model
  file is MISSING (must fail soft to today's ranking — verify, don't
  assume) and whether Diagnostics ▸ Downloads carries the SER model row as
  the heal path.
- The existing instruments before inventing any:
  `crates/detect/examples/arousal_probe.rs`, `arousal_scan.rs`,
  `crates/app/examples/arousal_adds.rs`, `detect_diag.rs` — read their
  headers; `arousal_adds` sounds like exactly the ranking-contribution
  measurement this slice needs (unverified — confirm).
- Whether ser has ever run under the CUDA build script
  (`scripts/cargo-cuda.bat`) — ort is already shipped via `face`, but
  verify the combined `face,align,ser` build compiles and boots before
  touching the release script.

## The slice (ONE implementation)

Make release builds carry the Signal, measured:

1. **Probe** (no code): a `ser`-flavored build; `arousal_probe` (or the
   verified equivalent) on a fixture range — the model loads, scores move.
2. **A/B the ranking on a real VOD** (measure-first, pre-register what
   "contributes sanely" means BEFORE looking — e.g. every refined Moment
   carries an arousal score; rank changes bounded and explainable; no
   Signal blending). The ECA podcast has 25 operator-saved Moments — do
   NOT clobber operator state: A/B through a diag path or a project.json
   copy, never a persisting re-detect over their file.
3. **Wire**: add `ser` to build-release.bat (+ its comment block) only
   after the A/B reads sane. Rebuild release, boot check.
4. **Cost honesty**: measure the added detect wall-time (CPU refine) and
   report it — quality-over-runtime is standing, but the number gets
   recorded, not assumed.
5. Suites green: default AND `face,align,ser` flavors.
6. **Operator gate**: the before/after top-N Moment table for their eye
   (they know their VODs); a re-ranked detect is a look change — the eye
   rules (ADR 0050/0057 lesson).

## Hard rules (standing)

- Measure before building; pre-register bars; fail-soft on missing model.
- GPU: idle-gate any decode-heavy step (probe decode is the decider);
  sweep orphans; background waits die ~25-40 min — wakeup-poll instead.
- PS 5.1: ASCII scripts; QUOTE "--features" "face,align,ser"; commit -F
  file; git-bash mangles workspace paths — PowerShell tool for runs.
- Caption code is OUT OF SCOPE (that arc is closed; see
  nextprompt-caption-general.md's banner).

## Ritual

/grill-with-docs first (operator may be present; if they said "do this
automatically", the pick is already recorded — proceed measure-first
through the slice above). Also still on the table for the operator, one
click each, their act alone: flipping Helmy Yahya Bicara + "local" to the
ensemble engine (ADR 0035/0061). Finish with /handoff + whatwedone.md
entry; commit as Evoyn with the model trailer; `git push origin main` has
standing permission; ALWAYS end with the next
`read nextprompt-<slug>.md and follow it.` line.
