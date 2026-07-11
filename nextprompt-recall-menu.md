# Session prompt — Recall arc closed; the operator picks the next lane

> **UPDATE 2026-07-12: the ADR 0049 #2 dwell lane below was TAKEN** under the
> operator's "do this automatically" (ADR 0057 — cramped huge-word runs group
> into compact lines; 62%→1% sub-0.40 on clip 3). Its burn gate is open:
> **read `nextprompt-toofast-verdict.md` first.** The other items stay valid.

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read the
context below, then run /grill-with-docs BEFORE any code — the grill's first
question is which menu item the operator wants.

## Where you are (read this first)

The caption recall arc (ADR 0052 -> 0056) is **closed on the operator's eye**
(2026-07-11 evening): the whisper-witness recall admission ships (ensemble
step 2b, three guards, zero-added on turn-taking controls), the 51-56 s hole
on clip 3 is filled, the garble-float is dead, and the eye-approved read is
pinned — JALANANNYA at 54.40 via `jalanannya -> jalanannya @3646.4`
(clip_only) in the per-clip store beside the gue/gemes pins;
`clip_alignburn.ass` matches; approved burn:
`_recall-hole-fill clip3 (ADR 0056 - AB2 jalanannya 54.4).mp4`.

Iteration lesson recorded in ADR 0056's addendum: when the operator calls a
pinned word "a touch early/late", the lever is the PIN VALUE vs the 0.75 s
onset-snap radius — move the pin past the offending onset's radius and the
ear's value applies verbatim (never widen/narrow the radius in code for one
cue).

## First: the pick (grill to order, then ONE implementation)

- **Connective words** (`lucu aku banget mereka` still dropped by design —
  distinctive-only rule): the measured-but-unshipped 2-witness RUN admission
  (whisper + raw V4 agree on the whole phrase; both unpadded). Was a recorded
  ADR 0056 NON-GOAL awaiting the eye's ask. Needs its own pre-registration +
  the ADR 0052 controls (zero words on turn-taking).
- **dedikornya -> "deddy corp"** (the SDC brand spelling in the vote):
  curation lane, operator's spelling call, per-clip store entry (maybe
  Creator-level if they want it everywhere).
- **3p overlap clip admissions** (maksudnya/bingung/parah measured on
  Tretan/Coki 1559.6-1629.7): re-render + their eye, if they want it.
- **Whisper-engine recall parity** (ADR 0052 scoped it out): whisper users
  still have the transcript override; a real parity mechanism needs its own
  grill (no vote to admit into on a single decoder).
- **ADR 0049 roadmap #2** — the too-fast dwell lane (cues < 0.40 s), untouched
  since 0049; latest dwell numbers in the align_diag prints (ALIGN 64%
  sub-0.40 on clip 3).
- Or a NEW residual their eye names — measure first (caption_recall_diag /
  caption_align_diag; kit scripts in C:\Users\Nebu\AppData\Local\Temp\
  yc_recall_hole\, may be cleaned — the recipe is in
  handoffs/2026-07-11-recall-eye-ab-kit.md).

## Hard rules (unchanged)

- Measure before building; gate on the operator's eye on the burn.
- Zero words added on turn-taking controls for any admission change.
- The vote stays text-only/time-blind at admission.
- Best accuracy over speed.
- GPU: idle-gate decode runs; never raise the watchdog budget; sweep orphan
  caption_*_diag.exe AND bash/sleep trees; background waits die after
  ~25-40 min in this harness — wakeup-polling + short foreground bursts
  (see memory: background-waits-get-killed-use-wakeup-polling).
- PowerShell: QUOTE cargo feature lists ("--features" "face,align").

## Ritual

/grill-with-docs first; finish with /handoff + whatwedone.md entry; commit as
Evoyn with the model trailer (-F file); `git push origin main` has standing
permission. PS 5.1 quirks per the standing memory notes.
