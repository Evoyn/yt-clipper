# Session prompt — post-title-arc: residuals + next feature (queued 2026-07-18, verdict IN)

The title arc CLOSED with the operator's eye, same day, two verdicts deep
(ADR 0071 + its Outcome; handoff `handoffs/2026-07-18-title-digest.md`):

1. Iteration 1 (digest + curiosity-gap rules): "we need more catchy and
   hooking" — refused as too flat.
2. Iteration 2 (two-beat hook+payoff build): **"i like it now"** — the
   shouty two-beat energy is APPROVED, including whole-phrase ALL-CAPS
   (do NOT re-leash caps; the operator explicitly kept the style).
3. Shipped as v2.2 = iteration 2's energy + three honesty leashes (payoff
   from THIS clip's words, names letter-for-letter, single-beat fallback on
   thin transcripts). The final ECA table is in ADR 0071 for the record.

## Rules of this ground (hard-won today, don't relearn)

- The prompt pins in `crates/detect/src/llm.rs::tests` now encode BOTH
  verdicts — extend, never delete, and never re-add a caps leash.
- Prompt-only iterations rebuild ONLY the judge:
  `scripts\cargo-cuda.bat build --release -p yc-llm-judge` (no --features;
  the crate declares none). App rebuilds are needed only for IPC changes.
- ECA A/B mechanics: byte-backup project/review json first, run headless
  `--detect` DETACHED (`Start-Process cmd /c` wrapper; the 10-min tool cap
  kills backgrounded runs), watch via `tail -F` Monitor, restore
  SHA-verified after. Launch ONLY behind a sustained idle gate that also
  checks for the operator's own yt-clipper.exe — their session appeared
  mid-slice today and a conditional launch is the shape that works.

## Open residuals (small, pick as ride-alongs)

- **Digest brief emits in English** (internal-only): add the closing
  language-reminder line to `build_digest_prompt` + pin. ~30 min.
- Bottom-ranked thin-transcript titles stay tame ("Bicara tentang Sabun
  Ekologis") — by design (honest fallback); only revisit if the operator
  asks.
- Transcript garbles can surface in titles ("Ken" at 21m15s) — that is
  caption-accuracy work, `nextprompt-caption-general.md` is the queued
  head for it.

## The next arc: grill it

Remaining `feature-implementation-plan.md` items (ADRs 0064-0071 closed
timeline/volume/thumbnail/music/undo/fades/titles): **#2 Manual caption
editing**, **#12 Favorite font presets**, **#10 Transition library**,
**#8 Auto Zoom keyframes**. /grill-with-docs on the operator's pick —
unless they say "do it automatically" again: then pick the smallest
coherent slice, pre-register bars in an ADR first, one implementation,
validate on the production path. Standing rules: quality over runtime,
PS 5.1 quoting, `git commit -F`, handoff + whatwedone + fresh nextprompt
+ the starter line at finish.
