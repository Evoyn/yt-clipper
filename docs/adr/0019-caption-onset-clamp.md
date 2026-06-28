# Caption onset clamp: never show a word before its audio

The operator reported captions intermittently **leading** the audio — a word
appears slightly before it is spoken. ADR 0013 fixed word *ends* (gap-fill from
whisper's zero-width DTW ends) but left each word's *start* at the raw DTW onset.
whisper's DTW onset is usually accurate, but it occasionally lands a little before
the word's acoustic onset, and `refine_caption_timing` passed that through to
every genre: huge-word shows the word at its onset, and rolling-pop / karaoke
start the whole line at its first word's onset — so a single early onset leads the
line. The lead is the **per-word DTW `start_s`**, not the line machinery.

## Decision

Add a bounded **acoustic-onset clamp** to `refine_caption_timing`, on the same
per-word loop that already computes the RMS envelope and the word's onset-window
peak for the silence-drop. For each kept word, scan the envelope forward from the
DTW onset to the first frame that rises past `ONSET_RISE_FRAC` (0.30) of the
word's **own** peak, and move the caption start there — bounded by
`ONSET_MAX_LEAD_S` (0.20 s) and never past the next word's onset. Audio already
present at the DTW onset leaves the start unmoved.

Properties:
- **Forward only.** A caption never *precedes* its sound; a correctly-timed word
  is never pulled earlier.
- **Bounded.** At most a 0.20 s correction, so a correct onset is never delayed
  past its own peak, and a pathological DTW error (or a peak window that caught a
  neighbour) can't shove the caption far late.
- **Per-word-peak-relative**, so it scales to quiet speech (the threshold tracks
  the word's own loudness, not a global level) — it composes with the quiet-speech
  work (item 5) rather than fighting it.
- **One fix, all genres.** Because it adjusts `start_s` upstream of the builders,
  huge-word, rolling-pop and karaoke all stop leading; rolling/karaoke `line_start`
  follows the clamped first-word onset automatically.

## Considered options

- **Per-word onset clamp (chosen).** Targets the actual cause (the DTW `start_s`),
  reuses the envelope/peak already computed, and is genre-agnostic.
- **Clamp only the line `Start` in the rolling/karaoke builders.** Rejected: it
  misses huge-word (also per-word) and would duplicate envelope analysis in the
  builders, which today take no audio. Fixing `start_s` once in refine covers all
  three.
- **Shift every onset back by a fixed latency.** Rejected: the lead is
  intermittent, not a constant offset; a blanket shift would delay the many
  correctly-timed words and could push some past their audio.
- **Unbounded clamp to the acoustic onset.** Rejected: a wrong DTW onset or a
  peak window that captured a later word could move a caption far too late; the
  operator asked for a *small* clamp. The 0.20 s cap keeps it conservative.

## Consequences

- `refine_caption_timing` now sets `start_s` as well as `end_s`. The gap-fill end
  is computed from the clamped start, so a clamped word's on-screen window starts
  when it is actually spoken. Constants `ONSET_RISE_FRAC` / `ONSET_MAX_LEAD_S` are
  tunable like the other caption knobs.
- `caption_diag` is updated to align refined→raw by walking the kept subsequence
  in order (its old start-keyed match would mis-report clamped words as dropped),
  and gains a per-word `lead` column + an "onset-clamped" summary — so the lead and
  its correction are measurable on a real clip.
- Three unit tests: clamp forward when the DTW onset leads; leave a word whose
  audio is already present; cap a large lead at `ONSET_MAX_LEAD_S`.
- Extends ADR 0013 (caption timing); orthogonal to the genre builders and ADR 0018.

## Outcome

**Shipped + verified (2026-06-28).** Clamp added to `refine_caption_timing`;
`caption_diag` aligned + given a `lead` column; 3 unit tests. 25 render tests
green (existing refine tests unmoved — they all have audio at the DTW onset).

**Live (`caption_diag` over a real 34 s Ino Gemink clip — the exact whisper+refine
render path):** the lead is real and intermittent — **11 of 27 kept words had
their DTW onset leading the audio**, by +0.02 to +0.20 s, **total 1.22 s
corrected** (max 0.20 s, the cap; "Raya"/"Pertama"/"Ambil" hit it, i.e. led by
≥0.20 s — a clearly visible lead). Each was clamped forward to its acoustic onset;
the 16 words with audio already present were left at +0.00. The silent "Terus" was
still dropped (silence-drop intact). A huge-word render confirmed captions still
burn correctly (no regression). The diag is the right live-verify here — a timing
shift of tens of ms is not eyeballable frame-by-frame, but the inspector replays
the real path and measures it directly.
