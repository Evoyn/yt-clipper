# Transcription: native whisper.cpp with large-v3, no Python sidecar

Captions animate per word, so transcription must produce reliable sub-segment timestamps in English, Bahasa Indonesia, and Japanese. We chose whisper.cpp (via whisper-rs, CUDA build) with `large-v3` as the default model, rejecting Python sidecars (faster-whisper, whisperX): whisperX's superior forced alignment is English-centric and adds little for ID/JA, while costing a bundled Python+CUDA runtime in a Rust-core app.

## Consequences

- Whisper emits token-level timestamps; a Rust grouping layer converts tokens into animatable caption units — space-delimited words for EN/ID, character chunks (kanji/kana boundaries) for JA. This layer is ours and is language-aware by design.
- Default model is full `large-v3` (quantized) for ID/JA accuracy; `large-v3-turbo` is offered as a faster option, with a known quality dip outside English. Model files are user-swappable GGML.
- Each VOD has a language setting (defaulted from its Creator). Auto-detect is fallback only — Indonesian streams code-switch into English constantly, and a pinned language behaves better than per-segment detection.
- Word timings may drift ±50–150 ms; the caption renderer may clamp/snap timings (e.g. minimum on-screen duration) rather than trusting raw timestamps blindly.
