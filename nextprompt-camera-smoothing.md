# Session prompt — Active-Speaker camera: stop the re-frame twitch at jump cuts

You are smoothing the Active-Speaker camera in yt-clipper (F:\yt-clipper,
pure-Rust egui app). Fresh session: read the context below, then run
/grill-with-docs BEFORE any code. One implementation this session:
**per-angle crop stability for the attribution regime's angle pieces**, with
its measurement loop. The operator reported it directly (2026-07-05, after
watching the diarization gate renders): "can we make the jitter tracking
more smooth" — confirmed to mean the EXPORTED video: the camera twitches
tighter/wider at the small jump cuts even while the same seat keeps
talking.

## Read first (in this order)
1. CONTEXT.md — Camera plan / Speaker track (seat!) / Voice lane.
2. docs/adr/0038-podcast-active-speaker-camera.md — especially the
   2026-07-05 refinements (angle-aware attribution shots: WHO from
   attribution, WHERE per angle piece; MIN_PIECE_S sliver folding).
3. docs/adr/0042-voice-diarization-spike-campplus.md — seats ≠ persons
   across angles; the angle-grouping-by-seat-geometry idea (brittle at
   60 px quantization — this session probably hardens or replaces it).
4. handoffs/2026-07-05-diarization-spike.md — traps + how to run
   everything.
5. crates/frame/src/speaker.rs — `plan_shots` step 5 (the per-piece
   framing chain), `solo_span_framing`, `static_span_crop`, `SOLO_ZOOM`.
6. crates/app/examples/speaker_diag.rs — THE harness. Extend it; never
   bypass it.

## The measured symptom (Deddy fixture, clip 9, this exact plan)
Adjacent same-subject pieces re-frame AND re-zoom at every source cut:
Person A's crop height runs 894 -> 702 -> 884 -> 736 -> 1080 px across
0..27 s (pieces #0-#4); position jumps ~150 px between pieces whose
subject barely moved. Each solo piece derives zoom from the face height
measured DURING THAT PIECE (`SOLO_ZOOM` x span median + band growth), so
detector noise + leans + per-camera differences make the framing breathe
at every jump cut. A human editor cutting back to the same camera reuses
the same framing; a 5-20% zoom pop every couple of seconds reads as
jitter.

CAREFUL, measured constraint from ADR 0042: on this source the SAME SEAT
holds DIFFERENT HUMANS in different angles (pieces #0 vs #1 frame
different people at one screen position). So "same subject" for crop
reuse must mean same seat WITHIN THE SAME CAMERA ANGLE — cross-angle crop
locking would frame the wrong person's geometry. The spike's
angle-grouping (seat-geometry signature, 60 px grid) exists in the
harness; it was too brittle (leans split angles). Hardening it — or
replacing it with previous-piece dead-zone comparison — is a grill
decision.

## Candidate design (grill will settle; do not pre-commit)
- **Dead-zoned piece re-framing**: a new piece REUSES the previous
  same-subject piece's crop exactly unless the subject's span center
  moved past a dead-zone (FOLLOW_DEADZONE_FRAC exists) or its face height
  changed past a threshold (~12%?). Most jump cuts move the person a few
  px -> identical crop -> zero twitch, and real angle changes still
  re-frame fully.
- **Zoom lock per (seat, angle)**: crop SIZE from the seat's median face
  height across the whole angle-group, position per piece. Needs the
  angle grouping hardened.
- Possibly both: size locks per angle, position dead-zones per piece.
- Explicitly NOT reopening pan-instead-of-cut (ADR 0038), NOT changing
  MIN_PIECE_S sliver folding, NOT touching the follow-visible regime
  (ANTITESA is signed off and must not change).

## Non-negotiable method (unchanged)
Build the feedback loop first: add adjacent-piece forensics to
speaker_diag (per same-subject piece pair: delta-center px, delta-height %,
same-angle verdict) and LOOK at the numbers on BOTH fixtures before
choosing thresholds. Validate on the production fixtures + render for the
operator's eyes. Zero regression on ANTITESA (its regime must produce a
byte-identical plan); the Deddy render must keep every operator-signed-off
behavior (cuts frame-exact, WHO unchanged — this session is WHERE-size/
position stability only).

## Fixtures + harness (same as ever)
- Deddy: `workspace/Deddy Corbuzier/BGN B NYA…. 😂 SEREM BGT NIH PODCAST
  ASUU‼️ Tretan, Coki, Adriano/data`, range `1559.6094450950623
  1629.7294450950621`.
- ANTITESA: `workspace/Leon Hartono/ANTITESA Cacing Cacing Naga Naga! -
  Ft. Andrew Susanto/data`, range `790.0 859.0`.
- `cargo run --release -p yt-clipper --example speaker_diag --features
  face -- "<data dir>" <start> <end> [csv]`; `YC_VOICE_RENDER=1` renders
  A/B gate files.

## Diarization gate: ANSWERED (2026-07-05 night; ADR 0042 verdicts section)
Split screen approved ("use it"); interjections < 2.4 s cut ONLY on a
confident voice attribution with the speaker on screen; the 4+-people
question answered (visible-count degradation; 2x2 grid is a possible
later gated extension). Integration is unblocked and QUEUED AFTER this
session — do NOT fold it into this one. This session is WHERE-stability
only; it must not change WHO or add any voice-driven behavior.

## Ritual
/grill-with-docs first; finish with /handoff to `handoffs/<date>-<slug>.md`
+ a dated entry prepended to whatwedone.md; commit as Evoyn with the
model's Co-Authored-By trailer (message via -F file); `git push origin
main` has standing permission. Windows PS 5.1 quirks per the standing
memory notes.
