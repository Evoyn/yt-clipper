//! `yc-llm-judge` — the out-of-process LLM judgment Signal worker (ADR 0010).
//!
//! whisper.cpp (linked by the app via `yc-transcribe`) and llama.cpp can't
//! co-link: both vendor their own `ggml`, and the duplicate symbols fail the link
//! (LNK1169). So the llama.cpp inference runs *here*, in a binary that links
//! llama only. The app shells out once per detect run — it writes a
//! [`JudgeRequest`] (the whole candidate batch) as JSON to our stdin and reads a
//! `Vec<`[`JudgeVerdict`]`>` (one per candidate, in request order) from our
//! stdout. The GGUF loads once, scores the batch, and frees its VRAM when this
//! process exits — sequential GPU staging holds across the process boundary.
//!
//! stdout carries *only* the JSON response; all logging (ours and llama.cpp's
//! own) goes to stderr so it never corrupts the payload.
//!
//! NOTE: the llama-cpp-2 v0.1.150 binding surface is written against the crate
//! source and verified on the first build of this crate — minor signature fixups,
//! if any, are isolated to this file. The grammar sampler needs llama-cpp-2's
//! `common` feature, which is enabled by default.

use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::Context as _;
use anyhow::{anyhow, Result};
use yc_core::Language;
use yc_detect::llm::{
    build_prompt, parse_output, Context, JudgeRequest, JudgeVerdict, Judgment, GRAMMAR, SYSTEM,
};

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaChatTemplate, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::TokenToStringError;

/// Context window for one candidate's prompt+output. The prompt (system rubric +
/// transcript + signals) is a few hundred tokens; 2048 leaves ample room (the
/// llama.cpp default of 512 would overflow a dense transcript).
const N_CTX: u32 = 2048;
/// Cap on generated tokens — the JSON object is tiny (score + short reason + a
/// <=60-char title; ADR 0015), this bounds a runaway. Raised from 96 to fit the
/// added title field comfortably.
const MAX_NEW_TOKENS: usize = 160;
/// Hard cap on collected output bytes (defensive, alongside MAX_NEW_TOKENS).
const MAX_OUT_BYTES: usize = 1024;

/// Context window for the caption correction (ADR 0030): a full caption plus
/// slang/name hints runs longer than a judge prompt, so 4096.
const N_CTX_CORRECT: u32 = 4096;
/// Generated-token cap for one corrected caption. The index-anchored reply echoes
/// every input line ("N: word"), so the cap scales with caption length: ~5 tokens
/// per word covers a dense ~100-word clip (a noisy 30 s clip hit 57 units here),
/// still well inside `N_CTX_CORRECT`. Bounds a runaway.
const MAX_CORRECT_TOKENS: usize = 512;

fn main() -> Result<()> {
    // All logs to stderr; stdout is the JSON response channel and must stay clean.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Silence llama.cpp's own logging - it dumps ~80 lines per llama_context (one
    // per candidate), which would bury the terminal far worse than the whisper
    // flood we just fixed. Our tracing::info covers status; llama API failures
    // still surface as Rust errors. (Flip to with_logs_enabled(true) to debug.)
    llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default().with_logs_enabled(false));

    // `--correct` (ADR 0030, WIP): caption-correction spike mode. Reads
    // {model_path, system, user} JSON and returns Qwen's free-form completion, so
    // the correction prompt can be iterated via the request without a rebuild. The
    // default (no arg) path stays the detect-time judge IPC.
    if std::env::args().skip(1).any(|a| a == "--correct") {
        return run_correct();
    }

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).context("reading request from stdin")?;
    let req: JudgeRequest = serde_json::from_str(&input).context("parsing JudgeRequest JSON")?;
    tracing::info!(candidates = req.candidates.len(), "llm-judge: scoring batch");

    let llm = Llm::load(Path::new(&req.model_path))?;
    let mut verdicts = Vec::with_capacity(req.candidates.len());
    for (i, c) in req.candidates.iter().enumerate() {
        match llm.score(&c.transcript, &c.context(), req.language) {
            Ok(j) => verdicts.push(JudgeVerdict { score: j.score, reason: j.reason, title: j.title }),
            Err(e) => {
                // A single bad candidate must not sink the batch: emit a 0 so the
                // response stays one verdict per candidate (z-scoring lines up).
                tracing::warn!("candidate {i} scoring failed: {e:#}");
                verdicts.push(JudgeVerdict { score: 0.0, reason: String::new(), title: String::new() });
            }
        }
    }
    drop(llm); // free the GGUF's VRAM before we exit

    let out = serde_json::to_vec(&verdicts).context("serializing verdicts")?;
    std::io::stdout().write_all(&out).context("writing verdicts to stdout")?;
    Ok(())
}

