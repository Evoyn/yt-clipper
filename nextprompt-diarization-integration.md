# Session prompt — Diarization integration: the voice lane joins the production planner

You are integrating the ADR 0042 voice lane into yt-clipper's production
speaker analysis (F:\yt-clipper, pure-Rust egui app). Fresh session: read
the context below, then run /grill-with-docs BEFORE any code. One
implementation this session: **the voice lane in `Job::AnalyzeSpeakers` +
the two operator-approved camera behaviors** (off-screen split, voice-gated
interjection cuts), measured in the harness and rendered for the operator's
eyes before anything ships.

## Open the grill with
(The camera-smoothing gate PASSED 2026-07-05 late night — operator watched
all three symptom windows: "i see it and it fixed". Do NOT re-collect it;
the camera behavior incl. framing memory, static-first pans, and the
camera audit is signed off. New camera changes this session must keep the
audit clean on both fixtures.)
1. Where the voice lane surfaces in the Studio UI (speaker timeline lane?
   confidence chip?), and whether `voice` stays inside the `face` feature.
2. How the off-screen split interacts with the framing memory (group
   pieces leave no anchors) and whether a voice-rescued interjection cut
   re-frames or reuses (it lands mid-angle, not at a source cut).

## Read first (in this order)
1. CONTEXT.md — Voice lane / Speaker track (seat!) / Camera plan.
2. docs/adr/0042-voice-diarization-spike-campplus.md — all of it,
   especially **Operator verdicts** (the gate PASSED; do not re-litigate)
   and Consequences (the two measured join limits).
3. handoffs/2026-07-05-diarization-spike.md — traps: in-sample join
   scoring lies; per-segment joins echo the mouth lane; never re-hold
   mouth-lane commitments in fusion; the whole-clip join is regime-scoped.
4. handoffs/2026-07-05-camera-smoothing.md — the framing memory + its
   gate state, ANTITESA regression hash.
5. crates/app/examples/speaker_diag.rs — `build_voice_lane` +
   `fuse_attribution` (the drafted production logic to lift) + the
   off-screen demo plan.
6. crates/frame/src/voice.rs and crates/frame/src/speaker.rs.

## The operator's verdicts (recorded in ADR 0042 — build them, don't re-ask)
- **Off-screen voice → the visible pair's split screen: approved ("use
  it").** A known voice with no seat in the on-screen angle means nobody
  on screen is framed solo as the speaker.
- **Interjections under the 2.4 s min-shot: cut ONLY on a confident voice
  attribution with the speaker on screen in the current angle.** Mouth
  motion alone never triggers it.
- 3+ visible stays the centered column; a 2x2 grid for a true 4-wide is a
  later, separately-gated `group_layout` extension.

## Likely shape (grill will settle; do not pre-commit)
- `Job::AnalyzeSpeakers` grows the voice lane (same windows/model, the
  CV-scored threshold sweep); `SpeakerAnalysis` carries voice identity +
  off-screen spans; `plan_shots` learns the two behaviors; the editor's
  speaker timeline gets the lane; the CAM++ model joins
  `App::download_specs` (ADR 0041 row — URL/SHA-256 in ADR 0042).
- Interactions to design in the grill: how an off-screen split piece
  interacts with the framing memory (group pieces leave no anchors), and
  whether a voice-rescued interjection cut re-frames or reuses (it lands
  mid-angle, not at a source cut).
- The voice joins lean on the harness's 60 px angle signature — brittle
  for framing (ADR 0042 pointer) but load-bearing for identity evidence;
  face re-id is the named upgrade if the operator's ear finds mis-joins.

## Non-negotiable method (unchanged)
Measurement loop first — extend speaker_diag, never bypass it. Validate on
BOTH fixtures. ANTITESA regression gate: `camera_diag.fg` byte-identical
(SHA-256 `9e07d81ff777bafceaebaf2992b4c90d407d2b8c5933a441f0159153527e8dad`)
— the follow-visible regime must not change. Deddy: every behavior change
must be one the operator approved, rendered for their eyes
(`YC_SMOOTH_RENDER` renders the production plan; `YC_VOICE_RENDER` writes
the diar_* A/B names — don't clobber gate artifacts, pick fresh names for
new gates). Full workspace suite green (293 tests today).

## Fixtures + harness (same as ever)
- Deddy: `workspace/Deddy Corbuzier/BGN B NYA…. 😂 SEREM BGT NIH PODCAST
  ASUU‼️ Tretan, Coki, Adriano/data`, range `1559.6094450950623
  1629.7294450950621`.
- ANTITESA: `workspace/Leon Hartono/ANTITESA Cacing Cacing Naga Naga! -
  Ft. Andrew Susanto/data`, range `790.0 859.0`.
- `cargo run --release -p yt-clipper --example speaker_diag --features
  face -- "<data dir>" <start> <end> [csv]`.
- The CAM++ model sits at
  `models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx`
  (NOT in git; URL/sha in ADR 0042 if missing).

## Ritual
/grill-with-docs first; /verify before committing (drive the Studio flow —
this session touches production analysis, not just the harness); finish
with /handoff to `handoffs/<date>-<slug>.md` + a dated entry prepended to
whatwedone.md; commit as Evoyn with the model's Co-Authored-By trailer
(message via -F file); `git push origin main` has standing permission.
Windows PS 5.1 quirks per the standing memory notes.
