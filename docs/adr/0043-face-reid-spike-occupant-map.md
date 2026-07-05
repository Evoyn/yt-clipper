# Face re-id spike: the occupant map — who occupies a seat, per angle (YuNet + SFace in speaker_diag)

ADR 0042 shipped the voice lane with two measured limits on record: the
voice⇄seat join leans on the mouth lane for co-occurrence (circularity),
and a seat is a screen position, not a person. The integration gate PASSED
2026-07-06 with ear-truth annotations that put a face on both limits: the
operator heard the split's bottom-panel man talk through 14–20 s — on
screen — while the lane's dominant claim there (V1, 4.4 s of the piece's
5.7 voiced seconds) said "off-screen". Frame forensics this session
established the ground truth the numbers had hidden: the source has exactly
**two real cameras** (four humans, two per camera) that the 60 px
seat-geometry signature fragmented into **eleven angles**, so only two ever
recurred, every pair-1 voice was structurally orphaned, and the drinker's
cup churn poisoned the "genuine mouth" reference through 14–22 s — the
join's out-of-sample sweep then validated against that poisoned reference,
because both CV halves of the stretch carry the same churn. Face identity
per (angle, seat) is the instrument that can see all of this. This ADR
records the spike: the **occupant map** measured in `speaker_diag`, gated
on the operator's eyes, before any production wiring (ADR 0029/0042
discipline, third time).

**Nothing in the production analysis path runs any of this.** The spike
lives in `yc_frame::face_id` (alignment, warp, YuNet decode — pure and
unit-tested; the two `ort` sessions behind the existing `face` feature) and
in `speaker_diag` (the face lane, the `faceselftest` mode, the join
replay). The only production-code touch is `voice::build_lane` gaining an
`angle_override: Option<&[usize]>` parameter (production passes `None`;
behavior-identical, proven byte-for-byte — see Regression note).

## Considered options

### The models — OpenCV zoo YuNet + SFace (chosen), pinned + verified

The pair OpenCV designed to work together, both taken at pinned commits
with the repo's own Git-LFS oids as the official checksums:

- **SFace** `face_recognition_sface_2021dec.onnx` — Apache-2.0,
  38,696,353 bytes, SHA-256
  `0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79`
  (`media.githubusercontent.com/media/opencv/opencv_zoo/ba91a3b9…/models/face_recognition_sface/…`).
  112×112 input, raw 0..255 RGB (the reference feeds BGR with swapRB=true),
  128-d L2-normalized embedding; expects the ArcFace 5-point template warp.
  OpenCV's own same-identity cosine floor is 0.363 — a calibration point.
