# Cleaned-voice captions: a gentle DeepFilterNet denoise before whisper

ADR 0027 diagnosed the operator's "wrong captions" as **audio-driven** (game SFX
masking the voice, not word-level errors) and deferred the fix — a speech-grade
voice cleaner — until there was a real bad-caption clip to build and A/B against,
because caption quality is judged in Indonesian (the operator is ground truth).

That clip arrived: the 2 h horror co-stream `BUDS9qx2jw0` ("guntur69"). A top-10
export surfaced the noisy moments; the operator picked **#7** (36:43, whisper
hallucinating *English* out of noise) and **#8** (1:21:57, the loudest clip, pure
gibberish) as targets, with **#9** (1:15:05, clean FPS talk) as a control.

## Model class: a denoiser, not a separator

ADR 0014 rejected **htdemucs** for captions: it is a *music* separator (sung
vocals out of songs) and dropped ~45 % of real spoken speech (108 → 60 units).
The lesson was not "audio processing is hopeless" but "a *music separator* is the
wrong tool." A **speech denoiser** is the right class — it is trained to *keep*
speech and suppress non-speech noise. **DeepFilterNet** (full-band 48 kHz, real
time, MIT/Apache) is the reference model, shipped as a self-contained
`deep-filter` binary with the model baked in — so it slots in as a **sidecar**
(like ffmpeg/yt-dlp), pulling **no new Rust dependency** and avoiding a from-scratch
re-implementation of its 3-model recurrent inference (which would have re-opened
the ADR-0014 risk of a subtle DSP bug silently corrupting captions).

## Finding (spike, measured before any integration)

Spiked standalone on #7/#8/#9 (denoise → whisper via the real `caption_diag`
caption path), exactly as `sep_spike` validated htdemucs before promotion:

- **Full denoise (`-a 100`, the default) DESTROYS speech.** On #7 the streamer's
  voice sat *under* the game audio, so maximum attenuation gouged it to **dead
  silence** — and whisper-large-v3 hallucinates into silence: raw units **40 →
  215**, but 211 of them were the single token "eh", and the silence-drop then
  deleted 204, leaving an almost-empty caption (**~40 real words → ~4**). This is
  ADR 0014's failure in a new mechanism (over-suppression, not mis-separation).
- **Gentle denoise (`-a 12`) RECOVERS speech.** The attenuation limit mixes ~12 dB
  of the original back, so noise is reduced but gaps never go dead-silent.
  - #7: garbled "buntur dakenyang…" → coherent "**Guntur** udah kenyang. … Ada
    kunci gak? … udah bisa di-interact … Pelan-pelan coba pelan-pelan." It even
    recovered **"Guntur"** — the co-streamer's actual name (`@guntur69`).
  - #8: 10 → 25 units, fuller and more structured (one local token, "nyahu",
    persists — possibly a real name).
  - #9 (control): unchanged coherent FPS talk, and it *dropped* a hallucinated
    "see you next video" outro the mix had. **No degradation.**

## Decision

Adopt **Cleaned-voice captions** behind an **off-by-default `enh` cargo feature**:
before whisper on the **caption path only** (`caption_samples`), extract the clip
range as 48 kHz mono, run the `deep-filter` sidecar at **`-a 12 -D`** (gentle
attenuation limit + delay compensation for caption timing), resample to whisper's
16 kHz, and transcribe that. The attenuation limit is a tunable const
(`ENH_ATTEN_LIM_DB`, like the caption-timing constants).

