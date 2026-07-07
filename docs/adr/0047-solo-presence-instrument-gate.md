# 0047 — Solo presence: per-solo-shot verification instrument, pre-declared bars, gate

Date: 2026-07-07
Status: GATE PASSED (attribution regime; follow-visible A-class documented partial) — instrument built, both fixtures clean, all three VIOR clips adjudicated. Grammar/audit wiring is the next slice.

## Context

The 2026-07-07 ship-shakedown (first fresh-material production run, VOD
o1SBOz5UK2Q; ledger `handoffs/2026-07-07-ship-shakedown-ledger.md`)
converged on one worst defect: **a solo Shot never verifies that what it
frames is the attributed person's real, currently-visible face.** Two
mechanisms, both confirmed in code this session:

- `plan_shots` step 5 (speaker.rs): a piece whose subject has no
  measurements reuses the previous piece's framing → a stale crop crosses
  a source cut into an angle it was never aimed at.
- `audit_camera_plan` skips any shot span with no measurements
  (`prev = None; continue`) — no measurement, no finding — and its adrift
  check trusts the track's path, so a false measurement passes.

The grill (2026-07-07, this session) pinned the slice BEFORE any code, per
the ADR 0045 pattern: instrument → pre-declared bars → gate → (grammar
next slice).

## Decisions (grilled, operator-ratified 2026-07-07)

1. **Gate scope**: all shakedown hits are must-flag for the instrument,
   including the measured-but-out-of-crop class (F5); the fallback
   grammar this arc ships targets the presence classes (A: unmeasured,
   B: false measurement), and whether an in-crop failure also triggers
   fallback is decided at the gate.
