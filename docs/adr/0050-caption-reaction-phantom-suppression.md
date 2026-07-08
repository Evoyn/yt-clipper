# Reaction-phantom suppression: drop the caption piles crammed onto shared laughter (ADR 0049 fix #1)

> **Status: REVERSED (2026-07-08), same day it shipped.** The premise was wrong.
> Watching the real BEFORE/AFTER burn of clip 3, the operator ruled the "phantom"
> opening — "YA SIAPA TAU MAU COBA (A)KU BAWA" — is **real speech, spoken fast and
> placed too early**, not laughter. "Siapa tau mau coba ku bawa" is a real phrase
> ("who knows, maybe I'll try to bring…"), and deleting it made the clip WORSE than
> shipped ("BEFORE is better"). So the whole failure the operator reported is
> **mis-onset** (real words on the wrong clock) + **mis-transcription** + **dropped
> words** — hardly any of it is phantom. Suppression deleting real fast speech is
> exactly the false-positive the hard rule forbids. Wiring + the pure functions +
> the `caption_suppress_gate` example + the CONTEXT.md "Reaction phantom" term were
> all removed (commit reverting this ADR); the `caption_overlap_diag` instrument and
> the `segment_seek` re-burn helper stay. The real fix is the **mis-onset re-anchor**
> (was ADR 0049 fix #3, now fix #1). **Lesson: a re-decode/instrument agreeing with a
> hand-labelled "phantom" is NOT the operator's eye on the burned clip — gate on the
> burn, not the label.** The record of the attempt is kept below.

ADR 0049 measured the shipped 4-person VIOR captions and named four
overlap-specific failure classes; **phantom** — words hallucinated onto a
shared-reaction stretch — was *believed* the cheapest, highest-confidence,
look-preserving fix. This ADR wired it into the production caption path and gated
it against the instrument — which passed, but the operator's eye on the real burn
did not (see the reversal banner above).

## Decision

Drop the cues of a **reaction pile**: a maximal run of consecutive too-fast cues
(dwell `< MIN_READ_S` = 0.40 s) that holds a CORE of `>= PILE_MIN` (3)
**consecutive masked** cues (onset `laugh >= REACTION_TAU` = 0.1). The whole fast
run drops — the masked core is a genuine laughter burst, and dropping the run
also absorbs the sub-readable **fade cues** on the burst's edges that dip just
below the mask (clip 3's "COBA AKU" after "YA SIAPA TAU MAU"); a held (readable)
cue breaks the run, so a real word after the laugh bounds the drop. The pure
decision is `yc_render::reaction_phantom_drops` / `suppress_reaction_phantoms`;
the render path (`ensure_transcript`, after the timing refine) recomputes the
laughter mask over the clip's **mixed** analysis.wav with the same
`yc_frame::reaction` tagger the camera lanes use, then applies it. Additive and
fail-soft, exactly like the reaction lane: a missing/broken tagger (or the
default non-`face` build) leaves captions untouched.

**Why the whole fast run, not just the masked cues (operator eyeball, 2026-07-08):**
a first cut dropped only the masked cues, which split the opening burst and left
its below-mask tail "COBA AKU" orphaned as a lone wrong word LEADING the clip —
the operator judged that worse than the buried pile. Absorbing the fast fade
edges fixes it. The below-mask cues right after a `>= 3` laughter core are the
same phantom; a genuinely-spoken word there is readable (held), which breaks the
run. This does NOT reach held mistranscriptions on quiet/no-laugh spans (clip 3's
"BAWA"@2.66, tagger laugh 0.004, and the early "SIAPA"@3.66) — those are the
mis-onset re-anchor's job (fix #3), not a laughter-mask fix.

## Why density × reaction, not attribution (the measure-first correction)

The ADR 0049 roadmap proposed gating on "a reaction-masked bin with **no clean
attributed speaker**." Measuring clip 3 refuted that guard: the opening "YA SIAPA
TAU MAU" pile is attributed `speaker 3, conf 1.0` — a laughing mouth moves, so
the mouth/voice lane attributes a speaker at full confidence right through the
laugh (the exact blindness the reaction lane exists to cover, ADR 0045). Gating
on "no attribution" would have *protected* the pile. What actually separates the
classes, measured:

- a **pile** is dense — a run of `>= 3` crammed flash cues on the mask — the
  hallucination the operator confirmed is laughter, not speech;
- a **real word mis-placed** onto laughter is a lone, gap-filled (held) cue, never
  a pile (every mis-onset word on clip 3 held `>= 0.58 s`; "gue buka" over the 29 s
  laugh is a run of only two) — spared here; its re-placement is the mis-onset
  re-anchor's job (fix #3);
- real overlapping **speech** sits BELOW the mask (the contested "masalah gue mau
  nyobain" run scored `laugh <= 0.05`) — excluded by the mask.

So the discriminator is DENSITY × REACTION; attribution plays no part. (Same shape
as ADR 0049's own drop-window bar, which measurement also proved wrong — a bar
moved by evidence, recorded, not quietly dropped.)

## The gate (re-run `caption_overlap_diag` on the suppressed clip.ass)

- **Phantom gone:** the full opening burst "YA SIAPA TAU MAU COBA AKU" drops
  (masked core + fade tail); phantom-pile auto-detection → none (was 1). 6 cues
  removed, 95/101 survive, no orphaned lead word (operator-confirmed on the burn).
- **No real word suppressed:** the 5 mis-onset words and every contested-speech and
  talk-over-laughter cue survive — only the pile went.
- **Controls clean:** ANTITESA (real laughter, mask peak 0.70) → 0 drops; Deddy 3p
  (78% too-fast contested speech) → 0 drops. The too-fast-run requirement
  structurally guarantees 0 drops on the 0–2%-sub-floor turn-taking clips.
- **No camera/analysis regression:** ANTITESA `camera_diag.fg` SHA-pin holds, both
  fixtures' audits clean, Deddy person-join + laughter + shared-reaction bars PASS;
  suites green both ways (356).

## Consequences

- Suppression runs inside `ensure_transcript` after the timing refine, so it is part
  of the **cached** transcript — the editor preview and every re-render show the same
  suppressed cues, on BOTH caption engines. It only DROPS (order-preserving), so
  refine's onset-clamped, gap-filled timing stands untouched.
- It is `face`/voice-feature-gated (the tagger is that `ort` session); the default
  whisper-only build is byte-identical to pre-0050. An operator-edited transcript is
  never suppressed (`ensure_transcript` skips the whole block for it).
- Suppression does **not re-place** the genuinely-spoken words a pile may have dragged
  early (e.g. "siapa tau" at ~5 s): the laugh is left clean and those words await the
  mis-onset re-anchor (fix #3). "Reaction phantom" is now a CONTEXT.md term.
- `caption_suppress_gate` (`crates/render/examples/`) produces a suppressed clip.ass
  from a shipped one + a per-bin CSV by running the *production* function, so the
  instrument can re-judge any fix without a GPU render.
