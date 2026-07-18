# Session prompt — title-gen verdict + next arc (queued 2026-07-18)

The two-stage digest titles SHIPPED (ADR 0071, commits `838e626` +
`5c54600` + the outcome commit; handoff
`handoffs/2026-07-18-title-digest.md`). Five mechanical bars passed on the
ECA A/B; **bar 6 — the operator's eye — is the open gate.**

## First: collect the verdict (no code until this is answered)

Put the ADR 0071 before/after table in front of the operator and ask for a
ruling, per the gate-on-the-burn rule:

1. **Titles**: are the B-column titles right for the channel? (0 English,
   0 filler, curiosity-gap shapes; 2 bottom-quartile rows still
   topic-describe — acceptable residual or not?)
2. **Ranks**: the podcast-aware rubric moved ranks hard (old #1 → #6, old
   #11 → #1). Right direction, or does the rubric need a leash?
3. If refused: one `git revert` of `5c54600` restores the old prompt; the
   ADR + table stay as the record.

They may also want a real Promote so a title names an actual rendered file
(that is the true burn; `segment_seek` + the Studio flow — cheap now, the
segment is cached).

## Small residuals ready to ride along after a positive verdict (pick ≤1)

- **Digest language pin** (~30 min): the brief came out in English; add the
  same closing-reminder pattern that fixed titles to
  `build_digest_prompt`, pin it in a test. Internal-only, zero risk.
- The 2 dead titles + the "Ken" garble are transcript-thinness /
  caption-accuracy issues — NOT title-rule work; they belong to the caption
  lane (`nextprompt-caption-general.md` is the queued head for that, and
  caption work must generalize per the operator's standing rule).

## Then: grill the next arc (do not pick silently)

The operator's `feature-implementation-plan.md` items still open, best
first-guesses from the ADR trail (0064-0070 closed timeline/volume/
thumbnail/music/undo/fades; 0071 closed titles):

- **#2 Manual caption editing** (New Caption button; insert/drag/resize/
  delete alongside AI captions) — egui, timeline-heavy.
- **#12 Favorite font presets** (save/rename/delete caption style presets)
  — smaller egui slice, pairs with the Studio's existing 6 presets.
- **#10 Transition library** beyond the shipped fades (cross dissolve,
  blur, dip, flash, zoom) — render/filtergraph work.
- **#8 Auto Zoom with AI keyframes** (presets: Subtle/Podcast/Dramatic/
  Interview) — builds on the shipped keyframe transforms.

/grill-with-docs on whichever the operator picks (unless they again say
"do it automatically" — then pick the smallest coherent slice, pre-register
bars in an ADR first, one implementation, and validate on the production
path). Standing rules: quality over runtime; PS 5.1 quoting; `git commit
-F`; handoff + whatwedone + fresh nextprompt + the starter line at finish.
