# Natural Moment length: the adaptive window replaces the fixed 30 s duration

The operator noticed almost every detected Moment ran ~30 seconds and asked why
(focus 2026-07, task 5). The cause was one line: `DetectParams::dur_s = 30.0` —
every peak became a fixed-duration window (`peak_to_range`), so the length said
nothing about the moment. A 55-second hype arc was cut mid-reaction; a 12-second
jumpscare dragged 18 seconds of dead air behind it. This ADR records the fix:
Moments now size themselves to the signal, bounded by an operator-configurable
maximum up to the 180 s YouTube-Shorts ceiling.

## Considered options

- **Adaptive sustain window (chosen).** Each surviving peak walks outward over
  the combined (chat + loudness) series while it stays *elevated* —
  `combined >= max(0.45 × peak, 0.5σ)` — then pads: 1 s before an early
  build-up, 2 s after the tail, the ADR 0020 signal-aware lead staying the
  *minimum* pre-roll. The result floors at `min_dur_s` (15 s) and caps at
  `max_dur_s` (90 s default; slider up to 180 s; `YC_MAX_CLIP_S` headless).
  The cap trims the tail first, and the build-up may take at most ~35% of the
  budget, so the peak itself can never fall outside its own window. A sharp
  one-off spike degrades to exactly the old behaviour (lead + floor).
- **Rejected: keep a fixed duration, make it configurable.** A knob on the old
  bug — every clip would still be the *same* length, just a different one.
  The focus asks for the *natural* length per moment.
- **Rejected: speech-boundary snapping (whisper-based).** Ending clips on
  sentence boundaries would be nice, but discovery deliberately runs before
  any whisper pass (ADR 0007's cheap-signals phase); the sustain walk gets
  most of the benefit for none of the GPU cost. Revisit if trailing words get
  clipped in practice. *(They did — ADR 0040 adds the snap at the refine
  stage, which already holds each candidate's transcript, and the window is
  now finalized there, not at discover.)*

Overlap handling had to change with variable lengths: the fixed-gap NMS
(`dur_s`-spaced peaks) became a light `min_dur_s`-spaced NMS plus explicit
**overlap suppression** — candidates keep strongest-first; a weaker window is
trimmed against every kept one, dropped when its peak sits inside a kept
window (a shoulder of the same moment) or when less than `min_dur_s` survives
the trim.

## Consequences

- Sustained moments now surface at 40–90 s; sharp ones stay ~15–20 s. The
  ranked list mixes lengths, and the rail shows each Moment's duration so the
  operator can see the spread.
- `DetectParams` loses `dur_s` for `min_dur_s`/`max_dur_s`; `rank_moments`
  output ranges are no longer uniform. Anything assuming 30 s windows
  (nothing in-tree did, beyond the arousal-scan example) must read the range.
- The GUI's "Max clip length" slider (30–180 s) feeds `Job::Detect`;
  detection re-runs are needed for a new cap to take effect (the window is
  chosen at discover time, by design — ADR 0002's retunable-signals contract
  is about *ranking*, not window geometry).
