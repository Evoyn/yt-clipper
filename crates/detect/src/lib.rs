//! Moment detection: the heuristic ensemble (chat-replay rate spikes, audio
//! loudness, transcript excitement lexicon) plus the local-LLM rerank stage
//! (ADR 0002). Every signal is stored per-Moment unblended (`yc_core::Signals`)
//! so ranking can be retuned without re-analysis.
//!
//! GPU discipline: this crate's LLM stage must never run while transcription
//! holds VRAM — staging is strictly sequential on the single 8 GB GPU.
//!
//! M3 scope: ensemble. M4 scope: LLM rerank (llama.cpp, GGUF, model-agnostic).
