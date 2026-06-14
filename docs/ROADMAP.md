# Roadmap

Milestones build tracer-bullet style: M1 threads every subsystem at minimum depth; later milestones deepen one subsystem each. Order confirmed 2026-06-11.

- **M0 — Skeleton** ✅ (2026-06-11): cargo workspace (`app` egui shell on egui 0.34, `core` domain, `ingest`, `transcribe`, `detect`, `render`), hello-window builds and launches, domain round-trip test green. Outstanding: operator must run `scripts/fetch-sidecars.ps1` (downloads gated behind operator approval).
- **M1 — Tracer bullet**: local file → audio extract → whisper transcript → manually picked time range → default stacked Layout (hardcoded Crops) → one caption preset → NVENC export. First real clip.
- **M2 — YouTube ingest**: audio-only download + chat-replay JSON at import; padded segment download when a Moment is promoted (ADR 0001).
- **M3 — Detection ensemble**: chat-rate spikes + loudness + excitement lexicon → scored Moments; review UI = waveform + transcript + audio playback (ADR 0002).
- **M4 — LLM rerank**: llama.cpp GGUF stage, sequential VRAM staging after Whisper unloads (ADR 0002).
- **M5 — Framing editor**: draggable Crops, Seam, single-panel fallback, live approximation + debounced true preview (ADR 0005).
- **M6 — Caption Styles**: rolling-pop, huge-word, karaoke-fill presets; per-Creator defaults; JA chunk grouping (ADR 0003/0004).
- **M7 — Hardening**: job-queue UX, error states, settings, packaging.

## Standing constraints

- Offline (see CONTEXT.md): network only for user-initiated ingestion.
- Finished VODs only — no live-stream clipping in v1.
- Single 8 GB RTX 3070 Ti: GPU stages strictly sequential, never concurrent.
- Languages: English, Bahasa Indonesia, Japanese end-to-end (transcription → captions).
- Persistence: folder-per-VOD with JSON files + global `creators.json`; no database.
