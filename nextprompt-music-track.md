# Session prompt — background music track (plan #6; operator queue-jump 2026-07-13)

The operator queued plan #6 next (their `feature-implementation-plan.md`,
THEIR file): **import music; drag, trim, split and delete music; support
multiple clips** — future-READY (not built now) for volume keyframes,
fade in/out, crossfades, multiple audio tracks. This is the arc ADR 0066
decision 1 pre-registered: **the Music track arrives WITH its render
machinery** — the amix IS this slice, so Music 1 may finally appear.
Read ADR 0066 + ADR 0067 first: the intro arc built the output-time
timeline and the wrap-the-finished-graph pattern this arc composes with.
(`nextprompt-title-gen.md` re-queued behind this again — operator jump.)

## What EXISTS today (verified in code 2026-07-13 — do not re-guess)

- **Export audio** (`crates/render/src/export.rs`): the args map `0:a:0`
  raw, or the graph's `[aout]` (razor concat, and/or `prepend_intro` —
  ADR 0067 — which wraps the FINISHED graph by renaming its terminal
  labels and re-terminating `[out]`/`[aout]`; each label appears exactly
  once, test-pinned). Music is the same move one layer further out:
  wrap AFTER everything (intro included), rename the terminal `[aout]`,
  amix, re-terminate. The output `-t` bound already caps length.
- **The timeline runs in OUTPUT time** (ADR 0067): playhead/ruler span
  `intro + clip`; source-anchored artifacts convert through ONE offset
  (`s_to_x`/`x_to_s`); `restart_playback(out_t)` (editor.rs) is the ONE
  seek/scrub/jump contract every restart flows through. Music clips
  anchor in OUTPUT time BY CONSTRUCTION: they may sit over the thumbnail
  intro, and a razor edit does NOT move them (CapCut semantics — the
  content shifts under them; their data never rewrites).
- **Preview audio** (`main.rs` ~2429–2475): rodio 0.19, ONE OutputStream
  + ONE `Sink`; `play_range` slices analysis.wav (mono, 16 kHz
  `WHISPER_SR`) into a `SamplesBuffer`; `EditorAction::Play(TimeRange)`
  is the app boundary; `stop_audio` kills the sink; master volume per
  sink. rodio resamples per-source to the device rate, so music may
  decode at 44.1k stereo beside the 16k voice. **The decoder is the
  pinned ffmpeg** (the ADR 0067 no-image-crate rule, audio edition):
  decode music to PCM via ffmpeg (`-f s16le`), feed `SamplesBuffer` —
  ffmpeg covers mp3/wav/flac/ogg/m4a for free; add NO codec crates.
  `rodio::Source::delay` exists for future-start cues.
- **Probing**: `yc_ingest::probe_segment(&ffprobe, …)` returns
  `duration_s` BUT its `parse_probe` requires width/height — it will
  FAIL on audio-only files. A music-duration probe needs a small
  audio-tolerant variant (`-show_entries format=duration`; pure parser,
  unit-tested like `parse_probe`). `AppPaths::ffprobe()` exists
  (main.rs:411); the editor already carries `ffmpeg` via `from_seed` —
  thread `ffprobe` the same way.
