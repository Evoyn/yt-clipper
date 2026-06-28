# YT Clipper

A Windows-first desktop app that turns long gaming VODs into vertical short-form clips — transcription, moment detection, framing, captioning, and export all happen on the user's own machine.

## Language

**Creator**:
A streamer whose VODs the operator clips with their permission. A Creator carries saved defaults (facecam layout, caption style, spoken language) that apply to every VOD of theirs.
_Avoid_: channel, streamer, user

**VOD**:
The long source recording a project is built around — a finished stream recording on YouTube, or a local video file. Always belongs to a Creator.
_Avoid_: stream, video, source

**Moment**:
A scored candidate time range within a VOD, surfaced by analysis (or marked manually by the operator), awaiting review.
_Avoid_: highlight, candidate, event

**Signal**:
One strand of evidence behind a Moment's score — chat-rate, loudness, lexicon, arousal, or LLM judgment — kept separate (never blended away) so the ranking can be retuned and the operator can see *why* a Moment surfaced.
_Avoid_: feature, metric, factor

**Arousal**:
The Signal measuring the emotional *activation* of the streamer's voice — how worked-up they sound — from a speech-emotion model's arousal axis. Deliberately ignores *which* emotion: a laugh, a rage, and a hype-moment all score high (all clip-worthy). Its job is to tell an emotional reaction apart from merely loud audio (a game explosion, music, a cutscene) that loudness alone cannot.
_Avoid_: sentiment / valence (positive-vs-negative — explicitly not measured), excitement (reserved for the lexicon), emotion (too broad — implies classifying which emotion)

**LLM judgment**:
The Signal from a local language model reading a Moment's transcript: a judgment of how clip-worthy the *streamer's* speech is, scored per-candidate and kept relative to the others. Unlike the lexicon (which counts excitement words), it weighs meaning in context — and it is explicitly told to discount scripted in-game dialogue and cutscene narration, so a dramatic game line is not clip-worthy unless the streamer reacts to it. Runs locally on the GPU after transcription unloads; no cloud.
_Avoid_: rerank (it is a Signal in the ensemble, not a post-sort stage), GPT / cloud (it is a local GGUF), sentiment (it judges clip-worthiness, not positive-vs-negative)

**Clip**:
A Moment the operator has promoted for production. A Clip gets framing, captions, and an export; a Moment that is never promoted gets nothing.
_Avoid_: short, video, segment

**Title**:
A catchy ≤60-character short-form title generated for a Moment by the LLM judge at detect time (it already reads the transcript — ADR 0010/0015), stored on the Moment and used to name the rendered Short (`<title>.mp4`) so a re-promote never overwrites a previous one. A manually-marked Moment has no generated title; the render then names the Short by timestamp (`clip-<m-ss>`). Distinct from the VOD's own `title` (the stream's name, which names the **stream folder**).
_Avoid_: caption (that is the on-screen burned text), filename, label

**Stream folder**:
A VOD's output directory, `workspace/<creator>/<stream-title>/` (sanitized from VOD metadata — ADR 0015). The rendered Shorts (named by their generated Title) sit at its root; every intermediate (analysis audio, downloaded Segments, `project.json`, captions, font) lives under its `data/` subfolder. It realizes Creator scoping on disk; the **Creator store** (`workspace/creators.json`) holds the matching per-Creator defaults.
_Avoid_: workspace (that is the parent holding every stream folder), project folder

