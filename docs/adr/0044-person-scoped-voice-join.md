# Person-scoped voice join: the production join rebuilt over the occupant map (purity-gated edges, positive-absence off-screen)

ADR 0043's spike proved the occupant map is a real instrument (gate PASSED:
"each row is one human") and its replay proved the naive merged-camera join
makes things worse: the churn-poisoned 14–22 s co-occurrence gets the
blessing the single-visit rule used to deny it. This ADR records the
production rebuild — the voice⇄seat join upgraded from angle-scoped to
PERSON-scoped, gated on a fresh A/B render (`diar_person.mp4` against the
passed `diar_integration.mp4`) and the ANTITESA byte-pin. **The gate PASSED
2026-07-06** — with a verdict that traded two good-looking splits for an
analysis that never lies (see Measured, below: the evidence that bought
those splits was proven false, and its honest replacement does not exist in
the current instruments).

## The design (settled in the session grill, then measured)

- **Purity-gated edges.** A (cluster, camera) join edge counts its
  co-occurrence only after four gates: the camera is seen 2+ times (a
  single-visit camera's co-occurrence is mouth echo), the evidence floors
  pass (`JOIN_MIN_BINS_SEG`, `JOIN_MIN_SHARE`), the claimed seat's occupant
  is KNOWN (the occupant both names the edge's person and proves that
  pooling the camera's segments pooled one human's seat), and the edge's
  **contested share** stays under `JOIN_MAX_CONTESTED` = 0.65. A contested
  bin (2+ mouths over the activity floor) is a margin coin-flip, not weak
  evidence — during the Deddy churn it was systematically wrong. Measured
  per-edge: clean edges run 16–17% contested, every poisoned edge announces
  itself at 87–100%; the whole-cluster bracket was 39% vs 91%. The constant
  sits mid the wide gap. Hard gate, no soft weighting — a weight is a
  tunable dial with tune-to-answer risk.
- **Layered person consistency.** Surviving clean edges resolve through the
  map to persons. All edges agree → the cluster IS that person: it claims
  the person's mapped seat in ANY segment (a map placement is face
  evidence, exempt from the single-visit rule — the identity evidence lives
  elsewhere), and it may assert off-screen. Clean edges still contradict →
  the cluster is provably impure: REFUSED a person identity (no transfer,
  no off-screen powers) but its clean camera-local claims stand — exactly
  the ADR 0042 seat semantics over cameras. Refuse the person, keep the
  clean local evidence.