2. **Instrument** (speaker_diag `== solo presence` table, additive only):
   per solo shot — measured-bin share + largest unmeasured run; in-crop
   share (the audit's own safe-region insets, pan-aware); full-res seek
   scans at span quantiles + the max-gap midpoint (YuNet over the planned
   crop = *is a real face visible in what renders*; YuNet+SFace on the
   track's own measurement vs the occupant-map person centroids = *is
   what the track matched a real face, and whose*); the attributed seat's
   occupant; the fallback ladder's structural pick.
3. **Gate bars, declared before the table existed**: a shot flags when
   any lane fails its bar; combined bars allowed. Must-flag: every
   ship-truth-confirmed publisher-visible hit. Hard zero flags across
   both fixture gate plans. Every extra flag on the VIOR clips is
   eye-adjudicated against the shipped renders; a confirmed false
   positive that no single bar placement can exclude while keeping all
   must-flags fails the gate.
4. **Fallback ladder** (grammar shape, wired only after the gate): a
   failing solo piece becomes (1) the split screen of the tracks measured
   ≥40% in the piece; else (2) hold the prior verified crop when no
   source cut was crossed; else (3) an honest wide of the angle. No
   framing anchor is written — the off-screen/reaction split precedent,
   third honest fuel into the same grammar.
5. **Production shape** (post-gate): draft plan → per-solo-shot
   verification pass (seeks, occupant-map machinery) → fallback rewrite →
   audit stays pure and consumes the verification artifact (new finding
   classes: unverified solo window; framed measurement fails face
   quality). Cost target: the instrument measured its own seek load —
   ~40 seeks / 7-12 s per 60-70 s clip — roughly doubling the occupant
   map's seek budget when wired.
6. **Term**: **Solo presence** (CONTEXT.md), the verified fact a solo
   Shot owes before it ships.

## What the instrument measured (evidence of record)

Fixtures (must-not-flag, all standing bars stayed green; ANTITESA fg
byte-pin `9e07d81f…` intact):

- Deddy person-join fixture: 14 solo shots — meas ≥ 72%, max unmeasured
  run ≤ 1.5 s, in-crop ≥ 96%, subject-scale crop-faces (h179–h284) at
  every sampled time on every shot. The 1.5 s gap (#3 @ 16.5–18.0, the
  laughing dip) has NO subject-scale face at its midpoint sample — the
  shipped render is fine (body centered, face down) — so *gap length
  alone cannot be a bar*; the gap is interior (bracketed by in-crop
  measurements both sides) and the shot is 80% measured.
- ANTITESA (follow-visible): 15 solo shots — meas ≥ 89%, in-crop 100%,
  crop-faces h185–h290 everywhere. A 0.1 s lead-in sliver (#0) carries a
  weak h78 det; it is 100% measured, so the presence bar never looks at
  it.
- Identity cosines are NOT bar material: clean fixture shots degrade to
  0.28 (face turning, Deddy #12), and singleton (`occ ?`) segments make
  expected-occupant checks unavailable on clean shots (#11/#12). The
  laugh build-up (clip 3 #16) hits 0.20 on a clean shot. Diagnostics
  only.

Clip 3 (resident segment = the app's own bytes; twin plan == shipped
render at every probed frame):

- **Render-confirmed real defects** (frame extracts, this session):
  - #8 13.4–14.1 (meas 0%): "friends" poster + the Wolverine figurine —
    the only YuNet det in the crop is the figurine's face, h25.
  - #13 23.6–24.2 (meas 0%): graffiti wall + shirt sliver — crop MISS.
    **New** (the shakedown eye never sampled this window).
  - #17 28.1–29.1 (meas 12%): subject's face sliced at frame-left, raised
    arm center — crop MISS at the sample.
  - #27 53.6–54.4 (meas 0%): poster + figurine again, h21. **New.**
- **Unmeasured but fine** (crop landed on a real face by continuity
  luck): #0, #2, #7, #12, #14, #21 — meas 0% with subject-scale
  crop-faces h185–h288 at every sample; render extracts clean.
- **Two ledger F9 hits dissolved by ship-truth**: the render at 13.09 is
  a well-framed laughing close-up (the actual blank is #8's window,
  13.4–14.1); the render + source at 22.19 are a clean wide two-shot
  with the subject exactly where the crop is aimed — no guitar in the
  angle (the "guitar body" screenshot was clip 1's F6 wall guitar; the
  prior session's screenshot→timestamp decode crossed clips). The
  burned-in source timecodes (`01:00:05:02` at clip 13.09, `01:00:14:04`
  at 22.19) pin the alignment beyond doubt.
- The B-class poster child was therefore a decode artifact; the true
  clip-3 defects are all "crop shows no subject-scale face". The
  occupant-map occupancy signal fires on #17 (`E=?`) but is silent on
  clean-by-luck windows and was satisfied (`B=P0`) on the dissolved
  22.19 — per-shot seek verification, not map occupancy, is the lane
  that discriminates.

**Clip 2 (attribution regime, byte-identical cached segment 15,045,254 B;
the app fetch was F2-blocked all session, so the prior-session cached
segments were used — deterministic, size-matched).** Bar P flags #2
(5.4–9.9, meas 12%, all-MISS), #3 (9.9–11.9, meas 0%, all-MISS — the
ledger's ~10 s empty crop), #7 (30.8–32.1, meas 0%, all-MISS — the
ledger's ~31 s empty crop), and #11 (40.8–45.1, **meas 0%, all-MISS over
4.3 s** — a NEW, large empty-crop window: source at 42.9 s shows the
speaker centre-left while #11's crop (920,20) targets the right side, so
the crop parks on the "friends" poster + wall). The clean shots
(#0/#1/#5/#6/#10) all embed as their expected occupant (P2/P3/P4/P0 at
cos 0.60–0.91). #12 (45.1–46.7, meas 0%) does NOT flag — the crop holds a
real face (h151–161) at every sample: benign-by-luck, correctly excluded.
Zero false positives.
- The shipped clip-2 render was made in **Wide layout** (F7: no
  active-speaker cam panel), so its framing is NOT this plan — clip-2
  flags are adjudicated against the source frame + the plan's own crop
  rect, which is self-consistent and production-faithful (the plan_shots
  path is byte-deterministic on the segment).
- Borderline (not must-flags, left unflagged by the strict all-samples
  arm): #8 (32.1–34.8, 2/3 MISS then h254) and #9 (34.8–36.1, 1/3 MISS) —
  mostly-empty crops with one late face. A "majority-of-samples MISS"
  arm2 would additionally flag #8; the declared all-samples arm keeps the
  bar zero-false-positive. A wiring-slice knob, recorded not resolved.

**Clip 1 (FOLLOW-VISIBLE regime, byte-identical cached segment
25,800,062 B).** Two solo defects, and the regime split is the finding:
- **F5 (#2, 28.2–34.2, Person E laughing out of crop)** — the existing
  camera **audit** flags `30.8–32.5s: Person E rides outside the crop's
  safe region`. My in-crop lane reads 97% aggregate (the excursion is a
  sustained 1.7 s run the audit's windowed check catches but an aggregate
  share dilutes), so **F5 stays audit-caught**; Bar C's <90% aggregate
  targets a heavier containment class, not F5.
- **F6 (#1, 4.9–28.2, Person A, the guitar wall)** — meas 31%, and the
  crop-scan is **fooled by pareidolia**: YuNet fires a face h153 @(94,434)
  on the **guitar body** (render at 9.6 s confirms — guitar centre-frame,
  Person A's head sliced at the right edge, turned away). Person A's
  reference (track box h144) puts h153 above the 0.35 floor, so Bar P's
  face-arm is satisfied by set dressing. Follow-visible builds **no
  occupant map**, so there is no identity anchor to reject the guitar
  face — the exact B-class blob the attribution regime rejects via
  `exp P#`. F6 is caught only by a **follow-visible-only meas-share bar**
  (meas < 50%: flags #1 at 31% and #2 at 49%, both real; ANTITESA, the
  follow-visible fixture, has min meas 89% → clean). Its limitation: no
  follow-visible benign-low-meas control exists to bound its false-
  positive rate.

## The bars (placed on the measured tables)

Two bars, both declared before the seek columns were read:

- **Bar P (presence)** — a solo shot flags when BOTH arms hold:
  1. its **unmeasured share ≥ 50%** of the shot's bins, AND
  2. **no seek sample finds a subject-scale face in the planned crop** —
     i.e. no det with height ≥ 0.35 × the subject's reference face
     height, where the reference is the span-median measured face height
     when the shot has any measurements, else the track's whole-clip box
     height.
- **Bar C (containment)** — a solo shot flags when its **in-crop share
  < 90%** (the audit's adrift class, quantified per shot).

**Why both arms of Bar P, not either alone** (the load-bearing finding):
- The face-height arm alone false-positives on a *measured* shot with a
  benign momentary dip. Deddy fixture #3 (14.2–22.1, 80% measured, a
  laughing lean) has a **h20** crop-face at its 1.5 s gap midpoint
  (17.2 s) — head-down, no subject-scale face for that instant — yet the
  render is fine because the shot is anchored by strong measurements
  either side. The ≥50%-unmeasured arm excludes it (unmeasured 20%).
- The unmeasured-share arm alone false-positives on the six clip-3
  **benign-by-luck** windows (#0/#2/#7/#12/#14/#21): 100% unmeasured, but
  the persisted crop happens to hold a real subject-scale face
  (h185–288) at every sample. The face-height arm excludes them (a face
  *is* found).
- Only their conjunction flags the real defects and nothing else.

**Measured separations** (clip 3, subject-scale reference ≈ track box h
250–270):
- Flags (both arms): #8 crop h25 @(985,617) — Wolverine shelf figurine;
  #13 crop **MISS**; #17 crop **MISS**; #27 crop h21 @(984,618) —
  figurine. Defect crop-faces are **MISS or h21–25** (≤ 0.1 × ref).
- No flag: every benign window's crop-face is **h185–288** (0.7–1.1 ×
  ref) — an order of magnitude above the 0.35 floor, which sits mid-gap
  with margin both ways.
- Bar C on clip 3: in-crop 94–100% everywhere measured → no flag (clip
  3's defects are all presence, class A; Bar C's target is the clip-1
  F5/F6 containment class).

**Fixture gate (must-not-flag): measured PASS.** Neither fixture reaches
Bar P's first arm (max unmeasured share: Deddy 28% at #4, ANTITESA 11% at
#13), and Bar C's minimum in-crop is 96% (Deddy) / 100% (ANTITESA). Zero
flags under P and C on both fixtures' solo shots. All standing bars held:
Deddy laughter `BARS: PASS at tau 0.1`, `15 shots (1 differ)`; ANTITESA
`camera_diag.fg` SHA-256 `9e07d81f…` byte-identical; both audits clean.

## Verdict

**GATE PASSED for the attribution regime; follow-visible A-class is a
documented partial.** Every ship-truth must-flag is flagged, zero fixture
solo shots flag, and every extra VIOR flag adjudicated to a real
(shakedown-unnoticed) defect — no false positive survived.

| must-flag (ship-truth) | regime | caught by |
| --- | --- | --- |
| Clip 1 F5 (#2 30.8–32.5) | follow-visible | existing camera audit (adrift) |
| Clip 1 F6 (#1 guitar wall) | follow-visible | meas-share bar (meas 31%) — face-arm fooled by pareidolia, no map |
| Clip 2 #3 (9.9–11.9) | attribution | Bar P (meas 0%, all-MISS) |
| Clip 2 #7 (30.8–32.1) | attribution | Bar P (meas 0%, all-MISS) |
| Clip 3 #8 / #13 / #17 / #27 | attribution | Bar P (meas 0–12%, MISS or h21–25) |

Extra real flags the eye had missed: Clip 2 #2 (5.4–9.9) and #11
(40.8–45.1, 4.3 s) — both all-MISS empty crops, source-confirmed. Zero
false positives on any clip; both fixtures clean under P and C.

The finding that shapes the wiring slice: **the occupant map is the
robustness anchor.** In attribution regime the per-shot seek's identity
check (`exp P#`) cleanly separates a stale crop on set dressing from a
crop that luckily still holds the subject — the same B-class blob that
fools the audit. Follow-visible builds no map, so its A-class (F6) can
only be caught by the cruder meas-share bar, which is clean on the sole
follow-visible fixture but has no benign-low-meas control. The defect is
*easier* to verify where the map exists — the inverse of the naive
expectation, and a direct input to how the fallback should differ by
regime.

## Consequences

- The audit's blind spot is now measurable per shot, and the winning
  discriminator needs FRAMES (per-solo-shot seeks at analyze time) — the
  pure signals are refuted as sole bars: gap length by the fixture's
  benign 1.5 s dip, map occupancy by the dissolved 22.19, and the
  crop-face height arm by clip 1's guitar pareidolia (which only identity
  rescues).
- The fallback ladder's rung data (who is measured ≥40% in a failing
  piece, whether a source cut was crossed) is already printed per shot,
  so the wiring slice starts from measured ground — and it now knows the
  ladder must branch by regime (identity-gated in attribution, meas-share
  in follow-visible).
- The shakedown ledger's F9 carries a correction entry (two hits
  dissolved to decode artifacts, two new hits added, one reclassified to
  group-framing) — the publisher record stays honest.
- Bar arm2's all-samples-MISS choice is deliberately conservative (zero
  false positives); the borderline clip-2 #8 (mostly-MISS, one late face)
  is a candidate for a majority-MISS refinement in the wiring slice.
