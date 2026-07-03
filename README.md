<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/branding/logo.svg">
    <img src="assets/branding/logo-light.svg" width="180" alt="yt-clipper — a vertical Short punched out of a wide VOD frame">
  </picture>
</p>

<h1 align="center">YT&nbsp;CLIPPER</h1>

<p align="center"><em>Turn long VODs into vertical Shorts — entirely on your machine.</em></p>

Windows-first desktop app that turns long VODs — gaming streams and podcasts — into vertical (9:16) short-form clips: transcription, moment detection, framing, speaker tracking, animated captions, and NVENC export, with zero cloud calls. Pure Rust (egui), no web stack.

## What it does

- **Finds the Moments.** A heuristic ensemble (chat-replay spikes, loudness, speech-emotion arousal, excitement lexicon) plus a local LLM judge scores candidate Moments across the whole VOD; every signal stays inspectable per Moment. Detected clips start and end on sentence boundaries, aiming for 45–180 s of real speech.
- **Opens a Studio, not a dialog.** Promoting a Moment opens a full-window editor: editable transcript (burned verbatim), Before/After preview with a WYSIWYG caption overlay, drag/resize caption placement, camera and framing tools, timeline with speaker lanes.
- **Cuts podcasts like an editor.** Podcast Mode tracks every visible face, attributes speech per time bin, and derives a cut-based Active-Speaker camera plan — solo shots, split-screen group shots, no amateur panning.
- **Captions like a human typed them.** whisper large-v3 (CUDA) or the Qwen3-ASR five-variant ensemble, per-Creator dialect stores that learn a streamer's slang and viewer names, a self-populating review queue, and an optional local-LLM correction pass.
- **Renders once, fast.** ffmpeg filtergraph + ASS subtitles + NVENC; karaoke, huge-word, and rolling caption styles in the bundled Anton face.
- **Heals itself offline-first.** The Diagnostics registry lists every tool and model the pipeline resolves; anything missing downloads in-app from its pinned official source, SHA-256-verified. The only network uses are user-initiated: ingesting a VOD and fetching a missing dependency.

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
# build/run through the env wrapper (or run plain `cargo` from a Developer PowerShell for VS)
scripts\cargo-cuda.bat run -p yt-clipper

# release build (includes the yc-llm-judge sidecar + face feature)
scripts\build-release.bat
```

Missing tools and models can be fetched from inside the app (**Diagnostics → Download**), or ahead of time with the scripts:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/fetch-sidecars.ps1   # ffmpeg, yt-dlp, deno, deep-filter
powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1     # whisper large-v3 (~3.1 GB), Silero VAD, Qwen3-ASR
powershell -ExecutionPolicy Bypass -File scripts/fetch-llama-sidecar.ps1  # pinned llama.cpp (ensemble decoder)
```

Model files live in `/models`, sidecar binaries in `/sidecars`, bundled fonts + the brand set in `/assets`, per-VOD working data in `/workspace` (all gitignored except `/assets`).
