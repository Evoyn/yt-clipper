//! `yc-whisper` — the out-of-process whisper.cpp decode engine (ADR 0072).
//!
//! ggml's CUDA error path is `GGML_ASSERT`/`abort()`: a failed VRAM
//! allocation kills the PROCESS, not the job. While whisper linked into the
//! app, opening any GPU-heavy program beside a running transcription killed
//! yt-clipper outright (the operator's 2026-07-20 crash report). The decode
//! therefore runs *here*: the app spawns one `yc-whisper` per resident model
//! hold (`Transcriber` in `yc-transcribe`), streams decode requests over
//! stdin, and reads raw-token responses from stdout. A CUDA abort now costs
//! one child process and surfaces as a failed job with a "GPU busy" message.
//!
//! **The cut line is the raw token.** This binary owns exactly the whisper-rs
//! surface: context load (`--model`, `--dtw`), `FullParams` assembly from the
//! request, `state.full()`, and the raw-token normalization loop (special
//! filter, segment-seam space marking, DTW fallback). Grouping, dialect
//! corrections, confidence, harvest — all caption POLICY — stay in
//! `yc-transcribe`, byte-for-byte where they always were; the fixture
//! identity bar (A3) pins this seam. The decode-policy constants that live
//! against measurement history in `yc-transcribe` (beam width, the trial
//! knobs) arrive resolved inside each request — this process reads NO env,
//! so the app's scoped env writes keep working (env is inherited at spawn,
//! not at request time).
//!
//! Protocol (`yc_transcribe::wire`): one JSON request line, then exactly
//! `n_samples * 4` bytes of raw f32-LE samples; one JSON response line back.
//! stdout carries ONLY response lines — all logging (ours and ggml's, via
//! the tracing hooks) goes to stderr. A ready line is emitted once the model
//! is resident; stdin EOF is the shutdown signal (the app dropping its
//! `Transcriber` closes the pipe; a kill is equally fine — the model dies
//! with the process, which is the entire point).
//!
//! Cancellation: deliberately NO whisper abort callback — installing one
//! collapsed CUDA-graph batching to a ~5% util crawl (measured; see the note
//! in `yc-transcribe`). The app cancels by killing this process.

use std::io::{BufRead, Read, Write};

use anyhow::{Context, Result};
use whisper_rs::{
    DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters,
};
use yc_transcribe::wire::{DecodeRequest, DecodeResponse, WireToken, MAX_SAMPLES};

fn main() {
    // All logging to stderr: stdout is the protocol channel.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    // Route whisper.cpp/ggml's own logging through `tracing` so its per-token
    // DEBUG flood is filtered instead of drowning stderr (and slowing decode).
    whisper_rs::install_logging_hooks();

    let (model, dtw) = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            respond(&DecodeResponse::err(format!("{e:#}")));
            std::process::exit(2);
        }
    };

    // Load the context NOW: the child's whole reason to exist is holding this
    // model resident. A load failure (missing file, GPU busy) answers the
    // ready line with the error and exits nonzero — `Transcriber::load` fails
    // exactly where the in-process load used to.
    let mut cparams = WhisperContextParameters::default();
    cparams.use_gpu(true);
    if dtw {
        cparams.dtw_parameters(DtwParameters {
            mode: DtwMode::ModelPreset { model_preset: DtwModelPreset::LargeV3 },
            ..Default::default()
        });
    }
    let ctx = match WhisperContext::new_with_params(&model, cparams) {
        Ok(ctx) => ctx,
        Err(e) => {
            respond(&DecodeResponse::err(format!("loading whisper model {model}: {e}")));
            std::process::exit(3);
        }
    };
    tracing::info!("yc-whisper ready: model={model} dtw={dtw}");
    respond(&DecodeResponse::ready());

    if let Err(e) = serve(&ctx) {
        // A broken pipe here usually just means the app dropped us mid-read;
        // exit nonzero so a *mid-decode* death is distinguishable, but keep
        // the message on stderr for the app's tail capture.
        tracing::error!("yc-whisper serve loop ended: {e:#}");
        std::process::exit(4);
    }
}

fn parse_args() -> Result<(String, bool)> {
    let argv: Vec<String> = std::env::args().collect();
    let mut model = None;
    let mut dtw = false;
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--model" => {
                model = Some(argv.get(i + 1).context("--model needs a path")?.clone());
                i += 2;
            }
            "--dtw" => {
                dtw = true;
                i += 1;
            }
            other => anyhow::bail!("unknown arg {other} (usage: yc-whisper --model <path> [--dtw])"),
        }
    }
    Ok((model.context("usage: yc-whisper --model <path> [--dtw]")?, dtw))
}

/// One response line on stdout, flushed — the protocol's only stdout writes.
fn respond(resp: &DecodeResponse) {
    let mut line = serde_json::to_string(resp).expect("wire response serializes");
    line.push('\n');
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(line.as_bytes());
    let _ = out.flush();
}

