# Qwen3-ASR ensemble captions (opt-in): words by vote, timing from whisper

## Context

The operator's standing caption pain — masked fast Indonesian slang — hit its
measured ceiling with single-decode whisper: ADR 0033 showed that near the
model's limit every decode perturbation just *reshuffles* the garbles, and the
dialect store (ADR 0014) can only patch the exact garbles of one decode config.
The Qwen3-ASR A/B trial (ROADMAP "ASR engine upgrade", 2026-07-02) then showed
Qwen3-ASR-1.7B reads real speech where whisper hallucinates (the 107-"eh" pile)
and recovers missed speech from the plain mix — but its own single decodes
garble other regions, and word timestamps do not survive the llama.cpp mtmd
path at all.

The operator's directive for the "Diskusi biasa" benchmark clip (2026-07-02,
ground truth in `benchmarks/diskusi-biasa.groundtruth.txt`): iterate until the
captions are as close to their transcription as possible, render time no
object, ideally with **no operator curation**.

## What the iteration measured (the loop's evidence)

Sixteen decode experiments on the benchmark clip, scored word-by-word against
the operator's ground truth (`asr_score`), established:

1. **Garbles anticorrelate across decode variants.** Denoise strength
   (deep-filter a6/a12), 5 s of leading audio context ("head-pad"), and a
   one-sentence biasing context each fix a different region and break another
   (e.g. the a6+head-pad decode nails "anjing ngeri banget bangke" but garbles
   "bajingan"→"pacingan"; the plain a6 decode has "bajingan bajingan" clean).
   No single config dominates — the same near-tie physics as ADR 0033.
2. **A token-level plurality vote across variants beats every single decode**:
   26/39 strict-correct words vs 16-18 for singles, recovering "bajingan
   bajingan", "ini satu", "mana tadi cok", "pusing cok", "pusing kan dibilang",
   "anjing ngeri banget bangke", and dropping padding bleed via a
   strict-majority insert rule. whisper's words join the vote as one voter
   (cross-engine: it contributed "kamu" and "di depan" evidence).
3. **Biasing context works but is dose-sensitive**: a one-sentence generic
   slang list fixed profanity bowdlerization ("bacingan"→"bajingan"); longer
   lists or added instructions derailed whole regions. The list ships as a
   per-language constant — generic gaming-stream vocabulary, never per-clip.
4. **The residue is dialect-class, not decode-class.** Six words stayed wrong
   in every one of ~30 decodes (both engines, bf16 included): dicegat, biadab,
   ayok, kreeng, ngeri(one position), anying — sounds genuinely absent from
   every model's posterior under game-SFX masking. This is ADR 0014's finding
   quantified: no decode fixes dialect; only knowledge (a store, or a future
   fuzzy-match store application) can.
5. **Dead ends, measured**: LLM repair/rescoring with the local Qwen2.5-7B
   judge (three prompt shapes — free repair, batched lattice choice, per-slot
   local choice) made the transcript WORSE each time (picked non-words,
   flipped correct votes); bf16 weights changed nothing (quantization is not
   the bottleneck); aggressive denoise (a20) and long bias prompts derail;
   uncleaned padded audio triggers an 80-repetition runaway ("kau main dulu"
   x80 — the llama.cpp #21847 failure class, avoided by deep-filter input).

## Decision

A new opt-in caption engine in `yc_transcribe::ensemble`, wired into the
render's caption stage (`do_render`) behind **`YC_QWEN_ENS=1`** (default OFF,
render provably unchanged when unset — the ADR 0033 opt-in contract):

- **Words**: five Qwen3-ASR-1.7B-Q8 decodes of the clip range (deep-filter
  a6/a12 x head-pad/no-pad + raw mix, one-sentence generic bias context,
  temp 0) via one-shot `llama-mtmd-cli` spawns; whisper's production words
  join as a voter; token-plurality vote, backbone = a6+head-pad, strict
  voter-majority for insertions.
- **Timing**: whisper's transcript (beam+DTW, exactly as today) is the timing
  skeleton. Voted words align onto whisper units: matches adopt the unit's
  span, whisper-only units DROP (the outvoted hallucination class), voted-only
  runs get character-proportional spans inside the enclosing whisper gap.
  Downstream refine/ASS/karaoke are untouched.
- **Store isolation**: the ensemble path neither reads nor writes dialect
  stores; auto-harvest is skipped when it ran (candidates are whisper-keyed
  and would misattribute). GPU stays sequential (whisper's one-shot load drops
  before the sidecar spawns; the sidecar exits between variants).
- Fails soft: any missing sidecar/model or stage error logs a warning and the
  whisper captions stand.

## Consequences

- The operator gains a per-render lever that measurably beats the production
  decode on masked clips (26/39 strict vs 16 whisper-raw on the benchmark;
  ~33/39 counting spelling-equivalent slang) with zero curation input — and
  costs ~5 extra sidecar decodes per clip (~60-90 s; operator accepted).
- The six residual dialect words on the benchmark still need knowledge. The
  existing per-Creator store carries several of them (dicegat, biadab anjing)
  from prior curation — a fuzzy (edit-tolerant) store application on the
  ensemble output is the natural follow-up, and the review queue remains the
  path for the rest. "Zero curation" is measured to be ~85% of ground truth,
  not 100%.
- The dialect store's coupling (ADR 0033) does not extend to the ensemble
  path: stores are keyed to whisper's garbles, and the ensemble output is not
  store-corrected. A store curated FOR ensemble output would be a different
  store — deferred until the operator judges the rendered result.
- The vote/fusion core is pure and unit-tested (7 tests); the recipe constants
  (variant set, bias sentence, pad length) are the benchmark's winners and
  will need re-validation on other clips before any default flip.
- bf16 GGUFs were staged during the search (`models/*bf16*`, ~4.2 GB) and are
  NOT used by the recipe — safe to delete if space matters.
