# Session prompt — GENERAL caption accuracy (operator directive 2026-07-12)

> **Operator ruling (2026-07-12, verbatim intent): stop polishing clip 3 /
> single videos. Fix captions so the fix applies to ALL FUTURE VIDEOS —
> the detection (dropped words), garble, and laugh classes, with the most
> accurate timing available.** The dedikornya verdict stays staged
> (nextprompt-deddy-verdict.md, ~2 min of their eye, no urgency); do NOT
> propose more per-clip curation as session work.

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read
the context below, then run /grill-with-docs BEFORE any code — the grill's
first question is which lane, the second is the new-Creator engine default.

## Ground truth the grill must hold (verified 2026-07-12)

- **The WhisperX method is ALREADY the production default — ensemble only.**
  ADR 0053 spiked WhisperX itself; ADR 0054 ported its core (wav2vec2-CTC
  forced alignment) to pure Rust (`align` feature, `models/w2v2-align-id`,
  ONNX); ADR 0055 flipped it to the ensemble's default timing source,
  validated on the operator's own hand-pinned onsets. `YC_FORCED_ALIGN=0`
  is the off-switch; DTW fusion is the fallback.
- **faster-whisper is NOT a lever**: same model, faster runtime, same
  DTW-class word timestamps — a speed play against ADR 0003's native
  architecture. Do not propose it.
- **The whisper ENGINE path never runs the aligner** (pipeline.rs ~1890:
  "Whisper Creators stay byte-identical - this block never runs") and never
  gets recall admission (vote-less single decode). Engine assignments today
  (workspace/creators.json): guru gembul / Leon Hartono / Deddy Corbuzier =
  qwen_ensemble; **Helmy Yahya Bicara + "local" + every NEW Creator default
  to whisper** — they get the old timing stack (ADR 0013/0018/0019/0020/0021)
  and no recall.
- The `at_s` pin pass already runs engine-parity (ADR 0051), and the
  `align_parity` example (crates/app, ADR 0054's instrument) already exists
  as measurement fuel.

## The lanes (grill to order, then ONE implementation)

1. **Whisper-engine forced-alignment timing (align parity) — RECOMMENDED
   FIRST.** Wire the shipped aligner into the whisper-only caption path so
   every engine gets the accurate skeleton. Grill points: interactions with
   the whisper timing stack (karaoke snap 0018, onset clamp 0019, pre-roll
   lead 0020, silence floor 0021 — which survive, which the aligner
   replaces?); failure fallback (aligner error -> whisper's own spans,
   captions never missing); controls (align feature off / model missing =
   byte-identical today); fixtures = the ADR 0049 cross-clip set on the
   whisper engine + suite both engines; gate = the eye on a burn.
   **Alternative/complement the grill MUST ask: should new Creators just
   default to the ensemble engine instead?** (Quality-over-runtime pulls
   that way; ADR 0033/0035 made switching deliberate — a default flip is an
   operator policy call. Costs: 5 sidecar decodes + models per clip.)
2. **Whisper-engine recall parity** (carried from the recall menu): no vote
   on a single decoder — the grill must design the witness (a second decode
   config? the ensemble's machinery on demand?) with ADR 0052's zero-added
   controls. Genuinely open design; do not improvise without pre-registration.
3. **Laughter-aware caption timing/holds (NEW lane).** Fuel exists: the
   production shared-reaction tagger (ADR 0045/0046) already masks laughter
   for the camera. Candidate uses, measure-first: cue HOLDS truncate at a
   laugh-mask onset (the CORP-hold observation, 2026-07-12 handoff);
   laugh spans as low-evidence zones in alignment (the measured CTC
   re-route class); a pop-on-laugh counter as the instrument, scored
   against the gt's phantom rows. HARD CONSTRAINT (ADR 0050, eye-reversed):
   never DELETE a word for sitting on a laugh — move / re-time / trim holds
   only. Needs its own pre-registered instrument + bars before any code.
4. **Garble generality**: the layered store + fuzzy cross-engine transfer +
   LLM pass + harvest/review queue ARE the general machinery (ADR
   0030/0031/0033). No new lane unless the operator names a specific
   generalizing class; per-clip pins remain the tail for overlap-hard clips.

## Hard rules (unchanged)

- Measure before building; pre-register bars; gate on the operator's eye on
  a burn (ADR 0050/0057: instruments passing is never the gate).
- Zero words added/removed on turn-taking controls for any admission or
  timing change; suite green both engines.
- Best accuracy over speed. Offline: models only from pinned sources via
  the Diagnostics downloads.
- GPU: probe-decode (<60 s healthy) is the gate decider under playback
  flutter; never raise the watchdog budget; sweep orphan diag exes AND
  bash/sleep trees; background waits die ~25-40 min — wakeup-poll +
  bounded foreground bursts.
- PS 5.1: ASCII scripts; QUOTE "--features" "face,align"; commit -F file.

## Ritual

/grill-with-docs first (the operator may be present — if they said "do this
automatically", apply the recorded pick rubric and default to lane 1);
finish with /handoff + whatwedone.md entry; commit as Evoyn with the model
trailer; `git push origin main` has standing permission; ALWAYS end with the
next `read nextprompt-<slug>.md and follow it.` line.
