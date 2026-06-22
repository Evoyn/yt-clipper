# Caption timing from whisper onsets + gap-fill, not the mixed-audio envelope

The operator's caption complaint — on `9-X80Ozwo1I` 34:10–34:40, talking over loud
background, "some captions don't appear, some are too fast / too slow" — was teed
up as the case *for* vocal separation (the presumed root cause: the streamer's mic
is masked in the mixed analysis audio). We diagnosed the real clip before building
(replaying `analysis.wav[2049.5..2079.5]` through the real `transcribe_range` +
`refine_caption_timing`, then a per-second energy profile), and the evidence
**overturned that framing**. The fix this session is **caption-timing robustness**;
vocal separation keeps the scope ADR 0008 always reserved for it (whole-VOD arousal
*discovery* + genuinely-masked clips) and becomes the next milestone, not the
caption fix. This supersedes the audio-driven caption timing introduced in
`a7cc606` / `efd75f5`.

## What the diagnosis showed (9-X80Ozwo1I 34:10–34:40)

- **whisper is not masked here.** All 30 raw units are coherent Indonesian *streamer*
  speech ("susah taik", "oke dapet satu dapet dua", "mampus kau", "bajingan bajingan");
  zero garbage / foreign / game-NPC tokens (Clash Royale has no NPC dialogue), and
  **0 non-monotonic DTW onsets**. The streamer's mic sits well above the game in this
  mix. So "missing captions" is not whisper skipping masked words.
- **The loud background is music/SFX, not speech** — the victory jingle (one second
  has RMS 0.63 with *no* word), battle SFX, the "HE HE HE HA" King emote (in the VOD
  title). The favourable case for a separator, but irrelevant to this complaint.
- **The "missing" gaps are real silence.** Between the captioned spans the whole mix
  drops to the noise floor (RMS 0.002–0.005) — the streamer paused. There are no
  masked words to recover. Only two words were dropped, both genuinely quiet
  ("apa sih", peaks ≈ 99 % of the old drop threshold) — i.e. the drop filter itself
  was *causing* a missing caption, not vocal masking.
- **The real cause of the wobble is timing re-derivation from the mixed envelope.**
  whisper's DTW gives precise word **onsets** but **zero-width ends** (13 of 30 units
  are single-point `dur 0.00`). `refine_caption_timing` exists to *synthesise* an
  on-screen duration — and it did so from the mixed RMS envelope, ending a word once
  energy fell a fixed fraction of the way from a per-clip baseline toward the word's
  **onset peak**. When that onset peak rides a loud transient (a SFX hit, a hard
  consonant), the threshold is set far above the streamer's sustained vocal level, so
  the word clears instantly (6 of 28 words pegged to the 0.10 s floor → "too fast").
  When the onset peak is modest, the threshold sits near baseline and ambient energy
  keeps the word "alive" → "too slow". The mixed envelope, not the words, drove the
  timing.

## Considered options

