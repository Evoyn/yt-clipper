//! Qwen3-ASR ensemble captions (opt-in, `YC_QWEN_ENS=1`) — words from a
//! multi-decode vote, timing from whisper.
//!
//! Measured basis (2026-07-02, the "Diskusi biasa" benchmark, operator ground
//! truth in `benchmarks/diskusi-biasa.groundtruth.txt`): near whisper's limit
//! on masked fast slang, ANY single decode — whisper's or Qwen3-ASR's, any
//! knob — garbles a different region (near-tie trajectories, ADR 0033's
//! finding). But the garbles ANTICORRELATE across decode variants (denoise
//! strength, context padding, biasing text), so a token-level plurality vote
//! across variants recovers what no single decode can: on the benchmark, the
//! ensemble read real speech through whisper's 107-"eh" hallucination pile,
//! recovered a 3 s missed-speech hole, "bangke", "bajingan bajingan", and
//! "pusing cok" from the plain mix — with zero dialect-store input.
//!
//! Shape: whisper still decodes the clip (it owns TIMING — DTW word onsets,
//! the karaoke path); the Qwen ensemble owns WORDS. The merged word stream is
//! aligned back onto whisper's units: matched words adopt whisper's span,
//! whisper-only units DROP (they are the hallucination class the vote
//! outvoted), ensemble-only runs get character-proportional spans inside the
//! enclosing whisper gap. Downstream (refine, ASS, karaoke, LLM correct) is
//! unchanged.
//!
//! ADR 0033 compliance: strictly opt-in (env knob, default OFF = byte-identical
//! renders), read here so the render and any diag can't diverge; the dialect
//! store is neither read nor written by the ensemble (auto-harvest is skipped
//! by the caller when the ensemble ran — harvest indexes are whisper-keyed).
//! GPU staging stays sequential: whisper's one-shot load is dropped before the
//! sidecar spawns (llama-mtmd-cli exits between variants).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use yc_core::{wait_killable, CaptionUnit, Language, NoConsole, TimeRange, Transcript};

/// The per-invocation engine override, tri-state since the per-Creator Caption
/// engine landed (ADR 0035): `Some(true)` forces the ensemble, `Some(false)`
/// forces whisper, `None` (unset / unrecognized) defers to the Creator's saved
/// engine. Both directions exist so the headless gate fixtures (ADR 0034) can
/// pin either engine regardless of how the operator has flipped a Creator —
/// the override is per-run and is never written back to the Creator store.
pub fn engine_override() -> Option<bool> {
    override_from(std::env::var("YC_QWEN_ENS").ok().as_deref())
}

/// Pure parse of the `YC_QWEN_ENS` value, split out so the tri-state contract
/// is unit-testable without touching process env.
fn override_from(raw: Option<&str>) -> Option<bool> {
    match raw.map(str::trim) {
        Some("1") | Some("true") | Some("on") => Some(true),
        Some("0") | Some("false") | Some("off") => Some(false),
        _ => None,
    }
}

/// Everything the ensemble needs from the caller (paths are derived by the
/// caller from its own sidecar/model layout; nothing here reads global state).
pub struct EnsembleConfig {
    /// llama-mtmd-cli.exe (pinned llama.cpp sidecar, fetch-llama-sidecar.ps1).
    pub mtmd_cli: PathBuf,
    /// Qwen3-ASR GGUF pair (fetch-models.ps1).
    pub qwen_model: PathBuf,
    pub qwen_mmproj: PathBuf,
    pub ffmpeg: PathBuf,
    /// deep-filter.exe when present — without it the denoised variants are
    /// skipped and the vote runs on the remaining decodes (weaker, still valid).
    pub deep_filter: Option<PathBuf>,
    /// Scratch directory for the variant wavs (the session's data dir).
    pub work_dir: PathBuf,
    /// Polled between stages and ~20x/s while any child runs; a `true` return
    /// kills the running child and aborts with a "cancelled" error. Before
    /// this existed the operator's Cancel was a NO-OP for the whole ensemble —
    /// every child ran to completion under a dead button, measured at 18 min
    /// per decode under GPU contention (2026-07-03).
    pub should_cancel: Box<dyn Fn() -> bool + Send + Sync>,
    /// Called as `(variant_number, variant_total)` right before each sidecar
    /// decode, so the UI can show "decode 2/5" instead of one static label
    /// over the longest stage the app has.
    pub on_stage: Box<dyn Fn(usize, usize) + Send + Sync>,
}

/// A sidecar decode that exceeded its wall-clock budget. Typed (not just a
/// message) so [`apply`] can tell it from an ordinary variant failure: one
/// timeout aborts the WHOLE ensemble, because the budget only trips when the
/// GPU is oversubscribed (WDDM silently demotes CUDA allocations to system
/// RAM and a ~12 s decode measures 18 minutes — 2026-07-03) and those
/// conditions hold for every remaining variant too. The caller then falls
/// back to whisper captions instead of crawling for an hour.
#[derive(Debug)]
pub struct DecodeTimeout {
    pub budget_s: u64,
}

impl std::fmt::Display for DecodeTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "decode exceeded its {} s budget (is the GPU busy with something else?)", self.budget_s)
    }
}

impl std::error::Error for DecodeTimeout {}

/// Spawn a stdio-inheriting child and wait killably: `cfg.should_cancel` is
/// polled ~20x/s and flipping it kills the child mid-run (the ffmpeg cuts and
/// deep-filter passes used to be waited on with a plain `.status()`, immune to
/// Cancel).
fn run_killable(cfg: &EnsembleConfig, cmd: &mut Command, what: &str) -> Result<()> {
    let mut child = cmd.spawn().with_context(|| format!("spawning {what}"))?;
    let status = wait_killable(&mut child, &*cfg.should_cancel)
        .with_context(|| format!("waiting on {what}"))?;
    let Some(status) = status else {
        anyhow::bail!("cancelled");
    };
    anyhow::ensure!(status.success(), "{what} failed ({status})");
    Ok(())
}

/// Seconds of leading audio context prepended to the clip range for the padded
/// variants. Measured: the decode "warms up" on the preceding speech and reads
/// the clip's opening better; TAIL padding only donates bleed, so none is used.
const HEAD_PAD_S: f64 = 5.0;

/// One decode variant: which audio view + how it is biased.
struct Variant {
    /// deep-filter attenuation limit (None = raw mix).
    atten: Option<u32>,
    head_pad: bool,
}

/// The measured voter set (H1/E8/E10a/E12-shaped). Order matters only for the
/// backbone: the first variant is the merge backbone (a6 + head-pad — the
/// benchmark's best-shaped single decode).
const VARIANTS: &[Variant] = &[
    Variant { atten: Some(6), head_pad: true },
    Variant { atten: Some(12), head_pad: false },
    Variant { atten: Some(6), head_pad: false },
    Variant { atten: Some(12), head_pad: true },
    Variant { atten: None, head_pad: false },
];

/// Biasing context handed to Qwen3-ASR's system prompt. GENERIC per language —
/// a global gaming-stream lexicon, never per-clip or per-Creator content (the
/// no-curation contract; measured to fix profanity bowdlerization, e.g.
/// "bacingan" -> "bajingan"). Long lists measurably DERAIL the decode — keep it
/// one sentence.
fn bias_context(language: Language) -> &'static str {
    match language {
        Language::Id => {
            "The audio is in Indonesian (bahasa Indonesia) from a horror gaming \
             livestream. Common slang and profanity that may occur: anjing, anying, \
             bangke, bajingan, cok, goblok, kampret, biadab, ngeri, pusing, njir, buset."
        }
        Language::En => "The audio is in English, from a gaming livestream.",
        Language::Ja => "The audio is in Japanese, from a gaming livestream.",
    }
}

