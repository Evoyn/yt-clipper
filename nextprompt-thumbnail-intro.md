# Session prompt — thumbnail intro (plan #5; operator queue-jump 2026-07-13)

The operator jumped the queue past title-gen, in their own words: **"add
new button to insert thumbnail, it will always insert for the first
1s-2s of the video, and make sure if user insert a thumbnail, everything
bellow should follow how many seconds thumnail add, so the caption and
camera cut doesnt error."** Plan #5's own text (`feature-implementation-
plan.md`, THEIR file): insert image, default duration 1 s (editable),
JPG/PNG/WebP, automatically fit the project aspect ratio. This lands on
the fresh multi-track shell (ADR 0066 + amendments, commit `b52355e`) —
read ADR 0066 first; its pre-registrations come due HERE: an Images-kind
artifact arrives WITH its render machinery, and track contents (image
paths) are the pre-registered project.json schema moment.

## What EXISTS today (verified in code 2026-07-13 — do not re-guess)

- **The ASS burn is POST-concat, once** (`crates/render/src/export.rs`):
  `build_filtergraph` = crop/scale → `[v]subtitles=<ass>:fontsdir=fonts[out]`;
  `build_camera_filtergraph` = per-shot trims (+ `atrim` pairs when the
  razor cuts audio) → `concat` → `[cat]subtitles=…[out]`, audio label
  `[aout]`. `export_args`/`export_args_script` map `[out]` + (`[aout]` |
  `0:a:0`). Tests PIN "subtitles once, post-concat" and the razor's
  a/v pairing (export.rs tests ~323–478) — extend them, never delete.
- Captions/camera/razor all live in SOURCE-relative clip time; the razor
  already remaps captions through kept spans (`remap_units_through_cuts`)
  so ASS times equal OUTPUT time today.
- **The app ships NO image decoder, deliberately** (`crates/app/
  Cargo.toml:39`) — every texture is raw RGB decoded by the pinned
  ffmpeg (frame extraction; `player.rs` live decode →
  `egui::ColorImage::from_rgb` + `ctx.load_texture`). The thumbnail's
  preview texture must come the same way (one ffmpeg rawvideo call);
  ffmpeg also covers plan #5's JPG/PNG/WebP for free. Do NOT add the
  `image` crate.
- `rfd = "0.15"` is already the file-picker (pattern at `main.rs:1846`).
- `yc_core::Clip` (`crates/core/src/lib.rs:365`) is the project.json
  record a persisted thumbnail belongs on.
- The timeline: `Viewport` maps strip x↔t; every block/marker draws
  through `t_to_x`; `RenderSpec` (editor.rs) is the editor→render
  bundle; `Job::Render` params thread through `pipeline.rs do_render`.
  Track headers are cards (ADR 0066 round 2); `+ Caption` and the
  scissors live in the transport row.

## The architecture their ruling implies (decide honestly on "automatic")

1. **Prepend AROUND the finished main stream — never shift the data.**
   The thumbnail becomes a second ffmpeg input (`-loop 1 -t D -i img`,
   scaled/padded to 1080×1920, `fps`+`setsar` matched) plus `anullsrc`
   silent audio (aformat-matched), concat'd AHEAD of the main stream
   **after** its ASS burn: `[thumbv][thumba][out][audio]concat=n=2:v=1:
   a=1`. Captions, camera cuts, and razor spans stay source-relative BY
   CONSTRUCTION — "the caption and camera cut doesnt error" becomes
   structural, not arithmetic. REJECT the alternative (adding +D to
   every caption/shot/ASS event): it touches every time consumer and
   re-shifts on every duration edit.
2. **The timeline shows the truth**: a thumbnail block occupying output
   `[0..D]` at Video 1's head (the CapCut prepend idiom — NOT a dead
   Images track; a full overlay track stays #14's arc). Everything else
   DISPLAYS at `t + D` while its data stays source-relative — one intro
   offset applied at the strip's t↔x boundary (ruler spans `D + dur`).
   Gestures/scrub subtract it back. Grill (or decide): the razor/marker
   verbs inside `[0..D]` should refuse (the intro is not razor-able
   source; its right edge IS its duration control).
3. **The button**: `+ Thumbnail` in the transport (rfd picker → copy or
   reference the path?). Block right-edge trims duration — default 1.0 s
   (plan #5), clamp to the operator's spoken 1–2 s window (grill the
   exact clamp; recommend 0.5–2.0 hard bounds with 1.0 default).
   Right-click removes; re-pick replaces. Track-card honesty per ADR
   0066: the block is Video 1 content, so no new header card until a
   real Images track exists.
4. **Preview**: playhead runs in OUTPUT time `[0 .. D+dur]`; parked
   inside the intro the canvas shows the thumbnail texture (ffmpeg-
   decoded, aspect-fit); the caption overlay/camera chip show nothing
   (nothing burns there). Play from inside the intro: hold the image
   for the remainder of D, then start audio+video (the wall-clock
   anchor + `video_aligned` machinery already models "video not started
   yet" — recommend that over skipping the intro). Scrub across the
   boundary must not desync audio (same restart contract as today).
5. **Persistence**: `Clip` gains the thumbnail (path + duration_s) in
   project.json — the ADR 0066 pre-registered moment. A picked file
   path is durable data (unlike session captions); recommend persisting
   in slice 1 and saying so in the ADR. Missing file on load = drop the
   intro with a note, never a crash.
6. **RenderSpec/Job::Render** gain the intro (path + duration); batch/
   headless renders pass `None` — untouched, test-pinned.

## Bars (pre-register in ADR 0067 BEFORE building)

- Filtergraph tests: thumb prepend sits AFTER the one `subtitles=`
  (count stays 1); concat pairs v+a; `export_args` maps the final
  labels; the ASS events are BYTE-IDENTICAL with and without an intro;
  razor + camera + intro compose in one graph.
- Editor tests: display-offset round-trip (block at source t draws at
  t+D, gesture at x edits source t); duration clamp; refuse-razor-in-
  intro; RenderSpec carries the intro only when set.
- The burn gate (the operator's eye, on a REAL export): thumbnail head
  1–2 s, first caption lands exactly on its word, a camera cut and a
  razor cut land where the strip shows them. Gate on the burn, not the
  preview.

## The ritual

/grill-with-docs UNLESS the operator says "automatic" — then decide by
their ruling + this file, pre-register ADR 0067, build, and keep the
ADR 0066 shell grammar intact (cards, viewport, pointer-tracked
gestures). Release builds FOREGROUND. Validate: cargo test -p yt-clipper
-p yc-core -p yc-render, clippy on touched files, `"--features"
"face,align,ser"` check, `scripts\build-release.bat`, operator's eye on
the burn. Standing rules: quality over runtime, PS 5.1 quoting, commit
via `git commit -F <file>`, captions never corrected via store JSON, AI
and operator artifacts never share a track. Finish: handoff +
whatwedone.md + fresh nextprompt + the starter line.
(`nextprompt-title-gen.md` re-queued BEHIND this arc — operator jump.)
