# Speaker diarization spike: a CAM++ voice lane beside the mouth lane, angle-scoped identity, off-screen detection

> **Integration shipped 2026-07-05 (the session after the verdicts)** — see
> the "Integration" section at the end for the production shape and the
> three decisions it added.

ADR 0038 chose mouth-motion attribution and named audio diarization the
upgrade path. The operator's verdict on the Deddy fix-validation render made
that path current (2026-07-05 evening): the cuts are right (WHERE signed
off), but "some of the cut error and not framing the one who talking. maybe
we need the diarization" — with tracking and framing structurally fixed, the
residual errors are WHO. This ADR records the spike: a **voice lane**
measured beside the mouth lane in `speaker_diag`, on the same production
fixtures, gated on the operator's eyes and ears before any production
integration (the ADR 0029 lesson: analysis-side spikes over-promise; only
production-path runs and the operator's verdict count).

**Nothing in the production analysis path runs any of this yet.** The spike
lives in `yc_frame::voice` (pure fbank + clustering, unit-tested; the `ort`
session behind the new `voice` cargo feature) and in the `speaker_diag`
harness (lanes, joins, fusion, the A/B gate renders). OFF by default until
the gate passes.

## Considered options

### The embedding model — 3D-Speaker CAM++ zh_en advanced (chosen)

`3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx` from the
sherpa-onnx `speaker-recongition-models` GitHub release (the typo is real):
Apache-2.0, 28,281,164 bytes, SHA-256
`aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2` (matches
the release's published checksum.txt), 192-dim embeddings, CAM++ — the
fastest surveyed architecture on CPU (published RTF 0.013 single-thread;
measured here: 64 windows of a 70 s clip embed in ~2 s). The exports take
80-mel Kaldi-style log-fbank `[1, T, 80]`, not waveform, so the spike
implements the exact kaldi-native-fbank frontend in Rust (25 ms/10 ms,
povey window, preemphasis 0.97, DC removal, snip_edges=false, mel 20–7600
Hz, no dither) plus per-window mean subtraction — ~200 lines, no new
dependency. On the known-speaker selftest (sherpa's sr-data wavs) it
separates same-speaker +0.60 from different-speaker +0.05 cosine.

- **Rejected: WeSpeaker VoxCeleb CAM++ LM** (the survey's paper favourite).
  It FAILED the known-speaker selftest under every documented convention
  (int16/unit scale × CMN on/off; best case same-speaker +0.26 vs
  different-speaker +0.66) with the identical fbank code that the chosen
  model separates cleanly on — rejected on measurement, not debugged
  further. If the chosen lane ever underwhelms, retry it with WeSpeaker's
  exact torchaudio frontend (snip_edges=true, high_freq=nyquist) before
  blaming the weights.
- **Rejected: pyannote embedding** — HF repo is gated (terms + token), no
  official ONNX (3.x removed onnxruntime), and its real embedder is a
  re-hosted WeSpeaker model anyway. Not pinnable, ADR 0041-incompatible.
- **Rejected: NVIDIA TitaNet** — no official ONNX export (NeMo Python
  export step), a different feature frontend (librosa mel, per-feature
  norm, `[N, 80, T]` + length input), English-centric training. Viable via
  sherpa's export if ever needed; most Rust work of the candidates.
- **Rejected: Rev.ai segmentation models** — non-commercial license.
- The selftest stays in the harness (`speaker_diag selftest <wav>…`):
  any future candidate must order same/different speakers correctly there
  before its fixture numbers mean anything.

### Windows, clustering, and the threshold — data-scored, not magic

Voiced spans from the production VAD (breaths ≤0.2 s bridged, spans
<0.3 s dropped) slice into 1.5 s windows hopped 0.75 s — WeSpeaker's own
diarizer operating point, and CAM++'s home turf. Agglomerative
average-linkage clustering over cosine distance; the cut threshold is swept
per clip and scored **out-of-sample end to end**: the cluster→seat join is
computed on alternating 2 s blocks and its seat lane scored on the
complementary blocks. In-sample scoring is disqualifying — measured on the
Deddy fixture, tiny overfit clusters "agree" 100% with the mouth lane while
carrying zero identity; cross-validated they collapse (3.6 s coverage vs
the picked cut's 9.4 s). The picked thresholds differ per clip (0.60 rowdy
Deddy, 0.40 clean ANTITESA) — that adaptivity is the point of scoring
through the join.

### The join is angle-scoped: a voice is a person, a seat is a position

The spike's deepest measured finding. On the Deddy fixture (a 4-person
episode cut between two-person angles), triangulation proved the "seats"
hold **different humans in different camera angles**: voice cluster V1
articulates on-screen as the left man of one angle (strip at 24.5 s,
"IYA DONG, GUE JUGA BINGUNG…") and speaks again at 14–21 s where the camera
shows a different pair — the man the mouth lane framed there was drinking
from a cup (the cup's luma churn faked "talking"), and the actual speaker
was **off-screen**. So:

- Voice clusters join seats **per angle** (segments grouped by seat
  geometry), and only an angle seen 2+ times may claim — a single-visit
  angle's co-occurrence can only echo the mouth lane (no information).
- The **whole-clip join is only valid where a track is one person**: the
  follow-visible regime (solo-cam multicam — each track is one person's
  framing). In the attribution regime with several angles it is banned —
  it was measured leaking one person's voice onto another person's seat.
- A joined voice with **no seat in the on-screen angle is an off-screen
  speaker** — the signal mouth motion cannot produce at all. The harness
  prints these as "off-screen suspects"; on Deddy they nail the proven
  14–21 s stretch.

### The fusion rule (drafted; integration is the next session)

Voice never replaces the visual join — it tiebreaks it:

- **Pass-through**: claims the mouth lane already committed (its own margin
  + hold) stand. (A first draft re-held them and VAD-gap resets pushed the
  mouth's legitimate 50.9 s switch to 69.2 s — double-holding is wrong.)
- **Margin tiebreak**: where a same-angle-joined voice disagrees and the
  mouth cannot refute that seat by its own switch margin (1.35×), the
  voice's seat takes the bin; the override run has its own confirm hold
  (0.8 s of claimed bins, breaths don't reset it) and commits
  **retroactively** to its start — the hold exists to stop flicker, not to
  shorten the interjection it rescues, and the analysis is offline.
- **Off-screen override**: a known voice with no seat in the current angle
  means nobody on screen should be framed solo as the speaker. The demo
  render shows the visible pair's split screen for such shots (podcast
  grammar for "the speaker isn't in this shot"); making the production
  planner do this is the integration slice, pending the operator's gate.

## Measured on the production fixtures

- **ANTITESA** (follow-visible regime, clean turns): the voice lane
  independently reproduces the mouth timeline — 95% agreement over 39.6 s
  claimed of ~49 s voiced; 7 of 9 switches confirmed within ±0.6 s. The
  camera plan is untouched by construction (the regime ignores
  attribution): **zero regression**, and the harness's baseline render of
  the Deddy plan is byte-count-identical to the operator's validated
  fix-validation export.
- **Deddy** (attribution regime, rowdy comedy): both mouths clear the
  motion floor 90–100% of the time (the listener grins and laughs — mouth
  attribution runs on margins between two moving mouths; mean confidence
  0.38 post-merge), and 55% of voiced time is one shared acoustic class
  (V0: laughter/cross-talk, co-occurring with both seats at every
  threshold) that no voice embedding can attribute to one person either.
  On the clean turns the voice lane claims 8.9 s at 93% agreement with
  same-angle evidence, finds the interjection at 20.8 s the mouth lane
  missed, and flags the proven off-screen stretch. The conservative fused
  plan changes **zero shots** (the rescued 1.1 s interjection is absorbed
  by the 2.4 s min-shot grammar — ADR 0038's deliberate behavior), so the
  gate A/B pairs the baseline against the **off-screen demo** render
  (two shots become the visible pair's split).

## Operator verdicts (2026-07-05, same night — the gate PASSED)

The operator watched the A/B renders and ruled:

- **Off-screen split screen: approved — "the split screen is good, use
  it."** The off-screen override (a known voice with no seat in the
  on-screen angle shows the visible pair's split) is greenlit for
  integration into the production planner.
- **Interjections under the min-shot: "cut when voice is sure."** The
  2.4 s minimum gains one exception: a short turn earns a cut only when
  the voice lane confidently attributes it AND its speaker is on screen
  in the current angle. Mouth motion alone never triggers it (that was
  the flicker the min-shot rule exists to stop).
- **Scaling question ("what if 4+ people on screen?") answered**: the
  group framing already degrades by visible-count (1 solo / 2 split /
  3+ the centered wide column, people counted at ≥40% presence in the
  shot). The 3+-visible column is the acknowledged compromise; a 2x2
  grid for a true 4-wide is a small, isolated `group_layout` extension
  to gate with the operator's eyes if a real clip ever wants it.
- **Order**: the camera-smoothing session
  (`nextprompt-camera-smoothing.md`, the jump-cut re-frame twitch) runs
  first; the integration slice is its own session after it.

## Consequences

- The operator gate (watching `diar_baseline.mp4` vs
  `diar_offscreen_demo.mp4`) decides integration, which needs two grammar
  decisions first: should a **known off-screen voice** show the visible
  pair's split screen (as demoed), and should **short interjections**
  (< 2.4 s) ever earn a cut. Both change ADR 0038 behavior; neither is
  assumed. *(Both were answered the same night — see Operator verdicts
  above: split approved, interjections cut only on confident voice.)*
- Integration also inherits two measured limits: the cluster↔seat join
  leans on the mouth lane for co-occurrence (a stretch where the mouth is
  consistently wrong can mis-join a single-angle voice — **face
  re-identification** is the de-circularizing anchor, the other half of
  ADR 0038's upgrade path), and shared laughter/cross-talk is
  unattributable by design (holding the current shot there is correct).
- The camera planner does **not** reuse the seat-geometry angle grouping:
  per-angle crop stability shipped instead as geometry-matched **framing
  memory** (ADR 0038 refinement, 2026-07-05) after the 60 px signature
  measured too brittle for framing (leans split angles — on the Deddy
  fixture every adjacent piece pair signed differently). The grouping stays
  a harness diagnostic for the voice lane's per-angle joins.
- On adoption the model gets an ADR 0041 registry row (URL + SHA-256 +
  size above; the Downloads UI heals a missing file). Until then the spike
  expects it at `models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx`.
- `yc_frame::voice` (fbank, windowing, clustering) is pure and
  unit-tested; only `VoiceEmbedder` needs `ort` (`voice` feature; the
  app's `face` feature pulls it so the harness builds unchanged). CPU cost
  is negligible beside face tracking (~2 s per 70 s clip, one core).
- `SPEAKER_FPS`, the caption decode path, and every production analysis
  and render path are untouched; the full workspace suite stays green
  (288 tests).

## Integration (2026-07-05, the session after the verdicts)

The lane joined `Job::AnalyzeSpeakers` with the two approved behaviors.
The grill settled four decisions the spike had left open:

- **The fused lane IS the production attribution.** `SpeakerAnalysis`
  gained `voice: Option<VoiceLane>` (per-bin joined seat, off-screen flag,
  and which bins fusion overrode), and `speaking`/`confidence` are the
  fused result — one source of truth for what the camera follows, and
  `plan_shots` keeps its signature (the voice rides inside the analysis).
  The join/sweep/fusion logic lifted verbatim into pure `yc_frame::voice`
  functions; `speaker_diag` now CALLS the production functions instead of
  carrying a copy (the ADR 0029 lesson, closed). Fusion runs in both
  regimes (only the join is regime-scoped); follow-visible plans ignore
  attribution by construction, so ANTITESA's `camera_diag.fg` stays
  byte-identical — the standing regression gate.
- **The off-screen split is decided per angle piece, before framing.** In
  `plan_shots` step 5, a solo piece whose voiced bins are ≥50% known
  off-screen voice for ≥1.2 s (the demo's measured thresholds) becomes the
  visible pair's split screen and **writes no framing anchor** — group
  pieces leave no anchors stays an invariant, and the return to the angle
  after the split reuses the pre-split crop verbatim (zero twitch across
  the interlude). The spike's post-pass demo would have left an anchor for
  a solo crop that never rendered; deciding before framing doesn't.
- **The interjection rescue lives in the min-shot absorption.** A
  sub-2.4 s run survives step-4 absorption only when it carries at least
  the fusion confirm hold (0.8 s) of voice-**overridden** bins — "cut when
  voice is sure" made mechanical, robust to silence-fill padding at the
  run's tail. "Speaker on screen in the current angle" is structural, not
  a check: only a same-angle-joined voice can override (an off-screen
  voice holds no seat there), so mouth motion alone — and off-screen
  speech — can never mint a sub-minimum cut. The interjection piece and
  the resuming shot frame through the existing framing memory: cut away,
  cut back, the resumed crop is verbatim the pre-interjection one.
- **The model soft-degrades and self-heals.** A missing or broken CAM++
  file logs a warning and yields exactly the pre-integration mouth-only
  analysis (never a failed job); the model is an ADR 0041 download row
  (`voice-model`, the sherpa-onnx `speaker-recongition-models` release
  URL, the SHA-256 above), and the Camera panel's voice status line says
  when the lane is off and why.

The Studio surfaces the lane: a thin voice row in the timeline strip
(spans colored by joined seat, off-screen spans distinct), a `voice` tag
on the tracking chip when the current bin was voice-overridden (or an
off-screen split), and a one-line voice summary in the Camera panel. The
integration gate artifact is `diar_integration.mp4` (`YC_INTEG_RENDER=1`
in the harness) against the operator's existing `diar_baseline.mp4`.
