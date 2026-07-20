# User Guide

Everything from a fresh checkout to an exported Short. The [README](README.md) has the elevator pitch; [ARCHITECTURE.md](ARCHITECTURE.md) explains how the pieces fit; [CONTEXT.md](CONTEXT.md) defines every capitalized term used here (Moment, Clip, Creator, Layout, …).

## Contents

1. [Requirements](#requirements)
2. [Build](#build)
3. [Tools and models](#tools-and-models)
4. [Your first Short](#your-first-short)
5. [The Studio editor](#the-studio-editor)
6. [Caption engines and curation](#caption-engines-and-curation)
7. [Per-Creator memory](#per-creator-memory)
8. [Headless CLI](#headless-cli)
9. [Environment variables](#environment-variables)
10. [Troubleshooting](#troubleshooting)

## Requirements

- **Windows** with an **NVIDIA GPU** (transcription is whisper.cpp CUDA; export is NVENC). Reference machine — everything was developed and tested on it: **Lenovo Legion 5i Pro 16IAH7H** (Intel Core i7-12700H, NVIDIA RTX 3070 Ti 8 GB, 16 GB RAM, 1 TB NVMe M.2 SSD). GPU stages run strictly sequentially, so 8 GB of VRAM is enough — but see the GPU-sharing note in [Troubleshooting](#troubleshooting).
- **Rust** (MSVC toolchain), **CMake**, the **CUDA Toolkit** (verified: 13.3, compute 8.6), and **LLVM** (`winget install LLVM.LLVM`) for `libclang` — whisper-rs generates its bindings at build time on Windows.
- Disk: the required whisper model is ~3.1 GB; the optional LLM judge is ~5.4 GB; everything else is small-to-moderate. Per-VOD working data (audio, segments, exports) lives under `workspace/`.

## Build

```powershell
# dev run (the wrapper discovers Visual Studio, CUDA, and LLVM, then calls cargo)
scripts\cargo-cuda.bat run -p yt-clipper

# production build: app + yc-llm-judge sidecar, thin-LTO, with face,align,ser
scripts\build-release.bat
```

The release binaries land in `target\release\` (`yt-clipper.exe` + `yc-llm-judge.exe` — the judge sidecar must sit beside the app exe, which it does there). The app resolves `sidecars/`, `models/`, `assets/`, and `workspace/` by walking up from the exe to the repo root, so you can launch `target\release\yt-clipper.exe` directly.

Build features (see the [README table](README.md#build-features)): the default build compiles without any ONNX Runtime; `face`, `ser`, and `align` are what `build-release.bat` ships, and each degrades gracefully at runtime if its model file is missing.

## Tools and models

The app shells out to pinned sidecar binaries and loads local models. **Diagnostics** (bottom of the import rail) lists every one with a green/red dot, its resolved path, and — for anything missing — a **Download** button wired to a pinned official source, SHA-256-verified. Nothing optional blocks the pipeline; each absence just switches its capability off:

| Dependency | Kind | Needed for | Without it |
| --- | --- | --- | --- |
| `ffmpeg.exe` / `ffprobe.exe` | sidecar | everything (extract, probe, render) | core flows fail — required |
| `yt-dlp.exe` | sidecar | YouTube import + segment fetch | local files only |
| `deno.exe` | sidecar (or winget install) | yt-dlp's YouTube signature solving | YouTube downloads 403 |
| `ggml-large-v3.bin` | model (~3.1 GB) | all transcription | no captions, no refine detection — required |
| `yc-llm-judge.exe` | sidecar (built with the app) | LLM judgment Signal + titles + correction pass | detection runs without the `llm` Signal |
| `qwen2.5-7b-instruct-q5_k_m.gguf` | model (~5.4 GB) | the LLM judge's brain | same as above |
| `w2v2-emotion/model.onnx` | model (`ser` builds) | arousal Signal | loud-but-flat moments rank blind |
| `version-RFB-320.onnx` (Ultraface) | model (`face` builds) | facecam auto-detect framing | full-frame gameplay fallback |
| CAM++ speaker embedding | model (`face` builds) | voice diarization lane | speaker analysis is mouth-only |
| YuNet + SFace | models (`face` builds) | face re-id occupant map | voice join stays seat-scoped |
| zipformer audio tagger + labels CSV | model (`face` builds) | shared-reaction (laughter) detection | reaction split-screens never arm |
| `llama-mtmd-cli.exe` + Qwen3-ASR GGUF pair | sidecar + models | the Qwen ensemble caption engine | Whisper engine still works |
| `w2v2-align-id/` | model dir (`align` builds) | forced-alignment caption timing | DTW fusion timing stands |
| `deep-filter.exe` | sidecar (`enh` builds) | cleaned-voice caption input | captions read the mixed audio |
| `htdemucs_ft_vocals.onnx` | model (`sep` builds) | vocal-stem experiments | — (off by default) |
| `ggml-silero-v5.1.2.bin` | model | the `YC_VAD=1` trial decode knob | knob is a no-op |

Prefetch scripts: `scripts/fetch-sidecars.ps1`, `scripts/fetch-models.ps1`, `scripts/fetch-llama-sidecar.ps1`.

## Your first Short

1. **Import a VOD.** Paste a YouTube URL and hit **Import URL**, or **Open a local file…**. Import downloads only the audio + chat replay (video comes later, per clip). *Defaults for this import* — Language (**Auto** applies the Creator's saved language), Caption style, Engine, Layout — can all stay as they are for a first run.
2. **Detect Moments.** After import, run detection. The signal ensemble (chat-rate, loudness, excitement lexicon, arousal) plus the LLM judge rank clip-worthy spans; each Moment shows its per-signal σ breakdown, transcript, the judge's reason, and a generated title. The **max clip length** slider caps how long a Moment may run (the detector picks each one's natural length below it, 45–180 s on sentence boundaries).
3. **Review by ear.** Click a Moment to read its transcript and play its audio (the whole-VOD waveform shows loudness + chat with clickable markers). You can also mark a Moment manually from the waveform.
4. **Open in editor.** Promoting a Moment downloads its full-quality Segment, auto-detects the Layout (stacked gameplay+facecam, full-cam, or full-gameplay), and opens the Studio. Transcription starts automatically in the background so captions are editable before any render; a two-plus-face frame also queues the speaker analysis (podcast mode).
5. **Export.** The summary shows length, resolution, captions, camera, and timeline cuts before the NVENC render. The finished Short lands in `workspace/<creator>/<stream-title>/`, named by its generated title — re-exports never overwrite.

For unattended work, select multiple Moments in the list and **Render N selected** — each renders auto-framed, sequentially. Same thing headless: [`--batch`](#headless-cli).

## The Studio editor

Full-window editor, four regions:

- **Left — transcript & captions.** Edit, add, delete, split, merge, censor caption units with `m:ss.cc` timestamps. Edited transcripts burn **verbatim** — no re-transcription, no re-timing, no second-guessing your words.
- **Center — Before/After preview.** *Original* shows the source frame with crop tools and face overlays; *Preview* shows the composited 9:16 output with captions and a safe-area guide. What the preview draws is what the render burns — same geometry, same ASS. Drag the caption block anywhere on the canvas, scroll to resize it, right-click to reset.
- **Right — properties.** Camera mode (**Manual / Center / Auto face / Active Speaker / Group**), framing incl. motion presets, and Caption style: genre (huge-word, rolling-pop, karaoke-fill) plus full styling (outline, shadow, back box, bold) with one-click presets — Classic, TikTok, Podcast, Minimal, Gaming, MrBeast — and your own saved presets.
- **Bottom — the timeline.** Multi-track, Premiere/CapCut-shaped: ruler, video filmstrip, two caption tracks (**auto** = the pipeline's, **yours** = your own additions — an AI artifact and an operator artifact never share a track), a music track, speaker lanes, cut markers, playhead. `Ctrl+wheel` zooms about the pointer; headers carry eye/lock/mute toggles where they're honest.

Timeline verbs:

- **Camera cut (gold ◆)** — split a camera-plan shot so the framing changes; drag the picture to reframe that shot yourself.
- **Timeline razor (white ✂)** — remove spans from the export outright: `▼ Mark` sets a reference, `✂←` cuts playhead-back-to-mark, `→✂` forward, `Delete` drops the span under the playhead. Video, audio, and captions go together; preview playback skips removed spans.
- **+ Music** — drop an audio file (mp3/wav/flac/ogg/m4a) at the playhead: trim, drag, per-clip gain, split. Music anchors to *output* time (razor edits shift content under it, CapCut-style) and is mixed after everything else — it never extends the Short and never touches the voice level.
- **Thumbnail intro** — a still image (JPG/PNG/WebP) shown for the first 0.5–2 s ahead of the clip; drag its right edge for duration.
- **Edge fades** — fade-in/out over the clip edges, video and mixed audio together; the preview plays the same envelope the burn applies.
- **Undo/redo** across the editor's gestures.

Podcast clips: the speaker analysis tracks each seat's face and mouth activity, embeds voices to tell *people* apart from *seats* across camera angles, detects shared reactions (group laughter → split-screen, never a solo on whoever's mouth moves), verifies solo shots actually frame the attributed person, and derives the cut-based **Active Speaker** camera plan. A camera audit (creep, causeless re-frames, subject riding the crop edge) runs before every render and surfaces in the Camera panel. Overriding is one click on another face; dragging the frame flips to Manual.

The transport carries a master playback volume (0–200 %, persisted; playback only — never the export).

## Caption engines and curation

Two engines, remembered per Creator:

- **Whisper** — single-decode whisper large-v3 with DTW word timing.
- **Qwen ensemble** — a five-variant vote (Qwen3-ASR decodes + whisper as a witness) with wav2vec2-CTC forced-alignment timing on `align` builds. New Creators start here.

Accent, slang, and viewer-name errors are what actually hurt caption quality, and no audio processing fixes them — so the app maintains **dialect stores**, layered per language: a bundled base dictionary, a per-Creator store (their confirmed slang/names, applied to all their VODs), and a per-clip store (that export's auto-harvest). Words the engine was unsure of harvest themselves into the **review queue** in the app's detail pane: fill in the right word — with a VOD jump link to hear it in context — and the correction applies to all that Creator's future clips. The optional **LLM correction pass** (`correct` builds, opt-in checkbox) additionally repairs context-sensitive garbles a global dictionary can't safely touch.

## Per-Creator memory

`workspace/creators.json` remembers, per Creator: caption style, spoken language, and caption engine. Importing a known Creator's VOD seeds all three; every render saves your (possibly overridden) picks back. Explicit picks always beat remembered ones.

## Headless CLI

Same worker and code paths as the GUI, no clicking:

```powershell
# one clip end-to-end; language/genre/layout optional
yt-clipper --headless <url-or-file> <start_s> <end_s> [en|id|ja] [huge|rolling|karaoke] [auto|stacked|cam|gameplay]

# detection only: prints ranked Moments with per-signal scores + titles
yt-clipper --detect <url-or-file> [en|id|ja]

# import -> detect -> render the top-k, auto-framed, named by their titles
yt-clipper --batch <url-or-file> [en|id|ja] [huge|rolling|karaoke] [k] [auto|stacked|cam|gameplay]
```

An `http(s)` target imports as a YouTube URL, anything else as a local file. Omitted language = **Auto** (the Creator's saved language). Layout tokens: `auto` (detect), `stacked`, `cam`, `gameplay`.

## Environment variables

| Variable | Effect |
| --- | --- |
| `YC_MAX_CLIP_S` | Headless/batch: max Moment length in seconds (the GUI slider's twin; clamped to the 180 s Shorts ceiling) |
| `YC_QWEN_ENS` | Tri-state engine override for one invocation: `1` forces the Qwen ensemble, `0` forces Whisper, unset = the Creator's saved engine. Never written back |
| `YC_FORCED_ALIGN` | `0` switches `align` builds back to the whisper-DTW fusion timing (default: forced alignment on) |
| `YC_CORRECT` | `1`/`on`/`true`/`yes`: run the LLM caption-correction pass in headless renders (`correct` builds) |
| `YC_VAD` | `1`: Silero-VAD-gated whisper decode (trial knob) |
| `YC_DIAG_OPEN` | Any value: open the Diagnostics section expanded at launch |
| `RUST_LOG` | Log filter (default `info`), e.g. `RUST_LOG=yt_clipper=debug` |

Advanced caption-tuning knobs (diagnostic; defaults are the shipped behavior): `YC_ENH_ATTEN` (DeepFilterNet attenuation limit), `YC_SUPPRESS_NST` / `YC_CAPTION_NOCTX` (whisper decode trials), `YC_LAUGH_TRIM` (laughter-aware hold trimming).

## Troubleshooting

- **YouTube import fails / 403.** yt-dlp needs a JS runtime for YouTube's signature challenges: make sure `deno.exe` is present (`scripts/fetch-sidecars.ps1`, the in-app Download row, or `winget install DenoLand.Deno`).
- **"Diagnostics — something is missing."** Open the section; every red row has a pinned Download. Only ffmpeg/ffprobe/yt-dlp/the whisper model/the `yc-whisper` engine (built with the app — rebuild or reinstall to restore) are hard requirements — the rest degrade gracefully.
- **GPU sharing: slowdowns and failed jobs.** All GPU stages share your one card by design (strictly sequential, never concurrent). Running a game alongside makes detects and renders crawl. Whisper transcription runs in its own `yc-whisper` process: if another app exhausts VRAM mid-job, that **job fails** with a "GPU busy — close it and retry" error and the app stays up — retry once the GPU is free. A warning appears before GPU stages when video memory already looks scarce. A driver-level reset (TDR) from a heavy 3D app can still close the app itself; `workspace/crash.log` records what happened — relaunch and re-import. Ordinary browser use is fine.
- **Captions mishear slang or names.** That's what the review queue is for — fill corrections once per Creator, they stick. For per-clip experiments, prefer the Studio's transcript editor (verbatim burn).
- **The build fails in whisper-rs.** Check CMake + CUDA Toolkit + LLVM are installed and use `scripts\cargo-cuda.bat`; don't set `WHISPER_DONT_GENERATE_BINDINGS`.
- **Window/launch oddities.** `scripts/diag-launch-flash.ps1` logs every window appear/vanish on a launch with timestamps.
- Logs: every run appends to `workspace/logs/yc.log` (and, windowless, `workspace/logs/stderr.log`); a crash writes `workspace/crash.log` with the job that was in flight. From a terminal, `RUST_LOG=info` (default) or `debug` prints live; child-process consoles are hidden by design.
