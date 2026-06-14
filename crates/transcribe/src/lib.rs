//! Transcription: whisper.cpp via whisper-rs (CUDA build), default model
//! `large-v3` quantized (ADR 0003). Emits token-level timestamps; the
//! language-aware grouping layer in this crate converts tokens into
//! animatable caption units — space-delimited words for EN/ID, character
//! chunks (kanji/kana boundaries) for JA.
//!
//! The whisper-rs dependency is added at M1, once the CUDA toolchain exists,
//! so that M0 builds clean on a fresh machine.
