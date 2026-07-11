# Session prompt — Recall lane eye-gate follow-through (ADR 0056 landed; the operator's eye decides what's next)

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read the
context below, then run /grill-with-docs BEFORE any code — the grill's first
question is the operator's verdict on the burn.

## Where you are (read this first)

ADR 0056 shipped the recall admission (`ensemble::admit_recall`, apply step
2b): whisper-witness, count-deficit, three structural guards (common/short,
disagreement-zone, range-edge). The 51-56 s hole on clip 3 is filled
(JALANANNYA@52.76 + NGEFANS@55.98 admitted), the ADR 0054 garble-float is
dead (GEMOY 56.06 -> 51.48, renamed GEMES by a new clip-scoped `at_s` pin
beside the gue pin), turn-taking controls admit NOTHING, decode measured
byte-deterministic, 372 tests green.

**The one open gate: the operator's eye** on
`_recall-hole-fill clip3 (ADR 0056 - production).mp4` (VIOR stream folder).
Gate-on-the-burn is the standing rule — nothing in this lane is DONE until
their eye passes it.

## First: collect the verdict

1. Ask the operator for their eye on the burn. Named checks from ADR 0056:
   - the 48-58 s stretch: GEMOY(49.7) PINGUIN(50.6) GEMES(51.5)
     JALANANNYA(52.8) NGEFANS(56.0) — hole filled, float gone?
   - **JALANANNYA at 52.76 vs their by-ear ~54** — if it reads early, the
     fix is a one-line `at_s` pin (jalanannya -> jalanannya @3646.0,
     clip_only, the gue shape) + re-emit clip_alignburn.ass + re-burn.
   - GUE still at 28.1; no clean caption moved (114/117 cues byte-stable
     was the instrument bar; their eye confirms).
2. If they name NEW residuals, that's the slice — measure first
   (caption_recall_diag / caption_align_diag; the runner scripts are in
   C:\Users\Nebu\AppData\Local\Temp\yc_recall_hole\, may be cleaned).

## Then, whichever the eye picks (grill to order)

- **Connective words** (`lucu aku banget mereka` still dropped by design —
  distinctive-only rule): the measured-but-unshipped shape is a 2-witness
  RUN admission (whisper + raw V4 agree on the whole phrase; both unpadded).
  Needs its own pre-registration + the same controls (ADR 0052 discipline).
- **dedikornya -> "deddy corp"** (the SDC brand spelling in the vote):
  curation lane, operator's call, store entry.
- **3p overlap clip admissions** (maksudnya/bingung/parah measured on
  Tretan/Coki 1559.6-1629.7): their eye if that clip re-renders.
- **Whisper-engine recall parity** (ADR 0052 scoped it out): the admission
  is ensemble-only; whisper users still have the transcript override.
- ADR 0049 roadmap #2 (the too-fast dwell lane) remains untouched.

## Hard rules (unchanged)

- Measure before building; gate on the operator's eye on the burn.
- Zero words added on turn-taking controls for any admission change.
- The vote stays text-only/time-blind at admission.
- Best accuracy over speed.
- GPU: idle-gate decode runs (probe with one llama decode, <60 s = healthy);
  never raise the watchdog budget; sweep orphan caption_*_diag.exe too.
- PowerShell: QUOTE cargo feature lists ("--features" "face,align").

## Ritual

/grill-with-docs first; finish with /handoff + whatwedone.md entry; commit
as Evoyn with the model trailer (-F file); `git push origin main` has
standing permission. PS 5.1 quirks per the standing memory notes.
