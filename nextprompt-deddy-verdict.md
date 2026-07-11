# Session prompt — the DEDDY CORP verdict — **WITHDRAWN, do not run**

> **RESOLVED 2026-07-12 (same day, operator rule): "dont touch any json to
> make correction for the captions... so we can tune the code."** The staged
> entry AND the three older pins (gue/gemes/jalanannya) were REMOVED from
> the per-clip store — it now holds only machine-harvested rows, so clip 3
> re-renders produce the RAW pipeline output (an unpolluted fixture for the
> general lanes). There is NO verdict to make; never paste any entry from
> this file back into a store. The burns (production / AB2 / deddy-corp)
> stay in the stream folder as reference videos only — their reads are no
> longer reproducible by re-render, by design. Backups if ever needed:
> `C:\Users\Nebu\AppData\Local\Temp\yc_deddy\store.allpins.bak.json`
> (sha 7C2A2779; pins also recorded verbatim in ADR 0055/0056 + handoffs).
> Active queue head: `nextprompt-caption-general.md`. Historical content
> below is kept for the record.

You are working in F:\yt-clipper. The 2026-07-12 menu-curation session staged
the `dedikornya → deddy corp` spelling (the SDC brand) on clip 3 as a per-clip
`at_s` pin. Everything is reversible; the operator's eye decides. Full state +
traps: `handoffs/2026-07-12-deddy-corp-curation.md`.

## Watch first (the A/B)

In the VIOR stream folder (`workspace/Deddy Corbuzier/MENUJU INDONESIA
CEMAS.../`):

- **A (approved, old spelling)**: `_recall-hole-fill clip3 (ADR 0056 - AB2
  jalanannya 54.4).mp4` — the eye-approved baseline; DEDIKORNYA at 24.61.
- **B (new)**: `_deddy-corp clip3 (store curation - dedikornya at_s
  25.10).mp4` — SUSU → DEDDY → CORP, one word at a time.

Named checks at ~24–27 s, in order:

1. **Text**: does DEDDY CORP read right as the brand? (Renders uppercase; the
   store value is lowercase `deddy corp`.)
2. **Placement**: DEDDY pops at 25.10 — ~0.5 s after the mouth starts the
   phrase. The acoustically-true onset (24.61) is UNREACHABLE by the pin (the
   only nearby speech onset belongs to SUSU; snapping there drags SUSU early
   — measured). Does 25.10 read acceptably?
3. **The hold**: CORP stays up 25.70–26.90, riding ~0.9 s over the laugh's
   start. That is the genre's normal 1.2 s hold (the old DEDIKORNYA held
   24.61–25.81 the same way); nothing POPS on the laugh. Does the eye mind?
4. Regression spot-checks (should be identical to A): GUE@28.14,
   GEMES@51.48, JALANANNYA@54.40, and everything outside 24–27 s.

## The branches (pick one; each is one short session)

### A. "Good" → approve
Nothing to run. The pin (`at_s` 3617.10, clip_only) stays in the per-clip
store; `data\clip_alignburn.ass` already matches it, so a re-render
reproduces the read. Strike the dedikornya item from
`nextprompt-recall-menu.md`; log the verdict in whatwedone.md.
**Optional follow-up question**: want `deddy corp` on ALL Deddy Corbuzier
clips? That is a per-Creator GLOBAL entry — but a global multiword right
changes the ALIGNER's input on every clip it fires on (measured on this clip:
it re-routed the laughter span). If yes, it gets its own small slice: add the
Creator-level entry AND emit-diff every clip it touches before burning.

### B. "Touch early / touch late" → move the pin by ear
The verbatim band is (25.08, 26.11]: any `at_s` in it lands EXACTLY at
(value − 3592.0) clip-seconds. Below ~25.08 the snap cliff grabs 24.28 and
drags SUSU (dead zone 24.61–25.08 — unreachable, recorded). Edit the pin's
`at_s` in `Aku fans Fadhil Vior dan Deddy Corvina.id.json`, then one emit +
one burn (kit scripts: `C:\Users\Nebu\AppData\Local\Temp\yc_deddy\
run_emit_only.ps1` + `burn_deddy.ps1`; recipes also in the handoff). Verify
the .ass diff is the one cue pair before burning.

### C. "Spell it differently" (e.g. SDC / deddy corbuzier)
Edit the entry's `right` (lowercase; render uppercases), same emit + burn.
Same geometry applies (a one-word right removes the CORP insert; the hold
then belongs to the single renamed cue).

### D. "No — put it back"
Restore section of the handoff: copy
`C:\Users\Nebu\AppData\Local\Temp\yc_deddy\store.prededdy.bak.json` over the
per-clip store (or delete just the dedikornya entry) and
`clip_alignburn.prededdy.bak.ass` over `data\clip_alignburn.ass` (verify sha
AD208751). Burns stay (prefer-keep). Re-annotate the menu item with the
negative and what the eye disliked.

## Hard rules (unchanged)

- Gate on the eye on the burn; prefer move/re-time over delete.
- GPU: probe-decode is the gate decider under playback flutter; never raise
  the watchdog budget; sweep orphans.
- PS 5.1: ASCII scripts, QUOTE "--features" "face,align", commit -F file.
- Ritual: /grill-with-docs on a fresh slice; /handoff + whatwedone.md on
  finish; end with the next `read nextprompt-<slug>.md and follow it.` line.
