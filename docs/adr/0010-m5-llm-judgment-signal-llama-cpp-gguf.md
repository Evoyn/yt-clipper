# M5 LLM judgment Signal: local GGUF via llama.cpp, per-candidate relative scoring, game-narration mitigation

ROADMAP M5 is the local-LLM stage (ADR 0002): a GGUF model judges each refine
candidate's clip-worthiness from its transcript. ADR 0009 gated this on measuring
whether refine transcripts carry the *streamer* (PASS: 76% streamer / 12%
game-dramatic on the cutscene-heavy worst case) and pre-committed the build design
to a grill once greenlit. This ADR records that design.

It is the **LLM judgment Signal**, not a post-sort "rerank": it slots into refine
exactly as arousal did (ADR 0008). `core::Signals.llm` already exists (scaffolded,
unpopulated); the refine loop already builds the per-candidate `texts`;
`pipeline.rs` already `drop`s the Transcriber "before any later GPU stage (M5
LLM)". M5 adds a `Weights.llm` term, one row in `combined_score`, a per-candidate
scorer, and the existing sort reranks.

## Considered options

- **Scoring semantics — relative z-score vs absolute judgment.** Chosen:
  **relative**, mirroring lexicon/arousal — one inference per candidate, a raw
  0-10 score, `robust_z` across the candidate set, then `combined_score`. Keeps
  the LLM on the same scale as the other four z-scored signals so the weighted
  mean stays scale-consistent. Rejected absolute-mapped (/10 -> [0,1]): it would
  let the LLM say "this whole set is weak," but the review UX is a ranked top-N
  with no absolute cutoff, so the relative signal is exactly enough to float
  genuine reactions above scripted-cutscene false positives — and mixing an
  absolute [0,1] into a weighted mean of z-scores is scale-inconsistent.
- **Per-candidate independent vs whole-set ranking.** Chosen: **independent**
  (each prompt sees one candidate). Mirrors every other signal, keeps the context
  tiny, and yields reliable structured output. Feeding all 25 candidates at once
  bloats context, degrades on a 7B, and breaks the apply-pattern.
- **llama.cpp binding — `llama-cpp-2`.** Maintained, CUDA-capable, the closest
  analogue to whisper-rs. Built through `cargo-cuda.bat`, which already supplies
  MSVC + CUDA + libclang + `CUDAARCHS=86`. **Risk (recorded because a future
  reader will hit it if the build fights back):** whisper-rs and llama-cpp-2 each
  vendor their own copy of ggml, so a single binary may hit duplicate ggml
  symbols at link time. Validated on the first build — and **the risk
  materialized**: linking llama's and whisper's ggml into one exe failed with
  LNK1169 (multiply-defined symbols). Resolved per the pre-committed fallback by
  isolating the LLM in a separate `yc-llm-judge` binary (links llama only; the app
  shells out over stdio). See the Consequences and the Outcome below.
- **Model — Qwen2.5-7B-Instruct Q5_K_M.** Must fit 8 GB after the whisper drop
  and be strong at EN/ID/JA (the corpus VOD is Bahasa Indonesia). Qwen2.5-7B is
  the multilingual leader at this size; Q5_K_M (~5.4 GB) full-offloads to ~6.7 GB
  alongside the resident wgpu GUI renderer. Model-agnostic per ADR 0002 —
  operator-swappable; this is only the bundled default. Rejected Llama-3.1-8B
  (weaker ID/JA) and Gemma-2-9B (tighter fit, slower).
- **Structured output — GBNF grammar.** Generation is constrained by a GBNF
  grammar to `{"score": <0-10>, "reason": "<short>"}`, guaranteeing a parseable
  object from a local 7B rather than relying on it to emit clean JSON. Greedy /
  temp 0 so scores are reproducible run-to-run. Fallback if the grammar API is
  awkward: lenient first-integer-0-10 extraction.

## Consequences

- The llama.cpp inference runs **out-of-process** in a new `yc-llm-judge` binary
  (`crates/llm-judge`) that depends on `llama-cpp-2` (CUDA) but **not** whisper, so
  it links a single ggml cleanly. `yc-detect/src/llm.rs` keeps only the pure,
  always-compiled pieces both sides share — the prompt + mitigation (`SYSTEM`,
  `GRAMMAR`, `build_prompt`), the lenient `parse_output`, the z-score `apply`, and
  the `JudgeRequest`/`JudgeVerdict` IPC structs — and has **no** llama dep or cargo
  feature. The app shells out to the judge per detect run (the whole candidate
  batch as JSON over stdin → verdicts on stdout); if the judge binary or the GGUF
  is absent, the `llm` signal is simply omitted (combined_score renormalizes),
  exactly like a missing SER model. Default `cargo build` / `test -p yc-detect`
  never touch llama.cpp; only `cargo build -p yc-llm-judge` pays the CUDA build.
- In refine, **after `drop(transcriber)`** frees VRAM (and after the CPU arousal
  pass, so the prompt can corroborate against the z-scored chat/loudness/arousal
  already written onto each Moment): load the GGUF full-offloaded, score each
  candidate from its transcript window, z-score across the set, weight into
  `combined_score`. GPU stages stay strictly sequential — whisper unloads before
  llama loads.
- **The mitigation is load-bearing** (ADR 0009, the #11 case: arousal 1.107 on a
  silent-streamer cutscene). The prompt feeds the corroborating per-candidate
  signals and instructs the model to score the *streamer's* reaction and
  **discount scripted in-game dialogue / cutscene narration** — a dramatic game
  line is not clip-worthy unless the streamer reacts to it. Robustness-by-
  corroboration, not betting the score on the words alone.
- `Weights` gains `llm`; the default rebalances to chat 0.30 / loudness 0.20 /
  lexicon 0.10 / arousal 0.15 / **llm 0.25** — the semantic judge gets a strong-
  but-not-dominant voice, and the lexicon drops because it overlaps the LLM (both
  read the transcript). Retunable without re-analysis (ADR 0002).
- The LLM's one-line `reason` is threaded to the review UI alongside the
  transcript (a `HashMap<u64, String>` on `Progress::Detected`, like
  `transcripts`) — CONTEXT.md's "the operator can see *why* a Moment surfaced,"
  now with the richest why.
- The GGUF download (~5.4 GB) is operator-gated via the credible-source rule
  (Hugging Face).
- Extends ADR 0002 (signals unblended, retunable), ADR 0007/0008 (cheap
  discovery, bounded refine, signals stored z-scored), and ADR 0009 (the gate
  that greenlit this and mandated the mitigation).

## Outcome

**Build (2026-06-19):** the predicted ggml-symbol collision materialized —
linking whisper.cpp's and llama.cpp's ggml into one exe failed with LNK1169
(multiply-defined symbols). Resolved by the pre-committed fallback: the inference
moved to a standalone `yc-llm-judge` binary (llama only), invoked by the app over
stdio. The app and default tests no longer link llama at all; only the judge does.

**Verified (2026-06-19):** the standalone judge smoke-test scored a streamer
reaction 8, a hype/win 10, the scripted-cutscene line **0 despite arousal 1.1
(the #11 antidote)**, and menu-reading 0 — reproducible (greedy/temp-0). A full
headless `--detect` ran end-to-end (whisper refine → app spawns the judge → the
`llm` column populates and reranks), and a real 25-candidate
`--detect --features ser` on a second VOD demoted loud intro chatter (llm −1.12)
while lifting a reaction (+1.12). VRAM staging is clean headless — whisper drops,
then the judge owns the card; the judge cannot co-reside with the GUI's wgpu on
the 8 GB card, so detection runs headless. **M5 done.**
