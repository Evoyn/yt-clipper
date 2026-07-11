# Session prompt — Recall lane: the eye's A/B verdict closes the ADR 0056 gate

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read the
context below, then run /grill-with-docs BEFORE any code — the grill's first
question is the operator's A/B pick.

## Where you are (read this first)

ADR 0056 shipped and its burn gate is still the one open gate — but it is now
an A/B, pre-staged 2026-07-11 evening (see the ADR's A/B addendum). Both burns
sit in the VIOR stream folder (`workspace\Deddy Corbuzier\MENUJU INDONESIA
CEMAS*`), identical except ONE cue:

- **A — production**: `_recall-hole-fill clip3 (ADR 0056 - production).mp4`
  JALANANNYA at **52.76** (the aligner's placement, `align_weighted`).
- **B — pinned**: `_recall-hole-fill clip3 (ADR 0056 - AB jalanannya at 54).mp4`
  JALANANNYA at **53.60** (the operator's ~54 by-ear pin, onset-snapped;
  whisper's own skeleton said 53.5).

The store is byte-exact shipped state — the pin is applied NOWHERE. Production
renders A until the eye says otherwise.

## First: collect the verdict (both burns, one sitting)

Named checks, 48–58 s stretch on each: GEMOY(49.7) PINGUIN(50.6) GEMES(51.5)
JALANANNYA(52.76 A / 53.60 B) NGEFANS(56.0) — hole filled, float gone, GUE
still at 28.1, no clean caption moved.

Then the pick:

1. **A reads right** (52.76 fine): gate CLOSES, nothing changes. Log it
   (whatwedone + ADR 0056 gets a one-line "eye passed on A"), then the menu.
2. **B reads right** (53.60): paste this block into the `corrections` array of
   `workspace\Deddy Corbuzier\MENUJU INDONESIA CEMAS*\Aku fans Fadhil Vior dan
   Deddy Corvina.id.json` (beside the gue/gemes pins), re-emit
   `clip_alignburn.ass` via the temp runner (or reuse
   `clip_alignburn.jalpin54.ass` from `C:\Users\Nebu\AppData\Local\Temp\
   yc_recall_hole\` if it survived), log gate CLOSED on B:

   ```json
   {
     "wrong": "jalanannya",
     "right": "jalanannya",
     "note": "JALANANNYA time pin (ADR 0056 A/B, eye-picked): operator's by-ear ~54 clip-relative = 3646.0 VOD; the aligner placed 52.76. Pure timing pin (wrong == right), clip-scoped. Delete if unwanted.",
     "status": "confirmed",
     "context": false,
     "clip_only": true,
     "at_s": 3646.0
   }
   ```

3. **Neither reads right**: their new by-ear time is the slice — change `at_s`
   to `3592.0 + <their seconds>`, re-run the kit (scripts in the temp folder:
   `chainA_jalpin_emit.sh` emit + `gate_burn_short.sh` burn; if temp was
   cleaned, the handoff `handoffs/2026-07-11-recall-eye-ab-kit.md` documents
   the full recipe).
4. New residuals named = the slice — measure first (caption_recall_diag /
   caption_align_diag).

## Then, whichever the eye picks next (grill to order, unchanged menu)

- **Connective words** (`lucu aku banget mereka`, dropped by design): the
  2-witness RUN admission (whisper + raw V4 agree on the whole phrase, both
  unpadded) is measured-but-unshipped, a recorded ADR 0056 NON-GOAL until the
  eye asks for it. Own pre-registration + the ADR 0052 controls.
- **dedikornya -> "deddy corp"** (SDC brand): curation lane, operator's call.
- **3p overlap clip admissions** (maksudnya/bingung/parah): their eye if that
  clip re-renders.
- **Whisper-engine recall parity** (scoped out in ADR 0052): transcript
  override remains the whisper-user lever.
- ADR 0049 roadmap #2 (too-fast dwell lane) remains untouched.

## Hard rules (unchanged)

- Measure before building; gate on the operator's eye on the burn.
- Zero words added on turn-taking controls for any admission change.
- The vote stays text-only/time-blind at admission.
- Best accuracy over speed.
- GPU: idle-gate decode runs; never raise the watchdog budget; sweep orphan
  caption_*_diag.exe AND bash/sleep trees (background waits get killed after
  ~25-40 min in this harness — use wakeup-polling + short foreground bursts,
  see memory).
- PowerShell: QUOTE cargo feature lists ("--features" "face,align").

## Ritual

/grill-with-docs first; finish with /handoff + whatwedone.md entry; commit as
Evoyn with the model trailer (-F file); `git push origin main` has standing
permission. PS 5.1 quirks per the standing memory notes.
