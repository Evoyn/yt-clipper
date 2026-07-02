# Ensemble captions productization: per-Creator Caption engine, time-anchored queue, decode cache

## Context

ADR 0034's generalization gate ran (2026-07-02): the eh-pile fixture turned
whisper's 107-"eh" hallucination pile into 54 real voted words, the Deddy
clear-audio control held whisper's quality (183 units, "almost perfect" by the
operator's ear), and the operator's four word rulings on the control encoded
as time-anchored, clip-only corrections that applied on the next render. The
gate also surfaced and fixed two engine defects (comma'd stream folders
killing the sidecar; hallucination-pile skeletons poisoning fusion — see the
0034 amendment) plus a store-identity gap (duplicate `wrong` at different
moments collapsed in the layer merge — `at_s` is now part of a correction's
identity).

The engine is production-quality on measured content but remains an env knob
(`YC_QWEN_ENS=1`), invisible to the GUI, costing ~60–90 s per render, with a
review queue that starves on ensemble renders (harvest is whisper-keyed and
deliberately skipped, ADR 0034). This ADR records the productization grill's
decisions (operator, 2026-07-02 evening).

## Decision

1. **Caption engine, a per-Creator closed enum** — `Whisper` (default) |
   `Qwen ensemble` — picked in the import rail and remembered in the Creator
   store exactly like Caption Style and language (ADR 0016). NOT a
   `models/`-scanning picker: the ensemble recipe's constants are measured
   winners for Qwen3-ASR-1.7B; an arbitrary GGUF riding them has unknowable
   quality. A different model earns entry only through the same gate ADR 0034
   defines.
2. **Engine switches warn from entry shape, no provenance stamping.** The
   ADR 0033 coupling is real but the transfer classes are structural:
   single-word confirmed corrections carry (edit-1 fuzzy tier), `at_s` pins
   are positional by design; multi-word dict wrongs and `context` entries do
   not carry (they stay live on whisper renders). Switching a curated
   Creator's engine shows exact counts computed from the store — no schema
   migration, honest numbers.
3. **Ensemble-native harvest: contested vote slots, born time-anchored.**
   Slots where the winner lacked a strict voter majority harvest the
   ON-SCREEN voted word (fixing it fixes the caption) into the per-clip store
   as `unverified`, with `at_s` pre-filled from the fused unit's VOD moment
   and the usual dictionary/common-names/length/cap filters. Queue fixes
   thereby apply positionally by default — real-word garbles ("tiga") stop
   being global hazards. This is the ROADMAP's time-anchored-curation
   migration arriving through the queue instead of a big-bang schema change.
4. **Transport stays one-shot `llama-mtmd-cli`.** Five spawns per render
   (~60–90 s total, operator-accepted) with zero lifecycle state and a
   trivially-true GPU-sequential contract. `llama-server` (staged in
   `sidecars\llama\`) is revisited only when latency hurts real sessions —
   and the decode cache below attacks the same latency with less risk.
5. **Per-clip decode cache: timing skeleton + variant word lists**, stored
   under the stream folder's `data/`, keyed by clip range + engine + recipe
   hash (variant set, bias sentence, model file). Corrections apply AFTER
   vote/fusion inputs, so post-ruling re-renders skip all decoding
   (~90 s → ~1 s to captions) and iterations become bit-identical — the
   run-to-run reproducibility the perfection loop lacked.
6. **No default flip.** New Creators start on Whisper; the operator flips
   chosen Creators to the ensemble once and the Creator store makes it stick.
   Engine changes are deliberate per-Creator acts (ADR 0033's conservatism);
   the ensemble is measured on Indonesian only (the EN axis remains open);
   `YC_QWEN_ENS` stays as the headless/CLI override.

Implementation lands one slice per session, in value order: **picker** (GUI
citizenship + stickiness) → **decode cache** (fast deterministic curation
loops) → **harvest** (self-populating queue).

## Consequences

- The ensemble becomes an operator-visible product feature with per-Creator
  memory instead of an env secret; whisper remains the untouched default for
  everyone else (ADR 0033's opt-in contract holds at the Creator level).
- The review queue gains a future where every new entry knows its moment;
  `clip_only` + `at_s` become the norm for real-word fixes, and the layer
  merge treats the moment as identity (same `wrong`, different moments
  coexist — measured on the Deddy control's three "blok on" pairs).
- The decode cache makes store-curation iterations cheap, at the cost of a
  cache file per clip and an invalidation key to maintain; a stale-cache bug
  would silently freeze decode improvements, so the recipe hash MUST cover
  every constant the vote consumes.
- Switching engines for a curated Creator is informed, not blind: the warn
  quantifies what carries. Nothing stops the operator — their ear remains
  ground truth.
