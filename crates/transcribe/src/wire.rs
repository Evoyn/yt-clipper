//! The `yc-whisper` sidecar wire protocol + client (ADR 0072).
//!
//! whisper.cpp's CUDA error path is `GGML_ASSERT`/`abort()` — while whisper
//! linked into the app, a failed VRAM allocation (another program grabbing
//! the GPU mid-job) killed yt-clipper outright. The decode now runs in the
//! `yc-whisper` child this module spawns and talks to; a CUDA abort costs
//! one child and fails one JOB, and cancellation finally works mid-decode
//! (kill the child — VRAM freed by process death, no abort callback needed,
//! so the CUDA-graph batching the in-process abort hook used to collapse
//! stays intact).
//!
//! **Protocol** (strictly serial, one child per resident model hold):
//! - spawn: `yc-whisper --model <path> [--dtw]` — the child loads the
//!   context immediately and answers ONE ready line (`ok`, or `!ok` + error
//!   and exits). Residency = the child's lifetime: `Transcriber` drop kills
//!   it, which is the release (exactly where the in-process drop freed the
//!   model — the ADR 0007 staging discipline is unchanged).
//! - request: one JSON [`DecodeRequest`] line, then `n_samples * 4` bytes of
//!   raw f32-LE samples (bit-exact, no base64, no temp files).
//! - response: one JSON [`DecodeResponse`] line carrying the normalized raw
//!   tokens — `group_tokens`' unchanged input, `(text, t0_s, t1_s, p)`.
//!   Floats survive serde_json's shortest-roundtrip printing bit-exact.
//!
//! Every decode-affecting knob crosses IN THE REQUEST, resolved app-side
//! from env by the caller: the child reads no env, so the render path's
//! scoped `YC_SUPPRESS_NST` write works by construction (env is inherited
//! at spawn — a long-lived child would otherwise never see it flip).

use std::collections::VecDeque;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use yc_core::NoConsole as _;

/// whisper's non-text special tokens ([_BEG_], `<|...|>` markers, timestamp
/// tokens) carry no caption text and must be dropped before grouping. Shared
/// with the sidecar (the filter runs there, against whisper's own token
/// stream), defined here so there is exactly one definition.
pub fn is_special(text: &str) -> bool {
    text.starts_with("[_") || text.starts_with("<|")
}

/// Wire cap on one request's sample count (~62 min at 16 kHz) — a desynced
/// stream must not read as a multi-GB allocation.
pub const MAX_SAMPLES: usize = 60_000_000;

/// One normalized raw whisper token: `[text, t0_s, t1_s, p]`. The exact
/// input `group_tokens` consumed in-process — any lossy field here is a
/// caption fork, which bar A3's byte diff exists to catch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WireToken(pub String, pub f64, pub f64, pub f32);

/// One decode request header line. `op` is versioning headroom (only
/// "decode" exists); the knobs mirror `FullParams` field-for-field and
/// arrive RESOLVED (env reads happen app-side, next to their doc'd
/// constants).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DecodeRequest {
    pub op: String,
    /// whisper language code ("en" / "id" / "ja"), from `lang_code`.
    pub language: String,
    /// `initial_prompt` (dialect-store priming); empty = none.
    pub prompt: String,
    /// `Some(n)` = beam search (the caption path, ADR 0027); `None` = greedy
    /// `best_of: 1` (the detect refine's bulk text-only pass).
    pub beam_size: Option<i32>,
    pub no_context: bool,
    pub suppress_nst: bool,
    /// Silero VAD model path when the caption path opted in (`YC_VAD=1`).
    pub vad_model: Option<String>,
    /// Exactly this many f32-LE samples follow the header's newline.
    pub n_samples: usize,
}

/// One response line: the ready ack (`ok`, empty tokens), a decode result
/// (`ok` + tokens), or a failure (`!ok` + error; the child stays alive for
/// the next request unless the failure WAS a child death — which the client
/// detects by the exit, not by a line).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DecodeResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub tokens: Vec<WireToken>,
}

impl DecodeResponse {
    pub fn ready() -> Self {
        Self { ok: true, error: String::new(), tokens: Vec::new() }
    }
    pub fn tokens(tokens: Vec<WireToken>) -> Self {
        Self { ok: true, error: String::new(), tokens }
    }
    pub fn err(error: String) -> Self {
        Self { ok: false, error, tokens: Vec::new() }
    }
}

/// How many child stderr lines the client keeps for the death report.
const STDERR_TAIL: usize = 40;

/// Resolve `yc-whisper.exe`: `YC_WHISPER_EXE` override, else beside the
/// current exe (the app's layout — built next to `yt-clipper.exe` like
/// `yc-llm-judge`), else the exe dir's parent (cargo examples live in
/// `target/<profile>/examples/`).
fn sidecar_exe() -> Result<PathBuf> {
    if let Some(over) = std::env::var_os("YC_WHISPER_EXE") {
        let p = PathBuf::from(over);
        anyhow::ensure!(p.is_file(), "YC_WHISPER_EXE points at {}, which is not a file", p.display());
        return Ok(p);
    }
    let exe = std::env::current_exe().context("resolving current exe")?;
    let dir = exe.parent().context("current exe has no parent dir")?;
    let mut candidates = vec![dir.join("yc-whisper.exe")];
    if let Some(parent) = dir.parent() {
        candidates.push(parent.join("yc-whisper.exe"));
    }
    for cand in &candidates {
        if cand.is_file() {
            return Ok(cand.clone());
        }
    }
    anyhow::bail!(
        "yc-whisper.exe not found beside {} (or one dir up). It is built with the app \
         (scripts\\build-release.bat, or `cargo build -p yc-whisper`); reinstall/rebuild to \
         restore it, or point YC_WHISPER_EXE at it.",
        dir.display()
    )
}

