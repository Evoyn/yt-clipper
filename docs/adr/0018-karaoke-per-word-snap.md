# Karaoke captions: per-word snap (`\k`), not a left-to-right fill (`\kf`)

The karaoke-fill Caption Style genre (M7) animated each line with ASS `\kf`
karaoke timing — a smooth left-to-right **sweep** that fills each word from the
unsung colour to the sung colour over the word's duration. The operator, testing
real Shorts, read this as "a loading bar filling" and asked instead for each
**word to change colour as a whole the moment it is spoken** — when the streamer
says "Hei", the word "Hei" *snaps* to the accent colour.

## Decision

Switch the karaoke builder from `\kf<cs>` to `\k<cs>`. ASS `\k` is the **instant**
karaoke primitive: when the karaoke cursor reaches a syllable it switches that
syllable from SecondaryColour (`\2c`, unsung = base) to PrimaryColour (`\1c`,
sung = accent) **in one frame**, with no sweep, and it stays switched. The
cursor maths are unchanged — each word's `<cs>` is still the dwell from its onset
to the next word's onset (the last word over its own gap-filled span, ADR 0013) —
so the snap lands on the same DTW onset the sweep started from. The visual is the
only change: a per-word highlight, not a fill.

This is **cumulative**: words light up one-by-one and stay lit, so the line is
fully accent by its end and then holds briefly (`LINE_HOLD_S`).

## Considered options

- **Cumulative `\k` (chosen).** The operator's literal instruction (`\kf` →
  `\k`), the smallest change (one tag), and the canonical "karaoke" read. Robust:
  reuses the existing karaoke cursor, no new per-word animation tags, no extra
  drift surface. Satisfies "the word snaps to the accent colour when spoken."
- **Spotlight (only the current word accent, prior words revert to base).**
  Considered — a punchier "bouncing highlight" that some short-form captions use,
  keeping exactly one word emphasised. Rejected for this slice: ASS karaoke has no
  "un-highlight" primitive, so it needs per-word absolute-time `\t(t1,t2,\1c…)`
  colour animations (or layered Dialogues), which is materially more ASS to
  generate and a larger correctness/drift surface — disproportionate to a tweak
  the operator framed as a one-tag switch. Recorded here as the fast follow-up if,
  on seeing the cumulative snap, the operator prefers the moving spotlight (it
  would slot in as a variant of this genre, not a rewrite).
- **Rename the genre `KaraokeFill` → `KaraokeSnap`.** Rejected: the
  `CaptionGenre::KaraokeFill` variant serialises as `karaoke_fill` in
  `creators.json` (ADR 0016) and `project.json`; renaming the variant would break
  those persisted defaults for no functional gain. The enum, serde tag, builder
  name (`karaoke_fill_events`), and "Karaoke Fill" UI label stay; only the
  animation and its docs change.

## Consequences

- One-line change in `render/src/ass.rs::karaoke_fill_events` (`\kf` → `\k`) plus
  doc + test updates (the two `\kf` assertions become `\k` snap assertions; the
  timing-tracks-onsets test is unchanged in maths). No core/enum/persistence
  changes.
- CONTEXT.md's **Caption Style** term notes karaoke is now a per-word snap.
- Extends the M7 genre work; orthogonal to ADR 0013 (the onset timing the snap
  rides on is unchanged).

## Outcome

**Shipped + verified (2026-06-28).** One-tag change in `karaoke_fill_events`
(`\kf` → `\k`); docs + the two `\kf` assertions updated to `\k` snap. 22 render
tests green.

**Live (real Ino Gemink clip, `karaoke` genre, stacked):** a caption-band
filmstrip across the line "DAN LAGI, TIDAK ADA" shows the highlight advancing
**word by word** — "DAN" gold (rest white) → "DAN LAGI," gold (TIDAK ADA white) →
… — with **every word wholly gold or wholly white in every frame**. No word is
ever half-filled (which the old `\kf` sweep produced), and earlier words stay gold
(cumulative). Confirms the per-word snap at the spoken onset the operator asked
for. (Whether a word snaps slightly before its audio is the separate caption-lead
concern — item 3 / ADR 0013, untouched here.)