- **Positive-absence off-screen.** The flag fires only when the voice's
  person is known, every seat on screen in the segment has a KNOWN
  occupant, and the person is not among them. Ignorance — an unsampled
  seat, an unknown occupant, an unmapped segment, missing face-id models —
  never sets the bit (the old "no learned seat here" flag minted the
  fixture's false off-screen calls; suspects still print in the harness for
  the operator's ear, with no plan effect).
- **Singletons are unknowns.** A one-entry face cluster (pose extreme —
  ADR 0043's P4/P5 were frame-verified the same humans as real persons) is
  an unknown occupant, not an identity: it carries no evidence, blocks
  absence-proofs in its segment, and keeps its segment out of the camera
  merge. No absorption pass: that would need a second threshold in the
  measured danger zone (cut 0.40, nearest different-person distance 0.56),
  risking the exact mis-join the spike's conservatism was praised for
  avoiding, for zero payoff on any acceptance span.
- **The whole-clip join survives only under a single camera** (no
  cross-camera leak is possible) and then only purity-gated — the
  single-camera fallback must not readmit contested evidence (a unit test
  pinned this after the first cut of the code did exactly that).
- **The sweep scores the join that ships.** The CV-scored threshold sweep
  runs the person join (map active) or the seat join (fallback) — the ADR
  0043 lesson that a sweep scored on a different objective validated
  against a poisoned reference. On Deddy the honest objective moved the
  picked threshold 0.60 → 0.55.
- **Wiring.** The occupant map joins `Job::AnalyzeSpeakers` as targeted
  full-res seeks (the spike's exact measured shape, ~15 s per 70 s clip,
  sequential, cancellable), computed only when ALL of: attribution regime
  (follow-visible ignores the join by construction — ANTITESA never pays),
  both face-id models present, voice lane viable. The pure machinery
  (sampling plan, region crop, track-face pick, clustering, camera merge,
  wildcard semantics) lives in `yc_frame::occupant`; the pipeline and
  `speaker_diag` share it — the harness stays the production twin. Missing
  or broken models log a warning, the join runs the shipped seat-scoped
  path, the Camera panel says why (per-lane note lines) — never a failed
  job. YuNet + SFace became ADR 0041 registry rows (`yunet-model`,
  `sface-model`; the opencv_zoo pins from ADR 0043, re-verified
  byte-identical against the full-hash URLs this session) with Downloads
  healing.

## Measured on the production fixtures

- **Deddy**: the purity gate kills every poisoned edge and keeps the one
  clean one — V2 × cam0, 3.1 s on seat B at 16% contested → person P1 (the
  peci man), independently corroborated at seg12 (1.0 s, 17% contested,
  100% share on the visible P1). V1 — the false "off-screen dominant" of
  14–20 s — is refused outright (both edges 87–100% contested): the false
  call is dead and the stretch holds honestly. The positive-absence flag
  fires twice, both times truthfully coherent: 20.8–21.4 s and 68.6–69.1 s
  are P1 speaking while visibly not seated — and the source itself cuts TO
  the peci man at 69.1 s mid-sentence (a J-cut the instrument read off the
  pixels). **The 20.8 s finding overturns the session's ear-truth**: the
  operator heard the cap man interject, but the clean two-camera evidence
  names the off-screen peci man — the interjection cut therefore must NOT
  fire (it would frame the wrong human), and does not.
- **The two approved splits lose their fuel.** Off-screen mass drops
  10.0 s → 1.5 s (the truthful residue), below the split thresholds, so the
  14.2–22.1 and 27.0–30.2 pieces revert to the mouth-only solos —
  `diar_person.mp4` equals the mouth-only plan byte-for-byte. Three
  candidate honest replacements for the laughter split were measured and
  all fail to discriminate: contested share is 74–100% on EVERY segment
  (grinning listeners), absence share peaks at 18%/35% (under every floor),
  and cluster composition inverts (the V0 blob covers 77–100% of monologue
  segments and 0% of the laughter segment, whose voiced time fragments into
  V4/V8/V9 scraps). CAM++ is a speech embedder; laughter is out-of-domain.
  Per the session grill's contingency terms, no unmeasurable grammar
  shipped.
- **ANTITESA**: byte-identical `camera_diag.fg` (`9e07d81f…`) through the
  entire rebuild — re-verified on the re-promoted fixture at clean HEAD and
  again on the final build. The occupant map is never computed in
  follow-visible, structurally.
- **Suites**: 327 green under both `cargo test --workspace` and
  `--features face` (315 + 12 new pure tests: occupant map semantics,
  person transfer, purity kill, refusal, positive absence).

## Operator verdict (2026-07-06, same session): PASS — ship it

The operator watched `diar_person.mp4` against `diar_integration.mp4` with
the full evidence story and ruled to ship: honest evidence over lucky
output. The split grammar stays armed for genuinely-proven off-screen
voices; the laughter-class instrument (an acoustic laughter/shared-reaction
detector — the named path to win the splits back with true evidence) queues
as a backlog candidate, not a commitment.

## Consequences

- The off-screen flag is now a **claim of positive evidence**: fewer,
  truer flags. A model-less install has no off-screen grammar until the
  Downloads page heals it (transient by design; the Camera panel's note
  says so).
- The ear is calibration, not ground truth: the 20.8 s overturn is the
  first case where the instrument corrected the operator's annotation, and
  the acceptance record now distinguishes "heard" from "proven".
- `voice::build_lane`'s `angle_override` (the ADR 0043 replay hook) is
  gone; the parameter is the occupant map itself, and the naive-merge
  replay demonstration lives only in ADR 0043's record.
- The Deddy 14.2–22.1 piece frames the cup-drinker solo again — the shot
  the operator originally disliked — now with a printout proving why
  nothing honest can improve it yet (V1 impure, green man unnameable by
  voice or co-occurrence). The candidate instruments: laughter/reaction
  class (above), or a mouth-motion classifier that can tell a cup from a
  jaw (the churn poisons everything downstream of it).
- Cost: ~15 s of targeted seeks per 70 s attribution-regime clip at
  analyze time, sequential; parallel seeks remain an unmeasured
  optimization if the operator ever feels the wait.
