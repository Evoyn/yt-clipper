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

## Refinement — in-shot follow + position-true framing (2026-07-04)

Operator testing on the Leon Hartono VOD surfaced three failure modes the
first cut missed, all from framing a shot by a **whole-clip** face position and
never moving inside a shot:

- **Loses a moving speaker.** Tracks matched detections to a track's *all-time
  mean* center, so a person who leaned toward the mic or shifted in their chair
  shed detections into a phantom second track and the camera framed where they
  *used* to sit. Matching now follows each track's **last-seen** position, so a
  slow mover stays one track.
- **Frames the wrong spot.** A solo shot cropped around the track's whole-clip
  median box — wrong whenever the speaker had moved since the clip opened. Each
  shot is now framed from that track's **path over the shot's own bins** (median
  box + a P10–P90 center band), so it frames where the person is *during that
  shot*, and the crop is grown to contain the band (a lean that comes back is
  never cropped through the face). AutoFace (one static crop for the clip) is
  sized over the track's whole path the same way.
- **Abrupt or off-frame on real drift.** When a subject drifts more than a
  dead-zone (12 % of the crop) between a shot's opening and closing seconds, the
  shot now carries a `pan_to`: the render glides the *same-sized* crop from
  opening to closing position with one time-expression `crop` filter
  (`x='x0+dx*min(t/dur,1)'`), and the Studio preview steps the identical
  `Crop::lerp` so the two can't disagree (ADR 0036). A short first run also
  folds into the next shot, so the camera never opens on a sub-minimum flash.

This does **not** reopen "pan instead of cut." Cuts remain the grammar
*between* speakers; the follow is a bounded, dead-zoned correction *within* a
solo shot so a moving person stays in a comfortable frame — the amateur look
the ADR rejected was a camera *hunting a static wide shot for the active
speaker*, not a slow follow of a subject who actually moved. The pan is
opt-in per shot (`Shot::pan_to`, `#[serde(default)]` so old plans load), size
never breathes, and a group/split shot never pans.

## Refinement — multicam sources + prop faces (2026-07-04)

The Leon Hartono / ANTITESA VOD exposed the deeper wrong assumption. Its source
is **already a finished multi-camera edit**: it cuts between a one-person camera
on each guest (never a shared wide shot), and a framed photo sits on the set
table. Running the static-wide plan on it produced the operator's two symptoms —
"can't tell Person A from B" and "camera on empty space":

- **The set's framed photo was tracked as a third speaker.** Ultraface fires on
  the printed face; it survived the presence gate (persistent, like a real
  guest). Fix: `finish` drops a track that is both **much smaller** than the
  tallest (`< PROP_MAX_H_FRAC`) **and** far less lively (`peak mouth motion <
  PROP_MOTION_FRAC ×` the liveliest) — a printed face only shimmers with codec
  noise (measured ~1/4 of a real mouth), so the bar is *relative* to real
  motion, not an absolute floor (which the noise cleared). Guarded so it can
  never empty the track list.
- **Framing a fixed per-person position parks on emptiness when the source cuts
  away.** In a multicam edit, "Andrew's position" is bare background the instant
  the source cuts to Leon's camera — and audio attribution lags the cut, so the
  crop held Andrew's now-empty spot while Leon was on screen (the operator's
  empty-crop screenshot). Fix: detect the regime by **mean faces visible per
  bin** (`mean_visible_faces`): a static wide shot keeps everyone in frame
  (≈ track count), a multicam/solo source shows ≈1 (`< MULTICAM_MAX_MEAN_FACES`
  → `plan_follow_visible`). The follow-visible plan ignores audio attribution
  entirely — the source **already** cut to its subject.

The follow-visible plan mirrors the source's own cuts (`subject_series` →
`plan_follow_visible`), and a second operator retest sharpened *how*:

