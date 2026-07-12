# New Creators default to the Qwen ensemble engine (operator ruling 2026-07-12) — the seed changes, existing records do not

ADR 0035 shipped the per-Creator Caption engine with an explicit
no-default-flip rule: every Creator started on whisper and the ensemble was
a deliberate per-Creator act. Since then the calculus inverted: the
ensemble's words are vote-cleaned and its timing is the eye-approved forced
aligner (ADR 0055), while ADR 0058 measured the whisper engine's alignment
path BLOCKED on exactly the vote it lacks — and the 2026-07-12 general
directive asks for the most accurate captions on ALL FUTURE VIDEOS. The
open policy question ("should new Creators just default to the ensemble?")
was put to the operator, and they ruled the same day: **"use ensemble for
new creators."** Cost accepted: five sidecar decodes + the qwen/align
models per clip (quality over runtime, their standing directive).

## Decision

- **`CaptionEngine::FOR_NEW_CREATORS = QwenEnsemble`** — a named seed used
  at the three unknown-Creator fallbacks: the GUI's initial state, the
  import's picker reset (an unknown Creator RESETS the picker so a previous
  session's pick can't leak — that reset now lands on the ensemble), and
  the render resolution's final fallback (headless/CLI included).
- **The serde/`Default` default stays Whisper, deliberately.** A
  never-flipped record carries no `caption_engine` key (the skip-guard), so
  flipping `Default` would silently reinterpret existing files — Helmy
  Yahya Bicara and "local" would change engines with zero operator action,
  exactly what ADR 0033/0035 forbid. The two defaults differing IS the
  design; a unit test pins both
  (`new_creator_seed_is_the_ensemble_while_old_records_still_read_whisper`).
- Saved engines, the `YC_QWEN_ENS` tri-state override, the ADR 0035
  switch-warning (correction carry-over), and the save-back-on-render flow
  are all unchanged. A new Creator's first render writes
  `caption_engine: qwen_ensemble` explicitly — durable, no ambiguity.

## Consequences

- Every first import of a new Creator runs the full ensemble: 5 sidecar
  decodes + wav2vec2-CTC alignment per clip. Missing models/sidecars fail
  soft to whisper captions exactly as before (the pipeline's existing
  contract) — the Diagnostics downloads heal it.
- **Helmy Yahya Bicara and "local" keep whisper** (their records are
  key-less and unrevised). If the operator wants local files on the
  ensemble, the rail picker flip is one click and persists (ADR 0035).
- The whisper engine now serves only explicitly-whisper Creators. The
  caption arc's whisper-engine lanes (ADR 0058's blocked lane 1; the
  recall-parity lane 2) drop in priority accordingly — recorded in
  nextprompt-caption-general.md for the next grill.
- CONTEXT.md's Caption engine term now names the ensemble as the
  new-Creator start.
