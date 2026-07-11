# Session prompt — ADR 0057 verdict: your eye on the too-fast regroup burn

One sitting decides the ADR 0057 gate. The A/B pair sits in the VIOR stream
folder (`workspace/Deddy Corbuzier/MENUJU INDONESIA CEMAS.../`):

- **A (the approved baseline)**:
  `_recall-hole-fill clip3 (ADR 0056 - AB2 jalanannya 54.4).mp4` — one word
  per cue; 62% of cues flash under 0.40 s (the ADR 0049 too-fast class).
- **B (new)**: `_toofast-regroup clip3 (ADR 0057 - grouped).mp4` — same
  words, same order, same audio; cramped runs render as compact 2–3 word
  lines at the line-genre size (96 px vs the huge 150). 1% sub-0.40, median
  dwell 0.32 → 0.65 s.

## Where to look

- **13.9–18.7 s** — the fast VIOR banter: was 12 consecutive flashing words,
  now a run of compact lines ("GUE MAU NYOBAIN", "BEGITU GUE COBAIN", ...).
- **The 48–58 s stretch you approved yesterday**: PINGUIN@50.60,
  GEMES@51.48, JALANANNYA@54.40, NGEFANS@55.98 are all UNTOUCHED (byte-exact
  cues); the two new compact lines nearby are "TUH GEMOY"@49.58 and
  "BANGET KAYAK"@50.02 (each second word appears ≤0.3 s before it is spoken,
  sharing its line).
- **Your pinned GUE@28.14** — unchanged, still its own huge cue.

## Verdict branches (pre-committed — pick one, the fix is one sitting)

1. **B reads better** → gate CLOSED; grouping is already the default
   huge-word behavior on every render, both engines. Log it, arc done.
2. **B is worse — one word at a time was better** → REVERSE (the ADR 0050
   pattern): drop the grouping walk in `preview_lines` (singletons are
   verbatim old code), banner on ADR 0057, re-burn to confirm.
3. **Better, but the compact size reads wrong** → `GROUP_FS_FRAC` in
   `crates/render/src/ass.rs` is the one-const lever (0.64 now = the line
   genres' 96 px at the default 150). Change, re-emit, re-burn.
4. **Better, but 3 words is too many** → `MAX_GROUP_WORDS` 3 → 2, same loop.
5. **A grouped word bothers your ear** (a line's later word is visible up to
   0.40 s before it is spoken) → the pre-committed iteration is the
   rolling-reveal variant: words pop at their own onsets INSIDE the grouped
   line. Its recorded cost: the line's last word gets only a sliver of solo
   visibility. Needs its own small slice + re-burn.

Note: the code is committed and LIVE for huge-word renders until reversed —
the burn gate decides whether it STAYS (gate-on-the-burn rule). For anything
beyond branch 1, /grill-with-docs first.

## If instead you want a different lane

The remaining menu (nextprompt-recall-menu.md): connective words (2-witness
RUN admission, needs its own pre-registration), dedikornya → "deddy corp"
spelling (curation, your call), 3p overlap clip admissions re-render,
whisper-engine recall parity (needs its own grill).

## Hard rules (unchanged)

- Gate on your eye on the burn; measure before building.
- GPU: idle-gate decode runs; never raise the watchdog budget; sweep orphan
  caption_*_diag.exe and bash/sleep trees; background waits die ~25–40 min —
  wakeup-polling + short foreground bursts.
- PowerShell: QUOTE cargo feature lists ("--features" "align"); ASCII-only
  .ps1; commit with -F file.
