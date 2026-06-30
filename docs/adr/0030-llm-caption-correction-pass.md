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

## Outcome

**Spiked and validated; integration pending (2026-06-30).** `--correct` mode added
to `yc-llm-judge`; Qwen reproduced 3/4 of the operator's clip-7 corrections exactly
with zero over-correction when fed the curated corrections. The render-path
integration is the next slice — built carefully against this ground-truth, off by
default, with operator sign-off before it ships (no repeat of the enh over-claim).
