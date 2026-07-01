# Caption decode trial knobs (opt-in), and the curation-coupling finding

The operator's standing caption pain is (a) clips where game SFX / music mask the
voice and (b) fast Indonesian slang — ADR 0014 adopted the dialect store for the
linguistic half, ADR 0029 tried (and shelved) denoising for the acoustic half.
whisper.cpp exposes standard anti-hallucination decoder settings the caption path
had never engaged: `no_context` (don't seed each 30 s window with the previous
window's text), `suppress_nst` (suppress non-speech tokens), and — since
whisper.cpp 1.7.5 / whisper-rs 0.16 — built-in **Silero VAD** pre-segmentation.
This slice wired all three into the caption (beam/DTW) load and **measured them
on the real caption path** (`caption_diag`, which decodes exactly as `do_render`)
against the operator's two documented hard clips.

## What the measurement showed (guntur69 `BUDS9qx2jw0`)

**"Diskusi biasa" (1881.5–1911.5 s, fast masked slang, the 23–26 s missed-speech
hole, fully curated per-clip store):**

- Old decode: 23 units. The curated fixes land (`dicegat`, `bajingan bajingan`,
  `dah cok`, `biadab anjing`, `anjing ngeri`) — but the 23–26 s hole is missed,
  "bangke" is a mistimed 4-second "pusing", and "kamu main dulu" is thin. This is
  the operator's current best output (mix + curation).
- `YC_SUPPRESS_NST=1` decode: **40 units**. It recovers **everything the rejected
  enh denoiser recovered — from the plain mix**: the 23–26 s hole ("Mana dah ini /
  Anjing busing…"), "bangke" at 19.7 s, "Kamu main dulu / dong". And it **breaks
  every curated anchor in the same stroke**: whisper's garbles shift under the
  perturbed decode (`dijekat`→`dijegat`, `pancingan`→`Pacingan`, the `tadi luar`
  window never forms), so the curated `wrong→right` pairs stop matching, plus a
  spurious "Turbiante" (enh produced the same phantom as "TORBIANTE").
- The clip is a single 30 s window, so `no_context` was inert here; the entire
  delta is `suppress_nst` — suppressing junk tokens renormalizes every decode
  step, and near whisper's limit the alternatives are near-ties, so the whole
  trajectory reshuffles.

**Clip #7 (2203–2233 s, the "eh"-pile / English-hallucination clip):** all three
knobs were a **wash** — the 115-unit zero-width "eh" pile reproduces identically
with and without them (the loop is intra-window, "eh" is a speech token, and
Silero classifies loud game audio as speech, so VAD trimmed nothing:
byte-identical output). The eh-pile is a *post-decode* problem (a repetition
guard / the timing pass), not a decoder-setting problem.

## The load-bearing finding: curation is coupled to the decoder

The dialect store's `wrong` keys are **whisper's exact garbles under one specific
decode configuration**. Change anything about the decode — enh (ADR 0029),
suppress_nst, temperature, a model swap — and a masked clip's garbles reshuffle,
so confirmed corrections silently stop matching. **Any decode change is a
curation-breaking event.** This is the third independent instance of the same
trade (htdemucs, enh, now suppress_nst), and it generalizes: there is no free
"just tune the decoder" win on already-curated content.

## Decision

1. **Ship all three knobs strictly OPT-IN, defaults byte-identical to before**
   (proven: the no-env `caption_diag` run reproduces the old output exactly).
   Env, not build flags, so a per-render A/B needs no rebuild (the `YC_ENH_ATTEN`
   precedent): **`YC_SUPPRESS_NST=1`**, **`YC_CAPTION_NOCTX=1`**, **`YC_VAD=1`**
   (needs `models/ggml-silero-v5.1.2.bin`, fetched by `fetch-models.ps1`).
   Read inside `yc_transcribe::Transcriber` so `do_render` and the inspectors
   can never diverge (the ADR 0030 fidelity lesson); the effective config is
   logged per decode (`caption decode: beam=5 no_context=… suppress_nst=… vad=…`).
2. **`no_context` yields to priming**: whisper.cpp routes `initial_prompt`
   through the same `prompt_past` that `no_context` disables, so the knob is
   ignored when the store primes (else it would silently kill `prime`).
3. **The recommended workflow** for a NEW hard clip: A/B `YC_SUPPRESS_NST=1` via
   `caption_diag` *before* curating, pick the decode whose transcript misses less
   real speech, then curate against that decode's garbles (per-clip store) —
   never flip a knob on an already-curated clip expecting the store to hold.
4. The detect refine load keeps whisper defaults unconditionally — its Moment
   ranking is operator-validated and reads word density, not exact words.

## Consequences

- Default renders are provably unchanged; the operator gains three per-render
  trial levers and loses nothing.
- The inspectors now install the app's tracing subscriber (they previously
  swallowed every diagnostic log, including whisper.cpp's own), so a diag run
  shows the dialect-store line, the decode config, and the correction stats.
- The curation-coupling finding raises the bar for ANY future decode-affecting
  idea (fine-tuned models, VAD-by-default, temperature schedules): it must be
  weighed against re-curating every affected Creator store, and A/B'd on a
  curated clip via `caption_diag` first.
- VAD is retained despite the null result: it is the only lever that could stop
  whisper free-running over genuinely silent/music-only stretches (a failure
  class the tested clips don't exhibit — their noise is loud game audio Silero
  calls speech). Re-probe if such a clip appears.
