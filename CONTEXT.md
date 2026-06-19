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

**Segment**:
The padded span of full-quality VOD video downloaded for a Clip when it is promoted — bounded by keyframes, so always a little longer than the Clip's range. The frame-accurate export is cut from the Segment; the VOD's video is never downloaded whole (ADR 0001, ADR 0006).
_Avoid_: clip, chunk, section, source

**Layout**:
The arrangement of a Clip's 1080×1920 canvas. Two variants: stacked (gameplay Panel above facecam Panel, divided by the Seam) and full-frame (a single gameplay Panel).
_Avoid_: composite, template, frame

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
A named preset describing how captions look and animate (font, colors, outline, animation genre — rolling-pop, huge-word, karaoke fill). Saved per Creator, overridable per Clip.
_Avoid_: theme, template, skin

**Offline**:
The core constraint: all analysis and rendering happens on the local machine. The only permitted network use is user-initiated ingestion of a VOD.
_Avoid_: air-gapped, local-only (both overstate it — ingestion may use the network)