- **Vocal separation first (the teed-up plan; ADR 0008's reserved contingency).**
  Rejected *as the caption fix*: the diagnosis shows the streamer is not masked on
  the repro (whisper already has every word) and the missing captions are real
  silence — so a clean vocal stem would not make the headline complaint disappear. It
  would help "too slow" (cleaner silence) and sharpen "too fast", but at the cost of a
  new ingest-time stem, its own model, and GPU/CPU staging — disproportionate to a
  problem that lives in a pure timing function. Vocal separation remains genuinely
  valuable for what ADR 0008 reserved it for (whole-VOD arousal *discovery*, which is
  gated on it, and genuinely-masked clips on games *with* NPC speech), so it is
  **committed as the next milestone**, decoupled from captions.
- **Reading-time model, no envelope at all** (`end = start + f(word length)`, clamped
  to the next onset). Robust and fully deterministic, but drops the
  whisper-hallucination guard (spurious tokens on silent / pure-music windows would
  caption — the ADR 0007 music-window problem) and can blank the screen between words
  even with no real pause. Rejected as too lossy for v1.
- **Keep the envelope, retune it** (baseline-relative threshold, higher floor,
  near-floor drop). The smallest change, but still envelope-centric and therefore
  still mixed-audio-sensitive — it treats the symptom (aggressive thresholds) without
  removing the cause (timing derived from a mixed signal). Rejected.
- **Trust whisper's non-DTW heuristic `t0/t1` ends instead of synthesising.** Rejected:
  those ends drift ±50–150 ms (ADR 0003) and we'd have to give up the tight DTW
  onsets to get them (a token carries one or the other). The onset is the part worth
  trusting; the end is the part to synthesise.

## Decision

`refine_caption_timing` keeps the precise DTW **onset** as each word's start and
**gap-fills** the end to the next word's onset, capped and floored:

```
end = min(next_onset, start + MAX_HOLD)   // a word never lingers into a real pause
end = max(end, start + MIN_READ)          // never a sub-readable flash, where there is room
end = end.min(next_onset)                 // applied LAST — one word at a time, never an overlap
```

The RMS envelope is **demoted to a single job**: drop a word whose onset window is in
near-silence — `peak < SILENCE_DROP_FRAC · loud_ref`, where `loud_ref` is the p95 of
the clip envelope. Anchoring on the loud reference (not the baseline) keeps quiet-but-
present speech (the "apa sih" case) and is robust when a clip is mostly silence
(baseline ≈ 0). This preserves the `efd75f5` hallucination guard without the
mixed-audio threshold that was cutting words short.

`MIN_READ ≈ 0.40 s`, `MAX_HOLD ≈ 1.2 s`, `SILENCE_DROP_FRAC ≈ 0.10` are tune-from-use
constants (like the M6 editor's), not load-bearing.

## Consequences

- "Too fast" is fixed where there is room (a word fills the gap to the next onset, up
  to `MAX_HOLD`); genuinely fast speech (onsets < `MIN_READ` apart) still clips to the
  next onset, faithfully. "Too slow" is fixed by `MAX_HOLD`. The two quiet words the
  old filter dropped are now kept. The fix is **genre-agnostic** — it sets `end_s`,
  which both huge-word and rolling-pop consume (ADR 0004: timing lives in the ASS
  generator).
- A separate, real bug is fixed alongside: `huge_word_events` applied its
  `WORD_MIN_S` floor *after* clamping to the next onset, pushing a word's end up to
  0.10 s **past** the next word's start (a brief two-word overlap). The clamp to the
  next onset is now applied last.
- We give up the envelope's one genuine value-add — holding a drawn-out scream for its
  exact vocal length. A sustained reaction now holds for `MAX_HOLD` (or to the next
  word), which reads fine; exact vocal-length hold can return cheaply on a clean vocal
  stem if it ever proves missed.
- whisper's zero-width DTW ends are now a non-issue for captions: the end is
  synthesised, never read from the (often zero) raw end.
- A throwaway diagnostic (`crates/app/examples/caption_diag.rs`) replays the real
  caption path over any wav range and reports per-word survival / duration / limiter
  against the *real* `refine_caption_timing` — the feedback loop this decision was
  made against, kept for future caption-timing work.

## Outcome (2026-06-22)

Gap-fill timing implemented in `crates/render/src/ass.rs`; `yc-render` 17/17 unit
tests pass (4 new: room → `MIN_READ` not the flash floor, pause → `MAX_HOLD` cap,
quiet-but-present kept, no huge-word overlap; the existing silence-drop test still
passes against the loud-ref anchor).

Verified through the real path (`caption_diag`, whisper + the real
`refine_caption_timing`):

- **Repro `2049.5–2079.5`:** 30/30 units kept (was 28 — the quiet "apa sih" is
  recovered), **0** flashes-with-room (was 6 words pegged to the 0.10 s floor),
  longest on-screen 1.20 s (the `MAX_HOLD` cap; was lingering / overlapping), and
  the OKE/DAPET overlap is gone (the word ends exactly at the next onset).
- **No overfitting** across three other moments (a loud reaction `5211.5`, a talky
  `2702.5`, a quiet `1541.5`): each shows 0 flashes-with-room, longest on-screen
  1.20 s, 0 non-monotonic DTW onsets, and only 1–2 isolated near-silent tokens
  dropped (legitimate, far below the old over-drop rate).
- **Headless render** of the repro (`workspace/segment/export.mp4`, auto-framed
  Stacked at persistence 0.875, NVENC) burned the new timings cleanly, no panics.
