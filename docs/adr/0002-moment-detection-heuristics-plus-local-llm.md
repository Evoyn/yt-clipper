# Moment detection: heuristic ensemble + local LLM rerank, from v1

Clip-worthiness must be judged with zero cloud calls. We decided v1 detection runs a heuristic ensemble — chat-replay message-rate spikes (when the VOD has chat), audio loudness/excitement spikes, transcript excitement lexicon — and then a local LLM pass that reads transcript windows around candidate moments and scores them semantically. Manual-only browsing and heuristics-only were rejected: the user wants semantic judgment from day one.

## Consequences

- The LLM stage is model-agnostic: any GGUF served by llama.cpp, selectable in settings. The bundled default must fit fully in 8 GB VRAM (8B-class or smaller); larger models (e.g. a 12B) are user-opt-in and run partially CPU-offloaded, slower.
- GPU stages are sequential, never concurrent: transcription fully completes and unloads before the LLM loads. The pipeline must tolerate minutes-long background analysis.
- Chat replay JSON is fetched during ingestion alongside the audio (allowed under the Offline constraint: user-initiated ingestion).
- Every signal (chat rate, loudness, lexicon, LLM score) is stored per-moment, not blended irreversibly — so the ranking formula can be retuned without re-analysis.