- **The subject is the source's on-screen framing, not the largest face.** One
  visible face → follow it; **two or more visible → a `Group` split** (the
  source cut to a shared/wide shot, so show everyone rather than pick one). A
  single face is unambiguous and a two-face frame is a group, so there is no
  "largest face" contest to flicker — the earlier flicker (the camera trading
  between two similar faces every frame in a wide shot) is gone by construction.
  Cutaways hold the previous subject; sub-`MULTICAM_MIN_SHOT_S` runs fold into
  their neighbour so a transition wobble never becomes a cut.
- **Each source shot is framed STATICALLY on its subject's median position** —
  no leading-edge pan (which lagged every cut by ~1 s while the crop slid into
  place) and no per-bin follow (which jittered with the detector). A static
  source camera wants a static crop, correct from the first frame. The
  whole-clip median box was the bug the first refinement fixed; the *per-source-
  shot* median is what a multicam edit actually needs (a person is at a
  different screen position in each camera). `group_layout_span` splits only the
  people **on screen during that shot**, not every camera framing of them.

Since different camera framings of the same person are distinct position tracks,
a heavy multicam edit yields several tracks per person; that is fine for framing
(each is a real position) though the A/B/C/D labels over-count people — merging
them needs face re-identification (an embedding model), the same upgrade path as
diarization. The static-wide active-speaker plan (attribution + margin/hold +
group collapse) is unchanged and still chosen whenever 2+ faces are usually in
frame; the follow-pan lives there, for a speaker who drifts across a long turn.

A third retest (a 69 s clip whose source is the same multicam edit) exposed a
blank crop during a 10 s wide two-shot, and diagnosis found the last root cause:

- **Track survival was gated on a *fraction of the clip* (20 %), which drops a
  real but short-lived multicam framing.** Each camera framing of a person is
  its own position track (they sit at different screen x per camera), so a 10 s
  wide-shot inside a 90 s clip is ~11 % of frames — under the 20 % gate — and was
  discarded. With no track for the wide-shot faces, `subject_series` saw nothing
  there and held the previous (single-camera) crop over an empty position: the
  blank. Detection was never the problem — the wide-shot faces detect at p=1.00
  at 5 fps; they were *found then thrown away*. Fixed by gating on **absolute
  on-screen time** (`MIN_TRACK_SECONDS`, 1.5 s) instead of a clip fraction — a
  false positive still flickers for a frame or two, but a real framing of any
  length survives. `MAX_TRACKS` also rose to 6 (a multicam edit legitimately has
  several framings per guest).
- **A set prop that catches passing hands/cups can beat the relative-motion prop
  filter**, so the size test gained a **hard floor** (`PROP_HARD_MIN_FRAC`): a
  face under 0.4× the tallest is a prop regardless of nearby motion (podcast
  guests sit at similar distances, so a real speaker is never a third the height
  of another). With the book gone and the wide-shot framings kept, the wide
  two-shot resolves to a clean two-person split.

A fourth retest found the residual: a **1-5 frame blank at every cut**, and a
wide shot that sometimes rendered as a three-way column or a split with an empty
panel. Two causes, two fixes:

- **The 5 fps analysis grid quantized every cut to a 0.2 s boundary.** The render
  held the old crop until the next analysis bin — up to ~5 frames (at 24 fps
  playback) after the source had already cut, over an off-screen position: the
  blank. **`SPEAKER_FPS` raised 5 → 24** so a cut lands on ~1 frame. Everything
  downstream is derived from this one rate (`<seconds> * SPEAKER_FPS` or
  `/ bin_s`), so it just sharpens timing; a ~3-minute clip is ~4 k CPU
  detections (~30 s) — the "quality over speed" the operator asked for. (The
  frame tests now express counts in seconds via an `nbins` helper, so the rate is
  no longer baked into them.) Detection *resolution* was again confirmed fine —
  the wide-shot faces detect at p=1.00; only the *timing grid* was coarse.
- **`group_layout_span` counted any track with a single frame in the span**, so a
  person's *other* camera framing catching a few frames as they lean (multicam →
  several position tracks per person) inflated a clean two-person split into a
  three-way column, and a stray framing showed an empty split panel. It now
  requires a track to be present for **`GROUP_PRESENCE_FRAC` (40 %)** of the shot
  to be one of its people — so a wide shot splits only the people actually in it
  for its duration.