/// Run the full ensemble for one clip range and fuse the voted words onto the
/// whisper transcript's timing. `whisper` is the production transcript for the
/// same range (its units carry the DTW times; its words join the vote pool).
/// Fails soft at the caller (whisper captions stand when anything here errors).
pub fn apply(
    cfg: &EnsembleConfig,
    analysis_wav: &Path,
    range: TimeRange,
    whisper: &Transcript,
    timing_extra: Option<&Transcript>,
    samples: &[f32],
    sample_rate: u32,
    lexicon: &crate::DialectLexicon,
) -> Result<Transcript> {
    // Cancel check FIRST — a cancel that landed during the preceding whisper
    // decodes must not even touch the filesystem here.
    anyhow::ensure!(!(cfg.should_cancel)(), "cancelled");
    anyhow::ensure!(cfg.mtmd_cli.is_file(), "mtmd sidecar missing: {}", cfg.mtmd_cli.display());
    anyhow::ensure!(cfg.qwen_model.is_file(), "qwen model missing: {}", cfg.qwen_model.display());
    anyhow::ensure!(
        cfg.qwen_mmproj.is_file(),
        "qwen mmproj missing: {}",
        cfg.qwen_mmproj.display()
    );
    std::fs::create_dir_all(&cfg.work_dir)?;

    // Watchdog budget per decode: healthy runs measure 3-5x realtime on this
    // class of GPU (a ~60 s clip decodes in 10-20 s), so 1.5x the audio plus
    // 30 s of load grace is ~6-10x healthy — it only trips the pathological
    // class (VRAM-oversubscription crawl, ~75x measured 2026-07-03).
    let budget = Duration::from_secs_f64(
        (30.0 + 1.5 * (range.duration_s() + HEAD_PAD_S)).max(120.0),
    );

    // --- 1. decode variants (one-shot sidecar spawns; GPU-sequential) -------
    let mut decodes: Vec<Vec<String>> = Vec::new();
    let mut onset_wav: Option<PathBuf> = None;
    for (i, v) in VARIANTS.iter().enumerate() {
        if v.atten.is_some() && cfg.deep_filter.is_none() {
            continue;
        }
        anyhow::ensure!(!(cfg.should_cancel)(), "cancelled");
        (cfg.on_stage)(i + 1, VARIANTS.len());
        let wav = match variant_wav(cfg, analysis_wav, range, v, i) {
            Ok(w) => w,
            Err(e) => {
                // A killed child surfaces as an ordinary error — distinguish
                // the operator's cancel from a real prep failure.
                anyhow::ensure!(!(cfg.should_cancel)(), "cancelled");
                tracing::warn!("qwen ensemble: variant {i} audio prep failed: {e:#}");
                continue;
            }
        };
        // The strongest no-pad denoised view doubles as the ONSET source:
        // on a loud mix the raw RMS envelope never dips below the silence
        // bar (no onsets -> uniform smear placement), while the cleaned
        // audio exposes the real speech starts. No-pad only: a padded wav's
        // clock is 5 s ahead of the clip's.
        if v.atten.is_some() && !v.head_pad && onset_wav.is_none() {
            onset_wav = Some(wav.clone());
        }
        match decode_one(cfg, &wav, whisper.language, budget) {
            Ok(words) if !words.is_empty() => {
                tracing::info!(
                    "qwen ensemble: variant {i} (atten={:?} pad={}) -> {} words",
                    v.atten,
                    v.head_pad,
                    words.len()
                );
                decodes.push(words);
            }
            Ok(_) => tracing::warn!("qwen ensemble: variant {i} produced no words"),
            Err(e) if e.downcast_ref::<DecodeTimeout>().is_some() => {
                // One crawl means they'd ALL crawl — abort the ensemble now
                // (the caller falls back to whisper) instead of burning the
                // budget four more times.
                return Err(e.context(format!("variant {i} decode timed out; aborting the ensemble")));
            }
            Err(e) => {
                anyhow::ensure!(!(cfg.should_cancel)(), "cancelled");
                tracing::warn!("qwen ensemble: variant {i} decode failed: {e:#}");
            }
        }
    }
    anyhow::ensure!(decodes.len() >= 2, "qwen ensemble: <2 variants decoded, vote impossible");

    // --- 2. vote (whisper's words join as a voter; backbone = variant 0) ----
    let whisper_words: Vec<String> =
        whisper.units.iter().flat_map(|u| normalize(&u.text)).collect();
    let backbone = decodes[0].clone();
    let mut voters: Vec<Vec<String>> = decodes[1..].to_vec();
    if !whisper_words.is_empty() {
        voters.push(whisper_words);
    }
    let merged = vote_merge(&backbone, &voters);
    tracing::info!(
        "qwen ensemble: vote over {} voters + backbone -> {} words",
        voters.len(),
        merged.len()
    );

    // --- 3. fuzzy store application (the curation-survives-engine-swap tier) ---
    // The dialect store's `wrong` keys are whisper's exact garbles (ADR 0033),
    // so they never literally match another engine's spelling of the SAME
    // mishear (whisper "dijekat" vs the ensemble's "dijegat"). An edit-1
    // tolerant match transfers the operator's existing confirmed corrections
    // across engines — no new curation. Guards make it conservative: single-
    // word wrongs of >=4 chars, never on a token that is a real dictionary
    // word, context-sensitive pairs skipped (they need the LLM pass by design).
    let mut merged = merged;
    apply_store_fuzzy(&mut merged, lexicon);

    // --- 4. fuse words onto the anchor skeleton + speech onsets -------------
    // Onset source: the denoised no-pad variant when it exists (captured in
    // the decode loop), else the mix samples the render already holds.
    let onset_samples: Option<Vec<f32>> = onset_wav.and_then(|w| {
        match wav_samples_f32(&cfg.ffmpeg, &w) {
            Ok(s) if !s.is_empty() => Some(s),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!("qwen ensemble: cleaned-onset read failed (mix onsets stand): {e:#}");
                None
            }
        }
    });
    let onset_src: &[f32] = onset_samples.as_deref().unwrap_or(samples);
    let mut fused = fuse_onto_timing(
        &merged,
        whisper,
        timing_extra,
        onset_src,
        sample_rate,
        range.duration_s(),
    );
    tracing::info!(
        "qwen ensemble: fused {} units (whisper had {}, extra skeleton {})",
        fused.len(),
        whisper.units.len(),
        timing_extra.map(|t| t.units.len()).unwrap_or(0)
    );

    // --- 5. positional (time-anchored) store pass ----------------------------
    // Corrections with an `at_s` pin apply AFTER fusion, to the occurrence
    // nearest their moment — the operator's ear as ground truth for WHERE a
    // word belongs, not just what it is (the time-anchored-curation seed).
    apply_store_positional(
        &mut fused,
        lexicon,
        range.start_s,
        &rms_onsets(onset_src, sample_rate),
        range.duration_s(),
    );
    Ok(Transcript { language: whisper.language, units: fused })
}

/// Cut (and optionally denoise) one variant's audio view of the range.
fn variant_wav(
    cfg: &EnsembleConfig,
    analysis_wav: &Path,
    range: TimeRange,
    v: &Variant,
    idx: usize,
) -> Result<PathBuf> {
    let start = if v.head_pad { (range.start_s - HEAD_PAD_S).max(0.0) } else { range.start_s };
    let dur = range.end_s - start;
    let cut = cfg.work_dir.join(format!("_ens_{idx}.wav"));
    let mut cmd = Command::new(&cfg.ffmpeg);
    cmd.no_console()
        .args(["-y", "-hide_banner", "-loglevel", "error", "-ss"])
        .arg(format!("{start}"))
        .arg("-t")
        .arg(format!("{dur}"))
        .arg("-i")
        .arg(analysis_wav)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(&cut);
    run_killable(cfg, &mut cmd, "ffmpeg (ensemble cut)")?;
    let Some(atten) = v.atten else { return Ok(cut) };
    let df = cfg.deep_filter.as_ref().expect("checked by caller");
    // deep-filter keeps the input basename in the output dir.
    let out_dir = cfg.work_dir.join(format!("_ens_df{idx}"));
    std::fs::create_dir_all(&out_dir)?;
    let mut cmd = Command::new(df);
    cmd.no_console()
        .arg("-a")
        .arg(atten.to_string())
        .arg("-o")
        .arg(&out_dir)
        .arg(&cut);
    run_killable(cfg, &mut cmd, "deep-filter (ensemble variant)")?;
    Ok(out_dir.join(cut.file_name().expect("cut has a name")))
}

/// Decode a wav to mono 16 kHz f32 samples via the bundled ffmpeg (the
/// deep-filter output's sample format is its own business — ffmpeg
/// normalizes it to the render's analysis format).
fn wav_samples_f32(ffmpeg: &Path, wav: &Path) -> Result<Vec<f32>> {
    let out = Command::new(ffmpeg)
        .no_console()
        .args(["-v", "error", "-i"])
        .arg(wav)
        .args(["-f", "f32le", "-ac", "1", "-ar", "16000", "-"])
        .output()
        .context("spawning ffmpeg for onset samples")?;
    anyhow::ensure!(out.status.success(), "ffmpeg onset decode failed ({})", out.status);
    Ok(out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// One one-shot llama-mtmd-cli decode -> normalized words, under the cancel
/// poll and a wall-clock `budget` (see [`DecodeTimeout`]). Head-padded
/// variants keep their extra leading words: the vote's strict-majority insert
/// rule outvotes pad bleed, and the timing fusion drops anything whisper's
/// range has no anchor or gap for.
fn decode_one(
    cfg: &EnsembleConfig,
    wav: &Path,
    language: Language,
    budget: Duration,
) -> Result<Vec<String>> {
    // The sidecar cannot be handed the wav's full path: `--audio` is a list
    // flag that SPLITS ON COMMAS, and stream folders carry VOD-title text
    // ("... Tretan, Coki, Adriano", emoji) that also trips its C-level file
    // open. The variant file NAME is ours (`_ens_N.wav`, pure ASCII) — spawn
    // in the wav's directory and pass the bare name; exe/model paths are
    // absolutized so the cwd change cannot break them.
    let dir = match wav.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let name = wav.file_name().context("variant wav has no file name")?;
    let mut child = Command::new(std::path::absolute(&cfg.mtmd_cli)?)
        .no_console()
        .current_dir(dir)
        .arg("-m")
        .arg(std::path::absolute(&cfg.qwen_model)?)
        .arg("--mmproj")
        .arg(std::path::absolute(&cfg.qwen_mmproj)?)
        .arg("--audio")
        .arg(name)
        .args(["--temp", "0", "-ngl", "99", "-p", "Transcribe the audio."])
        .args(["-sys", bias_context(language)])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning llama-mtmd-cli")?;
    // Both pipes are drained on threads so the child can never block on a full
    // pipe buffer while this thread only polls (llama.cpp writes its whole
    // load log to stderr — more than a pipe holds). After a kill the pipes
    // close and the readers finish on their own.
    let mut out_pipe = child.stdout.take().expect("stdout piped above");
    let mut err_pipe = child.stderr.take().expect("stderr piped above");
    let out_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut out_pipe, &mut buf);
        buf
    });
    let err_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut err_pipe, &mut buf);
        buf
    });
    let started = Instant::now();
    let status = loop {
        if (cfg.should_cancel)() {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("cancelled");
        }
        if started.elapsed() >= budget {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow::Error::new(DecodeTimeout { budget_s: budget.as_secs() }));
        }
        match child.try_wait().context("waiting on llama-mtmd-cli")? {
            Some(s) => break s,
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let stdout = out_h.join().unwrap_or_default();
    let stderr = err_h.join().unwrap_or_default();
    if !status.success() {
        // llama.cpp logs load noise first and any fatal line last — report the tail.
        let err = String::from_utf8_lossy(&stderr);
        let skip = err.chars().count().saturating_sub(600);
        anyhow::bail!(
            "llama-mtmd-cli failed ({}): {}",
            status,
            err.chars().skip(skip).collect::<String>()
        );
    }
    Ok(normalize(&String::from_utf8_lossy(&stdout)))
}

/// Lowercased word tokens; strips Qwen's `language X<asr_text>` header and all
/// punctuation. The same normalization the benchmark scorer uses.
pub fn normalize(text: &str) -> Vec<String> {
    let t = match text.find("<asr_text>") {
        Some(i) => &text[i + "<asr_text>".len()..],
        None => text,
    };
    t.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Ok,
    Sub,
    Del,
    Ins,
}

