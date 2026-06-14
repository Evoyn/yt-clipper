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

**Clip**:
A Moment the operator has promoted for production. A Clip gets framing, captions, and an export; a Moment that is never promoted gets nothing.
_Avoid_: short, video, segment

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