- **Caption input only.** The rendered clip's audible audio is always the mix; only
  what whisper *reads for the transcript* changes. Detection / Arousal keep reading
  the mix (feeding them the cleaned voice — ADR 0008's reserved use — is a separate,
  separately-A/B'd slice).
- **Precedence over `sep`.** `enh` is the validated caption fix; the rejected
  `sep` (htdemucs) stays off, kept only for a possible future arousal-discovery use.
- **A sidecar, gated on the binary's presence** (like the `sep` model): absent
  binary → silently captions the mix, no failure.

## Considered options

- **Full-strength denoise.** Rejected by measurement (destroys masked speech).
- **In-process runtime (tract `deep_filter` crate, or `ort` + the 3 ONNX models).**
  Deferred. The sidecar is the reference implementation (correct by construction),
  pulls no new Rust runtime, and matches the ffmpeg/yt-dlp precedent. Revisit only
  if process-spawn overhead per clip ever matters; the in-process port is the
  fallback, not the default.
- **On by default.** Rejected. Validated on one VOD / one language; ADR 0014's
  burn argues for opt-in until proven broadly. Off-by-default, evidence-first.
- **Also clean the detection/Arousal audio now.** Deferred — it changes Moment
  ranking, which the operator would then have to re-trust. Separate slice.
- **Skip audio, fix decoding instead** (whisper `no_speech`/repetition guards,
  or an LLM correction pass). Not mutually exclusive — the repetition/silence
  degeneracy (the "eh"/"Hai"/"bermanfaat" loops) is a real *parallel* lever worth
  its own slice; this ADR fixes the *masking* root cause.

## Consequences

- New pinned sidecar `sidecars/deep-filter.exe` (27 MB, model baked in, MIT/Apache).
  No new Rust dependency; `enh` is a pure code-gate feature.
- Per-clip caption cost gains an ffmpeg extract + a denoise pass (seconds); only
  when built `--features enh` and the binary is present.
- `caption_samples` gains an `enh` branch (precedence over `sep`); two new ffmpeg
  helpers + `run_deep_filter`; `ffmpeg_resample_16k_mono` is now shared by both.
- `ENH_ATTEN_LIM_DB` (12) is tune-from-use; other content/languages may want a
  different limit, and that is the first knob to turn if captions regress.

## Outcome

**Spike-validated, integrated, and confirmed end-to-end (2026-06-30).** `-a 12`
measured to recover game-masked Indonesian speech (operator-confirmed) while
leaving the clean control intact; full denoise measured to destroy it. Wired into
the caption path behind `enh`. A real `--headless` render of #7 with
`--features face,ser,enh` showed a new **`stage: Cleaning voice`** firing (the app
spawns `deep-filter` itself — no shell gate), then a coherent burned caption
("…ADA KUNCI… KANAN-KANAN… INTERAKSI SEMUA COWOK… SUARA SENDIRI… YANG HORROR ITU
BUTUH KUNCI… PELAN-PELAN COBA PELAN-PELAN") with **zero English hallucination** —
the mix's "duluaries onions… comic book… ministerspoin" is gone. Note: production
denoises the freshly-fetched **Segment** audio (48 kHz), not the clip-mix the spike
used, so exact words differ slightly (here "buntur", not the spike's "Guntur") —
both coherent. `ENH_ATTEN_LIM_DB` is the first knob if other content regresses.

## Correction (2026-06-30, operator A/B on the real renders)

The operator watched the actual rendered clips and the finding **overturns the win
claim for #7**: the *mix* render got the opening right ("guntur dah kenyang"); the
*enh* render gave "hai buntur sakitnya", "ah"→"hati hati" — **worse**. Root cause:
the spike validated `-a 12` on the **clip's re-encoded (AAC) audio**, but production
denoises the **Segment** audio, which is already cleaner — so denoising it
*over-processes*. **enh is therefore NOT validated on the production audio path** and
is not a confirmed win. It stays **off by default**; it may help genuinely noisy
clips (e.g. #8, never rendered end-to-end) but needs re-validation on the Segment
path, and may be re-tuned or dropped. **Lesson: a spike must use the same audio
source as production.** The remaining #7 errors are linguistic (mishear / slang /
spurious word), addressed by the LLM correction pass (ADR 0030), not denoising.

## Tuning probe (2026-06-30, later): attenuation strength + the "diam dulu" miss

Chasing a whisper **miss** the operator flagged on clip-7 ("diam dulu diam dulu ah",
never transcribed) led to probing `ENH_ATTEN_LIM_DB`. Made it runtime-overridable via
**`YC_ENH_ATTEN`** (default still `12`), then swept it. Two findings, both confirming
the source-sensitivity lesson:

- **Strength is backwards from intuition, and source-dependent.** On the 16 kHz
  `analysis.wav` region, `-a6` (gentler) *recovered* "diam" + the name "Guntur" and
  killed the eh-loop, while `-a12`/`-a24`/`-a40` drove whisper into a long "eh"/"hey"
  repetition loop. But on the production **Segment** audio, `-a6` did **not** recover
  "diam" (it produced "jantung aja"), and it *scrambled* other words (`cowok→jok`,
  `tidur→dur`) so the dialect overrides stopped matching — net worse for the slang.
- **"diam dulu" is not reliably recoverable from the mixed VOD audio.** It is masked
  game-over-voice; denoising trades it for different garbles. The real fix is the
  streamer's **separate mic track**, which yt-clipper doesn't have (it works from the
  mixed VOD). The repeated "eh"s there are largely *real reactions* the operator wants
  kept, so removing them (as a strong denoise does) is also undesirable.

Conclusion unchanged: **enh stays off**, now with a tuning knob for future probes; the
caption wins come from the dialect dict + the (curated-only) LLM correction (ADR 0030),
not denoising. `YC_ENH_ATTEN` lets the operator re-probe per-clip without a rebuild.

## Re-validation on a 2nd clip's production path (2026-07-01)

The review-queue work surfaced a fresh bad-caption clip — the guntur69 "Diskusi game
biasa" moment (1881.5–1911.5 s): fast masked slang with a ~3 s stretch (23–26 s) whisper
misses entirely. A `caption_diag` spike on the 16 kHz `analysis.wav` at `-a6` *again*
looked like a clean win (recovered the 23–26 s "Mana… Anjing pusing…" and fixed a
mistimed "pusing"→"bangke"). **But a real `--features enh,face` render on the production
Segment path (`YC_ENH_ATTEN=6`) confirmed the trade-off, not a win** — the same
analysis.wav-vs-Segment divergence that overturned the #7 claim:

- **Recovered:** the 23–26 s hole ("MANA DAH INI ANJING GUSING…"), "KAMU MAIN DULU",
  "BANGKE" at 20 s (was a mistimed "pusing"), "BANGKE" as the 2nd repeat at 29 s.
- **Scrambled (the cost):** `dijekat`→**`DIJEGAN`**, which **broke the curated
  `dijekat→dicegat` correction** (the mixed render captions it right); `pancingan`→
  `BACINGAN`; a spurious `TORBIANTE`; + 4 new enh-only harvested garbles.

So on the production path enh **trades missed speech for different garbles and can break
curated corrections** — the ADR 0029 conclusion, now with a 2nd clip. The mixed render +
the dialect corrections is the *cleaner* result here; the 23–26 s miss is an audio limit
(the streamer's separate mic track is the only clean fix). **enh stays off.** Lesson
re-confirmed — the analysis.wav spike over-promised again, so benchmark enh on the
**Segment/render path**, never the analysis.wav. Both renders kept for A/B in the stream
folder: `clip-31-22.mp4` (mix + curation) vs `clip-31-22 (2).mp4` (enh).
