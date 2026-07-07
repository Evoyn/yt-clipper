# Reaction-phantom suppression: drop the caption piles crammed onto shared laughter (ADR 0049 fix #1)

ADR 0049 measured the shipped 4-person VIOR captions and named four
overlap-specific failure classes; **phantom** — words hallucinated onto a
shared-reaction stretch — was the cheapest, highest-confidence, look-preserving
fix. This ADR wires it into the production caption path and gates it.

## Decision

Drop a caption cue when it belongs to a **reaction pile**: a run of `>= 3`
(`PILE_MIN`) consecutive cues that are each too short to read (dwell
`< MIN_READ_S` = 0.40 s) AND whose onset sits on the shared-reaction mask
(`laugh >= REACTION_TAU` = 0.1). The pure decision is
`yc_render::reaction_phantom_drops` / `suppress_reaction_phantoms`; the render
path (`ensure_transcript`, after the timing refine) recomputes the laughter mask
over the clip's **mixed** analysis.wav with the same `yc_frame::reaction` tagger
the camera lanes use, then applies it. Additive and fail-soft, exactly like the
reaction lane: a missing/broken tagger (or the default non-`face` build) leaves
captions untouched.

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

- **Phantom gone:** the "YA SIAPA TAU MAU" pile drops; phantom-pile auto-detection
  → none (was 1). Exactly 4 cues removed, 97/101 survive.
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