**Creator store**:
The global `workspace/creators.json` (`core::CreatorStore`) of per-Creator remembered defaults, keyed by Creator name (ADR 0016) — the long-deferred `creators.json`. Today it remembers a Creator's **Caption Style**: importing a known Creator's VOD seeds the render's genre to their last-used one, and each render saves it back (operator overrides always win). Language is recorded but not yet applied; seam/crop defaults are reserved for a later Creator-aware-framing slice.
_Avoid_: project.json (that is per-VOD, under the stream folder's `data/`), dialect store (that is per-language transcription corrections)

**Segment**:
The padded span of full-quality VOD video downloaded for a Clip when it is promoted — bounded by keyframes, so always a little longer than the Clip's range. The frame-accurate export is cut from the Segment; the VOD's video is never downloaded whole (ADR 0001, ADR 0006).
_Avoid_: clip, chunk, section, source

**Vocal stem**:
The streamer's voice track separated from a Segment's audio — music and game SFX stripped out by a source-separation model. It isolates *voice from music/SFX*, **not one speaker from another**: an in-game NPC or cutscene voice survives it (telling those apart is a separate, future concern). Produced per-Clip behind the `sep` feature. **Rejected for captions (ADR 0014)**: htdemucs is a music separator and drops real spoken speech, so the export captions the mixed audio; the validated runtime is kept off-by-default for a possible future arousal-discovery use.
_Avoid_: mic track / mic (it is not literally the streamer's microphone feed — any voice in the mix survives), denoised audio (it removes music/SFX, not just background noise)

**Dialect store**:
A curatable per-language file (`assets/dialect/<lang>.json`, the `DialectLexicon`) that lifts the caption-transcription ceiling for a Creator's accent / slang / viewer names — the *linguistic* error that no audio processing fixes (ADR 0014). It drives a deterministic `wrong → right` correction dict (always on), opt-in whisper priming (`prime`, off — it drifts), and **auto-harvest**: words whisper was unsure of that aren't in the bundled real-word dictionary are appended as `unverified` to-dos each caption run, so the operator's review queue self-populates and the dict improves over time.
_Avoid_: lexicon (overloaded — that is the excitement-word list for detection scoring), dictionary (that is the bundled real-word wordlist the harvest filters against, not the corrections)

**Layout**:
The arrangement of a Clip's 1080×1920 canvas. Two variants: stacked (gameplay Panel above facecam Panel, divided by the Seam) and full-frame (a single Panel filling the canvas — the gameplay, or the Facecam alone during a talking-session moment where the streamer's cam is the content). Chosen per Clip by M6 auto-detect (ADR 0011) unless the operator's **Layout preference** forces a specific one.
_Avoid_: composite, template, frame

**Layout preference**:
The operator's explicit choice of which Layout to frame a Clip with (ADR 0017), overriding M6 auto-detect: _auto_ (the ADR 0011 three-way decision), or a forced _stacked_ / _full cam_ / _full gameplay_. A forced choice still uses the detected Facecam Crop when one is found, else a sensible seed. A global session/per-invocation setting (GUI top-bar menu + a `--batch`/`--headless` CLI token), defaulting to _auto_; persisting it per-Creator is a later Creator-aware-framing slice (ADR 0016). It exists because the operator's preferred stacked framing must be selectable in `--batch`, which never opens the nudge editor.
_Avoid_: layout (that is the realized arrangement; this is the operator's pick of it), template

**Facecam**:
The region of the source video showing the streamer's webcam overlay. Its location and size are not fixed across a VOD — a small corner inset during gameplay, but the streamer may switch to a full-screen cam during a talking session — so it is detected per Clip from the promoted Segment's frames, not assumed (M6, ADR 0011). The detected Facecam seeds the facecam Panel's Crop in a stacked Layout, or the whole canvas in a full-frame talking-session Layout.
_Avoid_: webcam (the hardware), facecam Panel (that is the canvas region it is shown in, not the source region)

**Panel**:
A region of the canvas that displays exactly one Crop of the VOD, filled edge-to-edge — never letterboxed, never stretched.
_Avoid_: slot, zone, view

**Seam**:
The horizontal boundary between the gameplay and facecam Panels in a stacked Layout. Its position is per-Clip, defaulted from the Creator.
_Avoid_: split, divider

**Crop**:
The rectangle of source-video pixels a Panel displays. Aspect-locked to its Panel: resizing zooms, dragging pans.
_Avoid_: selection, region, window

**Caption Style**:
A named preset describing how captions look and animate (font, colors, outline, animation genre — rolling-pop, huge-word, karaoke). Saved per Creator, overridable per Clip. The karaoke genre (enum `KaraokeFill`) highlights **per word as a whole, snapping** to the accent colour at each word's spoken onset (ASS `\k`, cumulative — ADR 0018), not a left-to-right fill.
_Avoid_: theme, template, skin

**Offline**:
The core constraint: all analysis and rendering happens on the local machine. The only permitted network use is user-initiated ingestion of a VOD.
_Avoid_: air-gapped, local-only (both overstate it — ingestion may use the network)