- **Track shell** (ADR 0066): header cards + per-kind honest toggles;
  accents INFO/GOLD/OK/gray in theme.rs (music wants its own const —
  CapCut's music teal/violet family; operator eye decides). The
  Speakers zone appears only when analysis exists — the precedent for
  "Music 1 appears when ≥ 1 clip exists" (an empty lane is dead space;
  the `+ Music` button lives in the transport always, beside
  `+ Thumbnail`, rfd picker pattern).
- **Gesture model**: the caption blocks (editor.rs) — pointer-tracked
  `TimelineDrag` in TIME, body drag + edge trims + right-click menu,
  magnet snapping through the visible span. Mirror them.
- **Persistence precedent**: `Clip.thumbnail` (ADR 0067) — serde-default
  contents on the Clip; `RenderSpec`/`Job::Render` param with
  headless/batch passing the empty value, byte-identical and test-pinned.

## The architecture the plan implies (decide honestly on "automatic")

1. **Render**: music files ride as extra `-i` inputs AFTER the optional
   intro image (input base shifts — pin it in a test). New
   `mix_music(graph, clips, input_base) -> String`: rename terminal
   `[aout]`→`[premix]` (or wrap the raw `0:a:0` case first — with music
   the audio ALWAYS comes from the graph), per clip
   `[k:a]atrim=in:out,asetpts=PTS-STARTPTS,volume=g,adelay=pos_ms|pos_ms[mN]`,
   then `[premix][m0]…amix=inputs=N+1:duration=first:normalize=0[aout]`.
   `duration=first` + the `-t` cap keep output length = the video's;
   music never extends a Short. The ASS burn is untouched by
   construction (same bar as ADR 0067).
2. **Schema**: `yc_core::MusicClip { path, at_s (OUTPUT time), in_s,
   out_s (source trim), gain (1.0 default) }`; `Clip.music:
   Vec<MusicClip>` serde-default. Future-ready = additive: keyframes
   later sit beside the constant gain; fades = `afade` on clip edges;
   crossfade = the deliberate overlap verb; multiple audio tracks = the
   ADR 0066 decision-6 `Vec<Track>` promotion (NOT now — the operator
   still can't add/remove/reorder tracks, so named statics stay honest).
3. **Timeline**: Music 1 lane + card when clips exist. Blocks drag in
   output time, edges trim (clamped to neighbors, `in_s ≥ 0`,
   `out_s ≤ probed duration`), right-click: Split at playhead (two
   clips, source-continuous at the cut), Delete, gain control (one
   constant slider — keyframes are the future item; recommend the
   right-click popup, not a new panel). **No overlap on the one track**
   (clamp drags against neighbors) — crossfades arrive WITH overlap
   semantics later, never as an accident now.
   **The selected-clip cut grammar** (operator Q 2026-07-13, "how do i
   cut the music if my razor cannot cut it?" — the CapCut answer):
   clicking a music block SELECTS it, and while one is selected the
   transport verbs act on IT — ✂⏴ trims the selected clip's left side
   to the playhead, ⏵✂ its right side, Delete removes it, and the strip
   menu over the music lane offers Split here — with tooltips saying so.
   Nothing selected = the verbs act on the main timeline exactly as
   today (and the MAIN razor NEVER cuts music — removing video time
   must not secretly chop the music; that is WHY music is
   output-anchored). Esc / clicking empty strip deselects.
4. **Per-kind honest toggles**: Music gets **mute** (real THIS arc: the
   muted track leaves the amix AND the preview together — the first 🔇
   whose OFF state is true) + **lock** (gestures ignored); NO eye
   (nothing visual). Video 1's mute STAYS deferred to the mixer arc
   (#7) — do not scope-creep it in.
5. **Preview**: on every `restart_playback(out_t)`, the editor computes
   the cues intersecting `[out_t ..]` and hands them to the app —
   recommend `EditorAction::Play` growing a `Vec<MusicCue>` (path,
   source in/dur, delay from now, gain). The app spawns one sink per
   cue (ffmpeg-decoded PCM + `Source::delay`), tracked in a
   `Vec<rodio::Sink>`; `stop_audio` clears them all; volume =
   `prefs.volume × gain`. The same restart contract as today — scrub
   across a music boundary must not double-start anything.
6. **RenderSpec/Job::Render** grow `music: Vec<MusicClip>`;
   headless/batch pass `Vec::new()` — untouched, test-pinned. Export
   modal grows a "Music · N clips" row. Missing file at render = fail
   loudly with the path (the ADR 0067 rule).

## Bars (pre-register in ADR 0068 BEFORE building)

- Filtergraph: ONE amix, applied after the burn AND after the intro
  wrap; `subtitles=` count stays 1; intro + razor + camera + music
  compose in one graph; input indices correct with and without an
  intro; `music: []` → graph and args BYTE-IDENTICAL to today.
- Editor: razor edits never rewrite MusicClip data (output anchoring);
  trim clamps against neighbors and the probed duration; split
  produces two source-continuous clips; mute empties the spec's music
  (the ADR 0066 eye pattern); spec carries music only when clips exist;
  the cut verbs ROUTE by selection (music block selected → they edit
  that clip; none selected → the main razor, byte-identical to today).
- Probe: the audio-duration parser unit-tested pure (an mp3 with no
  video stream must probe; `parse_probe` is pinned to fail there).
- The burn gate (the operator's EAR, on a real export): music enters
  exactly where the strip shows it (over the intro if placed there),
  trims/splits land, gain audible as set, speech still legible, output
  ends with the video. Gate on the burn, not the preview.

## The ritual

/grill-with-docs UNLESS the operator says "automatic" — then decide by
this file + the plan text, pre-register ADR 0068, build, keep the ADR
0066 shell grammar + ADR 0067 output-time contract intact. Release
builds FOREGROUND. Validate: cargo test -p yt-clipper -p yc-core -p
yc-render, clippy on touched files, `"--features" "face,align,ser"`
check, `scripts\build-release.bat`, operator's ear on the burn.
Standing rules: quality over runtime, PS 5.1 quoting, commit via `git
commit -F <file>`, captions never corrected via store JSON, AI and
operator artifacts never share a track. Finish: handoff + whatwedone.md
+ fresh nextprompt + the starter line.
(`nextprompt-title-gen.md` stays queued behind this arc.)
