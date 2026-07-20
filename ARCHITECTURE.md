# Architecture

How yt-clipper is put together and why. The vocabulary here ([Moment](CONTEXT.md), Clip, Creator, Layout, Signal, …) is defined in [CONTEXT.md](CONTEXT.md) — code type names mirror it exactly, and when they drift, the glossary wins. "ADR NNNN" citations throughout the codebase refer to the project's internal architecture-decision records, numbered in the order the decisions were made.

## Workspace

Eight crates, one binary plus two built sidecars:

| Crate | Role |
| --- | --- |
| `crates/app` (`yt-clipper.exe`) | egui/eframe desktop shell, the serial pipeline worker, the Studio editor, headless CLI modes |
| `crates/core` | Domain model — type names mirror CONTEXT.md; the 1080×1920 canvas constants; `creators.json` store |
| `crates/ingest` | VOD ingestion: yt-dlp/ffmpeg audio extraction, chat replay, per-clip Segment fetch (HLS + native DASH section fetch), cancel-safe child processes |
| `crates/transcribe` | Caption policy + the whisper wire client (no GPU link): language-aware caption-unit grouping (EN/ID words, JA character chunks); the Qwen3-ASR ensemble vote; wav2vec2-CTC forced alignment (`align`) |
| `crates/detect` | Moment detection: cheap whole-VOD discovery (chat-rate + loudness), per-candidate refine (lexicon, arousal via `ser`), LLM-judge IPC structs; signals stored unblended |
| `crates/render` | ffmpeg filtergraph + generated ASS + NVENC export; the ASS generator is the single home of caption animation |
| `crates/frame` | Framing: Ultraface facecam detection (`face`), pure layout geometry (always compiled), podcast speaker tracking, voice diarization, occupant map, reaction tagging |
| `crates/whisper` (`yc-whisper.exe`) | Out-of-process whisper.cpp (CUDA, large-v3) decode engine (ADR 0072): raw-token server over stdio, one child per resident model hold |
| `crates/llm-judge` (`yc-llm-judge.exe`) | Out-of-process llama.cpp worker: whole-video digest, per-Moment clip-worthiness verdicts + titles, the caption-correction pass |

## Process model

The app itself links **no ggml at all**. llama.cpp lives in the **`yc-llm-judge` sidecar** (one-shot per detect run: candidate batch in as JSON on stdin, verdicts back on stdout, VRAM freed by exit). whisper.cpp lives in the **`yc-whisper` sidecar** (ADR 0072): a served child holding the model resident across a batch of decode requests — raw f32 samples in, normalized raw tokens out — so a CUDA out-of-memory `abort()` under GPU contention kills the child and fails the *job* with a "GPU busy — retry when free" error instead of the app, and Cancel can kill a decode mid-flight. All caption **policy** (grouping, dialect corrections, harvest, the ensemble vote, alignment) stays in-process in `yc-transcribe`; the seam is the raw token.

Everything else external is a **pinned sidecar child process**: `ffmpeg`/`ffprobe` (extract, probe, render), `yt-dlp` (+ `deno` for YouTube signature solving), `deep-filter` (caption-input denoise), `llama-mtmd-cli` (ensemble decodes). Child consoles are hidden (ADR 0025); Cancel and window-close kill the whole child tree.

**GPU staging is strictly sequential** — one 8 GB GPU is the design target. Whisper unloads before the judge loads; batch renders run one clip at a time; models held resident across a batch release when the queue runs dry.

## The pipeline worker

One background thread owns the pipeline (`app/src/pipeline.rs`). The UI sends `Job`s (Import, Detect, Prepare, Transcribe, AnalyzeSpeakers, Render) over a channel, polls `Progress` messages back, and never blocks — the same worker drives the GUI, `--headless`, `--detect`, and `--batch`, so every mode exercises the same code path.

Ingest is **audio-first, two-phase** (ADR 0001): import downloads only analysis audio + chat replay; the full-quality, keyframe-padded **Segment** is fetched per clip at promote time. The VOD's video is never downloaded whole.

Detection (ADR 0002/0007) runs cheap whole-VOD **discovery** (robust-z chat-rate + max-pooled loudness, peak-pick + NMS) and then **refines** each candidate: one resident-whisper transcription, excitement lexicon, speech-emotion arousal (`ser`), and the LLM judge — which first writes a whole-video digest, then scores and titles every candidate with that context (ADR 0071). Signals stay unblended per Moment so ranking is retunable and the operator can see *why* something surfaced.

## Data layout

Everything resolves relative to the exe (exe-plus-folders distribution, ADR 0005); in a dev tree the app walks up from `target/{debug,release}` to the repo root.

```
workspace/
  creators.json                  # per-Creator defaults: style, language, engine
  settings.json                  # app prefs: master playback volume, saved caption presets
  <creator>/
    <lang>.json                  # per-Creator dialect store (curated corrections)
    <stream-title>/              # one folder per VOD ("stream folder")
      <Generated Title>.mp4      # rendered Shorts at the root
      <Title>.<lang>.json        # per-clip dialect store (auto-harvest, beside its Short)
      data/                      # intermediates: analysis.wav, segments, project.json,
                                 # review.json, captions, fonts, camera plans
```

`project.json` + `review.json` restore a prior session's detected Moments (with transcripts and judge reasons) on re-import — no re-detection. Dialect stores are three layers merged per render, most-specific winning: bundled base (`assets/dialect/`) → per-Creator → per-clip (ADR 0031). egui window/panel state persists in the OS app-data dir.

## Rendering

One ffmpeg pass per export: crop/scale/vstack filtergraph, a generated ASS file burned via libass (the one home of caption animation — styles are data, not code paths), `h264_nvenc` encode (ADR 0004). Dynamic camera plans render as a per-shot trim/crop concat in the same single pass; the music track wraps the finished graph in one `amix` (never extending the Short, never touching voice level); a thumbnail intro concat-prepends *after* the burn so every timeline artifact stays source-relative by construction.

The Studio preview is built to be un-driftable from the burn: the same composite geometry drawn as egui UV sub-rects of decoded frames, cut-frame selection by the frame actually on screen, and the same fade envelope on preview audio that the burn applies.

## Design principles

- **Offline.** All analysis and rendering is local. Network only for user-initiated ingestion and pinned, SHA-256-verified dependency downloads (in-app Diagnostics).
- **Fail-soft dependencies.** Every optional tool/model absence degrades one capability and is visible (and healable) in Diagnostics — it never fails a core flow.
- **Signals stay unblended.** Detection stores each signal per Moment; combined score is a reweighting, never a destruction.
- **The operator's word is final.** Edited transcripts burn verbatim; explicit picks beat remembered defaults; AI camera modes flip to Manual the moment the operator drags. An AI artifact and an operator artifact never share a timeline track.
- **Preview = export.** Anything the preview shows is derived from the same data the render consumes.
- **Gates before ships.** Perceptual features (diarization, reactions, framing, titles) land behind pre-registered measurable bars plus the operator's eye on real burned clips — the ADRs record each gate.

## Verification

`cargo test --workspace` and `cargo test --workspace --features face` must both stay green (the non-face build has stub signatures that would drift silently otherwise). Production-path diagnostic harnesses live in `crates/app/examples/` (built with the features they exercise); `scripts/diag-launch-flash.ps1` watches window lifecycle on launch.
