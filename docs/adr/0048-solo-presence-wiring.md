# 0048 — Solo presence: wire the verified measurement into the production plan + audit

Date: 2026-07-07
Status: GATE PASSED (operator-approved 2026-07-07) — supersedes ADR 0047's "wired only after its gate" clause.

## Context

ADR 0047 built the solo-presence instrument, pre-declared **Bar P**, and
PASSED its gate: per solo shot, does the plan verifiably frame the
attributed person's real, currently-visible face? The instrument lives in
`speaker_diag` (`solo_presence()`), diagnostics-only, and its verdict
constrains this slice's shape. This ADR wires the verified measurement
into the production plan (`plan_shots` → presence pass → fallback rewrite)
and the camera audit — the ADR 0045→0046 pattern's final step
(instrument+gate → wiring gate).

The two production sites the shakedown identified (ADR 0047 Context):
`plan_shots` step 5 reuses a stale crop for an unmeasured piece
(`None => match &prev`, speaker.rs:1050), and `audit_camera_plan` skips
any no-measurement span (`prev = None; continue`, speaker.rs:1792/1803) so
it never sees the defect.

## Decisions (pinned in the 2026-07-07 wiring grill, operator-delegated
"follow your recommendations to the best output possible")

1. **Scope: attribution regime only this slice.** ADR 0047's load-bearing
   finding is *the occupant map is the robustness anchor* — attribution's
   per-shot identity check cleanly rejects the guitar-body/pegboard blob
   class, while follow-visible builds no map and its A-class (F6, the
   guitar wall) is catchable only by the meas-share bar, whose
   false-positive rate is **uncontrolled** (the sole follow-visible
   fixture, ANTITESA, has no benign-low-meas shot). Wiring a rewrite
   behind an unbounded bar risks the ANTITESA `camera_diag.fg` byte-pin.
   So follow-visible A-class stays a **documented partial** (F6 recorded
   as a known wart per the Publish bar), waiting for a temporal
   crop-face-stability signal or follow-visible identity. Consequence:
   the presence pass runs exactly where the occupant map runs —
   attribution regime, both face-id models present — and **clip 1's plan
   stays byte-identical**, a second byte-pin proving the wiring never
   reaches follow-visible.

2. **Production trigger: Bar P, both arms, a BOUNDED face, no identity.** A
   solo shot fails presence (→ fallback) when its **unmeasured share ≥
   0.5** AND **no seek finds a subject-scale face** in the planned crop —
   where "subject-scale" is a crop-face height in **[0.35, 1.9] ×** the
   subject's reference face height. The lower bound rejects the
   poster/figurine (h21–25, ≤ 0.1×); the **upper bound (1.9×) rejects the
   oversized false detection** — the back-of-head / guitar-body blob that
   fires ~2× the subject's face (clip 3 #12/#14, ref 138, crop h283–288 =
   2.05–2.09×). The original wiring had only the lower bound and the
   operator's re-test caught two blobs sailing through (see Correction).
   The identity/track-scan lane is the robustness anchor that *explains*
   attribution's safety but is **not** the trigger (identity cosine is not
   bar material — clean faces degrade to 0.28, ADR 0047). Reference face
   height = the shot's span-median measured face height when it has any
   measurement, else the track's whole-clip box height.

3. **The plausible-size B-class still waits for identity.** The upper
   bound (2) catches the CLEARLY oversized blob. It cannot catch a blob at
   a plausible size (~1× ref — a poster of a person at subject-scale), and
   the height gap between a real leaned-in face (clip 2 #8, Person E
   laughing, 1.85×) and a blob (2.05×) is narrow — height alone is a
   stopgap, not a robust separator. The robust discriminator is per-crop
   face **identity** (a blob embeds as no known person); wiring it needs
   SFace in the seek + the occupant map's centroids, deferred as a
   scoped open thread now that attribution B-class is proven real.

4. **Fallback ladder, unchanged from ADR 0047 decision 4, made precise.**
   A flagged solo piece becomes, in order: **(1) split** — the group
   framing of the tracks measured ≥ `GROUP_PRESENCE_FRAC` (0.40) in the
   piece, when that set is non-empty (`group_layout_span`, `track=None`);
   else **(2) hold** — the last *verified* crop (a prior solo shot that
   passed presence) when **no source cut separates it** from this piece
   (`cut_at_start` false: no cut within 0.05 s of the piece start); else
   **(3) wide** — the honest group view of the angle. No framing anchor is
   written — the same grammar as the off-screen and shared-reaction
   splits, a third honest fuel. The KEY FIX over the stale-crop bug: hold
   is gated on *both* a verified prior crop AND no cut crossed, where the
   bug reused `prev` unconditionally across source cuts.

5. **Bar C (containment / F5) stays audit-only.** The presence fallback
   targets the presence classes; the in-crop/adrift class is already
   audit-caught (ADR 0047: F5 is the existing adrift finding). Not a
   fallback trigger this slice.

6. **Bar arm2 stays all-samples-MISS (conservative, zero FP).** The
   clip-2 #8 borderline (mostly-MISS, one late face) is left unflagged; a
   majority-MISS refinement is a recorded future knob, not this slice —
   keeping the wiring zero-false-positive is what protects the clean shots
   and the fixture byte-pins.

## Production shape

Pure/impure split (ADR 0044 discipline — seeks in the pipeline glue, the
decision in `yc_frame::speaker`, unit-tested):

