# yt-clipper

Windows-first desktop app that turns long gaming VODs into vertical (9:16) short-form clips — transcription, moment detection, framing, animated captions, and NVENC export, all on the local machine. Pure Rust (egui), no web stack.

Start here:

- [CONTEXT.md](CONTEXT.md) — the domain language. Code mirrors these terms.
- [docs/adr/](docs/adr/) — why the architecture is the way it is (read 0001 first).
- [docs/ROADMAP.md](docs/ROADMAP.md) — milestone plan and standing constraints.

## Building

Prerequisites: Rust (MSVC toolchain), CMake, CUDA Toolkit (for whisper.cpp), an NVIDIA GPU.

```powershell
# one-time: fetch pinned ffmpeg + yt-dlp into /sidecars
powershell -ExecutionPolicy Bypass -File scripts/fetch-sidecars.ps1

cargo run -p yt-clipper
```

Whisper/LLM model files live in `/models` (fetch script lands with M1).