- **YuNet** `face_detection_yunet_2023mar.onnx` — MIT, 232,589 bytes,
  SHA-256 `8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4`
  (same repo, commit `f12e1279…`). Fixed **640×640** input (the export's
  static shape — the README's 320 is not what the ONNX declares), raw
  0..255 BGR, anchor-free heads at strides 8/16/32, score `sqrt(cls·obj)`.
  Not here to detect (Ultraface still does that): here for the **5
  landmarks** the alignment needs.
- **Rejected: InsightFace model zoo** (buffalo_l / w600k_r50 — the obvious
  ArcFace weights): non-commercial research license — the same bar that
  rejected Rev.ai in ADR 0042.
- **Rejected: EdgeFace** (CC BY-NC), **dlib resnet** (permissive but not
  ONNX — no `ort` path).
- **Backup: AuraFace-v1** (HF `fal/AuraFace-v1`, Apache-2.0, ResNet100
  ArcFace-style, trained on commercially-usable data) — the heavyweight row
  if SFace ever underwhelms on the selftest.

Reference conventions (alignment template, blob parameters, decode
formulas) were lifted from OpenCV's `FaceRecognizerSF`/`FaceDetectorYN`
sources the way ADR 0042 lifted the kaldi fbank — and re-implemented in
~250 pure lines (similarity via complex least squares, bilinear warp,
per-stride decode), unit-tested without any model.

### Alignment — YuNet landmarks REQUIRED; the box-only pipeline is rejected by measurement

The selftest (`speaker_diag faceselftest`, five 1920×1080 fixture frames,
ten faces, identities known from the frames themselves):

- **YuNet-aligned**: every same-person pair beats every different-person
  pair — same-person +0.299..+0.889 (the +0.299 tail is the cap man
  mug-occluded mid-drink), different-person max +0.282. PASS.
- **Box-pseudo-landmarks** (Ultraface box scaled onto the template, no
  real landmarks): a different-person pair at +0.397 beats same-person
  pairs at +0.351 — ordering violated. FAIL, rejected — the WeSpeaker
  discipline again: measured on the same code path, not debugged further.

### The lane's shape — full-res targeted seeks, averaged samples, gap-cut clustering

The tracking stream's faces (~57 px at 640-wide) are below any embedder's
useful floor, so crops come from the **source** at full resolution:
targeted `-ss` seeks (one per sampled time, both seats cropped from the
same frame), up to 4 samples per (segment, seat) at spread quantiles inset
from the cut edges, region = 2.0× the tracked box. Per (segment, seat) the
unit embeddings **average** (a blink can't mint a person); per clip the
entries cluster agglomeratively and the cut threshold is the **largest
dendrogram gap** — printed with the whole merge trail, so the same-face /
different-face margin is evidence, not a magic number. On the Deddy
fixture: within-person merges ≤0.23, gap to 0.56, cut at 0.40.

## Measured on the production fixtures

- **Deddy** (the torture case): 25 (segment, seat) entries from 52 frames
  in 13.3 s, zero crops without a usable detection. The map matched the
  frame-verified truth on **all 23 confident entries**: exactly 4 persons
  (P0 tattoo, P1 peci, P2 cap — including the mug-occluded segment, held
  by sample averaging — P3 green), and the 13 signature angles collapsed
  to the **2 real cameras over 11/13 segments**. The two residuals are
  pose extremes (hand-on-chin at ~63 s, looking-down at ~66 s) that split
  into SINGLETONS — conservative failure: never mis-joined, visibly the
  same humans on the sheet.
- **ANTITESA**: the lane runs clean end-to-end (2 entries, 2 cameras from
  its 1 cut) — but the fixture's regression numbers were unmeasurable this
  session, because the operator's 04:36 promote of a *different* Moment
  overwrote the fixture's `segment.mp4` (40 s vs the fixture's 69 s; the
  audio anchor cannot lock). See Regression note.
- **Suites**: 315 green under both `cargo test --workspace` and
  `--features face` (306 + 9 new pure tests).
- **Production untouched**: the Deddy run reproduces ADR 0042's production
  lane and plan exactly (thr 0.60, lane 8.9 s @ 93%, off-screen 10.0 s, 15
  shots with the 2 approved splits, camera audit clean).

### Regression note: the ANTITESA byte-pin caught a poisoned fixture, not a code change

The standing `camera_diag.fg` pin `9e07d81f…` broke — and bisecting by
stashing this session's changes proved committed HEAD **deterministically
produces the same drifted output byte-for-byte** (`93fb2b80…`, twice) on
the same input: the drift is entirely the overwritten fixture segment. The
pin re-arms once the operator re-promotes the 790.0–859.0 Moment (their
choice: they'll do it themselves — which doubles as their first look at
the Studio's voice row on the freshly-built release exe; the "no
difference in the Studio" report that opened this session was a stale
binary, built 2 h before the integration commit).

## The replay finding: identity exposes the join's poisoning; merging alone is NOT the fix

The harness replays the production join over occupant-merged cameras
(`build_lane` with the map as `angle_override` — diagnostic only). The
naive replay makes things *measurably worse*: claimed time balloons
8.9 s → 22.8 s at a fake "91%", off-screen mass 10.0 s → 24.4 s, because
the pair-1 camera is now multi-visit and the churn-poisoned 14–22 s
co-occurrence gets the blessing the single-visit rule used to deny it —
even V0, the 26.9 s laughter blob, joins a seat. The map makes the failure
*visible where it was invisible*: **V1 joins seat A in both cameras, but
camera 0's seat A is P0 and camera 1's is P2 — one voice, two humans**
(V2 likewise: P1 vs P2). A seat-scoped join cannot even express that
contradiction; a person-scoped one wears it on its face.

The measured design inputs for the production join upgrade (its own
session, its own gate):

- **Person-consistency gate**: a voice cluster joining different persons
  across cameras is impure or its evidence is — refuse or split. On this
  fixture that kills exactly the poisoned edges (V0/V1/V2's cam-1 joins)
  and keeps the clean ones (V3/V4/V5/V10).
- **Purity-weighted co-occurrence**: the poisoned evidence announces
  itself — V1's join bins are 91% both-mouths (contested), V0's 95%; the
  clean V2→B evidence is 39%. Weight or gate by the uncontested share.
- **The operator's ear-truth spans as the acceptance bar**: 14–20 s the
  green-shirt man on screen (bottom panel), 20.8–21.4 s the cap man's ~1 s
  on-screen interjection, 22–27 s cross-talk (hold), 27–30 s laughter
  (split correct). Plus the frame-verified occupancy for all 13 segments
  (this session's scratchpad frames; the truth table is reproducible from
  the segment).

## Operator verdicts (2026-07-06, same session)

- **The contact sheet PASSED: "each row is one human."** The occupant map
  is a real instrument; production wiring may build on it.
- The ADR 0042 integration gate passed the same morning ("it still looks
  good") with the ear-truth annotations above — recorded in ADR 0042's
  verdict section.

## Consequences

- `yc_frame::face_id`: pure similarity/warp/resize/decode/NMS +
  `FaceIdentifier` behind the existing `face` feature (no new feature, no
  new dependency — `ort` was already there). `speaker_diag` gains
  `faceselftest` and the face lane + replay; `voice::segment_bounds` is
  now the shared segment slicer.
- The models are **not** app-registry rows yet — the harness expects them
  at `models/face_detection_yunet_2023mar.onnx` and
  `models/face_recognition_sface_2021dec.onnx`; a missing file skips the
  lane with a note. ADR 0041 rows + Downloads healing land with the
  production integration, not before.
- The next slice is the **per-person join upgrade** in production
  (person-consistency gate + purity weighting + the single-visit rule
  re-derived over cameras), gated on a fresh A/B render against
  `diar_integration.mp4` — where the 14–20 s piece should attribute to the
  visible green-shirt man, and the 20.8 s interjection finally has the
  identity evidence to earn its operator-requested cut.
- Cost: the face lane adds ~13 s to a 70 s clip's harness run (52 seeks +
  50 embeddings, one core); a production integration would amortize crops
  into the existing tracking pass or accept the seeks at Promote time —
  measured there, not assumed here.
