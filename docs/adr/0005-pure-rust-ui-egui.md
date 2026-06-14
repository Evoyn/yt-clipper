# Pure-Rust UI: egui/eframe replaces the Tauri webview shell

The original pitch was a Tauri shell, but the operator wants no JS ecosystem in the project. We chose egui/eframe for the entire UI — one Rust workspace, no npm, no webview. This is viable specifically because of ADR 0001: the UI never scrubs a full VOD, so the webview's free `<video>` element buys little. The app only plays the analysis audio (native decode) and short downloaded segments (ffmpeg rawvideo piped to a texture).

## Consequences

- Preview is the real pipeline: the framing editor's true preview runs the actual ffmpeg+libass filtergraph at reduced resolution and pipes frames to a texture — exact WYSIWYG with the export, no simulation gap. Live crop-dragging shows a cheap egui-drawn approximation that the true preview replaces on a ~200 ms debounce. (Supersedes ADR 0004's webview-simulation consequence.)
- We own A/V playback plumbing: ffmpeg-process frame piping, rodio/cpal audio, manual sync clock, plus every widget (waveform, transcript list, timeline, crop editor) hand-rolled in egui.
- Aesthetics are utilitarian rather than web-glossy; CJK display needs bundled fonts (Noto Sans JP); IME-dependent text input is weaker than a webview — acceptable for a solo operator tool.
- Distribution becomes a single exe plus a sidecar/models folder; no installer-grade web runtime concerns.
