# Sentence-boundary clip bounds: detected Moments start and end on complete sentences

ADR 0037's revisit clause fired: the operator filed exactly the "trailing words
get clipped" defect it predicted — a detected Moment would cut off mid-sentence
because the adaptive window follows the *signal*, which knows nothing about
speech. This ADR records the fix: at the refine stage (which already transcribes
every candidate — ADR 0007), each detected Moment's bounds snap to sentence
boundaries, the minimum duration rises 15 s → 45 s and is reached through real
adjacent speech, and 180 s stays the hard ceiling. Manually-marked Moments are
exempt: verbatim range, no snapping, no floor (the Transcript-override
philosophy, ADR 0039 — operator intent is never "cleaned up").

## Considered options

- **Sentence snap at refine, growth through adjacent sentences (chosen).**
  Refine transcribes each candidate over a padded window (±15 s,
  `SNAP_PAD_S`) so snapping outward has material. Sentences come from whisper
  punctuation (`. ? ! …` + ja `。？！`, closers stripped; a ja terminal may sit
  mid-chunk), falling back to inter-word pauses (> 1.25 s) when punctuation is
  absent. A mid-sentence edge snaps *outward* to that sentence's start/end; a
  trailing edge in dead air retreats to the last complete sentence (the signal
  walk's tail pad is not content); a *leading* edge in dead air keeps the
  ADR 0020 pre-roll — a start in silence clips no words, and the event before
  the reaction is deliberate. A clip under 45 s pulls in adjacent sentences
  (smaller silence first, ties forward, never bridging > 4 s of dead air and
  never pulling the window's unclosed trailing run); when speech runs dry the
  shorter clip ships — the operator's call: context over duration, never
  dead-air padding. At the cap, the sentence that would cross it is dropped
  (end at the last complete sentence under the cap, floored so the cut never
  eats back into the signal window's own content); a single run-on spanning
  the cap region falls back to a breath pause within 10 s of the cap, then a
  word boundary — never mid-word. Pure and unit-tested over synthetic
  transcripts (`yc_detect::sentence`); the lexicon signal, LLM judge, and
  review pane all read the words inside the *final* bounds.
- **Rejected: snap at discover time.** Discovery deliberately runs before any
  whisper pass (ADR 0007's cheap-signals phase); snapping there would need a
  whole-VOD transcription — the exact multi-hour GPU cost ADR 0007 exists to
  avoid. Refine already holds each candidate's words.
- **Rejected: reach the 45 s floor in the signal geometry.** Keeping
  `adaptive_range`'s floor as the *delivery* mechanism would pad sharp spikes
  with dead air — the old "12 s jumpscare dragging 18 s of silence" defect in
  new clothes. The geometry's 45 s floor now only sizes the *seed* window
  (the transcribe/snap input); the sentence pass trims the dead tail and
  refills through real speech.
- **Rejected: 45 s as a hard minimum.** Padding to 45 s regardless (music,
  raids, an isolated aside) manufactures duration from silence. A shorter
  clip that is all real speech beats a 45 s clip that is one-third dead air.

## Consequences

- `DetectParams::min_dur_s` is 45 (was 15) and the GUI "Max clip length"
  slider floor rises 30 → 45 (`YC_MAX_CLIP_S` clamps the same way). NMS
  spacing and overlap-suppression survival deliberately inherit the 45 s
  value: at this duration scale, two peaks within one minimum-clip span cover
  the same content — the "one Moment per clip-length" semantics of the 15 s
  world, rescaled.
- The window is now *finalized at refine*, not discover — amending ADR 0037's
  "window is chosen at discover time" consequence. Re-detects are still needed
  for a new cap; refine costs ~30 s more audio per candidate (the pad), well
  inside ADR 0002's minutes-long-analysis tolerance.
- Accepted residual: two adjacent top candidates that both grow toward each
  other can, rarely, share an edge sentence (snap extensions are bounded by
  the pad, seeds are 45 s-spaced and non-overlapping). Logged at debug level;
  revisit with a sentence-aware post-pass if a real VOD ever shows it.
- Word timestamps at refine are heuristic (text-only load, no DTW) — a few
  hundred ms of drift on sentence edges, noise at clip scale. The rendered
  captions still use the caption-path timing (ADR 0013), not these.
- A speechless candidate (no usable sentences in the padded window) keeps its
  signal window unchanged — there are no word edges to protect, and a loud
  no-speech moment (a raid, a music drop) is still a valid clip.
