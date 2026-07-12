# Session prompt — GENERAL caption accuracy (operator directive 2026-07-12)

> **ARC CLOSED (2026-07-12, operator: "i want to move to next feature").**
> Every lane is terminal — do NOT queue caption sessions from this file
> (the focus-md pattern: historical once the arc closes):
>
> - **Lane 1 REFUSED** (ADR 0058) — reopens only behind a whisper-side
>   word cleaner.
> - **Lane 2 PARKED BY RANKING** (this close-out): it would rebuild, for a
>   single-decoder engine, the witness/vote machinery the ensemble already
>   IS — and since ADR 0061 it serves only the two legacy key-less records
>   (Helmy Yahya Bicara, "local"). The recorded path to that quality for
>   them is the operator's own ONE-CLICK rail flip to the ensemble
>   (ADR 0035/0061 — deliberate act, theirs alone; still on the table).
>   Reopen lane 2 only if the operator names a Creator they will NOT flip.
>   Fuel if reopened: `caption_recall_diag`, `decode_variants`, ADR 0052's
>   zero-added controls, ADR 0058's ready-made re-measure bars.
> - **Lane 3 REFUSED at the eye** (ADR 0062) — drawn-out words ("gemesss…")
>   ride INTO laughs; any successor needs an end-of-vocalization signal.
> - **Lane 4 = the existing machinery** — wakes only when the operator
>   names a garble class.
>
> **No staged verdicts remain anywhere**: the dedikornya verdict was
> WITHDRAWN by the same-day no-JSON rule (its file says so), and the
> laugh-hold A/B was ruled "A — raw production" the same day. The ritual
> line below about a staged dedikornya verdict is superseded by this
> banner. **Queue head: `nextprompt-arousal-release.md`.**

> **LANE 3 IS CLOSED — REFUSED AT THE OPERATOR'S EYE (2026-07-12, ADR
> 0062).** The hold trim measured floor-exact (every safety bar passed on
> the 5-clip corpus; R3's ≤30% letter failed on the readability floor's
> protectorate, so nothing wired) and the operator then ruled the A/B:
> **"raw production is better"** — the flagged holds cover DRAWN-OUT words
> ("gemesss…") whose sound stretches INTO the laugh; a laugh-mask onset is
> not an end-of-word marker, so trimming there cuts captions on live
> speech. Instrument + pure fn stay in-tree as the record. Any successor
> needs an END-OF-VOCALIZATION signal (the aligner's word ENDS are the
> named candidate) behind its own pre-registered gate — do NOT re-attempt
> without one. The autonomous default is **lane 2's measure-first phase**
> (below).

> **LANE 1 IS RESOLVED — MEASURED AND REFUSED (2026-07-12, ADR 0058).** The
> whisper-engine forced-align wiring was built, measured on the 5-clip
> corpus against pre-registered bars, and FAILED the defect-clip bar:
> without a vote to clean whisper's token list, the aligner amplifies
> whisper's double-transcriptions at phrase scale (SUSU DEDDY CORP dragged
> ~2.7 s early). Turn-taking controls measured clean-to-better; nothing
> shipped; `forced_align_retime` + `whisper_align_diag` are in-tree, ready
> to re-measure. **Lane 1's prerequisite IS lane 2** (a whisper-side word
> cleaner), OR the operator's engine-default policy call (below). Do NOT
> re-attempt the wiring without one of those.
>
> **THE POLICY CALL LANDED (2026-07-12, ADR 0061): "use ensemble for new
> creators."** New Creators now seed to qwen_ensemble (existing key-less
> records — Helmy, "local" — stay whisper; flipping them stays deliberate).
> This re-ranks the arc: lanes 1/2 now serve only explicitly-whisper
> Creators and drop in priority; the next grill should weigh lane 3
> (laughter-aware holds — applies to EVERY engine's captions) and the
> ADR 0058 re-measure as the live candidates, and confirm the ranking with
> the operator if present.

> **Operator ruling (2026-07-12, verbatim intent): stop polishing clip 3 /
> single videos. Fix captions so the fix applies to ALL FUTURE VIDEOS —
> the detection (dropped words), garble, and laugh classes, with the most
> accurate timing available.** The dedikornya verdict stays staged
> (nextprompt-deddy-verdict.md, ~2 min of their eye, no urgency); do NOT
> propose more per-clip curation as session work.
>
> **OPERATOR RULE (2026-07-12, broadened the same day): NEVER correct
> captions through store JSON — of any kind** ("dont touch any json to make
> correction for the captions, leave it just like what the llm produce it,
> so we can tune the code" — memory:
> captions-no-json-corrections-tune-code-only). No timing pins, no spelling
> entries, no multiword rewrites. A caption defect is fuel for a code lane
> below or gets surfaced with its measurement. Auto-harvested unverified
> rows are machine output and stay; the review queue is the operator's own
> product feature, not a session mechanism. **Fixture note:** the clip-3
> per-clip store was cleaned to raw the same day (all 4 confirmed entries
> removed; the eye-approved burns are reference videos only), so clip-3
> instruments now measure the pipeline's true output with no store layer —
> and `data\clip_alignburn.ass` there is a stale pinned emit until the next
> diag run refreshes it.

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

1. **~~Whisper-engine forced-alignment timing~~ — RESOLVED: measured and
   REFUSED at its pre-registered bar (ADR 0058, 2026-07-12).** The aligner
   is only safe over vote-cleaned words; whisper's unvoted token list is
   the binding constraint (double-transcribed brand → phrase-scale drag).
   The entry point + instrument are in-tree; the bars are ready-made for a
   re-measure once a whisper-side word cleaner exists. **The alternative
   the grill asked is now the LIVE question for the operator: should new
   Creators default to the ensemble engine instead?** (Quality-over-runtime
   pulls that way; ADR 0033/0035 made switching deliberate — a default flip
   is an operator policy call. Costs: 5 sidecar decodes + models per clip.
   One sentence from the operator decides it; a session can then wire the
   default + re-measure lane 1 behind it.)
2. **~~Whisper-engine recall parity~~ — PARKED BY RANKING (2026-07-12
   close-out; see the top banner).** The design question stays recorded (a
   second decode config? the ensemble's machinery on demand? ADR 0052's
   zero-added controls) but is not session work while the one-click
   ensemble flip covers the only two Creators it would serve.
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
   generalizing class. Text-only spelling entries stay legitimate curation;
   anything needing a TIMING value in a store file is banned (operator rule
   above) — surface it instead.

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
automatically", apply the recorded pick rubric; with lane 1
resolved-refused (ADR 0058) and lane 3 measured-and-staged (ADR 0062) the
autonomous default is **lane 2's measure-first phase** — design +
instrument + pre-registered bars for the whisper-engine witness/recall
question, production untouched until its own gate; note lane 2 now serves
only explicitly-whisper Creators (Helmy, "local") since ADR 0061. The one
still-staged verdict (dedikornya, nextprompt-deddy-verdict.md) goes at the
top of every handoff until the operator answers — ~2 min, no urgency);
finish with /handoff + whatwedone.md entry; commit as Evoyn with the model
trailer; `git push origin main` has standing permission; ALWAYS end with the
next `read nextprompt-<slug>.md and follow it.` line.