/// Token-level Levenshtein alignment (ops keyed to `a`=left, `b`=right).
fn align(a: &[String], b: &[String]) -> Vec<(Op, Option<usize>, Option<usize>)> {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0u32; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i as u32;
    }
    for j in 0..=m {
        d[0][j] = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[i - 1][j - 1] + u32::from(a[i - 1] != b[j - 1]);
            d[i][j] = sub.min(d[i - 1][j] + 1).min(d[i][j - 1] + 1);
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && d[i][j] == d[i - 1][j - 1] && a[i - 1] == b[j - 1] {
            ops.push((Op::Ok, Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if i > 0 && j > 0 && d[i][j] == d[i - 1][j - 1] + 1 {
            ops.push((Op::Sub, Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if i > 0 && d[i][j] == d[i - 1][j] + 1 {
            ops.push((Op::Del, Some(i - 1), None));
            i -= 1;
        } else {
            ops.push((Op::Ins, None, Some(j - 1)));
            j -= 1;
        }
    }
    ops.reverse();
    ops
}

/// Marker: a voter says "the backbone token isn't there".
const DEL_VOTE: &str = "\u{1}del";

/// Plurality vote per backbone position (backbone breaks ties); inserted runs
/// need a STRICT voter majority (the backbone implicitly votes "no", and half
/// an even pool sharing a padding artifact must not carry it). Ported verbatim
/// from the benchmark-validated `asr_merge` inspector.
pub fn vote_merge(backbone: &[String], voters: &[Vec<String>]) -> Vec<String> {
    let n = backbone.len();
    let mut votes: Vec<Vec<String>> = vec![Vec::new(); n];
    let mut gap_runs: Vec<Vec<Vec<String>>> = vec![Vec::new(); n + 1];
    for cand in voters {
        let ops = align(backbone, cand);
        let mut pending: Vec<String> = Vec::new();
        for (op, bi, ci) in ops {
            match op {
                Op::Ok | Op::Sub => {
                    let i = bi.expect("ok/sub keeps a backbone index");
                    if !pending.is_empty() {
                        gap_runs[i].push(std::mem::take(&mut pending));
                    }
                    votes[i].push(cand[ci.expect("ok/sub keeps a voter index")].clone());
                }
                Op::Del => {
                    let i = bi.expect("del keeps a backbone index");
                    if !pending.is_empty() {
                        gap_runs[i].push(std::mem::take(&mut pending));
                    }
                    votes[i].push(DEL_VOTE.to_string());
                }
                Op::Ins => pending.push(cand[ci.expect("ins keeps a voter index")].clone()),
            }
        }
        if !pending.is_empty() {
            gap_runs[n].push(pending);
        }
    }
    let majority = voters.len() / 2 + 1;
    let mut merged: Vec<String> = Vec::new();
    for i in 0..=n {
        if !gap_runs[i].is_empty() {
            let mut by_first: std::collections::HashMap<&str, Vec<&Vec<String>>> =
                std::collections::HashMap::new();
            for run in &gap_runs[i] {
                by_first.entry(run[0].as_str()).or_default().push(run);
            }
            if let Some((_, runs)) = by_first
                .iter()
                .max_by_key(|(_, v)| v.len())
                .filter(|(_, v)| v.len() >= majority)
            {
                let mut counts: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for r in runs {
                    *counts.entry(r.join(" ")).or_default() += 1;
                }
                let run = counts.into_iter().max_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0))).unwrap().0;
                merged.extend(run.split_whitespace().map(String::from));
            }
        }
        if i == n {
            break;
        }
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        *counts.entry(backbone[i].as_str()).or_default() += 1;
        for v in &votes[i] {
            *counts.entry(v.as_str()).or_default() += 1;
        }
        let backbone_count = counts[backbone[i].as_str()];
        let (winner, wc) = counts
            .iter()
            // deterministic: count first, then backbone-preference, then lexical
            .max_by_key(|(t, c)| (**c, **t == backbone[i].as_str(), std::cmp::Reverse(**t)))
            .map(|(t, c)| (t.to_string(), *c))
            .expect("counts never empty");
        let winner = if wc == backbone_count { backbone[i].clone() } else { winner };
        if winner != DEL_VOTE {
            merged.push(winner);
        }
    }
    merged
}

/// How a Creator's confirmed corrections split across the engine boundary
/// (ADR 0035 §2) — the numbers behind the GUI's engine-switch warn. Classes
/// mirror the two ensemble appliers' filters EXACTLY (below /
/// [`apply_store_positional`]); if those filters change, this must too.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferCounts {
    /// Single-word, un-pinned: carry to the ensemble via the fuzzy tier
    /// (exact or edit-1 token match).
    pub single_word: usize,
    /// `at_s`-pinned: carry via the positional pass (multi-word wrongs
    /// included — they match consecutive fused units).
    pub pinned: usize,
    /// Multi-word, un-pinned: whisper's global dict only — a whisper garble
    /// SHAPE that doesn't transfer across engines.
    pub multi_word: usize,
    /// Context-gated (ADR 0030): authored against whisper's confident
    /// spellings; the ensemble's store passes skip them. (The opt-in LLM
    /// pass still sees them on either engine, but per-occurrence judgment
    /// there is the exception, not the transfer contract.)
    pub context: usize,
}

impl TransferCounts {
    /// Corrections that keep working after a flip to the ensemble.
    pub fn carries(&self) -> usize {
        self.single_word + self.pinned
    }

    /// Corrections that only fire on whisper renders.
    pub fn stays(&self) -> usize {
        self.multi_word + self.context
    }

    pub fn total(&self) -> usize {
        self.carries() + self.stays()
    }
}

/// Classify a store's CONFIRMED corrections for the engine-switch warn
/// (ADR 0035 §2). Unverified entries (blank `right` — the review queue's
/// to-dos) apply on no engine and are not counted.
pub fn transfer_counts(corrections: &[crate::Correction]) -> TransferCounts {
    let mut t = TransferCounts::default();
    for c in corrections {
        if c.right.is_empty() || normalize(&c.wrong).is_empty() {
            continue;
        }
        if c.context {
            t.context += 1;
        } else if c.at_s.is_some() {
            t.pinned += 1;
        } else if normalize(&c.wrong).len() == 1 {
            t.single_word += 1;
        } else {
            t.multi_word += 1;
        }
    }
    t
}

/// Transfer the store's confirmed corrections onto the voted words with
/// edit-1 tolerance (see the call site for why exact keys can't match across
/// engines). Two tiers per single-word `wrong`: exact token match, or — only
/// when the token is NOT a real dictionary word and the wrong is >=4 chars —
/// an edit-1 match. `context: true` pairs are skipped (LLM-pass territory,
/// ADR 0030); multi-word wrongs are skipped in this tier (whisper's
/// multi-word garble shapes don't transfer across engines). `right` may be
/// multi-word; it splices in normalized.
pub fn apply_store_fuzzy(words: &mut Vec<String>, lexicon: &crate::DialectLexicon) {
    let pairs: Vec<(String, Vec<String>)> = lexicon
        .corrections
        .iter()
        // at_s entries are positional: they run AFTER fusion, on the occurrence
        // nearest their moment (apply_store_positional), never globally here.
        .filter(|c| !c.right.is_empty() && !c.context && c.at_s.is_none())
        .filter_map(|c| {
            let wrong = normalize(&c.wrong);
            if wrong.len() != 1 {
                return None;
            }
            Some((wrong.into_iter().next().expect("len checked"), normalize(&c.right)))
        })
        .collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    for tok in words.iter() {
        let exact = pairs.iter().find(|(w, _)| w == tok);
        let hit = exact.or_else(|| {
            if lexicon.dictionary.contains(tok.as_str()) {
                return None;
            }
            pairs.iter().find(|(w, _)| w.chars().count() >= 4 && edit1(tok, w))
        });
        match hit {
            Some((w, right)) if right != &[tok.clone()] => {
                tracing::info!("qwen ensemble: store fuzzy \"{tok}\" (~\"{w}\") -> \"{}\"", right.join(" "));
                out.extend(right.iter().cloned());
            }
            _ => out.push(tok.clone()),
        }
    }
    *words = out;
}

/// True when `a` and `b` are within one edit (sub/ins/del) of each other.
fn edit1(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (n, m) = (a.len(), b.len());
    if n.abs_diff(m) > 1 {
        return false;
    }
    if n == m {
        return a.iter().zip(&b).filter(|(x, y)| x != y).count() <= 1;
    }
    let (long, short) = if n > m { (&a, &b) } else { (&b, &a) };
    let mut i = 0;
    while i < short.len() && long[i] == short[i] {
        i += 1;
    }
    long[i + 1..] == short[i..]
}

/// Align the voted words onto a TIMED ANCHOR skeleton and lay the rest onto
/// speech-energy onsets.
///
/// The skeleton is the time-sorted union of the production whisper units and
/// (when provided) a second whisper decode's units — on masked clips the
/// default decode's spans are as unreliable as its words (a 4 s phantom unit
/// over three real words, holes over real speech; ADR 0033's measurements),
/// while the `suppress_nst` decode places units exactly where the default is
/// blind. Its WORDS stay out of the vote (they re-garble, ADR 0033) — only
/// its time grid is used.
///
/// Matching is similarity-gated: a DP substitution only adopts an anchor's
/// span when the two tokens look like the same word (edit distance <= 2 or a
/// prefix), otherwise the word falls through to onset placement — a text-blind
/// substitution would drag a correct word onto a DIFFERENT word's (possibly
/// phantom) anchor, which is exactly the "caption runs ahead of the streamer"
/// bug this replaces. Unmatched anchors are skipped entirely (a phantom must
/// not consume timeline). Inserted runs land on RMS onsets inside their gap
/// (rising crossings of the ADR 0021 silence bar), spreading
/// character-proportionally when the gap's onsets can't structure the run.
/// A skeleton whose anchors go mostly unclaimed (< 1/3 — the vote rejected
/// what the skeletons heard: the hallucination-pile clip class) is dropped
/// wholesale and every word onset-places instead.
/// Zero-width results are fine downstream (DTW single-token units already are).
pub fn fuse_onto_timing(
    merged: &[String],
    whisper: &Transcript,
    timing_extra: Option<&Transcript>,
    samples: &[f32],
    sample_rate: u32,
    clip_dur_s: f64,
) -> Vec<CaptionUnit> {
    // --- anchor skeleton: union of both decodes' units, time-sorted ---------
    let mut anchors: Vec<(f64, f64, String)> = whisper
        .units
        .iter()
        .chain(timing_extra.map(|t| t.units.iter()).unwrap_or_default())
        .map(|u| {
            let tok = normalize(&u.text).into_iter().next().unwrap_or_default();
            (u.start_s, u.end_s, tok)
        })
        .filter(|(_, _, t)| !t.is_empty())
        .collect();
    anchors.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let anchor_tokens: Vec<String> = anchors.iter().map(|(_, _, t)| t.clone()).collect();

    let onsets = rms_onsets(samples, sample_rate);
    let ops = align_weighted(&anchor_tokens, merged);

    // First pass: which merged word adopts which anchor (similarity-gated).
    let mut adopted: Vec<Option<usize>> = vec![None; merged.len()]; // merged idx -> anchor idx
    for (op, ai, mi) in &ops {
        match op {
            Op::Ok => adopted[mi.expect("ok keeps merged index")] = Some(ai.expect("ok keeps anchor")),
            Op::Sub => {
                let (ai, mi) = (ai.expect("sub keeps anchor"), mi.expect("sub keeps merged"));
                if similar_word(&anchor_tokens[ai], &merged[mi]) {
                    adopted[mi] = Some(ai);
                }
            }
            _ => {}
        }
    }
    // Enforce monotonic anchor starts across adoptions (two skeletons can
    // interleave): a later word may not adopt an anchor that starts before a
    // previously adopted one.
    let mut last_start = f64::NEG_INFINITY;
    for a in adopted.iter_mut() {
        if let Some(ai) = *a {
            if anchors[ai].0 < last_start {
                *a = None;
            } else {
                last_start = anchors[ai].0;
            }
        }
    }
    // Skeleton trust: when the vote rejected most of what the skeletons heard
    // (the 107-"eh" hallucination pile), the few claimed anchors are accidents
    // — a lone mid-pile "ada" match boxed 21 real words into a 0.12 s gap.
    // The signal is SKELETON-side (units left unclaimed), not word-side:
    // edit-1 lookalikes ("deh"/"es" against "eh") keep word-side adoption high
    // on exactly the clips whose skeleton is pure hallucination. Under 1/3 of
    // anchors claimed, drop the skeleton entirely and place every word by the
    // clip's speech onsets; the `at_s` store pass still pins operator-heard
    // moments afterwards.
    let adopted_n = adopted.iter().filter(|a| a.is_some()).count();
    if adopted_n * 3 < anchors.len() {
        tracing::info!(
            "qwen ensemble: skeleton distrusted ({adopted_n}/{} anchors claimed) \
             -> onset-spread fallback",
            anchors.len()
        );
        adopted.fill(None);
    }

    // Second pass: place words — adopted ones on their anchor span, runs of
    // unadopted ones onto onsets/proportional spread inside their gap.
    let mut fused: Vec<CaptionUnit> = Vec::new();
    let mut i = 0;
    let mut prev_end = 0.0_f64;
    while i < merged.len() {
        if let Some(ai) = adopted[i] {
            let (s, e, _) = anchors[ai];
            let s = s.max(prev_end.min(clip_dur_s)).min(clip_dur_s);
            fused.push(CaptionUnit { text: merged[i].clone(), start_s: s, end_s: e.max(s) });
            prev_end = e.max(s);
            i += 1;
            continue;
        }
        // run of unadopted words [i, j)
        let mut j = i;
        while j < merged.len() && adopted[j].is_none() {
            j += 1;
        }
        let gap_start = prev_end;
        let gap_end = if j < merged.len() {
            anchors[adopted[j].expect("loop bound")].0.max(gap_start)
        } else {
            clip_dur_s.max(gap_start)
        };
        place_run(&merged[i..j], gap_start, gap_end, &onsets, &mut fused);
        prev_end = fused.last().map(|u| u.end_s).unwrap_or(gap_start).max(gap_start);
        i = j;
    }
    respread_flashes(&mut fused, clip_dur_s);
    fused
}

/// Minimum readable width a fused word should get when its neighborhood has
/// the room (the refine pass extends further, but only into room that exists —
/// this pass CREATES the room by reclaiming unclaimed timeline).
const MIN_WORD_S: f64 = 0.15;

/// Redistribute flash-runs into their enclosing slack: when consecutive words
/// got squeezed against an anchor (alignment ambiguity between two similar
/// phrases can consume a later anchor and box the words in between) while
/// unclaimed timeline sits right next to them, re-place the whole run evenly
/// across the window between its timed neighbors. A run that is genuinely
/// boxed (window no bigger than the run) is left alone.
fn respread_flashes(fused: &mut [CaptionUnit], clip_dur_s: f64) {
    let n = fused.len();
    let mut i = 0;
    while i < n {
        if fused[i].end_s - fused[i].start_s >= MIN_WORD_S {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && fused[j].end_s - fused[j].start_s < MIN_WORD_S {
            j += 1;
        }
        let win_start = if i == 0 { 0.0 } else { fused[i - 1].end_s };
        let win_end = if j == n { clip_dur_s } else { fused[j].start_s };
        let need = (j - i) as f64 * MIN_WORD_S;
        if win_end - win_start > need {
            // RIGHT-aligned in the window, capped per word: the squeeze always
            // happens against the run's FOLLOWING anchor (that is where the
            // words actually belong — DP boxed them there), so the run stays
            // adjacent to it instead of drifting to the window's far start
            // (measured: a lone squeezed word handed the whole 3.4 s window
            // landed ~3 s before its speech).
            let total = ((j - i) as f64 * 0.5).min(win_end - win_start);
            let start = win_end - total;
            let total_chars: usize =
                fused[i..j].iter().map(|u| u.text.chars().count().max(1)).sum();
            let mut t = start;
            for u in fused[i..j].iter_mut() {
                let frac = u.text.chars().count().max(1) as f64 / total_chars as f64;
                let w = total * frac;
                u.start_s = t;
                u.end_s = (t + w).min(win_end);
                t += w;
            }
        }
        i = j;
    }
}

/// Rising crossings of the caption silence bar over the clip's RMS envelope —
/// where speech (or any voiced burst) begins. Same envelope constants as the
/// caption timing pass (hop 20 ms, win 40 ms) and the ADR 0021 bar
/// (min(10% of the loud reference, absolute floor 0.006) — here max'd with the
/// absolute floor so pure-noise clips don't sprout onsets everywhere).
///
/// `pub` because the whisper caption path runs the same [`apply_store_positional`]
/// pass (engine parity for `at_s` time-pins, ADR 0051) and must snap to this
/// exact onset grid — the ensemble computes it internally from its cleaned-onset
/// source; the whisper path computes it over the mixed caption samples.
pub fn rms_onsets(samples: &[f32], sample_rate: u32) -> Vec<f64> {
    if samples.is_empty() {
        return Vec::new();
    }
    let hop = (sample_rate as f64 * 0.02) as usize;
    let win = (sample_rate as f64 * 0.04) as usize;
    let mut env: Vec<f32> = Vec::with_capacity(samples.len() / hop.max(1) + 1);
    let mut i = 0;
    while i < samples.len() {
        let e = (i + win).min(samples.len());
        let w = &samples[i..e];
        env.push((w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt());
        i += hop;
    }
    let mut sorted = env.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let p95 = sorted[((sorted.len() as f64 * 0.95) as usize).min(sorted.len() - 1)];
    let bar = (0.10 * p95).max(0.006);
    let mut onsets = Vec::new();
    let mut last = f64::NEG_INFINITY;
    for (k, w) in env.windows(2).enumerate() {
        if w[0] < bar && w[1] >= bar {
            let t = (k + 1) as f64 * 0.02;
            if t - last >= 0.15 {
                onsets.push(t);
                last = t;
            }
        }
    }
    onsets
}

/// Lay a run of words into [gap_start, gap_end]. With onsets to spare the run
/// spreads across them in order; with more words than onsets the onsets
/// delimit speech segments and the words distribute across segments by
/// duration mass, char-proportionally within each. Each word ends where the
/// next begins (the refine pass owns display durations).
fn place_run(
    words: &[String],
    gap_start: f64,
    gap_end: f64,
    onsets: &[f64],
    fused: &mut Vec<CaptionUnit>,
) {
    if words.is_empty() {
        return;
    }
    let usable: Vec<f64> = onsets
        .iter()
        .copied()
        .filter(|t| *t >= gap_start && *t < gap_end - 0.02)
        .collect();
    let mut starts: Vec<f64> = Vec::with_capacity(words.len());
    let (m, n) = (usable.len(), words.len());
    if m >= n {
        // Enough onsets: spread the run across them in order (taking the
        // FIRST n crams the run into the gap's head — the words are spoken
        // through the gap, not at its start).
        for k in 0..n {
            starts.push(usable[if n == 1 { 0 } else { k * (m - 1) / (n - 1) }]);
        }
    } else {
        // Fewer onsets than words: the onsets delimit speech segments —
        // allocate words to segments by duration mass (largest remainder),
        // then spread char-proportionally within each, so every burst opens
        // with a word ON its onset. m == 0 degenerates to one segment (the
        // whole-gap proportional spread). First-N-onto-onsets stacked the
        // remainder against the gap's end (measured: 47 words into 1.85 s).
        let mut bounds = Vec::with_capacity(m + 2);
        bounds.push(gap_start);
        bounds.extend_from_slice(&usable);
        bounds.push(gap_end.max(gap_start));
        let durs: Vec<f64> = bounds.windows(2).map(|w| (w[1] - w[0]).max(0.0)).collect();
        let total: f64 = durs.iter().sum();
        let mut counts: Vec<usize> = if total > 0.0 {
            durs.iter().map(|d| ((n as f64) * d / total).floor() as usize).collect()
        } else {
            vec![0; durs.len()]
        };
        let mut used: usize = counts.iter().sum();
        let mut order: Vec<usize> = (0..durs.len()).collect();
        order.sort_by(|&a, &b| {
            let frac = |i: usize| {
                if total > 0.0 { (n as f64) * durs[i] / total - counts[i] as f64 } else { 0.0 }
            };
            frac(b).total_cmp(&frac(a))
        });
        let mut oi = 0;
        while used < n {
            counts[order[oi % order.len()]] += 1;
            used += 1;
            oi += 1;
        }
        let mut wi = 0;
        for (si, c) in counts.iter().enumerate() {
            if *c == 0 {
                continue;
            }
            let (s0, s1) = (bounds[si], bounds[si + 1]);
            let seg = &words[wi..wi + c];
            let total_chars: usize = seg.iter().map(|w| w.chars().count().max(1)).sum();
            let mut t = s0;
            for w in seg {
                starts.push(t.min(s1));
                t += (s1 - s0).max(0.0) * (w.chars().count().max(1) as f64 / total_chars as f64);
            }
            wi += c;
        }
    }
    // monotonic guard (onsets are sorted; the proportional tail could start
    // before an earlier onset only if the gap math degenerated)
    for k in 1..starts.len() {
        if starts[k] < starts[k - 1] {
            starts[k] = starts[k - 1];
        }
    }
    for (k, w) in words.iter().enumerate() {
        let s = starts[k];
        let e = starts.get(k + 1).copied().unwrap_or(gap_end).max(s);
        fused.push(CaptionUnit { text: w.clone(), start_s: s, end_s: e.min(gap_end).max(s) });
    }
}

/// Positional (time-anchored) store pass over the FUSED units: each `at_s`
/// correction targets the occurrence of its `wrong` token(s) nearest that
/// VOD moment (±3 s guard) — a multi-word `wrong` matches consecutive units
/// and collapses them into the first, donating their spans — replaces its
/// text (a multi-word `right` expands to consecutive units; `wrong == right`
/// is a pure timing pin), and PINS the
/// first word's caption onto the speech onset closest to the moment (±1.5 s,
/// else the moment itself). Units that would then overlap the pin from the
/// left are re-placed briefly (0.3 s/word cap) so they stay readable — the
/// measured case: stretched "cok main" anchors pushing "pusing kan dibilang"
/// seconds late; the kan/dibilang pins pull the tail back and the stretched
/// pair compresses to its true brief spans.
pub fn apply_store_positional(
    fused: &mut Vec<CaptionUnit>,
    lexicon: &crate::DialectLexicon,
    range_start_s: f64,
    onsets: &[f64],
    clip_dur_s: f64,
) {
    let mut pins: Vec<(f64, Vec<String>, Vec<String>)> = lexicon
        .corrections
        .iter()
        .filter(|c| !c.right.is_empty() && !c.context)
        .filter_map(|c| {
            let at = c.at_s?;
            let wrong = normalize(&c.wrong);
            if wrong.is_empty() {
                return None;
            }
            // `right` verbatim by token: normalize() would split the
            // operator's spelling at punctuation ("blo'on" -> "blo on") —
            // their written form is the display truth, exactly as the dict
            // path renders it.
            let right: Vec<String> = c.right.split_whitespace().map(str::to_string).collect();
            Some((at - range_start_s, wrong, right))
        })
        .collect();
    pins.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (pin_t, wrong, right) in pins {
        if pin_t < 0.0 || pin_t > clip_dur_s {
            continue;
        }
        // nearest occurrence of `wrong` (consecutive units for a multi-word
        // wrong — "blok on" collapsing to "blo'on") to the moment. EXACT token
        // matches outrank garble-similar ones — edit-2 "similarity" pairs
        // absurdities on short words ("main"~"kan"), so it is only the
        // fallback when the exact words are absent (the vote respelled the
        // garble again).
        let nearest = |exact: bool| {
            (0..(fused.len() + 1).saturating_sub(wrong.len()))
                .filter(|&k| {
                    wrong.iter().enumerate().all(|(i, w)| {
                        // Case-insensitive: `wrong` is normalized (lowercased),
                        // and the ensemble's fused text already is — but the
                        // whisper path (ADR 0051) carries whisper's mixed-case
                        // units ("SDC", "Gue"), where a caps acronym is edit-3
                        // from its lowercased pin and similar_word would miss it.
                        // Lowercasing the unit is a no-op on the lowercase ensemble
                        // stream, so this leaves ensemble renders byte-identical.
                        let ft = fused[k + i].text.to_lowercase();
                        if exact {
                            &ft == w
                        } else {
                            similar_word(&ft, w)
                        }
                    })
                })
                .min_by(|&a, &b| {
                    (fused[a].start_s - pin_t).abs().total_cmp(&(fused[b].start_s - pin_t).abs())
                })
        };
        let target = nearest(true).or_else(|| nearest(false));
        let Some(k) = target else {
            tracing::warn!(
                "qwen ensemble: at_s pin \"{}\"@{pin_t:.1}s: no such word(s) in the caption",
                wrong.join(" ")
            );
            continue;
        };
        if (fused[k].start_s - pin_t).abs() > 3.0 {
            tracing::warn!(
                "qwen ensemble: at_s pin \"{}\"@{pin_t:.1}s: nearest occurrence is {:.1}s away - skipped",
                wrong.join(" "),
                (fused[k].start_s - pin_t).abs()
            );
            continue;
        }
        // Snap to the closest speech onset near the moment — but only a TIGHT
        // match (0.75 s): on continuously-loud spans (scream + SFX) the nearest
        // rising edge can sit a second before the pin, and honoring it would
        // undo the operator's correction. Past the radius, their ear wins.
        let snap = onsets
            .iter()
            .copied()
            .filter(|t| (t - pin_t).abs() <= 0.75)
            .min_by(|a, b| (a - pin_t).abs().total_cmp(&(b - pin_t).abs()))
            .unwrap_or(pin_t);
        tracing::info!(
            "qwen ensemble: at_s pin \"{}\"@{pin_t:.1}s -> \"{}\" at {snap:.2}s (was {:.2}s)",
            wrong.join(" "),
            right.join(" "),
            fused[k].start_s
        );
        // A multi-word wrong collapses: its trailing units are removed and
        // donate their timeline to the first (the "blok on" pair becomes one
        // "blo'on" spanning both words' time).
        let collapsed_end = fused[k + wrong.len() - 1].end_s;
        fused.drain(k + 1..k + wrong.len());
        // replace text (multi-word right expands into consecutive units)
        let old_end = fused[k].end_s.max(collapsed_end).max(snap + 0.15);
        fused[k].text = right[0].clone();
        fused[k].start_s = snap;
        fused[k].end_s = old_end.min(snap + 0.6);
        for (extra_i, w) in right[1..].iter().enumerate() {
            let s = fused[k + extra_i].end_s;
            fused.insert(
                k + extra_i + 1,
                CaptionUnit { text: w.clone(), start_s: s, end_s: s + 0.15 },
            );
        }
        // Left neighbors that overlap the pin compress into the space before
        // it. When that space is too tight to keep the chain readable, WIDE
        // predecessors join the chain as donors — the measured case: a
        // stretched first "pusing" (anchor artifact) hogging 1.1 s while the
        // words after it got 0.03 s each; re-placing the whole chain gives
        // every word its share and shortens the stretched one to its real say.
        let mut first_conflict = k;
        while first_conflict > 0 && fused[first_conflict - 1].end_s > snap {
            first_conflict -= 1;
        }
        while first_conflict > 0
            && first_conflict < k
            && ((snap - fused[first_conflict].start_s) / ((k - first_conflict) as f64))
                < MIN_WORD_S
            && fused[first_conflict - 1].end_s - fused[first_conflict - 1].start_s > 0.45
        {
            first_conflict -= 1;
        }
        if first_conflict < k {
            let n_chain = k - first_conflict;
            let chain_start = fused[first_conflict]
                .start_s
                .min(snap - n_chain as f64 * 0.3)
                .max(if first_conflict == 0 { 0.0 } else { fused[first_conflict - 1].end_s })
                .min(snap);
            let span = snap - chain_start;
            let total_chars: usize =
                fused[first_conflict..k].iter().map(|u| u.text.chars().count().max(1)).sum();
            let mut t = chain_start;
            for u in fused[first_conflict..k].iter_mut() {
                let w = span * (u.text.chars().count().max(1) as f64 / total_chars as f64);
                u.start_s = t;
                u.end_s = (t + w).min(snap);
                t += w;
            }
        }
        // right neighbors that the pin/insertions now overlap shift forward;
        // non-overlapping units just advance the cursor (no early break — an
        // inserted unit starts exactly at the cursor and units after it may
        // still overlap).
        let mut t = fused[k].end_s;
        for u in fused[k + 1..].iter_mut() {
            if u.start_s < t {
                let w = (u.end_s - u.start_s).max(0.0);
                u.start_s = t;
                u.end_s = (t + w).min(clip_dur_s).max(t);
            }
            t = t.max(u.end_s);
        }
    }
}

/// Similarity-weighted alignment for TIMING fusion: equal 0.0 / similar-sub
/// 0.5 / ins-or-del 1.0 / dissimilar-sub 2.2. A dissimilar substitution costs
/// more than skip+insert, so the DP can never pair a word with a different
/// word's anchor; a similar garble pairing beats skipping (0.5 < 2.0). This is
/// what routes a REPEATED phrase to its own anchors — with uniform costs the
/// clip's two "pusing ..." phrases tied, the backtrace picked the wrong slots,
/// and the second phrase compressed against the clip edge (operator-heard).
fn align_weighted(a: &[String], b: &[String]) -> Vec<(Op, Option<usize>, Option<usize>)> {
    const SIM: f64 = 0.5;
    const GAP: f64 = 1.0;
    const DIS: f64 = 2.2;
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0.0_f64; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i as f64 * GAP;
    }
    for j in 0..=m {
        d[0][j] = j as f64 * GAP;
    }
    let sub_cost = |x: &str, y: &str| {
        if x == y {
            0.0
        } else if similar_word(x, y) {
            SIM
        } else {
            DIS
        }
    };
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[i - 1][j - 1] + sub_cost(&a[i - 1], &b[j - 1]);
            d[i][j] = sub.min(d[i - 1][j] + GAP).min(d[i][j - 1] + GAP);
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    let eq = |x: f64, y: f64| (x - y).abs() < 1e-9;
    // Tie-break order: diagonal, then INS, then DEL. Preferring Ins over Del
    // at equal cost biases words toward LATER anchors — measured on the
    // phantom-anchor case: [phantom-pusing, bangke] anchors vs
    // [bangke, pusing] words costs 2.0 both ways, and the Del-first path
    // adopts the phantom (the exact bug); the Ins-first path matches bangke
    // to its real anchor and lets pusing fall to its later onset.
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && eq(d[i][j], d[i - 1][j - 1] + sub_cost(&a[i - 1], &b[j - 1])) {
            let op = if a[i - 1] == b[j - 1] || similar_word(&a[i - 1], &b[j - 1]) {
                Op::Ok
            } else {
                Op::Sub
            };
            ops.push((op, Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if j > 0 && eq(d[i][j], d[i][j - 1] + GAP) {
            ops.push((Op::Ins, None, Some(j - 1)));
            j -= 1;
        } else {
            ops.push((Op::Del, Some(i - 1), None));
            i -= 1;
        }
    }
    ops.reverse();
    ops
}

/// "Same word, different garble": edit distance <= 2, or one is a prefix of
/// the other (whisper's multi-word store collapses keep only the first token).
fn similar_word(a: &str, b: &str) -> bool {
    if a == b || a.starts_with(b) || b.starts_with(a) {
        return true;
    }
    let (av, bv): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (n, m) = (av.len(), bv.len());
    if n.abs_diff(m) > 2 {
        return false;
    }
    // bounded Levenshtein (cap 2)
    let mut prev: Vec<u32> = (0..=m as u32).collect();
    for i in 1..=n {
        let mut row = vec![i as u32; m + 1];
        for j in 1..=m {
            let sub = prev[j - 1] + u32::from(av[i - 1] != bv[j - 1]);
            row[j] = sub.min(prev[j] + 1).min(row[j - 1] + 1);
        }
        if row.iter().min().copied().unwrap_or(u32::MAX) > 2 {
            return false;
        }
        prev = row;
    }
    prev[m] <= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn apply_bails_before_any_io_when_already_cancelled() {
        // A cancel that landed during the preceding whisper decode: apply must
        // return "cancelled" without touching the (bogus) paths — the check
        // sits before the file ensures, which is what makes this test need no
        // fixtures.
        let cfg = EnsembleConfig {
            mtmd_cli: PathBuf::from("nonexistent-mtmd.exe"),
            qwen_model: PathBuf::from("nonexistent-model.gguf"),
            qwen_mmproj: PathBuf::from("nonexistent-mmproj.gguf"),
            ffmpeg: PathBuf::from("nonexistent-ffmpeg.exe"),
            deep_filter: None,
            work_dir: PathBuf::from("."),
            should_cancel: Box::new(|| true),
            on_stage: Box::new(|_, _| {}),
        };
        let whisper = Transcript { language: Language::Id, units: Vec::new() };
        let err = apply(
            &cfg,
            Path::new("nonexistent.wav"),
            TimeRange { start_s: 0.0, end_s: 10.0 },
            &whisper,
            None,
            &[],
            16000,
            &crate::DialectLexicon::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("cancelled"), "got: {err:#}");
    }

    #[test]
    fn decode_timeout_survives_context_wrapping() {
        // apply() aborts the whole ensemble on the watchdog class by downcast;
        // the caller distinguishes it the same way through added context.
        let e = anyhow::Error::new(DecodeTimeout { budget_s: 120 })
            .context("variant 0 decode timed out; aborting the ensemble");
        assert!(e.downcast_ref::<DecodeTimeout>().is_some());
        assert!(e.root_cause().to_string().contains("120 s budget"));
    }

    #[test]
    fn vote_restores_majority_word_over_backbone_garble() {
        // The benchmark's bajingan/pacingan case: backbone garbles, plurality
        // holds (a TIE keeps the backbone, so the win needs a strict count).
        let backbone = words("pacingan pacingan bangke");
        let voters = vec![
            words("bajingan bajingan bangke"),
            words("bajingan bajingan bangke"),
            words("racengan racengan bangke"),
        ];
        assert_eq!(vote_merge(&backbone, &voters), words("bajingan bajingan bangke"));
        // Tie (2 bajingan vs backbone+1 pacingan) -> backbone stands.
        let voters =
            vec![words("bajingan bajingan bangke"), words("bajingan bajingan bangke"), words("pacingan pacingan bangke")];
        assert_eq!(vote_merge(&backbone, &voters), words("pacingan pacingan bangke"));
    }

    #[test]
    fn vote_insert_needs_strict_majority() {
        // Two of four voters carrying tail bleed must NOT insert it.
        let backbone = words("pusing kan dibilang");
        let voters = vec![
            words("pusing kan dibilang suka dah"),
            words("pusing kan dibilang suka dah"),
            words("pusing kan dibilang"),
            words("pusing kan dibilang"),
        ];
        assert_eq!(vote_merge(&backbone, &voters), words("pusing kan dibilang"));
        // Three of four DO carry it.
        let voters = vec![
            words("pusing kan dibilang suka dah"),
            words("pusing kan dibilang suka dah"),
            words("pusing kan dibilang suka dah"),
            words("pusing kan dibilang"),
        ];
        assert_eq!(vote_merge(&backbone, &voters), words("pusing kan dibilang suka dah"));
    }

    #[test]
    fn vote_drops_backbone_token_on_del_plurality() {
        // Pad bleed: only the backbone (and no voter) has the leading words.
        let backbone = words("ada nggak itu kebuka");
        let voters = vec![words("kebuka"), words("kebuka"), words("kebuka")];
        assert_eq!(vote_merge(&backbone, &voters), words("kebuka"));
    }

    /// n seconds of silence at 16 kHz (no onsets -> proportional placement).
    fn silence(secs: f64) -> Vec<f32> {
        vec![0.0; (16000.0 * secs) as usize]
    }

    /// Silence with 0.1-amplitude bursts at the given [start, end) second spans.
    fn bursts(secs: f64, spans: &[(f64, f64)]) -> Vec<f32> {
        let mut s = silence(secs);
        for (a, b) in spans {
            let (a, b) = ((a * 16000.0) as usize, ((b * 16000.0) as usize).min(s.len()));
            for x in s[a..b].iter_mut() {
                *x = 0.1;
            }
        }
        s
    }

    #[test]
    fn fuse_adopts_whisper_timing_for_matches_and_drops_whisper_only() {
        let whisper = Transcript {
            language: Language::Id,
            units: vec![
                CaptionUnit { text: "mana".into(), start_s: 1.0, end_s: 1.4 },
                CaptionUnit { text: "eh".into(), start_s: 2.0, end_s: 2.0 },
                CaptionUnit { text: "bangke".into(), start_s: 3.0, end_s: 3.5 },
            ],
        };
        let merged = words("mana bangke");
        let fused = fuse_onto_timing(&merged, &whisper, None, &silence(5.0), 16000, 5.0);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].text, "mana");
        assert_eq!((fused[0].start_s, fused[0].end_s), (1.0, 1.4));
        assert_eq!(fused[1].text, "bangke");
        assert_eq!((fused[1].start_s, fused[1].end_s), (3.0, 3.5));
    }

    #[test]
    fn fuse_lays_inserted_run_into_the_whisper_gap() {
        let whisper = Transcript {
            language: Language::Id,
            units: vec![
                CaptionUnit { text: "keren".into(), start_s: 1.0, end_s: 2.0 },
                CaptionUnit { text: "banget".into(), start_s: 6.0, end_s: 6.5 },
            ],
        };
        // qwen recovered two words whisper missed inside the 2.0-6.0 hole
        let merged = words("keren mana dah banget");
        let fused = fuse_onto_timing(&merged, &whisper, None, &silence(8.0), 16000, 8.0);
        assert_eq!(fused.len(), 4);
        assert_eq!(fused[1].text, "mana");
        assert_eq!(fused[2].text, "dah");
        assert!(fused[1].start_s >= 2.0 - 1e-9 && fused[2].end_s <= 6.0 + 1e-9);
        assert!(fused[1].end_s <= fused[2].start_s + 1e-9);
        // ordering stays monotonic
        for w in fused.windows(2) {
            assert!(w[0].start_s <= w[1].start_s + 1e-9);
        }
    }

    #[test]
    fn fuse_tail_run_bounded_by_clip_duration() {
        let whisper = Transcript {
            language: Language::Id,
            units: vec![CaptionUnit { text: "mulai".into(), start_s: 0.5, end_s: 1.0 }],
        };
        let merged = words("mulai satu dua");
        let fused = fuse_onto_timing(&merged, &whisper, None, &silence(4.0), 16000, 4.0);
        assert_eq!(fused.len(), 3);
        assert!(fused[2].end_s <= 4.0 + 1e-9);
        assert!(fused[1].start_s >= 1.0 - 1e-9);
    }

    #[test]
    fn fuse_never_drags_a_word_onto_a_different_words_phantom_anchor() {
        // The bug the operator heard: whisper's phantom 4 s "pusing" unit sits
        // where "bangke" is actually said; a text-blind fusion adopted it and
        // the caption ran ~5 s ahead of the streamer. With similarity gating
        // neither word adopts a dissimilar anchor — both fall through to the
        // speech onsets (bursts at 19.7 and 24.0).
        let whisper = Transcript {
            language: Language::Id,
            units: vec![CaptionUnit { text: "pusing".into(), start_s: 19.5, end_s: 23.5 }],
        };
        let extra = Transcript {
            language: Language::Id,
            units: vec![CaptionUnit { text: "bangke".into(), start_s: 19.7, end_s: 19.9 }],
        };
        let samples = bursts(30.0, &[(19.7, 20.0), (24.0, 24.4)]);
        let merged = words("bangke pusing");
        let fused = fuse_onto_timing(&merged, &whisper, Some(&extra), &samples, 16000, 30.0);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].text, "bangke");
        assert!(
            (fused[0].start_s - 19.7).abs() < 0.15,
            "bangke lands on its real onset, got {}",
            fused[0].start_s
        );
        assert_eq!(fused[1].text, "pusing");
        assert!(
            (fused[1].start_s - 24.0).abs() < 0.15,
            "pusing lands on the LATER onset, not the phantom anchor, got {}",
            fused[1].start_s
        );
    }

    #[test]
    fn fuse_sub_adopts_anchor_only_for_the_same_garbled_word() {
        // dijegat (voted) vs dijekat (whisper's spelling): same word, edit-1 -
        // adopts the anchor span. A dissimilar pair must not.
        assert!(similar_word("dijekat", "dijegat"));
        assert!(similar_word("apaan", "apa")); // prefix
        assert!(!similar_word("pusing", "bangke"));
        let whisper = Transcript {
            language: Language::Id,
            units: vec![CaptionUnit { text: "dijekat".into(), start_s: 2.1, end_s: 2.32 }],
        };
        let merged = words("dicegat");
        let fused = fuse_onto_timing(&merged, &whisper, None, &silence(4.0), 16000, 4.0);
        assert_eq!(fused.len(), 1);
        assert_eq!((fused[0].start_s, fused[0].end_s), (2.1, 2.32));
    }

    #[test]
    fn fuse_routes_a_repeated_phrase_to_its_own_anchors() {
        // The clip has "pusing cok main" THEN "pusing kan dibilang"; with
        // uniform alignment costs the two phrases tied and the second one
        // compressed against the clip edge (operator-heard). Weighted costs
        // route kan/dibilang onto their own anchors.
        let whisper = Transcript {
            language: Language::Id,
            units: vec![
                CaptionUnit { text: "pusing".into(), start_s: 24.5, end_s: 24.9 },
                CaptionUnit { text: "main".into(), start_s: 25.1, end_s: 25.3 },
                CaptionUnit { text: "kan".into(), start_s: 26.34, end_s: 26.5 },
                CaptionUnit { text: "bilang".into(), start_s: 26.78, end_s: 27.0 },
                CaptionUnit { text: "cimri".into(), start_s: 29.26, end_s: 29.46 },
            ],
        };
        let merged = words("pusing cok main pusing kan dibilang");
        let fused = fuse_onto_timing(&merged, &whisper, None, &silence(30.0), 16000, 30.0);
        assert_eq!(fused.len(), 6);
        assert_eq!((fused[0].text.as_str(), fused[0].start_s), ("pusing", 24.5));
        assert_eq!((fused[2].text.as_str(), fused[2].start_s), ("main", 25.1));
        assert_eq!((fused[4].text.as_str(), fused[4].start_s), ("kan", 26.34));
        assert_eq!((fused[5].text.as_str(), fused[5].start_s), ("dibilang", 26.78));
        // the second pusing lands between its neighbors, not at the clip edge
        assert!(
            fused[3].start_s >= 25.3 && fused[3].start_s < 26.34,
            "second pusing placed in its gap, got {}",
            fused[3].start_s
        );
    }

    #[test]
    fn fuse_distrusts_a_hallucination_pile_skeleton() {
        // The eh-pile clip class: whisper hallucinated "eh" wall-to-wall, the
        // vote replaced nearly all of it, and the skeleton's lone accidental
        // match ("ada" mid-pile, far from where the word is really said) boxed
        // the whole run against itself — 21 words in 0.12 s on the export.
        // Below-1/3 adoption must drop the skeleton and spread by the clip.
        let mut units: Vec<CaptionUnit> = (0..40)
            .map(|k| {
                let t = 0.5 + k as f64 * 0.2;
                CaptionUnit { text: "eh".into(), start_s: t, end_s: t + 0.15 }
            })
            .collect();
        // two accidental matches whose spans leave a 0.1 s window — the four
        // words voted between them have nowhere to go (the export's pileup)
        units.push(CaptionUnit { text: "ada".into(), start_s: 2.0, end_s: 8.5 });
        units.push(CaptionUnit { text: "kunci".into(), start_s: 8.6, end_s: 8.8 });
        let whisper = Transcript { language: Language::Id, units };
        let merged = words("nggak mau es krim aku ada kanan deh kayak gini kunci tor");
        let fused = fuse_onto_timing(&merged, &whisper, None, &silence(12.0), 16000, 12.0);
        assert_eq!(fused.len(), 12);
        // no pileup: every word gets readable width across the clip
        for u in &fused {
            assert!(
                u.end_s - u.start_s >= MIN_WORD_S - 1e-9,
                "{} squeezed to {:.3}s",
                u.text,
                u.end_s - u.start_s
            );
        }
        for w in fused.windows(2) {
            assert!(w[0].start_s <= w[1].start_s + 1e-9);
        }
        // the run actually reaches the clip's tail instead of stacking early
        assert!(fused.last().unwrap().start_s > 9.0);
    }

    #[test]
    fn place_run_spreads_words_across_dense_onsets() {
        // 19 onsets, 3 words: the run is spoken THROUGH the gap — first-N
        // placement crammed all three into the first 1.5 s.
        let onsets: Vec<f64> = (1..=19).map(|k| k as f64 * 0.5).collect();
        let mut fused = Vec::new();
        place_run(&words("satu dua tiga"), 0.0, 10.0, &onsets, &mut fused);
        assert_eq!(fused.len(), 3);
        assert_eq!(fused[0].start_s, 0.5);
        assert_eq!(fused[1].start_s, 5.0);
        assert_eq!(fused[2].start_s, 9.5);
    }

    #[test]
    fn place_run_distributes_words_by_segment_mass_when_onsets_are_scarce() {
        // One late onset, eight words: first-N snapping stacked seven words
        // into the last 0.85 s. Segment allocation gives the 9 s head its
        // seven and opens the burst at 9.0 with the eighth.
        let mut fused = Vec::new();
        place_run(&words("a b c d e f g h"), 0.0, 10.0, &[9.0], &mut fused);
        assert_eq!(fused.len(), 8);
        assert!(fused[0].start_s < 0.5, "run starts at the gap, got {}", fused[0].start_s);
        assert_eq!(fused[7].start_s, 9.0, "the burst opens with a word on its onset");
        for u in &fused {
            assert!(
                u.end_s - u.start_s > 0.5,
                "{} squeezed to {:.3}s",
                u.text,
                u.end_s - u.start_s
            );
        }
    }

    #[test]
    fn place_run_opens_each_speech_burst_on_its_onset() {
        // Bursts at 2.0 and 6.0 split the gap into 2s/4s/4s segments: five
        // equal words allocate 1/2/2 and each burst starts ON its onset.
        let mut fused = Vec::new();
        place_run(&words("aa bb cc dd ee"), 0.0, 10.0, &[2.0, 6.0], &mut fused);
        assert_eq!(fused.len(), 5);
        assert_eq!(fused[1].start_s, 2.0);
        assert_eq!(fused[3].start_s, 6.0);
        for w in fused.windows(2) {
            assert!(w[0].start_s <= w[1].start_s + 1e-9);
        }
    }

    #[test]
    fn respread_reclaims_unclaimed_timeline_for_flash_runs() {
        // b and c got squeezed to (near-)zero width against d's anchor while
        // 0.9 s of unclaimed timeline sits between a and d — the run respreads.
        let mut fused = vec![
            CaptionUnit { text: "a".into(), start_s: 1.0, end_s: 2.0 },
            CaptionUnit { text: "bb".into(), start_s: 2.0, end_s: 2.0 },
            CaptionUnit { text: "cc".into(), start_s: 2.0, end_s: 2.05 },
            CaptionUnit { text: "d".into(), start_s: 2.9, end_s: 3.5 },
        ];
        respread_flashes(&mut fused, 4.0);
        assert_eq!(fused[1].start_s, 2.0);
        assert!((fused[1].end_s - 2.45).abs() < 1e-6);
        assert!((fused[2].start_s - 2.45).abs() < 1e-6);
        assert!((fused[2].end_s - 2.9).abs() < 1e-6);
        // neighbors untouched
        assert_eq!(fused[0].end_s, 2.0);
        assert_eq!(fused[3].start_s, 2.9);
    }

    #[test]
    fn respread_right_aligns_a_lone_squeezed_word_near_its_anchor() {
        // A single flash word before an anchor gets up to 0.5 s ending AT the
        // anchor — not the whole slack window (that dragged it seconds early).
        let mut fused = vec![
            CaptionUnit { text: "bangke".into(), start_s: 1.0, end_s: 2.0 },
            CaptionUnit { text: "mana".into(), start_s: 5.36, end_s: 5.40 },
            CaptionUnit { text: "tadi".into(), start_s: 5.40, end_s: 6.0 },
        ];
        respread_flashes(&mut fused, 8.0);
        assert!((fused[1].start_s - 4.9).abs() < 1e-6, "got {}", fused[1].start_s);
        assert!((fused[1].end_s - 5.4).abs() < 1e-6);
    }

    #[test]
    fn respread_leaves_genuinely_boxed_runs_alone() {
        let mut fused = vec![
            CaptionUnit { text: "a".into(), start_s: 1.0, end_s: 2.0 },
            CaptionUnit { text: "b".into(), start_s: 2.0, end_s: 2.05 },
            CaptionUnit { text: "c".into(), start_s: 2.1, end_s: 2.2 },
        ];
        let before: Vec<(f64, f64)> = fused.iter().map(|u| (u.start_s, u.end_s)).collect();
        // window after "a" to clip end is 2.0..2.2 via next... c is also <MIN so
        // the run is b,c with window 2.0..2.2 (clip end) = 0.2 < 2*0.15 -> no-op
        respread_flashes(&mut fused, 2.2);
        let after: Vec<(f64, f64)> = fused.iter().map(|u| (u.start_s, u.end_s)).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn rms_onsets_find_burst_starts() {
        let samples = bursts(4.0, &[(1.0, 1.2), (2.5, 2.7)]);
        let onsets = rms_onsets(&samples, 16000);
        assert_eq!(onsets.len(), 2, "got {onsets:?}");
        assert!((onsets[0] - 1.0).abs() < 0.1);
        assert!((onsets[1] - 2.5).abs() < 0.1);
    }

    #[test]
    fn store_fuzzy_transfers_correction_across_engine_spellings() {
        // whisper's garble was curated as dijekat->dicegat; the ensemble
        // spells the same mishear dijegat — edit-1 from the stored key.
        let mut lex = crate::DialectLexicon::default();
        lex.corrections.push(crate::Correction {
            wrong: "dijekat".into(),
            right: "dicegat".into(),
            ..Default::default()
        });
        lex.dictionary.insert("dicegat".into());
        let mut w = words("kau dijegat mana");
        apply_store_fuzzy(&mut w, &lex);
        assert_eq!(w, words("kau dicegat mana"));
    }

    #[test]
    fn store_fuzzy_never_touches_real_dictionary_words_or_context_pairs() {
        let mut lex = crate::DialectLexicon::default();
        // context pair (LLM-only, ADR 0030) must not fire even on exact match
        lex.corrections.push(crate::Correction {
            wrong: "cowok".into(),
            right: "cok".into(),
            context: true,
            ..Default::default()
        });
        // fuzzy would match maen~main, but maen is a real (dictionary) word
        lex.corrections.push(crate::Correction {
            wrong: "main".into(),
            right: "kelamin".into(),
            ..Default::default()
        });
        lex.dictionary.insert("maen".into());
        let mut w = words("cowok maen");
        apply_store_fuzzy(&mut w, &lex);
        assert_eq!(w, words("cowok maen"));
    }

    fn pin_lex(entries: &[(&str, &str, f64)]) -> crate::DialectLexicon {
        let mut lex = crate::DialectLexicon::default();
        for (w, r, at) in entries {
            lex.corrections.push(crate::Correction {
                wrong: (*w).into(),
                right: (*r).into(),
                at_s: Some(*at),
                ..Default::default()
            });
        }
        lex
    }

    #[test]
    fn positional_pin_targets_nearest_occurrence_and_moves_it() {
        // two "anjing"s; the pin at VOD 105s (clip 5s) must move ONLY the
        // second one (4.6s away from the first, 0.4s from the second).
        let mut fused = vec![
            CaptionUnit { text: "anjing".into(), start_s: 1.0, end_s: 1.4 },
            CaptionUnit { text: "mana".into(), start_s: 2.0, end_s: 2.4 },
            CaptionUnit { text: "anjing".into(), start_s: 4.6, end_s: 4.8 },
        ];
        // pure pin (wrong == right), onset available at 5.2
        apply_store_positional(&mut fused, &pin_lex(&[("anjing", "anjing", 105.0)]), 100.0, &[5.2], 10.0);
        assert_eq!(fused[0].start_s, 1.0, "first occurrence untouched");
        assert!((fused[2].start_s - 5.2).abs() < 1e-9, "second pinned to onset, got {}", fused[2].start_s);
    }

    #[test]
    fn positional_pin_reanchors_a_mis_onset_word_to_its_real_speech_onset() {
        // The measured clip-3 case (ADR 0051): whisper's DTW anchored "gue" at
        // 25.84 s — onto a laughter burst — but the operator hears the word at
        // ~28 s. Automatic re-anchoring was refuted by measurement (an early onset
        // before a laugh is acoustically identical to a correct word before a
        // pause; only the ear separates them), so the fix is the operator's time
        // pin snapping the word onto its real speech onset. The whisper path runs
        // this SAME pass now (engine parity) — the ensemble always did. The words
        // the pin did not name never move: the false-positive guarantee the
        // auto-detector could not offer.
        let mut fused = vec![
            CaptionUnit { text: "corp".into(), start_s: 24.88, end_s: 25.84 },
            CaptionUnit { text: "gue".into(), start_s: 25.84, end_s: 25.84 },
            CaptionUnit { text: "kalo".into(), start_s: 28.64, end_s: 28.88 },
        ];
        // Clip VOD-range starts at 3592 s; the pin is gue at clip-relative 28.0 s.
        // The real speech onset (measured) is 28.12; the wrong 25.84 is a laughter
        // burst with no clean speech onset of its own.
        apply_store_positional(
            &mut fused,
            &pin_lex(&[("gue", "gue", 3592.0 + 28.0)]),
            3592.0,
            &[24.36, 28.12],
            61.0,
        );
        let gue = fused.iter().find(|u| u.text == "gue").expect("gue kept");
        assert!(
            (gue.start_s - 28.12).abs() < 1e-9,
            "gue re-anchored to its real onset, got {}",
            gue.start_s
        );
        // corp (before) and kalo (after) — words the pin never named — are untouched.
        assert_eq!(fused[0].text, "corp");
        assert!((fused[0].start_s - 24.88).abs() < 1e-9, "corp untouched, got {}", fused[0].start_s);
        let kalo = fused.iter().find(|u| u.text == "kalo").expect("kalo kept");
        assert!(
            kalo.start_s >= 28.64 - 1e-9,
            "kalo not dragged by the pin, got {}",
            kalo.start_s
        );
        for w in fused.windows(2) {
            assert!(w[0].start_s <= w[1].start_s + 1e-9, "monotonic");
        }
    }

    #[test]
    fn positional_pin_matches_a_whisper_caps_acronym() {
        // Whisper-path parity (ADR 0051): whisper keeps a word's case ("SDC"),
        // unlike the ensemble's normalized-lowercase stream. normalize() lowercases
        // the pin's `wrong`, so an all-caps acronym is edit-3 from it — similar_word
        // would miss it — and the operator's "SDC" mis-onset would silently no-op on
        // the whisper engine. The match is case-insensitive; `right` keeps casing.
        let mut fused = vec![
            CaptionUnit { text: "juga".into(), start_s: 19.0, end_s: 19.5 },
            CaptionUnit { text: "SDC".into(), start_s: 21.0, end_s: 22.0 },
            CaptionUnit { text: "Susu".into(), start_s: 24.0, end_s: 24.4 },
        ];
        apply_store_positional(
            &mut fused,
            &pin_lex(&[("SDC", "SDC", 3592.0 + 23.0)]),
            3592.0,
            &[22.76],
            61.0,
        );
        assert_eq!(fused[1].text, "SDC", "display casing preserved");
        assert!(
            (fused[1].start_s - 22.76).abs() < 1e-9,
            "caps acronym matched case-insensitively and re-anchored, got {}",
            fused[1].start_s
        );
    }

    #[test]
    fn positional_pin_compresses_stretched_left_neighbors() {
        // "cok main" stretched over 26.1-27.6 while "kan" belongs at 26.3:
        // the pin pulls kan back and the pair compresses briefly before it.
        let mut fused = vec![
            CaptionUnit { text: "pusing".into(), start_s: 24.9, end_s: 26.1 },
            CaptionUnit { text: "cok".into(), start_s: 26.1, end_s: 27.2 },
            CaptionUnit { text: "main".into(), start_s: 27.2, end_s: 27.6 },
            CaptionUnit { text: "kan".into(), start_s: 28.2, end_s: 28.5 },
            CaptionUnit { text: "dibilang".into(), start_s: 28.7, end_s: 29.2 },
        ];
        apply_store_positional(&mut fused, &pin_lex(&[("kan", "kan", 126.3)]), 100.0, &[], 30.0);
        assert!((fused[3].start_s - 26.3).abs() < 1e-9, "kan pinned, got {}", fused[3].start_s);
        // cok+main compressed to end at the pin, each still visible
        assert!(fused[2].end_s <= 26.3 + 1e-9);
        assert!(fused[1].end_s <= fused[2].start_s + 1e-9);
        assert!(fused[1].end_s - fused[1].start_s > 0.05);
        assert!(fused[2].end_s - fused[2].start_s > 0.05);
        // pusing keeps its place (no overlap with the compressed chain)
        assert!(fused[0].end_s <= fused[1].start_s + 1e-9);
    }

    #[test]
    fn positional_pin_expands_multiword_right_and_shifts_overlaps() {
        // operator: "tur biadab anjing" is spoken at ~8s, right before
        // "depan sini" - the insertion rides the depan occurrence nearest 8s.
        let mut fused = vec![
            CaptionUnit { text: "depan".into(), start_s: 1.2, end_s: 1.5 },
            CaptionUnit { text: "satu".into(), start_s: 4.6, end_s: 4.8 },
            CaptionUnit { text: "depan".into(), start_s: 7.5, end_s: 8.5 },
            CaptionUnit { text: "sini".into(), start_s: 8.5, end_s: 9.3 },
        ];
        apply_store_positional(
            &mut fused,
            &pin_lex(&[("depan", "tur biadab anjing depan", 108.0)]),
            100.0,
            &[8.0],
            12.0,
        );
        let texts: Vec<&str> = fused.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(texts, ["depan", "satu", "tur", "biadab", "anjing", "depan", "sini"]);
        assert!((fused[2].start_s - 8.0).abs() < 1e-9, "tur pinned at 8.0, got {}", fused[2].start_s);
        // ordering monotonic, sini shifted past the inserted words
        for w in fused.windows(2) {
            assert!(w[0].start_s <= w[1].start_s + 1e-9);
        }
        assert!(fused[6].start_s >= fused[5].end_s - 1e-9);
    }

    #[test]
    fn positional_pin_guard_skips_far_occurrences() {
        let mut fused = vec![CaptionUnit { text: "kreeng".into(), start_s: 2.0, end_s: 2.5 }];
        // pin at clip 16s, occurrence at 2s -> 14s away -> skipped
        apply_store_positional(&mut fused, &pin_lex(&[("kreeng", "kreeng", 116.0)]), 100.0, &[], 30.0);
        assert_eq!(fused[0].start_s, 2.0);
    }

    #[test]
    fn positional_pin_collapses_multiword_wrong_and_keeps_operator_spelling() {
        // The Deddy gate ruling: the vote heard "blok on" where the speaker
        // says "blo'on" — a multi-word wrong collapses both units into one,
        // and the operator's apostrophe survives (normalize() would have
        // split it back into "blo on").
        let mut fused = vec![
            CaptionUnit { text: "tapi".into(), start_s: 46.5, end_s: 46.7 },
            CaptionUnit { text: "blok".into(), start_s: 46.78, end_s: 47.0 },
            CaptionUnit { text: "on".into(), start_s: 47.06, end_s: 47.19 },
            CaptionUnit { text: "tuh".into(), start_s: 47.19, end_s: 47.4 },
        ];
        apply_store_positional(
            &mut fused,
            &pin_lex(&[("blok on", "blo'on", 1846.8)]),
            1800.0,
            &[],
            60.0,
        );
        let texts: Vec<&str> = fused.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(texts, ["tapi", "blo'on", "tuh"]);
        // the collapsed unit spans (capped) both source words' time
        assert!((fused[1].start_s - 46.8).abs() < 1e-9);
        assert!(fused[1].end_s > 47.1);
        for w in fused.windows(2) {
            assert!(w[0].start_s <= w[1].start_s + 1e-9);
        }
    }

    #[test]
    fn positional_pins_fix_each_pair_independently() {
        // Three "blok on" pairs, the middle one is a different word — each
        // pin targets its own occurrence by time.
        let mut fused = Vec::new();
        for (s, w) in [
            (46.78, "blok"),
            (47.06, "on"),
            (47.5, "tuh"),
            (47.96, "blok"),
            (48.34, "on"),
            (49.58, "blok"),
            (49.76, "on"),
        ] {
            fused.push(CaptionUnit { text: w.into(), start_s: s, end_s: s + 0.15 });
        }
        apply_store_positional(
            &mut fused,
            &pin_lex(&[
                ("blok on", "blo'on", 1846.8),
                ("blok on", "goblok", 1848.0),
                ("blok on", "blo'on", 1849.6),
            ]),
            1800.0,
            &[],
            60.0,
        );
        let texts: Vec<&str> = fused.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(texts, ["blo'on", "tuh", "goblok", "blo'on"]);
    }

    #[test]
    fn normalize_strips_asr_header_and_punctuation() {
        assert_eq!(
            normalize("language Indonesian<asr_text>Mana? Ini satu."),
            words("mana ini satu")
        );
    }

    #[test]
    fn engine_override_is_tri_state() {
        // Force-on, force-off, and defer-to-Creator (ADR 0035): unset or an
        // unrecognized value must NOT force whisper — it defers.
        for on in ["1", "true", "on", " 1 "] {
            assert_eq!(override_from(Some(on)), Some(true), "{on:?}");
        }
        for off in ["0", "false", "off", " 0 "] {
            assert_eq!(override_from(Some(off)), Some(false), "{off:?}");
        }
        for defer in [None, Some(""), Some("yes-ish"), Some("2")] {
            assert_eq!(override_from(defer), None, "{defer:?}");
        }
    }

    #[test]
    fn transfer_counts_mirror_the_applier_filters() {
        let mk = |wrong: &str, right: &str, context: bool, at_s: Option<f64>| crate::Correction {
            wrong: wrong.into(),
            right: right.into(),
            context,
            at_s,
            ..Default::default()
        };
        let corrections = vec![
            mk("kreeng", "kirain", false, None),      // single-word -> fuzzy tier
            mk("tiga", "tiga-tiga", false, Some(9.0)), // pinned -> positional pass
            mk("blok on", "blo'on", false, Some(12.0)), // multi-word BUT pinned -> carries
            mk("cepet cepet", "cepet-cepet", false, None), // multi-word -> whisper only
            mk("cowok", "cok", true, None),           // context -> whisper only
            mk("cowok", "cok", true, Some(3.0)),      // context wins over the pin (both passes skip it)
            mk("harvested", "", false, None),         // unverified to-do -> not counted
        ];
        let t = transfer_counts(&corrections);
        assert_eq!(
            (t.single_word, t.pinned, t.multi_word, t.context),
            (1, 2, 1, 2),
            "{t:?}"
        );
        assert_eq!((t.carries(), t.stays(), t.total()), (3, 3, 6));
    }
}