## Refinement — the source's real cut frames are the shot boundaries (2026-07-04)

Even at 24 fps a residual ~2-frame blank remained at cuts: *any* fixed sample
rate quantizes a cut to a bin, and the source frame rate need not match the
grid. The frame-precise answer is to stop *inferring* cuts from sampled
detections and read the source's **own cut frames** directly. `do_analyze_
speakers` runs one extra ffmpeg pass — `select='gt(scene,0.2)',metadata=print`
— over the clip: pixel-level scene detection at the full frame rate (cheap, no
model), giving the exact clip-relative time of every hard cut (measured: hard
cuts score ~0.3, in-shot motion stays under ~0.1). Those times flow into
`plan_shots(.., cuts)`; the multicam path's new `plan_by_scene_cuts` makes each
**inter-cut span one shot**, framed on whoever is on screen during it
(`segment_subject`: a `Group` when 2+ share the span, else the dominant face),
with adjacent same-subject spans merged (a scene trigger that changes nothing
visible — a false positive, or a re-cut to the same camera — is not a cut).
Every cut then lands on the exact source frame regardless of sample or source
rate — 100 % cut accuracy, the operator's ask. `plan_follow_visible` (per-bin
runs) remains the fallback when scene detection returns nothing (a genuinely
static single shot, or a failed pass). The 24 fps grid still governs track
building and per-shot framing; the *cut timing* no longer depends on it.

## Refinement — trim boundaries carry full pts precision (2026-07-05)

The operator's residual "1-3 frame blank" at a few cuts (the ANTITESA export)
was **not** detection, planning, or analysis-fps — it was the render formatting
shot boundaries at 3 decimals. A boundary is a real source frame's pts (scene
detection returns the first frame of the incoming shot); ffmpeg `trim`'s
`start` is inclusive (`pts >= start`) and `end` exclusive. `{:.3}` rounds
~half of all cut pts **up** past the cut frame's own pts (e.g. 13.302833 →
13.303), so the incoming shot's trim rejects its own first frame and the
outgoing shot's trim keeps it: the first frame of the **new** scene renders
through the **old** shot's crop — one frame of "empty seat" / half-out person
exactly at the cut. Which cuts flash is a per-cut coin flip (does the pts
round up?), which is why only some cuts showed it.

Measured on the production clip (`segment.mp4`, `-ss 7.885`, 23.976 fps): 14
cuts, 7 flashed — exactly the 7 whose pts round up at 3 dp; the 7 that round
down were clean. The export's own scene-score series is the flash detector: a
clean switch is a **single** inter-frame spike at the boundary's output slot;
a stranded frame is a **double spike one frame apart**. The shipped export had
7 doubles; after the fix, 0, with every switch on its exact predicted output
frame, and frame strips at the worst cuts visually clean. The source's cuts
are all single-frame hard cuts (no transition frames anywhere in the series).

Fix: `build_camera_filtergraph` prints boundaries with Rust's shortest
round-trip float `Display` (`{}`), never a fixed precision — even `{:.6}`
rounds a 7-decimal pts like 0.0812889 up by 1e-7 (absorbed by the µs→tick
rescale at mp4's common timebases, but the bug class only dies with lossless
printing). The regression test pins the two measured production pts.

Two prior theories are **ruled out for hard-cut sources**, and their
uncommitted code was dropped rather than shipped: a fixed post-cut hold
(`CUT_LEAD_S` — the detector does *not* fire early on this footage; every
detected cut IS the settled first frame of the new shot, so a +60 ms hold
would strand 1-2 new-scene frames in the old crop at **every** cut, making
the flash universal), and tracked-subject boundaries for dissolves the
detector missed (no dissolve exists in this footage, and bin-grid times are
not frame pts — re-inviting the same stranding). If a source with real
cross-dissolves ever appears, that work needs its own frame-level evidence
first.
