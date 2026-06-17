# Roadmap

Milestones build tracer-bullet style: M1 threads every subsystem at minimum depth; later milestones deepen one subsystem each. Order confirmed 2026-06-11.

- **M0 — Skeleton** ✅ (2026-06-11): cargo workspace (`app` egui shell on egui 0.34, `core` domain, `ingest`, `transcribe`, `detect`, `render`), hello-window builds and launches, domain round-trip test green. Outstanding: operator must run `scripts/fetch-sidecars.ps1` (downloads gated behind operator approval).
- **M1 — Tracer bullet** ✅ (2026-06-17): local file → audio extract → whisper transcript → manually picked time range → default stacked Layout (hardcoded Crops) → one caption preset → NVENC export. First real clip rendered end-to-end (`workspace/clip/export.mp4`, 1080×1920) on a YouTube-pulled 360p segment (1080p deferred to M2 — see below). Added a `--headless <video> <start_s> <end_s> [en|id|ja]` app mode for render iteration without GUI clicking. GUI builds; interactive path not yet exercised. Post-render polish: fixed overlapping captions (end-cap each line at the next line's start) + line-break on >1 s silence, and made the transcription **language operator-selectable** (En/Id/Ja, UI dropdown + headless arg). **Known limitation:** whisper transcribes the loudest speech in the single mixed audio track — it does NOT isolate the streamer's mic from in-game voices, so clips with prominent game dialogue caption the *game* (the 360p test clip did). True streamer-only captions need a vocal-separation pre-step (e.g. an `ort`-hosted source-separation model) — future work, affects detection quality too.
- **M2 — YouTube ingest**: audio-only download + chat-replay JSON at import; padded segment download when a Moment is promoted (ADR 0001). _Ingest reality (found in M1): yt-dlp needs a JS runtime (deno) on PATH or it 403s; its native downloader works but the ffmpeg `--download-sections` path hangs with no read-timeout, and googlevideo throttles the DASH 1080p stream from some regions — so "download only the padded segment at full quality" is the real engineering problem here, not a given._
- **M3 — Detection ensemble**: chat-rate spikes + loudness + excitement lexicon → scored Moments; review UI = waveform + transcript + audio playback (ADR 0002).
- **M4 — LLM rerank**: llama.cpp GGUF stage, sequential VRAM staging after Whisper unloads (ADR 0002).
- **M5 — Framing editor**: draggable Crops, Seam, single-panel fallback, live approximation + debounced true preview (ADR 0005). _Candidate enhancement (operator request, M1): auto-detect the facecam rectangle — sample frames, run a small ONNX face model via `ort` (e.g. YuNet; not OpenCV), cluster the boxes to locate the static webcam overlay once, and seed the Creator's saved facecam default. Auto-suggest, operator drag-adjusts._
- **M6 — Caption Styles**: rolling-pop, huge-word, karaoke-fill presets; per-Creator defaults; JA chunk grouping (ADR 0003/0004).
- **M7 — Hardening**: job-queue UX, error states, settings, packaging.

## Standing constraints

- Offline (see CONTEXT.md): network only for user-initiated ingestion.
- Finished VODs only — no live-stream clipping in v1.
- Single 8 GB RTX 3070 Ti: GPU stages strictly sequential, never concurrent.
- Languages: English, Bahasa Indonesia, Japanese end-to-end (transcription → captions).
- Persistence: folder-per-VOD with JSON files + global `creators.json`; no database.
