# Session prompt — Face re-id spike: who OCCUPIES a seat, per angle

You are giving yt-clipper's speaker analysis person identity across camera
angles (F:\yt-clipper, pure-Rust egui app). Fresh session: read the context
below, then run /grill-with-docs BEFORE any code. One implementation this
session: **a face-embedding spike measured in speaker_diag** — an embedding
per Speaker track per angle segment, angles joined by the PERSON's face —
gated on the operator's eyes/ears before any production wiring (the ADR
0029 lesson stands: the CAM++ spike earned integration through exactly this
path).

## Open the grill with
1. The **diar_integration.mp4 verdict** (A/B against diar_baseline.mp4 —
   rendered 2026-07-06, verdict outstanding; the visible change is the two
   approved off-screen splits) AND the operator's first look at the
   Studio's voice row/chip on a real Promote. If their ear finds a
   mis-join or the splits read wrong, fix THAT first (thresholds are
   measured constants — re-measure via the harness before moving one).
2. What face re-id must unlock, in priority order: (a) single-visit angles
   become known identities so the 20.8 s-class interjection cut can fire
   ("cut when voice is sure" — today it structurally can't on Deddy), (b)
   de-echo the voice⇄seat join (today it leans on the mouth lane's
   co-occurrence — the measured circularity limit in ADR 0042), (c) harden
   off-screen calls (a suspect confirmed by "his face is NOT on screen" is
   no longer a suspect).

## Read first (in this order)
1. CONTEXT.md — Voice lane / Speaker track (seat ≠ person!) / Camera plan.
2. docs/adr/0042-voice-diarization-spike-campplus.md — the **Integration**
   section (the four decisions) + the survey discipline the face model
   must repeat (selftest before fixtures; rejection by measurement).
3. handoffs/2026-07-06-diarization-integration.md — gate state + traps
   (YC_SMOOTH_RENDER now clobbers a passed gate artifact; overridden is
   filled by fusion, not build_lane; rescue-fires-zero-on-Deddy is by
   design, face re-id is the unlock).
4. crates/frame/src/voice.rs — build_lane's angle grouping (the 60 px seat
   signature the re-id would replace as identity evidence) + fuse rule.
5. crates/frame/src/speaker.rs — SpeakerTrack (path = per-bin face boxes:
   the crops a face embedder would eat), plan_shots steps 4-5.

## Likely shape (grill will settle; do not pre-commit)
- Survey → pick a face-embedding ONNX (ADR 0041-compatible: pinnable URL +
  SHA; CPU; selftest on known same/different faces BEFORE fixture numbers
  mean anything — the WeSpeaker rejection discipline).
- Harness first: embed each track's face crops per angle segment (the
  tracking frames are already streamed — decide whether to re-stream or
  cache crops), cluster/match across angles, print an identity map
  (angle × seat → person), score it against the voice lane's angle joins
  on BOTH fixtures.
- The join upgrade it enables (production, only after its own gate):
  per-(cluster, PERSON) evidence instead of per-(cluster, angle) — a
  single-visit angle inherits its occupants' identities.
- 3+-visible stays the centered column; group_layout 2x2 remains a later,
  separately-gated extension.

## Non-negotiable method (unchanged)
Measurement loop first — extend speaker_diag, never bypass it (it now
CALLS the production voice functions; keep that property). Validate on
BOTH fixtures. ANTITESA regression gate: `camera_diag.fg` byte-identical
(SHA-256 `9e07d81ff777bafceaebaf2992b4c90d407d2b8c5933a441f0159153527e8dad`).
Suites green BOTH ways: `cargo test --workspace` AND `--features face`
(306 today). Renders for the operator's eyes use FRESH names — never
clobber diar_*.mp4 / camera_smoothing.mp4 (passed-gate artifacts).

## Fixtures + harness (same as ever)
- Deddy: `workspace/Deddy Corbuzier/BGN B NYA…. 😂 SEREM BGT NIH PODCAST
  ASUU‼️ Tretan, Coki, Adriano/data`, range `1559.6094450950623
  1629.7294450950621`.
- ANTITESA: `workspace/Leon Hartono/ANTITESA Cacing Cacing Naga Naga! -
  Ft. Andrew Susanto/data`, range `790.0 859.0`.
- `cargo run --release -p yt-clipper --example speaker_diag --features
  face -- "<data dir>" <start> <end> [csv]`.
- Verify recipe (GUI drive incl.): `.claude/skills/verify/SKILL.md`.

## Ritual
/grill-with-docs first; /verify before committing; finish with /handoff to
`handoffs/<date>-<slug>.md` + a dated entry prepended to whatwedone.md;
commit as Evoyn with the model's Co-Authored-By trailer (message via -F
file); `git push origin main` has standing permission. Windows PS 5.1
quirks per the standing memory notes.
