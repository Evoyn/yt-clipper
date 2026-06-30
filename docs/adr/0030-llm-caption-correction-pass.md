# LLM caption-correction pass: hybrid, confidence-aware, dialect-store-fed

The operator watched the real renders of the bad-caption corpus and reported the
captions still have "too much error", asking to "fix the captions so it can detect
gibberish also" and get "correct meaning". Their clip-7 ground-truth shows the
remaining errors are **linguistic**, not audio masking:

| kind | example | whisper was |
|------|---------|-------------|
| garble | `dimalai malai → dimarahin` | unsure (low conf) |
| slang | `cowok → cok` | **confident** (real word) |
| name | `tadi tidur → tadi tur` (Guntur) | **confident** (real word) |
| spurious | `yang horror itu → yang horror` | confident |
| missed | `diam dulu diam dulu` (no text) | — (un-fixable) |

Denoising (ADR 0029) does not touch these — and on this clip it made #7 *worse*
(see the ADR 0029 correction). The lever ADR 0027 queued for exactly this is an
**LLM correction pass**.

## The risk this must design around (ADR 0027)

The LLM **cannot hear the audio**, so on a garbled word it can "correct" to a
*confident-but-wrong* word and make captions worse — the operator's exact
complaint. ADR 0014 likewise rejected priming for drift. So the whole design is
guardrailing against over-correction.

## Decision (the hybrid, operator-chosen)

A **confidence-aware, dialect-store-fed correction** run after whisper + the
dialect dict, before the ASS, via the existing `yc-llm-judge` sidecar (Qwen2.5-7B,
greedy/temp-0, already in the box):

- **Auto-fix only the words whisper was UNSURE of** (its per-word min-token
  confidence, the same signal the harvest uses) — to coherent Indonesian, using
  context.
- **Override a CONFIDENT word only when it matches a curated correction** the
  operator has confirmed in the dialect store (e.g. `cowok→cok`, `tidur→tur`). The
  LLM applies these *in context* — which is exactly what a blind global dict cannot
  do safely (it would corrupt every real "cowok"/"tidur"). The LLM never
  free-invents over a confident word.
- **Per-word, timing-preserving:** HugeWord captions are one word per ASS line, so
  corrections map word→word onto each word's DTW timing; a clearly-spurious word may
  be **deleted** (its time absorbs into neighbours); **no words are added** (a
  whisper-missed gap stays missing — inventing it *is* the hallucination risk).

## Spike (measured before integration — the enh lesson, applied)

Added a `--correct` mode to `yc-llm-judge` (free-form completion; the prompt rides
in the JSON request so it tunes without a rebuild) and ran Qwen on clip-7's caption
against the operator's ground-truth:

- **First pass (slang hint only):** fixed `cowok→cok`, **zero over-correction**,
  but conservatively missed the rest. The safe direction — fixes what it's told,
  never invents.
- **With the curated corrections supplied** (as the dialect store would hold them):
  **3 of 4 exact** — `dimarahin`, `cok`, `tur` all correct, every good word
  preserved; only miss was not deleting the spurious `itu`. This is the validation:
  the hybrid works on real ground-truth.

## Considered options

- **Full free rewrite for meaning.** Rejected (operator + risk): catches more but can
  invent confident-but-wrong words on audio it can't hear — the exact failure to avoid.
- **Confidence-gate only (never touch confident words).** Rejected as the whole
  answer: cannot do `cowok→cok` / `tidur→tur` (those are confident). It is the *safe
  core*; the curated overrides extend it.
- **Dialect dict alone (deterministic).** Insufficient: `cowok→cok` can't be a global
  rule (breaks real "cowok"). The dict handles unambiguous garbles; the LLM handles
  context. They compose.
- **A bigger/cloud model.** Rejected: Offline. Qwen-7B already handles the curated
  case; hard unhinted garbles (unaided `dimalai malai→dimarahin`) it misses — those
  fall to the dialect store.

## Consequences

- New `--correct` mode on `yc-llm-judge` (free-form `complete`, `N_CTX_CORRECT`,
  `MAX_CORRECT_TOKENS`); the detect-time judge IPC is unchanged.
- Integration (NOT yet done) needs: a caption-path call after the dialect dict that
  sends units + confidence + the dialect store's corrections + Creator/topic context;
  map the returned words back onto unit timing (handle deletions); off-by-default
  feature, gated on the sidecar + a flag; A/B on clip-7 + operator sign-off.