```
let plan = plan_shots(...);                       // draft (pure, no frames)
let heights = presence_seeks(plan, ...);          // impure: per-solo-shot
                                                  //   full-res seeks over the
                                                  //   planned crop (attribution
                                                  //   + models only)
let verdicts = evaluate_solo_presence(analysis, plan, &heights);  // pure: Bar P
let plan = rewrite_for_presence(plan, analysis, &verdicts, cuts); // pure: ladder
for f in audit_camera_plan(analysis, &plan, &verdicts) { ... }    // + artifact
```

The seek pass reuses the occupant-map machinery (`crop_rgb`,
`FaceIdentifier`, the full-res `stream_frames_rgb` seek) at per-shot times
(span quantiles + the max-gap midpoint) — YuNet over the planned crop only
(no SFace/centroids for the class-A trigger). Cost: ~doubles the occupant
map's seek budget (ADR 0047 measured ~40 seeks / 7–12 s per 60–70 s clip);
**measured and logged, gated on the ADR 0044/0046 cost discipline.**

## The pre-declared wiring gate (bars set before the code)

PASS requires ALL of:

- **Fixtures byte-pinned:** ANTITESA `camera_diag.fg` SHA-256
  `9e07d81f…` byte-identical; Deddy integrated plan `15 shots (1 differ)`
  and all ADR 0044/0045/0046 bars green; **both camera audits clean (0
  findings)** — the fixtures never flag presence, so the new audit class
  emits nothing on them.
- **Regime byte-pins:** clip 1 (follow-visible) plan byte-identical — the
  attribution-only wiring never touches it.
- **Suites:** `cargo test --workspace` AND `--features face` both green;
  count of record **337 both ways**.
- **Healing (the point):** a NEW render on a VIOR attribution clip (clip 2
  and/or clip 3, a FRESH artifact name — never the four passed gate
  artifacts, never the shakedown's shipped renders) shows every
  Bar-P-flagged window healed (split / hold-verified / wide instead of set
  dressing) with the previously-flagged shots no longer flagging on a
  re-measure, and the clean/benign-by-luck shots **unregressed** (their
  crops unchanged).
- **Cost:** the presence pass's added analyze-time wall-time measured and
  within ~2× the occupant map's budget as predicted.

A confirmed regression of any clean shot, a fixture bar breaking, or a
flagged window not healing fails the gate.

## The split must never show one person twice (operator rule, 2026-07-07)

The first render of the healed #12/#14 exposed a second defect: the fallback
**split** framed the **same person on both panels**. Root cause (clip 3
cam9, a tight multicam angle): the wide-shot seat tracks don't correspond to
the tight framing, so seat A mis-tracked onto the **guitar** (420, 239)
beside the real guest at seat C (850, 360); each panel's wide `SPLIT_ZOOM`
crop then spanned guitar→guest and both read as the same person. Neither the
occupant map (it believed A=P4, C=P1) nor centre-distance catches this — the
panels *overlap in rendered content*. Fix, universal in `group_layout` so
every split obeys it: a 2-panel split whose panel crops overlap past
`SPLIT_MAX_PANEL_OVERLAP` (0.5 of the smaller panel) is one region framed
twice → the honest **centered wide** instead. Validated: clip 3 #12/#14
collapse to a clean single wide of the guest; #8/#27 (distinct people) and
the Deddy reaction split (#5, `vstack` intact — smaller faces, panels below
the threshold) stay genuine splits. The general rule — depend on rendered
content, not on track/identity labels that a tight angle corrupts.

## Correction to ADR 0047 (operator re-test, 2026-07-07)

ADR 0047 adjudicated clip 3's meas-0 windows by eye against render extracts
and listed **#12 (22.4 s) / #14 (24.2 s) among the benign-by-luck** set
("subject-scale crop-faces h185–288 … render extracts clean"). The
operator's re-test of the first wired render caught both still showing a
guitar body + the back of a head — real defects. The instrument had the
evidence all along (crop h288/h283 at meas 0%) but the ADR 0047 reading
compared **raw** heights across seats: it could not tell Person B's 2× blob
(ref 138 → h288) from Person D's 1× real face (ref 271 → h280), so it
pooled them. Dividing by the per-shot reference separates them cleanly
(blobs 2.05–2.09×, benign real faces 0.68–1.03×). The lesson repeats the
standing one — **validate on the production path against the operator's
ground truth** — and is why the height bar is now two-sided.

## Verdict

**GATE PASSED (harness + suites + render + operator re-test, 2026-07-07).**

- Fixtures byte-pinned: ANTITESA `camera_diag.fg` = `9e07d81f…`, Deddy
  `15 shots (1 differ)`, both audits clean; both twins correct (Deddy
  0-flagged, ANTITESA follow-visible skip). Clip 1 (follow-visible) plan
  byte-identical.
- Suites **349 both ways** (the pre-slice 337 + 12 new tests).
- Healing: clip 3 flags **#8/#12/#13/#14/#17/#27** (every ship-truth
  defect + the two ADR 0047 mis-adjudicated as benign), clip 2 flags
  **#2/#3/#7/#11**. Zero false positives: the benign windows (≤ 1.03× ref)
  and clip 2 #8 (Person E's 1.85× leaned-in laughing face) stay unflagged.
  The re-render frames the 22–25 s windows on the visible guest (a clean
  centered wide, the duplicate-split fix) and #8/#27 as genuine two-person
  splits — set dressing gone, no panel ever showing one person twice.
- Cost: the harness measured the full instrument at ~40 seeks / 12.8 s;
  production's arm-1 pre-filter seeks only unmeasured shots (~0 on the
  well-measured fixtures, 4–6 on the defect clips), so it is far cheaper
  than the ~2× budget ceiling.

Open threads: the plausible-size B-class (identity, Decision 3); clip 2 #8
majority-MISS (ADR 0047's recorded knob); follow-visible A-class (F6).
