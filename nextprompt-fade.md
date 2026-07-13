# Session prompt — fade in/out at the Short's edges: video and ALL audio together (operator pick 2026-07-13; plan #4's first slice)

The undo gate closed ("ok all good", ADR 0069 amendment) and the operator
picked the next arc in their own words: **"next we will work on the fade
out effect both in the music sound and video."** This is plan #4
(`feature-implementation-plan.md` — Transition Menu: Fade to Black / Fade
to White, configurable durations, "Transitions should affect both video
and audio"), sliced honestly: **fades at the SHORT'S EDGES** — a fade-in
from black/white at output 0 and a fade-out to black/white at the output
end, video and the whole mixed audio (voice + music) dying together. ONE
fade covering "music sound and video" is exactly the one-wrap idiom the
operator's ask implies. Cut-boundary dips (fade-out/HOLD/fade-in between
razor segments — plan #4's hold + plan #10's library) are the NAMED next
slice, not this one; per-music-clip edge fades (plan #6's future item)
likewise deferred until their ear asks.

## What EXISTS today (verified in code 2026-07-13 — do not re-guess)

- **Every export graph terminates in `[out]`** (video); audio terminates
  in `[aout]` ONLY when razor cuts, an intro, or music forced the audio
  through the graph — otherwise `export_args_inner` maps raw `0:a:0`
  (`audio_from_graph` in `crates/render/src/export.rs`). A fade wrap must
  therefore create the audio chain when none exists (`[0:a]afade=…[aout]`
  + force `audio_from_graph`) — the same `main_a` selection dance
  `mix_music` already does at export.rs:246-278.
- **Wrap order is burn → intro → amix, applied in `do_render`**
  (pipeline.rs:2341-2382, both the camera-script path and the plain
  path). The fade wraps LAST — around the finished graph, after
  `mix_music` — so the faded `[aout]` already contains the music. Same
  rename idiom: `[out]`→`[prefade]`, `[aout]`→`[preafade]`, re-terminate
  in `[out]`/`[aout]` so the arg mapping is untouched.
- **THE `-t` TRAP (the two-clocks lesson, third verse)**: `export_args`
  receives `range.duration_s()` — the FULL clip — even with razor cuts;
  `-t` only bounds, the razor concat output simply ENDS earlier. So the
  fade-out `st=` must anchor on the REAL output end: `intro_d +
  kept_total` — which is exactly `export_clock(out_dur, d_intro, keep)`
  applied at spec time (the pure function from ADR 0068 decision 3,
  editor.rs). Anchor on `-t` and every razor-cut export fades too late
  or not at all — the pre-registered graph test must pin a razor-cut
  case.
- **Preview parity surface** (ADR 0036: the preview cannot drift from
  the burn): the canvas draws output frames via `draw_output` /
  `draw_intro_frame` (editor.rs); the playhead runs in STRIP output time
  and playback SKIPS removed spans, so "export time remaining" ==
  intro-remaining + KEPT time remaining after `src_t()` — computable
  pure from `razor.kept_spans(dur)` without inverting `export_clock`.
  A black/white alpha ramp painted over the canvas (`ui.painter_at`)
  when export-time-remaining < fade_out_s (mirror for fade-in) shows the
  burn's truth, intro frames included. Preview AUDIO fade: the app owns
  every rodio sink (main.rs, ADR 0036/0068) and applies `prefs.volume ×
  gain` — a per-frame master multiplier ramp is feasible; decide there
  whether the slice ships it or the ear gates on the burn only (the
  honest minimum: visual ramp in preview, audio fade proven on the
  export — say so in the tooltip).
- **The document/undo contract (ADR 0069, one session old)**: fade
  settings are DOCUMENT state → they MUST join `Snapshot` +
  `restore()` + a push at their knobs (coalesced like the style knobs —
  the `("style", 0)` burst pattern) — a missed site is the P1 class the
  ADR names. Byte-identity bar: fade off → `Snapshot` unchanged graphs
  unchanged.
- **Persistence pattern**: `Clip` gains the fade fields serde-default +
  skip-if-empty (the `Clip.music` precedent — old project.json loads
  clean, pinned by test); `RenderSpec` carries them; headless/batch pass
  none → byte-identical args (pinned).

## The architecture the ask implies (decide honestly at grill/build)

1. **`yc_core::FadeSpec { in_s: f64, out_s: f64, color: FadeColor }`**
   (`FadeColor::Black | White`; 0.0 = that edge off; suggested range
   0–3 s, CapCut's feel). `RenderSpec.fade: Option<FadeSpec>` — `None`
   when both edges are 0 — and `Clip.fade` persisted.
2. **`yc_render::fade_edges(graph, fade, out_total_s, has_graph_audio)
   -> (String, bool /*audio now from graph*/)`** — one wrap, LAST:
   video `fade=t=in:st=0:d=IN:c=COLOR` + `fade=t=out:st=OUT_TOTAL-OUT:
   d=OUT:c=COLOR` chained on `[prefade]`; audio `afade=t=in` +
   `afade=t=out` (no color) on `[preafade]` or a fresh `[0:a]` chain.
   `fade: None` → graph BYTE-IDENTICAL, args byte-identical (the
   standing bar). `out_total_s` computed in `do_render` as
   `intro_d + kept_total` (see the `-t` trap above).
3. **UI**: a "Fade" block in the Properties panel (the Customize-
   captions card grammar): two sliders (Fade in / Fade out, 0–3 s,
   0 = off) + Black/White chips; export modal gains a summary row when
   active. Knob pushes coalesce (`("fade", 0)` burst); chips are verbs.
   The operator's eye rules the look (tofu rule for any glyph).
4. **Preview**: the canvas ramp (decision above) anchored on export-time
   -remaining/elapsed so razor cuts and the intro read correctly; the
   ramp draws OVER captions and safe-area guides (it is the outermost
   wrap in the burn too).
5. **Pre-register ADR 0070** with: the edges-only slice (hold + cut
   dips + per-clip music fades deferred, named), the wrap order, the
   `-t` trap and its `export_clock` anchor, the preview-parity decision
   (visual always; audio preview yes/no and why), the undo/document
   contract, byte-identity, and rejected alternatives (fading inside
   `mix_music`; anchoring on `-t`; a xfade-based end card; trailing
   black hold at the end — pointless without a next segment).

## Bars (pre-register in ADR 0070 BEFORE building)

- Graph tests pin: fade wraps AFTER amix (both `[out]` and the mixed
  `[aout]`); the no-graph-audio path grows `[0:a]afade…[aout]` and the
  args flip to map it; `c=white` variant; fade-in st=0 over the intro;
  **a razor-cut case where `st` anchors on `intro + kept_total`, NOT
  `-t`**; `fade: None` → graph and args byte-identical to today.
- Editor: fade fields ride `Snapshot`/`restore` with a family test
  (knob burst = one step, chip = one step); `render_spec` carries
  `Some` only when an edge is non-zero; old `project.json` without
  fade loads clean.
- Preview: the ramp appears exactly where the burn fades (eye check on
  a razor-cut clip — the ramp must NOT start over veiled spans).
- The burn gate (operator's eye AND ear, real export): video dims to
  black/white exactly as the preview showed; voice AND music die
  together, no hard cut at the end; fade-in from black over the
  thumbnail intro reads right; a razor-cut export fades at its REAL
  end; the white variant; a no-fade export byte-identical.

## The ritual

/grill-with-docs UNLESS the operator says "automatic" — then decide by
this file, pre-register ADR 0070, build, keep the ADR 0066 shell grammar
+ ADR 0067 output-time contract + ADR 0068 music grammar + ADR 0069 undo
contract intact. Release builds FOREGROUND. Validate: cargo test -p
yt-clipper -p yc-core -p yc-render, clippy on touched files,
`"--features" "face,align,ser"` check, `scripts\build-release.bat`, the
operator's eye+ear on the burn gate. Standing rules: quality over
runtime, PS 5.1 quoting, commit via `git commit -F <file>`, captions
never corrected via store JSON, AI and operator artifacts never share a
track. Finish: handoff + whatwedone.md + fresh nextprompt + the starter
line. (`nextprompt-title-gen.md` stays queued behind this arc.)
