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
- **Fuzzy store transfer (curation survives the engine swap)**: after the
  vote, the layered store's confirmed corrections apply to the voted words
  with edit-1 tolerance — whisper's curated garble key ("dijekat") matches
  the ensemble's spelling of the same mishear ("dijegat") even though the
  exact strings differ. Conservative guards: single-word `wrong` of >=4
  chars; the fuzzy tier never fires on a token that is a real dictionary
  word; `context: true` pairs are skipped (ADR 0030's LLM-pass territory);
  multi-word wrongs don't transfer (whisper's multi-word garble shapes are
  engine-specific). This is the answer to ADR 0033's coupling for this path:
  EXISTING curation transfers, no re-curation.
- **Store write isolation**: the ensemble path never writes dialect stores;
  auto-harvest is skipped when it ran (candidates are whisper-keyed and
  would misattribute). GPU stays sequential (whisper's one-shot load drops
  before the sidecar spawns; the sidecar exits between variants).
- Fails soft: any missing sidecar/model or stage error logs a warning and the
  whisper captions stand.

- **Time-anchored corrections (`at_s`, added after the operator's watch
  feedback)**: a correction may carry a VOD-absolute moment. The ensemble path
  applies it positionally — the occurrence of `wrong` nearest that moment,
  pinned to the speech onset there (tight 0.75 s snap radius; their ear wins
  past it), with neighbor repair (left compression with wide-donor sharing,
  right shifting). `wrong == right` is a pure timing pin; a multi-word `right`
  inserts words no engine's posterior contains (the benchmark's scream-masked
  "tur biadab anjing"). The whisper dict path skips `at_s` entries — global
  application would hit every occurrence, which the anchor exists to prevent.
  This is the working seed of the ROADMAP's time-anchored-curation migration;
  harvest notes already record the same timestamps (ADR 0022), so the review
  queue can grow an "and it's at THIS moment" field later.

## Consequences

- The operator gains a per-render lever that measurably beats the production
  decode on masked clips (26/39 strict vs 16 whisper-raw on the benchmark;
  ~33/39 counting spelling-equivalent slang) with zero curation input — and
  costs ~5 extra sidecar decodes per clip (~60-90 s; operator accepted).
- The six residual dialect words on the benchmark still need knowledge. The
  fuzzy transfer recovers those the operator already curated (dicegat on the
  benchmark); the review queue remains the path for the rest (kreeng, ayok,
  ngeri-at-one-position, anying — sounds measured absent from every engine's
  posterior). "Zero NEW curation" is the honest contract: ~85% of ground
  truth from the vote alone, plus whatever the Creator's existing store
  transfers.
- The vote/fusion core is pure and unit-tested (7 tests); the recipe constants
  (variant set, bias sentence, pad length) are the benchmark's winners and
  will need re-validation on other clips before any default flip.
- bf16 GGUFs were staged during the search (`models/*bf16*`, ~4.2 GB) and are
  NOT used by the recipe — safe to delete if space matters.

## Amendment (2026-07-02): the generalization gate found two engine defects

Running the recipe unchanged on the two held-out fixtures (this ADR's
required gate before any default talk) surfaced and fixed two defects —
both invisible from the exports alone, because the engine fails soft:

1. **Stream-folder paths break the sidecar.** llama-mtmd-cli's `--audio` is a
   list flag that SPLITS ON COMMAS, and its C-level file open trips on
   non-ASCII names — the Deddy control's stream folder ("... Tretan, Coki,
   Adriano") killed all five variants ("Unable to open file ... Tretan"), the
   render soft-failed to whisper captions, and the export looked fine. The
   spawn now runs in the variant wav's directory and passes the bare
   `_ens_N.wav` name (ours, pure ASCII), with exe/model paths absolutized.
   Failure capture now reports the sidecar stderr's TAIL (llama.cpp puts the
   fatal line last) — the original head-capture showed only load noise.
2. **A hallucination-pile skeleton poisons the fusion.** On the eh-pile
   fixture the vote replaced ~107 of whisper's 115 "eh" units, but fusion
   still trusted the skeleton's few accidental matches as anchors and boxed
   21 real words into a 0.12 s window (zero-width caption piles at two
   instants). Fusion now measures skeleton trust ANCHOR-side — under 1/3 of
   anchors claimed, the whole skeleton is dropped and every word places by
   speech onsets — and `place_run` spreads a run across dense onsets (first-N
   crammed the gap's head) or character-proportionally when onsets are too
   scarce to structure it (words > 2x onsets). Word-side adoption is the
   wrong trigger: edit-1 lookalikes ("deh"/"es" vs "eh") keep it high on
   exactly this clip class.

Gate outcome (YC_QWEN_ENS=1 renders, huge genre): eh-pile — 115 whisper units
(the documented pile) -> 54 voted real-content units, fuzzy store transfer
firing cross-engine ("buntor"->"guntur", "sakenyang"->"dah kenyang"), fallback
log line `skeleton distrusted (17/230 anchors claimed)`, monotonic readable
timing across the clip. Deddy control — all five variants decode (186–204
words), vote -> 183 units on a trusted skeleton, no regression vs whisper's
159-unit read. The operator's ear rules the final gate verdict on both
exports (`clip-36-43 (2).mp4`, `clip-30-00 (4).mp4`).
