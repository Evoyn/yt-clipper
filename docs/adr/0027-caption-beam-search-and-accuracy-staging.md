# Caption decoding: beam search on the render path; and the accuracy-staging finding

The operator reported "too many wrong captions, even ones not caught by `id.json`"
and asked for ~99% accuracy, render time no object. They chose a staged plan —
voice isolation + beam search + an LLM correction pass, LLM-correction first.

Before building the heavy stages, `caption_diag` was run on real clips to see what
the errors actually **are** — and the finding reframes the staging.

## Finding (diagnosis)

On a **clear** clip (the 2-voice "guntur69" segment, and the Ino horror segment)
whisper-large-v3 is already **accurate**: e.g. "disuruh icip biasanya kan cewek
kalau masak … roti mariam" is coherent Indonesian; the few oddities are reading an
on-screen donor name ("LeherSakitOG" → "ler sakit") or emphatic repeats ("asli asli
asli"), not transcription errors. So the operator's bad captions are most likely
**audio-driven** — game SFX / music masking the voice on *gameplay-heavy* clips, and
two overlapping voices — not word-level context errors.

That ordering matters: **voice isolation** (clean the streamer's voice before
whisper) attacks the root cause and is the higher-value lever; an **LLM
word-correction** pass mainly fixes context/homophone errors, of which clean clips
show few — and on a *garbled* (noisy) word the LLM can't hear the audio, so it may
"correct" to a plausible-but-wrong word. The decoder itself (greedy vs beam) is also
not the bottleneck on clean audio.

## Decision (this slice)

Ship the one change that is **standard, low-risk, and matches "time is no object"**:
**beam search** on the caption (render) path.

- `Transcriber` decodes with `SamplingStrategy::BeamSearch { beam_size: 5 }` on the
  **DTW / caption** load (`load`), and stays `Greedy { best_of: 1 }` on the
  **text-only detect** load (`load_text_only`) — detect scans many candidates for the
  excitement lexicon, where greedy's speed matters and exact words don't. The flag is
  tied to the DTW load (`beam: dtw`).
- `best_of: 1` greedy was whisper.cpp's *fastest, lowest-quality* setting; beam search
  is its conventional quality decoder. Reversible (one bool + `BEAM_SIZE`).

The heavier stages are **deferred to operator-steered work**, because caption quality
is judged in Indonesian — the operator is the ground truth, and a wrong-priority or
over-correcting pass would *worsen* the very thing they want fixed:

- **Voice isolation** (a speech-grade enhancer — DeepFilterNet / a speech separator,
  *not* the music-separator htdemucs that ADR 0014 measured dropping speech): the
  likely-biggest lever, but a model integration to validate against a real noisy clip.
- **LLM correction pass**: build with strong guardrails (sparse, in-place, word-count
  preserving) once there is a real bad-caption example to target and the operator can
  A/B the result.

## Considered options

- **Beam search on both detect + caption.** Rejected: beam × many detect candidates
  is much slower for no detection benefit (detect reads density, not exact words).
- **Build the LLM correction now (the operator's first pick).** Deferred per the
  finding: clean-clip errors are few and the lever for noisy clips is the *audio*; an
  unverifiable (by me) correction pass risks worsening captions. Better built against a
  real bad-caption example with the operator validating.

## Consequences

- One new `Transcriber.beam` field + `BEAM_SIZE` const; the caption path is slower
  (acceptable) and, in general, more robust to greedy's local mistakes.
- A genuine A/B is the operator's: on the clean test clip beam gave a comparable
  transcript with minor differences (e.g. "gitu gitu" → "gitu-gitu"; it also collapsed
  one unclear donor-name shout-out) — clip-dependent, as expected.

## Outcome

**Shipped (2026-06-28).** Beam search on the caption path; 19 transcribe tests green;
runs clean on a real clip (48 units, 0 dropped, timing intact). The voice-isolation
and LLM-correction stages are surfaced to the operator with the finding, to be built
against a real bad-caption clip with their native-speaker validation.