/// A live `yc-whisper` child holding one whisper model resident on the GPU.
/// Owned by `Transcriber`; strictly serial. Drop kills the child — that IS
/// the model release.
pub(crate) struct SidecarConn {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    lines: mpsc::Receiver<String>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

impl SidecarConn {
    /// Spawn the child and wait for its ready line (the model load — multi
    /// second; blocking, exactly like the in-process load it replaces). A
    /// load-time death (missing model, GPU busy) returns the child's error
    /// with its stderr tail.
    pub(crate) fn spawn(model: &Path, dtw: bool) -> Result<Self> {
        let exe = sidecar_exe()?;
        let mut cmd = std::process::Command::new(&exe);
        cmd.no_console()
            .arg("--model")
            .arg(model)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if dtw {
            cmd.arg("--dtw");
        }
        let mut child =
            cmd.spawn().with_context(|| format!("spawning yc-whisper at {}", exe.display()))?;

        let stdout = child.stdout.take().context("yc-whisper stdout unavailable")?;
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = child.stderr.take().context("yc-whisper stderr unavailable")?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL)));
        let tail_writer = Arc::clone(&stderr_tail);
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                // The child's stderr carries whisper.cpp/ggml logs (routed
                // through its tracing) — forward at debug so a dev console
                // sees them without flooding the info-level default.
                tracing::debug!(target: "yc_whisper", "{line}");
                let mut tail = tail_writer.lock().unwrap_or_else(|p| p.into_inner());
                if tail.len() >= STDERR_TAIL {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });

        let stdin = child.stdin.take().context("yc-whisper stdin unavailable")?;
        let mut conn = Self { child, stdin: Some(stdin), lines, stderr_tail };
        let ready = conn.await_response(|| false).context("waiting for yc-whisper to load")?;
        anyhow::ensure!(
            ready.ok,
            "yc-whisper failed to load {}: {}",
            model.display(),
            ready.error
        );
        Ok(conn)
    }

    /// Send one decode and await its response. `should_abort` is polled every
    /// ~50 ms; on abort the child is KILLED (mid-decode cancel — the model's
    /// VRAM is freed by process death) and this returns "cancelled".
    pub(crate) fn decode(
        &mut self,
        req: &DecodeRequest,
        samples: &[f32],
        should_abort: impl FnMut() -> bool,
    ) -> Result<Vec<WireToken>> {
        debug_assert_eq!(req.n_samples, samples.len(), "header/sample count must agree");
        let mut buf = serde_json::to_vec(req).context("serializing decode request")?;
        buf.push(b'\n');
        buf.reserve(samples.len() * 4);
        for s in samples {
            buf.extend_from_slice(&s.to_le_bytes());
        }
        let write = (|| {
            let stdin = self.stdin.as_mut().context("yc-whisper stdin already closed")?;
            stdin.write_all(&buf)?;
            stdin.flush()?;
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(e) = write {
            // A broken pipe mid-write means the child died under us (the
            // GPU-contention abort class) — report it as the death it is.
            return Err(self.death_report(format!("writing decode request failed: {e:#}")));
        }
        let resp = self.await_response(should_abort)?;
        anyhow::ensure!(resp.ok, "yc-whisper decode failed: {}", resp.error);
        Ok(resp.tokens)
    }

    /// Await one response line; poll `should_abort` and child liveness while
    /// waiting. No decode watchdog by design (ADR 0072): a GPU-contention
    /// CRAWL is a slow decode, not a dead one — quality over runtime.
    fn await_response(&mut self, mut should_abort: impl FnMut() -> bool) -> Result<DecodeResponse> {
        loop {
            match self.lines.recv_timeout(Duration::from_millis(50)) {
                Ok(line) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    return serde_json::from_str(line.trim())
                        .with_context(|| format!("parsing yc-whisper response: {line}"));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if should_abort() {
                        self.kill();
                        anyhow::bail!("cancelled");
                    }
                    if let Ok(Some(status)) = self.child.try_wait() {
                        return Err(self.death_report(format!("exited with {status}")));
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // stdout closed without a response: the child is dead or
                    // dying — reap it for the status.
                    let status = self
                        .child
                        .wait()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|e| format!("unwaitable: {e}"));
                    return Err(self.death_report(format!("exited with {status}")));
                }
            }
        }
    }

    /// The child died mid-work — almost always the GPU-contention class
    /// (ggml aborts on a failed CUDA allocation). Say so, with its last
    /// stderr lines, and make the retry-when-free action explicit.
    fn death_report(&mut self, what: String) -> anyhow::Error {
        // Give the stderr reader a beat to drain the final lines.
        std::thread::sleep(Duration::from_millis(150));
        let tail: Vec<String> = {
            let tail = self.stderr_tail.lock().unwrap_or_else(|p| p.into_inner());
            tail.iter().cloned().collect()
        };
        let tail = if tail.is_empty() { "  (no stderr captured)".to_string() } else { tail.join("\n  ") };
        anyhow::anyhow!(
            "the whisper engine (yc-whisper) died mid-job: {what}. The GPU was most likely \
             busy — another program holding video memory (a game, an editor) starves the \
             decode and ggml aborts. Close it and retry the job; the app itself is fine.\n\
             yc-whisper stderr tail:\n  {tail}"
        )
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for SidecarConn {
    fn drop(&mut self) {
        // Dropping stdin EOFs a healthy idle child; the kill right after
        // covers a busy one (mid-decode on a contended GPU) so the model's
        // VRAM is released NOW — the same synchronous free the in-process
        // drop gave (ADR 0007's staging discipline depends on it).
        self.stdin = None;
        self.kill();
    }
}