- Tunables: prompt assertiveness vs over-correction; whether to mark low-confidence
  words explicitly (the spike got zero over-correction without it — may be optional).

## Integration (2026-06-30, this session)

Built the render-path pass behind an **off-by-default `correct` cargo feature**,
gated also on the sidecar + GGUF being present — a default build captions exactly as
before (maximum safety on the path the operator ships). `do_render` now, after the
dialect dict + harvest and **before** caption timing, runs `correct_captions` ->
`run_llm_correct` (a `yc-llm-judge --correct` shell-out mirroring the detect judge's
IPC). The pure logic lives in `yc_transcribe::correct`, unit-tested without a
model/GPU:

- **Index-anchored IPC.** Units are sent as `N: word` (the ones whisper was unsure of
  marked `[?]`); the model replies `N: word` / `N: DELETE`; corrections map back **by
  index**, so each unit's DTW timing is preserved and a rephrased reply can't desync
  the mapping. A merge is `replace + DELETE` on adjacent units; a deletion's slot is
  absorbed by the existing gap-fill timing; **words are never added**.
- **The `context` flag (data model).** `Correction { context: true }` marks a
  context-sensitive override (`cowok -> cok`, `tidur -> tur`): a real word whisper
  reads confidently but the streamer meant as slang/a name. The always-on global dict
  **skips** these (it would corrupt every real `cowok`); they are fed to the LLM as
  hints and **enforced in code** as the only edits allowed over a *confident* word.
- **Confidence gate (the guardrail).** `CORRECT_UNSURE_P = 0.50`. An *unsure* word the
  model may rewrite or drop; a *confident* word it may change only via a matching
  curated override, and never delete unless it is a repeat. The code enforces this —
  the prompt is a hint, the gate is the safety. (Measured: the model proposed a
  confident `pelan-pelan -> pelan`; the gate rejected it.)
- **Deterministic filler collapse.** whisper repetition-hallucinated a **22-long
  `eh eh eh ...` run** on clip-7; left in, it also made Qwen **miscount** the per-line
  index protocol (the numbering drifted by one after ~20 deletions, which would
  mis-map every later word onto the wrong unit). `collapse_adjacent_duplicates` merges
  runs of the same short (<= 3-char) filler into one unit *before* the request — the
  spam goes deterministically and the list stays short enough to count. Word-length
  emphasis (`asli asli asli`) is preserved (only <= 3-char words merge); reduplication
  (`kanan-kanan`) is one token, never an adjacent pair, so it is untouched.
- **Prompt.** A timid first prompt made Qwen *echo the input unchanged*; a directive,
  few-shot prompt applies the curated terms and fixes obvious garbles while leaving
  coherent words alone. It rides in the sidecar JSON (tunes without a rebuild). A
  separator-only rewrite (`kanan-kanan -> kanan_kanan`) is treated as no change.

### Re-validation on clip-7 (real beam transcript, production audio source)

Ran the real path (`transcribe_range_full` -> collapse -> build -> `yc-llm-judge
--correct` -> apply) on the guntur69 `analysis.wav` 2203.5-2233.5s — the same audio
source and functions the render uses:

- `dimalai-malai -> dimarahin` (global dict) ✓, `cowok -> cok` ✓, `tidur -> tur` ✓
  (curated overrides, applied by the LLM in context) — the spike's 3/4, now
  end-to-end through the shipped code path.
- The 22-`eh` hallucination run collapsed to one (deterministically).
- **Zero over-correction**: every real word (`semua`, `horor`, `interaksi`, `kunci`,
  `pelan-pelan`, ...) preserved; the model's confident `pelan-pelan -> pelan`
  over-reach was rejected by the gate; a `kanan-kanan` separator rewrite ignored.
- Still missed: dropping the spurious trailing `itu` (`yang horor itu`), the same 1/4
  the spike missed — it is a *confident* real word, which v1 deliberately won't delete.

Honest scope: on this clip the LLM's marginal contribution is exactly the **two
context overrides** the global dict cannot do safely; the garble fix came from the
dict (operator-curated) and the filler cleanup from deterministic code. That is the
design working as intended (dict + LLM + collapse compose), not the LLM alone.

