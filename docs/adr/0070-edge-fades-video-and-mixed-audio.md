# Edge fades: fade-in/out at the Short's edges, video and the whole mixed audio together

The undo gate closed (ADR 0069 amendment) and the operator picked the next
arc in their own words: **"next we will work on the fade out effect both in
the music sound and video."** This is feature plan #4 (Transition Menu:
Fade to Black / Fade to White, configurable durations, "Transitions should
affect both video and audio"), sliced honestly to **the Short's edges**: a
fade-in from black/white at output 0 and a fade-out to black/white at the
output end — the video and the export's final mixed audio (voice + music)
dying together under ONE wrap. Run "automatic" (2026-07-13); this ADR
pre-registers the decisions before the build.

## Scope (named, not implied)

IN: one fade-in edge + one fade-out edge on the finished export, one color
(black or white) for both edges, 0–3 s per edge (0 = that edge off),
preview parity on the canvas AND in the preview audio, persistence on the
`Clip`, undo/document membership.

DEFERRED, named: cut-boundary dips (fade-out/HOLD/fade-in between razor
segments — plan #4's hold + plan #10's library) are the next slice;
per-music-clip edge fades (plan #6's pre-registered future item) wait for
the operator's ear; every other plan-#10 transition (crossfade, slide,
zoom) waits for the library shape.

## Decisions

1. **`yc_core::FadeSpec { in_s: f64, out_s: f64, color: FadeColor }`**,
   `FadeColor::Black | White` (serde snake_case, the `CaptionGenre`
   idiom). `0.0` = that edge off; the UI range is 0–3 s (CapCut's feel),
   values quantized to centiseconds at the knob (the `quantize_cs` rule: a
   spec never bakes in precision the panel can't show). `FadeSpec::MAX_S =
   3.0` lives beside `ThumbnailIntro`'s bounds — shared by the editor
   clamp and any future consumer. `Clip.fade: Option<FadeSpec>`
   serde-default + skip-if-none (the `Clip.music` precedent: old
   project.json loads clean, pinned by test). `RenderSpec.fade:
   Option<FadeSpec>` — `None` whenever both edges are 0, so untouched
   sessions send exactly today's spec.
2. **One wrap, LAST: `yc_render::fade_edges(graph, fade, out_total_s,
   has_graph_audio) -> (String, bool)`.** Wrap order becomes burn → intro
   → amix → **fade** in `do_render` (both the camera-script and plain
   paths) — the fade must hear the FINISHED output: it dims the intro's
   first frames and kills the amix's mixed tail, voice and music together
   (the operator's "music sound and video" is one envelope, not two).
   Same rename idiom as `prepend_intro`/`mix_music`: `[out]`→`[prefade]`,
   `[aout]`→`[preafade]`, re-terminate in `[out]`/`[aout]` so the arg
   mapping is untouched. Video: `fade=t=in:st=0:d=IN:c=COLOR` +
   `fade=t=out:st=TOTAL-OUT:d=OUT:c=COLOR` chained; audio: `afade=t=in` +
   `afade=t=out` (afade has no color). A graph with no `[aout]` (plain
   path, no razor/intro/music) grows a fresh `[0:a]afade…[aout]` chain
   and the returned bool tells the caller the audio now maps from the
   graph — the same `main_a` selection dance `mix_music` does. `fade:
   None` (or both edges 0) returns the graph byte-identical and the bool
   unchanged — the standing byte-identity bar; headless/batch always pass
   `None`.
3. **The fade-out anchors on the REAL output end, never `-t` (the
   two-clocks lesson, third verse).** `export_args` receives
   `range.duration_s()` even with razor cuts — `-t` only bounds; the
   razor concat simply ENDS earlier. `do_render` computes `out_total_s =
   intro_d + kept_total` where `kept_total` on the camera path is the CUT
   plan's `kept_spans()` sum (sliver drops included — the caption remap's
   own truth) and `range.duration_s()` on the plain path (no razor there
   by construction). Times print shortest-round-trip (`{}`), the camera
   trim idiom.
4. **Preview parity (ADR 0036), visual AND audio.** Visual: a
   black/white overlay ramp painted over the whole canvas — captions,
   safe-area guides, intro frame included (the fade is the outermost wrap
   in the burn, so it draws last in the preview too). Anchored on the
   EXPORT clock: elapsed = `export_clock(playhead, intro_d, keep)`,
   remaining = `(intro_d + kept_total) − elapsed` — so the ramp is
   constant across veiled (removed) spans and never starts early over
   them, and the fade-in dims the intro exactly as the burn will. Ramp
   strength mirrors the chained filters: visibility = `in_vis × out_vis`
   (each edge's linear ramp), overlay alpha = `1 − visibility`. Audio:
   the app owns every rodio sink (ADR 0036/0068) and already re-tunes
   volumes live under the master slider — the editor exposes
   `preview_fade_gain()` (= the same visibility product) and the app
   applies `prefs.volume × clip_gain × fade_gain` per frame while the
   Studio is open, voice and music sinks together. Playback quantizes the
   envelope to frame rate; the burn's `afade` is sample-accurate — the
   burn gate still rules the ear.
5. **Document/undo contract (ADR 0069).** The three fade fields
   (`fade_in_s`, `fade_out_s`, `fade_color`) are DOCUMENT state: they
   join `Snapshot` + `restore()`; the sliders coalesce on a `("fade",
   0)` burst (the style-knob pattern — one slider ride = one step); the
   color chips are verbs (one step each, refused-if-same). A missed
   membership is the P1 class ADR 0069 names.
6. **UI: a "Fade" block in the Properties panel** (between Framing and
   Caption presets — it is an output property, not a caption one), the
   Customize-captions card grammar: "Fade in" / "Fade out" sliders
   (0–3 s, 0.1 steps, 0 = off) + Black/White chips shown only while an
   edge is active. The export modal gains a "Fade" summary row when
   active (the intro/music precedent: the export's edges are never a
   surprise). Tooltip states the audio-preview honesty: preview fade is
   per-frame gain; the export's fade is sample-accurate.

## Considered and rejected

- **Fading inside `mix_music` / per-input `afade`** — fades each source
  before the mix, so a music clip entering during the fade-out window
  would fade on ITS own clock, not the Short's; the operator asked for
  the Short's edges. One wrap after the amix is the one-envelope truth.
- **Anchoring the fade-out on `-t` (`duration_s`)** — every razor-cut
  export would fade late or not at all (`-t` bounds; the concat output
  ends earlier). The `export_clock`-shaped anchor is decision 3.
- **An xfade-based end card / trailing black hold** — pointless without
  a next segment to transition INTO; a hold after the last frame just
  pads the Short with dead air. The end card is plan #5/#10 territory.
- **Audio-only or video-only fade toggles** — the operator's ask is one
  fade covering "music sound and video"; two independent envelopes is
  plan #10 configurability, not this slice.
- **Skipping the audio preview (visual ramp only, tooltip honesty)** —
  considered as the honest minimum, rejected because the sinks already
  re-tune live under the volume slider (the idiom exists; the multiplier
  is a few lines) and an audible hard-cut in preview where the burn
  fades IS the drift ADR 0036 forbids.

## Accepted trade-offs (recorded, not hidden)

- Overlapping edges on a very short output (in + out > total) apply
  both ramps multiplied — the middle never reaches full visibility.
  CapCut behaves the same; the sliders cap at 3 s each.
- The preview audio envelope is frame-rate-quantized and restarts
  mid-ramp re-tune within one frame (~16 ms) of sink spawn; inaudible in
  practice, and the burn is sample-accurate regardless.
- Scrubbed past the export's end (parked inside a removed tail), the
  canvas shows the fade's END state — those strip times are after the
  export's last frame, and showing "faded out" there is the honest
  reading.

## Bars (pre-registered before the build)

- Graph tests pin: the fade wraps AFTER the amix (both `[out]` and the
  mixed `[aout]`); the no-graph-audio path grows `[0:a]afade…[aout]`
  and the args flip to map it; the `c=white` variant; fade-in `st=0`
  over the intro wrap; **a razor-cut case where the fade-out `st`
  anchors on `intro + kept_total`, NOT the `-t` duration**; `fade:
  None` → graph and args byte-identical to today.
- Editor: the fade fields ride `Snapshot`/`restore` with a family test
  (slider burst = one step, chip = one step); `render_spec` carries
  `Some` only when an edge is non-zero; an old `project.json` without
  fade loads clean (serde-default pin).
- Preview: the ramp/gain anchor on the export clock — a razor-cut case
  pins that the ramp does NOT start over veiled spans and the intro
  case pins the fade-in over `[0..D)`.
- The burn gate (operator's eye AND ear, real export): video dims to
  black/white exactly as the preview showed; voice AND music die
  together, no hard cut at the end; fade-in from black over the
  thumbnail intro reads right; a razor-cut export fades at its REAL
  end; the white variant; a no-fade export byte-identical.

## Validation

`cargo test -p yt-clipper -p yc-core -p yc-render` green; clippy clean on
touched files; `--features face,align,ser` check; release build
(foreground); the burn gate stays with the operator.