/// Request loop: JSON header line, `n_samples * 4` raw f32-LE bytes, decode,
/// respond. EOF = clean shutdown. A malformed header is a desynced stream —
/// bail out (the app treats child death as the decode's failure).
fn serve(ctx: &WhisperContext) -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).context("reading request header")? == 0 {
            tracing::info!("stdin EOF - shutting down (model released with the process)");
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let req: DecodeRequest =
            serde_json::from_str(line.trim()).context("parsing request header")?;
        anyhow::ensure!(
            req.n_samples <= MAX_SAMPLES,
            "n_samples {} over the wire cap {MAX_SAMPLES}",
            req.n_samples
        );
        let mut bytes = vec![0u8; req.n_samples * 4];
        reader.read_exact(&mut bytes).context("reading sample payload")?;
        let samples: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        drop(bytes);
        tracing::info!(
            "decode: {} samples ({:.1}s), beam={:?} lang={}",
            req.n_samples,
            req.n_samples as f64 / 16_000.0,
            req.beam_size,
            req.language,
        );
        match decode(ctx, &req, &samples) {
            Ok(tokens) => respond(&DecodeResponse::tokens(tokens)),
            Err(e) => respond(&DecodeResponse::err(format!("{e:#}"))),
        }
    }
}

/// One whisper decode → normalized raw tokens. MOVED VERBATIM from
/// `yc-transcribe`'s in-process `Transcriber::run` (ADR 0072): the params
/// mirror the request field-for-field, and the token loop below is the
/// pre-move code with `WireToken` in place of the tuple. Behavior changes
/// here are caption forks — bar A3 diffs them; don't make them.
fn decode(ctx: &WhisperContext, req: &DecodeRequest, samples: &[f32]) -> Result<Vec<WireToken>> {
    let mut state = ctx.create_state().context("creating whisper state")?;

    let strategy = match req.beam_size {
        Some(beam_size) => SamplingStrategy::BeamSearch { beam_size, patience: -1.0 },
        None => SamplingStrategy::Greedy { best_of: 1 },
    };
    let mut params = FullParams::new(strategy);
    params.set_language(Some(&req.language));
    if !req.prompt.is_empty() {
        params.set_initial_prompt(&req.prompt);
    }
    params.set_token_timestamps(true); // populate per-token t0/t1 for word timing
    params.set_translate(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_print_special(false);
    if req.no_context {
        params.set_no_context(true);
    }
    if req.suppress_nst {
        params.set_suppress_nst(true);
    }
    if let Some(vad) = req.vad_model.as_deref() {
        params.set_vad_model_path(Some(vad));
        params.set_vad_params(whisper_rs::WhisperVadParams::default());
        params.enable_vad(true);
    }
    // NOTE: no abort callback — see the module doc. Cancel = the app kills us.

    state.full(params, samples).context("whisper transcription failed")?;

    // Collect (text, t0_s, t1_s, prob) for every real token — the exact
    // pre-move normalization: special filter, segment-seam space marking,
    // DTW-preferred timing with heuristic fallback.
    let mut raw_tokens: Vec<WireToken> = Vec::new();
    for s in 0..state.full_n_segments() {
        let segment = state
            .get_segment(s)
            .ok_or_else(|| anyhow::anyhow!("segment {s} out of bounds mid-read"))?;
        // A segment boundary always starts a new word. whisper emits most
        // word-initial tokens space-led, but a segment's FIRST token can lack
        // the space (after punctuation, or a mid-word window split) — the
        // grouping would then fuse it onto the *previous segment's* last word,
        // stretching that word's span across the 30 s seam. Mark the first
        // real token space-led so `group_into_words` starts a fresh unit
        // (JA chunking trims the space; EN/ID display trims it too).
        let mut first_real_token = true;
        for t in 0..segment.n_tokens() {
            let Some(token) = segment.get_token(t) else {
                continue;
            };
            let mut text = token.to_str_lossy().context("reading token text")?.into_owned();
            if yc_transcribe::wire::is_special(&text) {
                continue;
            }
            if first_real_token {
                first_real_token = false;
                if s > 0 && !(text.starts_with(' ') || text.starts_with('\u{2581}')) {
                    text.insert(0, ' ');
                }
            }
            let data = token.token_data();
            // Prefer the DTW-aligned time; fall back to the heuristic t0/t1
            // when DTW produced no value for this token (t_dtw == -1). DTW
            // gives a single aligned point per token, so a word's span runs
            // from its first token's time to its last — the caption builders
            // handle the (zero-width) single-token case.
            let (t0, t1) = if data.t_dtw >= 0 {
                (data.t_dtw, data.t_dtw)
            } else {
                (data.t0, data.t1)
            };
            raw_tokens.push(WireToken(text, t0 as f64 / 100.0, t1 as f64 / 100.0, data.p));
        }
    }
    Ok(raw_tokens)
}