/// Caption-correction spike (ADR 0030, WIP): read `{model_path, system, user}`
/// from stdin, run Qwen free-form (greedy, reproducible), write the raw completion
/// to stdout. Keeps the correction prompt out of the binary so it is tuned via the
/// request, not a rebuild. Uses `serde_json::Value` to avoid touching the shared
/// judge IPC structs.
fn run_correct() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).context("reading correct request from stdin")?;
    let v: serde_json::Value =
        serde_json::from_str(&input).context("parsing correct request JSON")?;
    let model_path = v["model_path"].as_str().context("correct request: model_path missing")?;
    let system = v["system"].as_str().context("correct request: system missing")?;
    let user = v["user"].as_str().context("correct request: user missing")?;
    tracing::info!("llm-correct: running caption-correction completion");
    let llm = Llm::load(Path::new(model_path))?;
    let out = llm.complete(system, user, MAX_CORRECT_TOKENS)?;
    drop(llm); // free the GGUF's VRAM before we exit
    std::io::stdout().write_all(out.as_bytes()).context("writing completion to stdout")?;
    Ok(())
}

/// The llama.cpp backend is a process-global singleton (it may be initialized
/// once). A `OnceLock` keeps it alive for the process; the model (and its VRAM)
/// is freed when the `Llm` drops.
static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();

fn backend() -> Result<&'static LlamaBackend> {
    if let Some(b) = BACKEND.get() {
        return Ok(b);
    }
    let b = LlamaBackend::init().map_err(|e| anyhow!("llama backend init: {e}"))?;
    Ok(BACKEND.get_or_init(|| b))
}

/// A GGUF model kept resident on the GPU for the batch. Loading is the expensive
/// step (full GPU offload); each [`Llm::score`] spins up a cheap fresh context +
/// grammar sampler so candidates never share state.
struct Llm {
    model: LlamaModel,
    template: LlamaChatTemplate,
}

impl Llm {
    /// Load a GGUF fully offloaded to the GPU.
    fn load(gguf: &Path) -> Result<Self> {
        anyhow::ensure!(gguf.is_file(), "GGUF not found: {}", gguf.display());
        let backend = backend()?;
        // 999 GPU layers -> llama.cpp clamps to the model's layer count = full
        // offload (Q5_K_M 7B fits 8 GB once whisper has unloaded; ADR 0010).
        let model_params = LlamaModelParams::default().with_n_gpu_layers(999);
        let model = LlamaModel::load_from_file(backend, gguf, &model_params)
            .map_err(|e| anyhow!("loading GGUF {}: {e}", gguf.display()))?;
        // Prefer the model's baked-in chat template (Qwen ships ChatML); fall back
        // to ChatML by name so a template-less GGUF still works.
        let template = match model.chat_template(None) {
            Ok(t) => t,
            Err(_) => LlamaChatTemplate::new("chatml").expect("chatml literal has no null"),
        };
        tracing::info!("llm-judge: model loaded (full GPU offload)");
        Ok(Self { model, template })
    }

