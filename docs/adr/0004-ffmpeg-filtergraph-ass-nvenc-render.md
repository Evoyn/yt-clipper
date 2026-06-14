# Render path: ffmpeg filtergraph + ASS caption burn + NVENC

A Clip's Layout and Caption Style must become a 1080×1920 MP4 on an 8 GB RTX 3070 Ti. We chose a single bundled ffmpeg sidecar doing everything in one filtergraph — crop/scale/vstack the Panels, burn captions with libass from a generated ASS file, encode with h264_nvenc — rejecting a custom GPU compositor (a mini video engine before the first clip ships) and a hybrid Rust-rendered caption overlay (custom JA text shaping for marginal v1 gain).

## Consequences

- Caption expressiveness is capped at what ASS tags can do: per-word/chunk pop, scale, color, karaoke fill, outline, shadow, fonts — but no emoji and no spring-physics easing. All three v1 Caption Style presets fit inside this ceiling. Revisit (hybrid overlay) only when a wanted style provably doesn't fit.
- ~~The webview framing UI is a simulation (CSS-clipped video + DOM captions), not ground truth. A one-click low-res true-preview render (same filtergraph, scaled down) exists precisely because the simulation may differ from libass output.~~ Superseded by ADR 0005: the UI is pure Rust and the preview runs the real filtergraph directly — no simulation gap.
- ffmpeg.exe and yt-dlp.exe ship as pinned Tauri sidecars; no system-installed dependencies.
- The ASS generator is the single place caption animation logic lives; caption presets are data (style parameters), not code paths.
