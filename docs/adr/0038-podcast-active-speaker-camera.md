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

## Refinement — the preview crop binds to the frame on screen (2026-07-05)

With the render fixed, the operator still saw the blank **in the Studio
preview**: the same disease, different quantizer. The preview picked the
camera shot by the **playhead clock**, but the playhead is not what is on
screen — the picture quantizes it. Paused or scrubbing, the ~2-4 fps
filmstrip shows the frame *nearest* the playhead (up to half a strip
interval — hundreds of ms — away), so parked just past a ✂ marker the
incoming shot's crop painted over the outgoing shot's pixels for as long as
you looked at it. Playing, the live pipe decoded at a hardcoded `fps=24`
over a 23.976 source, re-quantizing every cut by up to a frame — the same
per-cut coin flip the render had.

Fix, one principle: **crops, overlays, and face-click retargeting are picked
at `display_time` — the content time of the frame actually displayed — never
at the playhead.** The playhead remains the audio/caption clock. Paused,
`display_time` is the shown strip frame's extraction time; playing, it is
the **midpoint** of the newest delivered live frame (midpoints tolerate up
to half a frame of seek/grid phase, since a cut boundary is an exact frame
pts). The live pipe now decodes on the **source's own grid** (`fps=<probed
r_frame_rate>`, threaded `SegmentProbe::fps` → `Progress::Prepared::src_fps`
→ the editor → `PreviewPlayer::spawn`; 24 stays the fallback for unprobeable
sources), so delivered-frame counts convert exactly to content time. The
result is not "the preview switches at the cut pts" — it is stronger: the
preview switch **always lands on the same displayed frame as the content
switch**, whatever the strip or stream resolution.

UX merge, same session (operator ask): the Camera panel's separate **"Detect
speakers" button is gone** — it queued the identical `AnalyzeSpeakers` job
that picking **Active Speaker / Group** already auto-queues, and read as a
second feature. The AI mode chips carry the cost note ("first use runs the
speaker analysis"); Failed keeps its Retry.

## Refinement — same-seat track merge + angle-aware attribution shots (2026-07-05)

The Deddy Corbuzier clip (a 4-person episode cut between TWO-PERSON angles
and jump cuts — the burned studio clock leaps minutes across a cut) broke
the attribution regime three ways at once: a drifting camera (slow pans
right/down "chasing" nobody), twin labelled boxes on one head in the
editor, and attribution flipping to the "wrong" person. `speaker_diag`
(new inspector, the production glue replicated 1:1) measured the shared
root: **2 people on screen became 4 tracks**. A detection gap (mic/hand/
profile) plus a shift past the last-seen match radius mints a new track,
and the stale one later reclaims its old spot — fragments 20 px and 104 px
apart, co-visible 0.0-0.1 s, one with a 31.8 s hole. Fragmented tracks
fragmented the mouth signal (mean attribution confidence 0.21, "switches"
between one person's own halves), and framing windows landed inside a
fragment's gaps, producing pans toward stale positions (a −114 px glide
while the subject moved +2 px; a +95 px "down" drift computed after its
track had vanished).

Two fixes, both in `yc_frame::speaker`, both measured on the same clip:

- **`merge_same_seat_fragments`** (in `TrackBuilder::finish`, before the
  persistence gate so short fragments can be saved by reuniting): tracks
  merge when **near** (median centers within 0.8× the wider face — measured
  same-seat pairs sit 20-104 px apart vs 519 px to the next person),
  **temporally complementary** (co-visible ≤ 0.35 s — one person can't be
  detected twice), and — the safety gate — **each fragment shares the frame
  with someone outside the pair** for ≥ 40 % of its bins. That last test is
  what keeps solo-camera multicam edits (Leon/ANTITESA) unmerged: there,
  two *different people's* solo framings also alternate near the same
  position, but a solo framing shows no other face, so it never clears the
  bar, while any two-person angle does. Result on the clip: 4 tracks → the
  2 real seats (right seat 100 % presence, zero gaps), confidence 0.21 →
  0.38, switches only between real people.
- **Angle-aware attribution shots** (`plan_shots` step 5): when the
  "static wide" source turns out to be an edit anyway (`cuts` non-empty —
  faces-per-frame alone cannot tell a true static wide from a show cutting
  between two-person angles), each attribution run splits at the source's
  own cut frames and each piece frames from ITS bins alone; slivers under
  `MIN_PIECE_S` (0.35 s) fold into their neighbour, and a piece whose
  subject was never detected keeps the previous piece's framing. A cut
  re-positions everyone (~100 px here), so one crop or glide spanning a cut
  was framed on an average position existing in neither angle — the
  cropped-off forehead and the drift. WHO stays attribution's job; WHERE is
  per-angle. Result: 15 pieces, remaining pans match measured real motion
  to the pixel ((+56,+42) pan vs (+57,+42) moved — an actual lean,
  followed), the re-render sweep shows 14 single switch spikes with 0 flash
  doubles, and the frame strips regain headroom.

The operator asked for "100 % who-is-speaking, remember the voice, detect
the lips": lips ARE the current signal (mouth-region luma motion, chosen at
the top of this ADR); voice-remembering is diarization — still the
documented upgrade path, now easier to justify since attribution operates
on whole seats. No detector is 100 %; this refinement removes the
structural errors (identity fragmentation and cross-angle framing), which
is where the visible failures lived.

## Refinement — framing memory: per-angle crop stability at jump cuts (2026-07-05)

With cuts frame-exact and WHO structurally fixed, the operator's next
verdict on the Deddy renders was WHERE-stability: "can we make the jitter
tracking more smooth" — the exported camera re-framed AND re-zoomed at
every small jump cut while the same seat kept talking. Each angle piece
derived its crop from its own bins alone (`SOLO_ZOOM` × the piece's median
face height + center-band growth), so Person A's crop height ran
894→702→884→736 px across 0..27 s: detector variance, leans and per-camera
differences re-derived a new framing at every cut a human editor would have
cut back to with an unchanged one.

The diag harness gained adjacent-piece forensics (each solo piece vs its
best prior same-seat piece), and the numbers split bimodally on BOTH
production fixtures:

- **returns to an already-framed camera**: center ≤ 0.26 anchor face
  heights, face height ≤ 7% off (the Deddy returns: 45/16/38/27/11/43/13 px)
  — yet the re-derived crops jumped up to 79 px / 6.6% there: the twitch;
- **real changes**: center ≥ 0.39 fh (a different angle re-positions the
  seat ~100 px) or height ≥ 21.5% (a +24.7% piece is the source itself
  cutting tighter).

**Framing memory** (`piece_framing`, wrapping the step-5 solo piece chain —
the attribution regime only): per seat, whole-clip, every framed piece
leaves a FIXED anchor (band-center + median face height + the emitted crop;
a panning piece anchors its CLOSING state, where the subject ended). A new
piece searches its seat's anchors most-recent-first:

- **inside the dead-zone** (center ≤ `REUSE_CENTER_FH` 0.30 fh AND height
  ≤ `REUSE_H_FRAC` 12%, both mid-gap of the measured distributions) → the
  anchor's crop VERBATIM — the zero-twitch jump cut. A reuse never
  re-baselines the anchor, so a slow slide accumulates delta against the
  original and earns ONE honest re-frame when it becomes real, always at a
  source cut, where a re-frame is perceptually free;
- **height matches at a genuinely new position** → the anchor's SIZE
  re-placed on the subject: a lean moves the camera, never the zoom;
- **otherwise** a fresh framing, a new anchor. Every reuse must also keep
  the piece's whole P10–P90 center band a face's own extent plus air inside
  the reused crop (`REUSE_GUARD_X_FH`/`REUSE_GUARD_Y_FH`) — a remembered
  framing can never crop through a bobbing face; the guard falls through to
  fresh instead.

Angle identity is deliberately NOT computed. The ADR 0042 constraint — the
same seat holds different humans across angles, so crop reuse must mean
"same seat within the same camera angle" — is satisfied by the geometry
match itself: a different angle either fails the dead-zone (different
position/size → full re-frame at the cut) or matches so closely that the
reused framing frames the new occupant correctly anyway. The spike's
seat-geometry signature grouping (60 px grid) measured too brittle for this
job — on the fixture EVERY adjacent piece pair got a different signature
(leans split angles) — and stays a harness diagnostic for the voice lane
only.

Measured result on the Deddy fixture: boundaries, WHO and pans identical to
the signed-off plan; 7 of 13 solo re-derives became verbatim reuses (crop
delta 0 px, 0.0%); Person A's 0..27 s alternation collapsed to exactly the
source's two camera framings (894 and 702 px) and a pan's next same-seat
shot now holds the pan's landing crop exactly. ANTITESA (follow-visible
regime, which never enters step 5) re-planned byte-identical
(`camera_diag.fg` hash-equal). The operator gate artifact is
`camera_smoothing.mp4` (the production export command via
`YC_SMOOTH_RENDER=1`), beside the untouched diarization A/B renders.

Rejected: **hardening the signature grouping into production** (an
angle-identity mechanism with measured false splits, superseded by direct
geometry matching); **rolling anchor re-baselines** (each piece comparing
to its predecessor — a sub-dead-zone slide would then never re-frame and
walk the face out of the crop); **previous-piece-only comparison** (an
A-B-A camera alternation re-derives on every return, keeping the 894-vs-884
class of pop the memory exists to kill).

**Operator retest, same night: 22–26 s still jittered left.** That window
is the follow-PAN piece — untouched by the memory (pan pieces never
reuse), and rendered identically before and after it. The per-bin series
showed why it reads wrong: the subject lunges 365 px left and RETURNS
(22.1–27.0 s, the face undetected for the first 1.3 s), and a linear
head→tail glide models that as a slow leftward drift — the camera slides
away from a subject who is already coming back. The discriminator is
measured and bimodal: genuine one-way drifts carry an off-drift band
remainder ("extra") of ≤ 0.36× their drift (#6/#9/#12: 18/34/35 px);
the lunge carried ~1.7× (≈200 px extra on a 114 px drift). A pan now
requires the drift to EXPLAIN the excursion (`PAN_EXTRA_FRAC` 0.6,
mid-gap); otherwise the piece holds ONE static crop grown over the whole
band — the same containment grammar static pieces already use. On this
piece the (365,0)→(251,0) glide becomes a stationary full-height frame at
(255,0); the three genuine drift pans and every reuse are unchanged, and
ANTITESA stays byte-identical. The prior refinement's "remaining pans
match measured real motion to the pixel" claim was true of head→tail
NETS — it could not see a there-and-back excursion inside one piece;
extra-vs-drift is the shape test that can.

**Second retest, same night: 30–40 s — the wander-crawl.** The 11 s pan
(net +56 px ≈ 5 px/s) passed the monotonicity test and still read as
jitter: the per-bin series is wander-and-settle (a shift, a rest, a
lunge, a rest — never sustained travel), so a linear crawl keeps the
camera in PERMANENT micro-motion over a subject who is mostly still —
while the whole band would fit a static crop grown just 1.06×. The rule
that survives all three operator verdicts is **static-first**: a pan must
be monotonic AND necessary. `PAN_STATIC_GROWTH` (1.25): when one static
crop contains the span band by growing ≤ 25 % over the base zoom, hold
static — camera motion needs a reason a slightly wider frame can't
supply. Genuine cross-frame travel (the Leon-class case the follow was
built for) needs ~3.8× and keeps its pan. On the fixture this converts
ALL FOUR pans to static frames: with the framing memory, the whole 70 s
clip settles to three framings for seat A and four for seat B, every
remaining crop change sitting on a source cut with a measured subject
change behind it.

**And the detector (operator ask: "detect first so we don't have this
kind of bug again if I export new videos")**: `audit_camera_plan` — pure,
O(shots × bins) — checks every plan against the subject evidence for the
three shipped defect classes: **camera creep** (a crop moving ≥ 4 px per
1 s window while the subject moves less than half of that, sustained
≥ 2 s — the wander-crawl and the lunge-chase), **re-frame without cause**
(consecutive same-seat shots whose crop jumps > 10 % zoom or > 0.2
crop-widths while the subject stayed inside the reuse dead-zone — the
jump-cut twitch, audited so any future planner path regressing it is
caught), and **subject adrift** (a subject outside the crop's safe region
≥ 1 s). The diag harness prints the audit per plan, `AnalyzeSpeakers`
logs each finding, and the Studio Camera panel + export summary show them
BEFORE any render. Zero findings on both production fixtures is the
regression bar; each defect class has a true-positive unit test.

**Operator verdict (2026-07-05, late night): PASSED** — watched the
round-3 render (`camera_smoothing.mp4`, all three symptom windows):
"i think thats good, i see it and it fixed." The framing memory,
static-first pans, and the camera audit are signed off as the production
camera behavior.