    /// Score one candidate's transcript. Returns the parsed [`Judgment`] (raw
    /// 0..=SCORE_MAX score, reason, and generated Shorts title).
    fn score(&self, transcript: &str, ctx: &Context, language: Language) -> Result<Judgment> {
        let messages = vec![
            LlamaChatMessage::new("system".to_string(), SYSTEM.to_string())
                .map_err(|e| anyhow!("system message: {e}"))?,
            LlamaChatMessage::new("user".to_string(), build_prompt(transcript, ctx, language))
                .map_err(|e| anyhow!("user message: {e}"))?,
        ];
        // add_ass=true leaves the prompt hanging at the assistant turn; the
        // template already supplies all special tokens, so add no extra BOS.
        let prompt = self
            .model
            .apply_chat_template(&self.template, &messages, true)
            .map_err(|e| anyhow!("apply chat template: {e}"))?;
        let tokens = self
            .model
            .str_to_token(&prompt, AddBos::Never)
            .map_err(|e| anyhow!("tokenize prompt: {e}"))?;

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_batch(N_CTX);
        let mut lctx = self
            .model
            .new_context(backend()?, ctx_params)
            .map_err(|e| anyhow!("new llama context: {e}"))?;

        let mut batch = LlamaBatch::new(N_CTX as usize, 1);
        batch.add_sequence(&tokens, 0, false).map_err(|e| anyhow!("batch add: {e}"))?;
        lctx.decode(&mut batch).map_err(|e| anyhow!("decode prompt: {e}"))?;

        // Greedy (temp 0 -> reproducible) constrained by a GBNF grammar to a tiny
        // JSON object. Grammar is best-effort: if it fails to init we still sample
        // greedily and parse_output's lenient path recovers.
        let mut samplers = Vec::new();
        match LlamaSampler::grammar(&self.model, GRAMMAR, "root") {
            Ok(g) => samplers.push(g),
            Err(e) => tracing::warn!("grammar disabled ({e}); using prompt + lenient parse"),
        }
        samplers.push(LlamaSampler::greedy());
        let mut sampler = LlamaSampler::chain_simple(samplers);

        let mut pos = tokens.len() as i32;
        let mut out: Vec<u8> = Vec::new();
        for _ in 0..MAX_NEW_TOKENS {
            let idx = batch.n_tokens() - 1;
            let mut data = lctx.token_data_array_ith(idx);
            // apply selects (grammar masks, greedy picks); accept advances the
            // grammar exactly once — avoids the auto-accept ambiguity of
            // `LlamaSampler::sample`.
            sampler.apply(&mut data);
            let Some(token) = data.selected_token() else { break };
            if self.model.is_eog_token(token) {
                break;
            }
            sampler.accept(token);
            out.extend_from_slice(&piece_bytes(&self.model, token)?);
            if out.len() > MAX_OUT_BYTES {
                break;
            }
            batch.clear();
            batch.add(token, pos, &[0], true).map_err(|e| anyhow!("batch add: {e}"))?;
            pos += 1;
            lctx.decode(&mut batch).map_err(|e| anyhow!("decode token: {e}"))?;
        }

        let raw = String::from_utf8_lossy(&out);
        Ok(parse_output(&raw))
    }

    /// Free-form completion for the correction spike (ADR 0030): the same decode
    /// loop as [`score`] but with no grammar (raw text out) and a larger context.
    /// Greedy / temp 0, so a given prompt is reproducible.
    fn complete(&self, system: &str, user: &str, max_tokens: usize) -> Result<String> {
        let messages = vec![
            LlamaChatMessage::new("system".to_string(), system.to_string())
                .map_err(|e| anyhow!("system message: {e}"))?,
            LlamaChatMessage::new("user".to_string(), user.to_string())
                .map_err(|e| anyhow!("user message: {e}"))?,
        ];
        let prompt = self
            .model
            .apply_chat_template(&self.template, &messages, true)
            .map_err(|e| anyhow!("apply chat template: {e}"))?;
        let tokens = self
            .model
            .str_to_token(&prompt, AddBos::Never)
            .map_err(|e| anyhow!("tokenize prompt: {e}"))?;

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX_CORRECT))
            .with_n_batch(N_CTX_CORRECT);
        let mut lctx = self
            .model
            .new_context(backend()?, ctx_params)
            .map_err(|e| anyhow!("new llama context: {e}"))?;

        let mut batch = LlamaBatch::new(N_CTX_CORRECT as usize, 1);
        batch.add_sequence(&tokens, 0, false).map_err(|e| anyhow!("batch add: {e}"))?;
        lctx.decode(&mut batch).map_err(|e| anyhow!("decode prompt: {e}"))?;

        let mut sampler = LlamaSampler::chain_simple(vec![LlamaSampler::greedy()]);
        let mut pos = tokens.len() as i32;
        let mut out: Vec<u8> = Vec::new();
        for _ in 0..max_tokens {
            let idx = batch.n_tokens() - 1;
            let mut data = lctx.token_data_array_ith(idx);
            sampler.apply(&mut data);
            let Some(token) = data.selected_token() else { break };
            if self.model.is_eog_token(token) {
                break;
            }
            sampler.accept(token);
            out.extend_from_slice(&piece_bytes(&self.model, token)?);
            batch.clear();
            batch.add(token, pos, &[0], true).map_err(|e| anyhow!("batch add: {e}"))?;
            pos += 1;
            lctx.decode(&mut batch).map_err(|e| anyhow!("decode token: {e}"))?;
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

/// Raw bytes for one generated token, growing the buffer if 32 is too small.
fn piece_bytes(model: &LlamaModel, token: LlamaToken) -> Result<Vec<u8>> {
    match model.token_to_piece_bytes(token, 32, false, None) {
        Ok(b) => Ok(b),
        Err(TokenToStringError::InsufficientBufferSpace(i)) => model
            .token_to_piece_bytes(token, (-i) as usize, false, None)
            .map_err(|e| anyhow!("token_to_piece: {e}")),
        Err(e) => Err(anyhow!("token_to_piece: {e}")),
    }
}
