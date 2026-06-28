# Caption silence-drop: an absolute floor, so quiet speech survives a loud clip

The caption silence-drop (ADR 0013) removes whisper's hallucinated tokens on
silent / pure-music windows by dropping any word whose onset-window RMS peak is
below `SILENCE_DROP_FRAC * loud_ref` — a bar **relative** to the clip's p95
loudness. The operator found quiet / whispered speech — especially quickly-spoken
**viewer names** — not getting captioned, which also threw the timing off (a
dropped word leaves a gap the previous word fills).

`caption_diag` confirmed the cause on a real clip: the word "Terus" (common
Indonesian, *then/continue*) was dropped with an onset peak of **0.0091**, just
under the relative bar **0.0126** (10% of the loud p95, 0.1262). True silence on
that clip sits at p25 ≈ 0.002. So on a clip with loud reactions the relative bar
rises above genuine quiet asides and drops them. whisper *did* transcribe the word
— it was dropped in refine, not missed in transcription — so a pre-whisper gain
boost would not have helped.

## Decision

Drop a word only when its onset-window peak is below **both** the relative bar and
a new **absolute floor** `SILENCE_DROP_ABS` — i.e. the effective threshold is
`min(SILENCE_DROP_FRAC * loud_ref, SILENCE_DROP_ABS)`. The floor (0.006) sits
between true silence/hallucination level (~0.002) and quiet speech (~0.009), so:

- on a **loud** clip the relative bar exceeds the floor, the floor caps it, and a
  quiet aside (a whispered name) survives;
- on a **uniformly quiet** clip the relative bar is the lower of the two and still
  governs, so the absolute floor never over-drops there (it adapts to mic gain via
  the relative bar exactly as before);
- a true-silence hallucination (peak below the floor) is still dropped.

This also addresses the operator's "viewer name skipped → timing wobble": the name
is kept, so the previous word no longer gap-fills across the hole.

## Considered options

- **Absolute floor combined via `min` (chosen).** Keeps the relative bar's
  gain-adaptiveness while putting a hard, physics-based floor under quiet speech on
  loud-dynamic clips. Calibrated to the measured drop (0.0091) with margin above
  silence (0.002).
- **Just lower `SILENCE_DROP_FRAC`.** Rejected as the sole fix: a lower relative
  bar still scales with p95, so a loud-enough clip re-creates the problem, and on a
  quiet clip it uniformly raises hallucination risk. (The `min` form leaves the
  frac in place for quiet clips, where it is the right control.)
- **Pre-whisper gain / AGC.** Rejected for this cause: `caption_diag` showed
  whisper already transcribes the quiet word — the loss is the refine drop, not
  whisper sensitivity. Normalising also scales p95 and the word peak together, so
  it does not change a *relative* drop at all. (A gain stage may still help a
  genuinely-faint whole clip; out of scope here.)
- **Lower the whole-clip no-speech guard `SILENT_CLIP_PEAK`.** Rejected: that guard
  (peak < 0.01 ⇒ skip transcription) exists to avoid whisper's DTW process-abort on
  a silent clip, and the operator's case (quiet aside amid loud moments) has a clip
  peak far above it, so it never triggers here. Lowering it would risk re-introducing
  the crash for no benefit.

## Consequences

- One `min` in `refine_caption_timing` + one new tunable `SILENCE_DROP_ABS` (0.006).
  No change to the relative-bar machinery or to transcription.
- `caption_diag` now prints `loud_ref`, the relative bar, the absolute floor, the
  effective threshold, and — per dropped word — its onset peak with a flag when the
  floor would keep it, so the floor is recalibratable against any clip.
- A unit test mirrors the real case: a quiet aside (peak 0.04) below the relative
  bar (~0.05) but above the floor is kept, while a truly-silent token is dropped.
- Extends ADR 0013 (the silence-drop) and underpins item 7b (a quietly-spoken name
  now captions, so its harvested correction is identifiable).

## Outcome

**Shipped + verified (2026-06-28).** One `min` in `refine_caption_timing` +
`SILENCE_DROP_ABS` (0.006); `caption_diag` extended to print the drop thresholds
and each dropped word's onset peak. 26 render tests green (+1: a quiet aside below
the relative bar but above the floor is kept, silence still dropped).

**Live (`caption_diag` on the real 34 s Ino clip — the exact whisper+refine path):**
before, the real word "Terus" was dropped (peak 0.0091 < relative bar 0.0126) →
**27 kept / 1 dropped**; after, the effective threshold is the 0.006 floor and
"Terus" is kept → **28 kept / 0 dropped**, slotting cleanly after "air" (no
gap-fill across the hole — the timing wobble the operator tied to skipped names is
gone). The floor (0.006) sits well above this clip's silence (p25 ≈ 0.002), so a
true-silence hallucination would still drop. Read-only diag run — nothing dirtied.
