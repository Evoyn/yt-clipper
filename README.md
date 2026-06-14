# yt-clipper

Windows-first desktop app that turns long gaming VODs into vertical (9:16) short-form clips — transcription, moment detection, framing, animated captions, and NVENC export, all on the local machine. Pure Rust (egui), no web stack.

Start here:

- [CONTEXT.md](CONTEXT.md) — the domain language. Code mirrors these terms.
- [docs/adr/](docs/adr/) — why the architecture is the way it is (read 0001 first).
- [docs/ROADMAP.md](docs/ROADMAP.md) — milestone plan and standing constraints.

## Building

Prerequisites (Windows, MSVC):

- Rust (MSVC toolchain), CMake, and the **CUDA Toolkit** — whisper.cpp's CUDA backend is compiled from source (verified against CUDA 13.3, compute 8.6).
- **LLVM**, for `libclang` — whisper-rs runs `bindgen` at build time, and its bundled bindings are Linux-only, so a Windows build must generate its own. Install with `winget install LLVM.LLVM`; the build looks for `libclang.dll` (set `LIBCLANG_PATH` if it lands somewhere non-standard). Note: do **not** set `WHISPER_DONT_GENERATE_BINDINGS` on Windows — the bundled bindings won't compile here.
- An NVIDIA GPU.

The whisper crate compiles inside an MSVC + CUDA environment. The simplest path is the bundled wrapper, which discovers VS, CUDA, and LLVM and sets the env for you:

```powershell
# one-time: fetch pinned ffmpeg + yt-dlp into /sidecars, and the whisper model into /models
powershell -ExecutionPolicy Bypass -File scripts/fetch-sidecars.ps1
powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1   # ggml-large-v3.bin (~3.1 GB)

# build/run through the env wrapper (or run plain `cargo` from a Developer PowerShell for VS)
scripts\cargo-cuda.bat run -p yt-clipper
```

Model files live in `/models`, bundled fonts in `/assets`, per-VOD working data in `/workspace` (all gitignored except `/assets`).
