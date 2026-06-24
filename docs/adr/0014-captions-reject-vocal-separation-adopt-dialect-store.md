# Captions: vocal separation rejected (harms completeness), dialect-correction store adopted

The vocal-separation milestone — reserved by ADR 0008 and deferred a second time
by ADR 0013 — was finally **built and measured**. The runtime is validated
(in-process `ort` + htdemucs-ft-vocals ONNX, waveform I/O, ~2x realtime CPU) and
wired into the export caption path behind a `sep` feature, transcribing a per-Clip
**Vocal stem** instead of the mixed analysis audio. But measuring it on real
content **overturned the caption justification a third — and final — time**:
separation does not just fail to help, it actively **harms** caption completeness.
The real caption pain was, every time, **linguistic** — the streamer's Medan
accent, local slang, and viewer names — so this ADR rejects separation as the
caption fix and adopts a **curatable dialect-correction store** instead.

## What the measurement showed (Cibaduy / `CWN_qbRZBSo`, Indonesian, min 3)

- **Separation drops real speech.** htdemucs is a *music* separator (trained to
  pull *sung* vocals out of songs: drums/bass/other/vocals). On spoken gaming
  chatter where the streamer's voice is quieter or tangled with loud game SFX, it
  misclassifies real speech as "non-vocal" and removes it. The stem of min3 fell
  from **108 raw whisper units to 60** — the entire first half of the speech was
  gone (the envelope floor dropped while the dropped span read as silence). For
  captions, where *completeness* is what matters, losing ~45% of the words is far
  worse than keeping them with some dialect garble. The operator confirmed by ear:
  the mixed transcript is better than the stem.
- The earlier "loud intro music" A/Bs (this VOD's opening, ADR 0013's repro,
  `d_F8KwQLQXo`) were **washes** — the mic sat above the music; the only caption
  gap was real silence. Three VODs, zero caption gain, and now a measured loss.

## Considered options

- **Vocal separation for captions (the built milestone).** Rejected. Validated
  runtime, real working code — but it *removes* real speech on accented spoken
  word. Kept behind the off-by-default `sep` feature: the runtime is the one piece
  worth preserving, and it may still serve the use ADR 0008 reserved it for
  (whole-VOD *arousal discovery*, scoring the streamer not the game), which
  tolerates lossy separation. It is simply wrong for captions.
- **Whisper `initial_prompt` priming alone.** Rejected as a default. A
  content-describing sentence ("Streamer Indonesia main game live…") makes
  whisper-large-v3 abandon the audio and hallucinate YouTube boilerplate
  ("Jangan lupa like & subscribe") on repeat — **108 units → 14**. Even a bare
  term list *drifts* the whole transcript (108 → 77). Priming biases everything,
  not just the target words, so it is opt-in only (`prime` flag, default off).
- **A correction dict (the adopted core).** A deterministic, whole-word
  `wrong → right` fix-up applied after transcription. On min3 it fixed
  `mendokong → mendoakan` and `protesi → profesi` with **zero drift** (108 units
  unchanged but for the corrected words). Safe, cheap, and it improves as the
  store is curated. This is the win.
- **An LLM post-correction pass (reuse the `yc-llm-judge` sidecar).** Deferred.
  The remaining errors are viewer names and hyper-local Medan slang that a general
  Indonesian LLM knows no better than whisper, and it carries the same drift risk
  as priming. Only the operator has the ground truth — which the dict captures.

## Decision

Reject vocal separation as the caption fix (off-by-default `sep`), and adopt a
**curatable per-language dialect-correction store** (`assets/dialect/<lang>.json`,
`yc_transcribe::DialectLexicon`) driving three mechanisms:

1. **Correction dict (always on).** Confirmed `wrong → right` pairs patch
   whole-word mishears after transcription (captions *and* the detection lexicon
   read the corrected text). The risk-free, deterministic win.
2. **Priming (`prime`, default off).** A *bare term list* (never the descriptive
   `note`) primes whisper's decoder. Opt-in per Creator because it drifts.
3. **Auto-harvest (`harvest`, default on).** Each caption run, words whisper was
   least sure of (min token probability) that are **not in a bundled real-word
   dictionary** (`assets/dialect/<lang>.words.txt`) and not already known are
   appended to the store as `unverified` to-dos — capped per clip, least-confident
   first, confidence recorded. The store self-populates a review queue; the
   operator fills the blank `right` fields over time and the dict keeps improving.

The dictionary filter is what makes harvest usable: on accented audio whisper is
uniformly unsure, so confidence alone flags many *correct* words; skipping
real-dictionary words leaves the garbles/slang/names (min3: `berapa/kalau/keren`
filtered out, `teong/kodo/popek/mandem` surfaced).

`HARVEST_MAX_P`, `HARVEST_MAX_PER_CLIP`, and the dict-vs-confidence balance are
tune-from-use, like the M6 editor and caption-timing constants.

## Consequences

- The export captions the **mixed** audio (the historical path); only what
  whisper *hears for the transcript* could change (and `sep`, being off, leaves it
  the mix). The rendered clip's audible audio is always the mix.
- New net dep: `serde`/`serde_json` in `yc-transcribe` (the store is JSON). The
  bundled `id.words.txt` (~79.9k words, geovedi/indonesian-wordlist) is read
  lossily so a non-UTF-8 byte never sinks the load.
- `transcribe`'s signature gains a `&DialectLexicon`; a `transcribe_with_harvest`
  / `transcribe_range_harvesting` variant returns the harvest candidates, used
  only on the caption path (detection's refine transcribe pays nothing for it).
- The store is per-language today; per-Creator scoping arrives with the Creator
  store (still unbuilt). The hardcoded `corrections()` map is gone, replaced by
  the file.
- Deferred, in rough priority: an LLM post-correction pass for the long tail;
  remembering operator *deletions* so a rejected harvest word is not re-flagged;
  multi-word phrase corrections (the dict matches whole single words).

## Outcome (2026-06-24)

Implemented and verified end-to-end on `CWN_qbRZBSo` min3 (`caption_diag` + a real
headless render): dict fixes `mendoakan`/`profesi` with zero drift; priming
confirmed to drift/hallucinate and left off; auto-harvest writes a clean queue
(`pahal, ralih, teong, kodo, popek, mandem` — real garbles — after the dictionary
filters `berapa/kalau/keren/begitu`). 8/8 `yc-transcribe` tests pass; workspace
compiles; production binary is no-`sep`. The `sep` runtime is recorded as
validated-but-rejected-for-captions, available for a future arousal-discovery use.
