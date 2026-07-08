# Mis-onset re-anchor: automatic detection refuted by measurement — the fix is the operator's time-pin, lifted to the whisper engine (ADR 0049 fix #3)

ADR 0049 named **mis-onset** (a real word anchored ~1.5 s early, onto a nearby
reaction) as one of four overlap-specific caption failures, and queued a
**re-anchor** as fix #3: after a stage-attribution pass, re-place the early word
at its real onset. This ADR records the measure-first work on that slice, which
**refuted the automatic approach the roadmap assumed** and shipped the safe one:
the operator's time-pin, made to work on both caption engines. Same discipline as
ADR 0049's drop-window bar and the ADR 0050 reversal — a plausible plan killed by
measurement before it shipped, and the negative result kept as the record.

## Stage attribution (the un-run opening question, now measured)

`caption_diag` on clip 3 (whisper-only, raw DTW) vs the shipped **ensemble**
`clip.ass`: the times are **byte-identical at every mis-onset cue** (SIAPA 3.66,
SDC 20.78, GUE 25.84, FADIL 31.32, JALANANNYA 53.48). So the early placement is
**whisper's own DTW onset**, inherited verbatim by `fuse_onto_timing` (the voted
words matched whisper's, so they adopted its spans) and by the refine. The
ensemble is **not** the culprit, and the defect **reproduces on the default
Whisper engine**. The fix therefore belongs in the shared caption path, not
`fuse_onto_timing` (the roadmap's leading hypothesis, refuted).

## Why automatic re-anchoring is not safely possible (measured, clip 3)

`caption_reanchor_diag` joins the three signals a mask-driven detector would use —
whisper's DTW onsets, the ADR 0046 laughter mask over the same analysis.wav, and
the `rms_onsets` speech grid — and the discrimination collapses:

- **The mask is not on the wrong onsets.** 3 of the 5 mis-onset words sit on
  CLEAN audio at their wrong time (Siapa L=0.07, SDC L=0.01, Fadil L=0.07). A
  mask-keyed detector cannot see them.
- **The mask fires on the protected words.** The laughter run `0.00–2.25 s`
  covers the opening pile the operator ruled real speech (ADR 0050). Auto-detect
  on the mask would endanger exactly those.
- **The mask cannot pick the target either.** 4 of 5 real onsets are INSIDE
  laughter (targets 5.0/28/32/54 score 0.46/0.85/0.14/0.63) — the group laughs
  *through* the speech — so "snap to a clean onset" pushes *away* from the real
  word (gue's nearest clean onset is 4.0 s off).
- **The "long DTW span" smear signal is unreliable.** It catches 3 of 5 but
  false-positives on correct words whose energy starts late — `Ngeituin`@45.44 is
  correct yet its first RMS onset is 0.76 s late, identical to a smear — and
  misses Siapa/Gue.

The root is fundamental: **an early onset followed by a gap or a laugh is
acoustically identical whether the word is mis-placed or correctly placed before a
pause.** Only the operator's ear separates them (on this clip whisper transcribed
"siapa tau" twice and mis-aligned which copy is which). No instrument can, which
is the same lesson as ADR 0049's drop bar.

## Decision: the operator's time-pin, on both engines

The re-anchor is the operator's `at_s` **time-pin**: they supply the *where*, and
the code snaps the pinned word onto the nearest `rms_onsets` speech onset (±0.75 s
— NOT mask-filtered, because a real onset can be masked by co-occurring
laughter). The measurement's one positive result: every one of the 5 targets has
an RMS onset within **0.42 s** (5.06/22.76/28.12/32.06/53.58), so the existing
snap lands them on their real speech onset. Zero false-positive risk — only a
pinned word ever moves.

`apply_store_positional` (the pin pass) already did this **inside `ensemble::apply`
only**; a whisper render never ran it, so a store time-pin **silently no-op'd on
the default engine**. This ADR lifts the pass into `ensure_transcript` for the
whisper branch too (after harvest, whose `unit_index` addresses the pre-pin units;
before refine, which then gap-fills the moved word), guarded by `any(at_s)` so a
store with no time-pins skips even the onset envelope and the default render stays
byte-identical. `rms_onsets` is now `pub` so both engines snap to the same grid.

**Case-insensitive matching (whisper parity).** Whisper's units keep their case
(`SDC`, `Gue`); the ensemble's fused stream is normalized-lowercase. A pin's
`wrong` is lowercased by `normalize()`, so an all-caps acronym is edit-3 from it
and `similar_word` would miss it — the operator's `SDC` mis-onset would no-op on
whisper. The match now lowercases the unit text; the ensemble (already lowercase)
stays byte-identical.

## The gate (the operator's eye on a re-burn — ADR 0050's lesson)

- **Re-anchor lands (whisper production path):** SIAPA 3.66→5.06, SDC 20.78→22.78,
  GUE 25.84→28.12, FADIL 31.32→32.26, JALANANNYA 53.48→53.60 — each on its measured
  speech onset.
- **No correctly-placed word moves (on the burn):** the fast opening pile is
  untouched, and every correct duplicate stays put — the other GUE@29.86, both
  other FADILs (34.98, 44.58). The discrimination that protects them is that they
  were never pinned.
- **Faithful:** the BEFORE re-burn is **byte-identical to the shipped Short**
  (50,478,075 B via `segment_seek` SEEK 2.000 + `camera.fg`), so the AFTER is
  exactly production with the pins.
- **Operator ruling (2026-07-08): PASS on timing.** "Most of the mis-timing is
  fixed." The remaining eye-caught defects are other classes (below).
- **Suites green both ways:** 351 default + 30 face-app, 0 failures; 8 positional
  unit tests (incl. the clip-3 re-anchor and the caps-acronym match).

## Consequences

- The mis-onset defect is now **operator-curated** — the timing analog of the
  dialect store's text corrections, durable in the store, working on both engines.
  Automatic detection is recorded as infeasible (measure-first), not silently
  dropped.
- **Duplicated-common-word caveat.** A pin moves the occurrence NEAREST `at_s`, so
  a word said many times is ambiguous: `gue`@28 grabbed a *correct* `gue`@29.86
  (nearer 28 than the mis-onset `gue`@25.84), so it must be pinned toward its
  current slot (27.6). Distinctive words (names/acronyms) have no twin and pin at
  their heard time — they are the ones actually worth a durable time-pin; a common
  word's timing is a per-clip Studio edit at best.
- `caption_reanchor_diag` (`crates/app/examples/`, `face`-gated) is the in-tree
  instrument: the measure-first probe that refuted auto-detect, and the BEFORE/AFTER
  clip.ass emitter for the re-burn — the reusable eyeball path for caption-timing
  work, no GPU camera re-decode.
- **Recorded for the next slices (operator's publish-bar findings on the AFTER
  burn):** drops — "siapa tau" (~20 s, spoken before the real word "SDC" and
  swallowed by whisper's 20.78–23.28 SDC span), "otot kayaknya" (6.5 s),
  "kreatin-kreatin" (8.5 s), "pinguin" (52 s) — are the **recall lane** (ADR 0049
  fix #4, next session); "proten→protein" (10.5 s) is the operator's dialect
  curation; **"SDC" is a real word the podcaster said, not a mishear** (operator
  correction, 2026-07-08), so this slice's re-anchor correctly moved it to where it
  is spoken (20.78→22.78 ≈ 23 s); and "ya"@12 is the one **phantom** candidate
  (faint background "itu yang keluar mungkin gatau"?), its own question.
