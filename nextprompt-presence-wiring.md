# Session prompt — Solo-presence wiring (the gate PASSED; wire the grammar)

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session:
read the context below, then run /grill-with-docs BEFORE any code — the
grill's job here is to pin the fallback-ladder's ACCEPTANCE bars and the
follow-visible A-class decision BEFORE the wiring, because the instrument
already exists and its verdict (ADR 0047) constrains the shape.

## Why this exists

The solo-presence instrument (ADR 0047, 2026-07-07) MEASURED the
shakedown's worst defect and PASSED its gate: per solo shot, does the
plan verifiably frame the attributed person's real, currently-visible
face? The instrument lives in `speaker_diag` (the `== solo presence`
table + `solo_presence()`), diagnostics-only. This slice WIRES the
verified measurement into the production plan + audit — the ADR 0045→0046
pattern's final step (instrument+gate → wiring gate).

## What the gate settled (do not relitigate)

- **Bar P (presence)**: a solo shot is a presence failure when it is
  ≥50% unmeasured AND no per-shot seek finds a subject-scale face
  (height ≥ 0.35× the subject's reference face height) in the planned
  crop. Flagged every ship-truth defect, zero fixture flags, zero false
  positives.
- **The occupant map is the robustness anchor**: attribution regime gets
  a per-shot identity check (is the crop-face the expected occupant?)
  that cleanly rejects the guitar-body / pegboard blob class. Follow-
  visible has NO map — clip 1's F6 fooled the crop-face arm (YuNet fired
  on a guitar body) and was catchable only by the meas-share bar.
- **The fallback ladder** (grammar shape, operator-ratified in the ADR
  0047 grill): a failing solo piece becomes (1) the split of the tracks
  measured ≥40% in the piece; else (2) hold the prior verified crop when
  no source cut was crossed; else (3) an honest wide. No framing anchor
  written. The `fb` column already prints this structural pick per shot.

## The slice (wiring, gate-shaped)

1. **Move the verification into production** (`crates/frame` + the
   analyze-time seek path in `crates/app/src/pipeline.rs`): a per-solo-
   shot presence pass between the draft `plan_shots` output and a
   fallback rewrite. Reuse the occupant-map seek machinery
   (`build_occupant_map_via_seeks` already does full-res targeted seeks;
   the presence pass roughly doubles that budget — measure it). The pure
   decision (given per-shot presence + crop-face + identity → fallback
   rung) belongs in `yc_frame::speaker`, unit-tested; the seeks stay in
   the pipeline glue.
2. **Branch the fallback by regime** (the ADR 0047 finding): attribution
   uses identity-gated presence; follow-visible uses the meas-share bar
   (document that its false-positive rate is uncontrolled — the sole
   follow-visible fixture has no benign-low-meas shot). Decide in the
   grill whether follow-visible A-class ships a fallback THIS slice or
   waits for a temporal-stability signal.
3. **Extend the audit** to consume the verification artifact: new finding
   classes — an unverified/low-presence solo window (the class the audit
   was structurally BLIND to), and a framed measurement that fails face
   quality. The audit stays pure (reads the artifact, computes nothing
   new from frames).
4. **Pre-declare the wiring gate** with the operator: the fixtures'
   plans must not change (ANTITESA fg byte-pin, Deddy 15-shots-1-differ +
   all bars), the four gate-artifact renders in BGN B NYA are never
   re-rendered, and a NEW render on the VIOR clips must show the flagged
   windows healed (split/hold/wide instead of set dressing) WITHOUT
   regressing the clean shots. Name a fresh gate artifact (do not clobber
   `diar_reaction.mp4` et al).

## Read first

1. `docs/adr/0047-solo-presence-instrument-gate.md` — bars, the
   three-clip verdict table, the regime-split finding, the borderline
   knobs (majority-MISS arm2).
2. `handoffs/2026-07-07-solo-presence-gate.md` — session state + traps
   (segment-fetch F2, the cached deterministic segments, the Wide-layout
   clip-2 render caveat, the awk to read the table).
3. `crates/app/examples/speaker_diag.rs` `solo_presence()` — the measured
   instrument to port; the `fb` column is the ladder's structural pick.
4. `crates/frame/src/speaker.rs` `plan_shots` step 5 (the stale-crop
   reuse at the `None => match &prev` arm) + `audit_camera_plan` (the
   `prev = None; continue` blind spot) — the two sites the wiring changes.
5. verify SKILL.md — the fixture bars that stay green through ANY change.

## Hard rules

- Fixture bars stay green (Deddy person-join + laughter, ANTITESA fg
  byte-pin `9e07d81f…`, audits clean, suites 337 both ways).
- The four gate artifacts in BGN B NYA are never re-rendered; the VIOR
  renders + ledger are the shakedown record — never overwrite `Nego
  Nutrisi…`, `Diskusi…`, `Aku fans…`.
- AnalyzeSpeakers stays editor-only; the harness twin is speaker_diag.
- Production seek cost is real — measure the presence pass's added
  wall-time and gate on it (the ADR 0044/0046 discipline).

## Open threads (not this slice, keep visible)

- Follow-visible A-class needs identity or temporal crop-face stability
  (F6 / the guitar pareidolia) — the meas-share bar is a stopgap.
- Bar arm2's all-samples-MISS vs majority-MISS (clip-2 #8 borderline).
- Voice-join scrap fragmentation on rapid banter (Clip 2's 0.0 s lane —
  ADR 0044 texture generalizing).
- F2/F3: segment-fetch retry schedule spanning minutes + a Retry
  affordance + error surfacing on the Failed chip (measured evidence now
  across TWO sessions).
- F1: fold `ser` into the production build line? (operator decision.)
- Clip-2 Wide-layout framing wart (F8's 45 s left-panel crop) — a
  group/layout thread, distinct from solo presence.

## Ritual

/grill-with-docs first; /verify before committing; finish with /handoff
+ whatwedone.md entry; commit as Evoyn with the model trailer (-F file);
`git push origin main` has standing permission. PS 5.1 quirks per the
standing memory notes.
