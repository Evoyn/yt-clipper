# Podcast Mode: mouth-motion speaker attribution + a cut-based Active Speaker camera

The app's next content target is podcasts (focus 2026-07, task 3): a static
wide shot, 2–4 visible people, and a Short that must keep the camera on
whoever is talking. This ADR records the detection design and the camera model
that renders it.

## Considered options

### Who is speaking — visual mouth motion gated by audio (chosen)

Per promoted Clip, the worker streams tracking frames (long edge ~640, 5 fps,
one frame resident at a time via `stream_frames_rgb`), detects faces per frame
(the existing Ultraface `face` path), and matches them into **speaker tracks**
by screen position — podcast cameras are static, so a track's box barely
moves. Each track measures per-frame **mouth activity**: mean absolute luma
change over a fixed 16×10 sample grid inside the lower-central face box (a
talking mouth moves; a listening one doesn't; the fixed grid makes near and
far faces comparable). An RMS **voice gate** over the clip audio kills
attribution through silence. Per voiced bin, the speaker is the track with the
most smoothed activity, behind a switch margin (1.35×) held for 0.8 s — a lip
twitch never steals the camera. Off-screen speech (voice with no moving mouth)
holds the incumbent rather than guessing.

Rejected **audio diarization** (speaker-embedding clustering): the right tool
for overlapping voices and off-screen speakers, but it needs a new model +
runtime and answers "which *voice*", which still has to be joined to a *face*
to drive framing. The mouth-motion join is direct, model-free, CPU-only, and
testable synthetically. Diarization can *join* the ensemble later exactly like
detection Signals do; it does not replace this.

Rejected **landmark-based lip tracking** (a face-mesh model): more precise
mouth geometry, but another ONNX dependency for precision the 5 fps grid can't
use. The luma-diff patch is deliberately crude and measured to separate a
talking mouth from a listening one by >5× in tests.

### How the camera moves — cuts, not pans (chosen)

The speaker timeline becomes a **camera plan**: contiguous shots, each a
static Layout — solo shots are a 9:16 crop zoomed to the speaker (face ≈ 3.6
face-heights of crop height, headroom-biased), and a rapid exchange (3+
consecutive short turns) collapses into a **group shot** (2 people: the
stacked split screen; 3+: a centered column). Minimum shot length 2.4 s;
silence holds the current shot; interjections too short to cut to are
absorbed. Human podcast editors *cut* between speakers — a virtual camera
panning across a static wide shot reads as amateur, so panning was rejected
outright, and with it the whole per-frame-crop rendering problem.

Render: each shot is `trim → crop/scale (→ vstack) → concat`, one ffmpeg pass,
ASS burned once over the concatenated stream (shots are contiguous from 0, so
caption timing is untouched). The graph grows with the shot count, so it
travels as a `-filter_complex_script` file (`camera.fg`). Validated end-to-end
against the pinned ffmpeg: a 3-shot solo/split/solo plan renders 1080×1920
with frame-accurate cuts.

### Where it runs

`Job::AnalyzeSpeakers`, CPU-only, on the prepared Segment. The editor
auto-queues it when Prepare's facecam detection saw 2+ persistent faces (a
podcast-looking frame); otherwise it's the Camera panel's "Detect speakers"
button. Results (`SpeakerAnalysis` + seed `CameraPlan`) drive the editor's
face overlays, speaker timeline lanes, confidence chip, and the Active
Speaker / Group camera modes; the operator can retarget any shot by clicking
another face (focus's manual override).

## Consequences

- Speaker detection is heuristic and visual: overlapping speech attributes to
  the most animated mouth, and an off-screen voice holds the last shot. Both
  are acceptable podcast behaviour; diarization remains the upgrade path.
- The `face` feature stops being optional in practice — `build-release.bat`
  now builds with it (the pure-geometry fallback still compiles without).
- `yc_frame::speaker` is pure and unit-tested (tracks, VAD, attribution,
  shot planning, framing); only the Ultraface inference + ffmpeg streaming
  live in the pipeline glue, mirroring the M6 split.