A second clip (#3 "Streamers Repetitive Chat", no curated overrides) stress-tested the
*auto-fix* path the curated clip-7 didn't exercise: of 25 units the model changed
exactly one — a 0.09-confidence garble `ngomplok -> ngumpul` — and left every other
word, including coherent-but-unsure ones (`pilih`, `lagi`, `ngomong`, `aja`, `ke`) and
an odd-but-confident `torah`, untouched. So the surgical, zero-over-correction
behaviour holds on the un-curated path too (semantic correctness of the one fix is the
operator's to confirm).

### Real-render A/B (2026-06-30, this session)

Rendered clip-7 both ways from the VOD (`--features correct,face`, cached
`analysis.wav`), toggled by a new **`YC_CORRECT` env switch** (a `correct` build runs
the pass unless `YC_CORRECT=0` — so the operator A/Bs from one build, same segment).
The treatment render applied **4 corrections, 0 rejected**: `cok` + `tur` (curated),
two unsure garble auto-fixes, plus the **21-unit filler collapse** — captions go from
a `cowok / tidur` + `eh x22` mess to clean text. **But the real render also exposed a
caveat the curated spike hid:** with only the generic per-language topic (no guest
name), the two unsure auto-fixes were *questionable* — `buntur -> buntut`,
`dakenyang -> dakanya` (neither clearly right; `buntur` is really the guest "Guntur").
The curated + dict + collapse wins are solid; the **un-curated garble auto-fix is the
weak spot** — the proper fix is curation (confirm `buntur -> Guntur`) or a more
conservative auto-fix (only replace into a real dictionary word). The operator's A/B
verdict decides. The two renders are `clip7_A_NO-correction.mp4` /
`clip7_B_WITH-correction.mp4` in the guntur69 stream folder.

### Narrowed to curated-only (2026-06-30, operator A/B feedback)

The operator watched render B and reported the parts that were *worse*: the unsure
**auto-fix guessed wrong** on this streamer's names/slang (the guest "Guntur", split
by whisper into "Hai buntur", became "buntut"; `dakenyang` -> "dakanya"), and the
**filler collapse ate real repeated shock-reactions** ("eh eh eh"). The curated
overrides (`cok`, `tur`) were fine. Diagnosis: the model can't hear the audio, so on
domain words it doesn't know (local names/slang) it produces confident-but-wrong
guesses — exactly the ADR 0027 risk — and a blind collapse can't tell a hallucinated
zero-width pile from genuine spaced repeats.

So the pass was **narrowed to do only the one thing the global dict cannot**: apply a
curated **context override** (a real word the streamer meant as slang/a name) where
the context fits. Removed: the unsure-word auto-fix, the repeated-filler collapse, and
all deletion. Consequences:

- **Garbles/names are curated, not guessed.** They harvest -> the operator confirms ->
  the deterministic dict fixes them (accurate; the operator is ground truth). This
  session curated `dimalai-malai -> dimarahin`, `buntur -> Guntur` (+ a **multi-word**
  `hai buntur -> Guntur` that collapses whisper's split and drops the spurious "Hai"),
  `dakenyang -> dah kenyang`, and `teh -> eh` (context — `teh` is a real word "tea").
- **Repeated reactions are kept.** No collapse; the timing pass already renders the
  zero-width hallucination pile to ~nothing while the genuine spaced "eh"s show.
- **The LLM applies only curated overrides**, index-anchored, refusing any other
  change (`apply_correction` is curated-only; a non-curated replacement or a delete is
  rejected). `build_correction_request` returns `None` when the store has no context
  overrides, so a clip with none skips the GPU entirely.

Re-rendered clip-7 (real path): B now reads `Guntur dah kenyang ... eh eh eh (kept) ...
semua cok ... itu tur ... harus dimarahin ...` — every operator-flagged error fixed,
"3 applied, 1 rejected" (the one non-curated guess refused). Remaining: the spurious
confident `itu` (`yang horor itu`) is not dropped (this pass never deletes), and
`diam dulu diam dulu` stays missing (whisper didn't hear it — audio limit, not
linguistic). 30 transcribe tests green.

## Outcome

**Integrated, narrowed to curated-only after the operator's A/B, re-rendered; OFF by
default, pending the operator's sign-off (2026-06-30).** The pass now applies only the
operator's curated context overrides (the one thing the dict can't do safely);
everything else is curated into the dict or left for the timing pass. Real clip-7
render B fixes every error the operator flagged. It stays **off** (`--features
correct`; `YC_CORRECT=0` disables at runtime for the A/B) until the operator signs off
on the re-rendered B — the enh over-claim lesson
([[validate-on-production-path-before-claiming]]). Open, lower-priority: dropping a
*confident* spurious word (the `itu` miss) needs a safe rule; recovering whisper-missed
speech (`diam dulu`) is an audio problem (the rejected/unvalidated enh path), not this
pass's job.
