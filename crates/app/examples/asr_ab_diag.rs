//! ASR engine A/B inspector (ROADMAP "ASR engine upgrade", 2026-07-02).
//!
//! Puts whisper-large-v3 (the production caption engine) side by side with
//! Qwen3-ASR-1.7B (GGUF, via the pinned llama.cpp `llama-mtmd-cli` sidecar) on
//! the operator's benchmark clips, so the operator — the ground truth — can
//! judge per clip whether the new engine reads more real speech. Three blocks
//! per clip:
//!
//!   1. whisper RAW   — empty lexicon: the bare engine, no store corrections.
//!   2. whisper PROD  — the layered dialect store, decoded exactly as
//!                      `do_render` (beam=5 + DTW; corrections post-decode).
//!   3. Qwen3-ASR raw — temp 0, no vocabulary biasing, same 16 kHz mono slice.
//!
//! ADR 0033 compliance: this is a read-only inspector. It never writes a
//! dialect store, never touches the render path, and stages the GPU models
//! strictly sequentially (whisper is dropped before the Qwen sidecar spawns).
//! Timestamp fidelity of the Qwen output is REPORTED, not gated — if word
//! times don't survive the mtmd path, Qwen3-ForcedAligner is the named
//! companion (ROADMAP).
//!
//!   scripts\cargo-cuda.bat run --release -p yt-clipper --example asr_ab_diag
//!   scripts\cargo-cuda.bat run --release -p yt-clipper --example asr_ab_diag -- <wav> <start_s> <end_s> [id|en|ja]
//!
//! No args = the three benchmark fixtures (guntur69 "Diskusi biasa" + eh-pile,
//! Deddy Corbuzier clear-audio control). The report prints to stdout and is
//! also written to `target/asr-ab/report.md`; raw sidecar output is kept in
//! `target/asr-ab/` for forensics.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use yc_core::{Language, TimeRange};
use yc_ingest::{read_range_samples, WHISPER_SR};
use yc_transcribe::{DialectLexicon, Transcriber};

const WHISPER_MODEL: &str = "models/ggml-large-v3.bin";
const QWEN_MODEL: &str = "models/Qwen3-ASR-1.7B-Q8_0.gguf";
const QWEN_MMPROJ: &str = "models/mmproj-Qwen3-ASR-1.7B-Q8_0.gguf";
const MTMD_CLI: &str = "sidecars/llama/llama-mtmd-cli.exe";
const OUT_DIR: &str = "target/asr-ab";

struct Fixture {
    label: &'static str,
    wav: PathBuf,
    range: TimeRange,
    lang: Language,
}

/// One engine's take on one fixture, normalized to displayable units.
struct EngineTake {
    /// Engine tag for the report header.
    name: &'static str,
    /// (start_s, end_s, text) — Qwen (no per-word times) uses one pseudo-unit.
    units: Vec<(f64, f64, String)>,
    /// Verbatim decoder output (Qwen only): what the model actually printed,
    /// before any parsing — the timestamp-fidelity evidence.
    raw: Option<String>,
    wall_s: f64,
}

fn lang_token(lang: Language) -> &'static str {
    match lang {
        Language::En => "en",
        Language::Ja => "ja",
        Language::Id => "id",
    }
}

/// The first stream folder under `workspace/<creator>/` that has a
/// `data/analysis.wav` — resolved at runtime so the (emoji-heavy) VOD titles
/// never need to appear in source.
fn find_analysis_wav(creator_dir: &Path) -> anyhow::Result<PathBuf> {
    for e in std::fs::read_dir(creator_dir)?.flatten() {
        let candidate = e.path().join("data").join("analysis.wav");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("no stream folder with data/analysis.wav under {}", creator_dir.display())
}

fn benchmark_fixtures() -> anyhow::Result<Vec<Fixture>> {
    let guntur = find_analysis_wav(Path::new("workspace/Ino Gemink Live Streaming"))?;
    let deddy = find_analysis_wav(Path::new("workspace/Deddy Corbuzier"))?;
    Ok(vec![
        Fixture {
            label: "Diskusi biasa (ID hard: fast masked slang, curated)",
            wav: guntur.clone(),
            range: TimeRange { start_s: 1881.5, end_s: 1911.5 },
            lang: Language::Id,
        },
        Fixture {
            label: "eh-pile clip #7 (ID hard: hallucination pile)",
            wav: guntur,
            range: TimeRange { start_s: 2203.0, end_s: 2233.0 },
            lang: Language::Id,
        },
        Fixture {
            label: "Deddy Corbuzier podcast (ID clear-audio control)",
            wav: deddy,
            range: TimeRange { start_s: 1800.0, end_s: 1860.0 },
            lang: Language::Id,
        },
    ])
}

/// The layered lexicon exactly as `do_render` / `caption_diag` resolve it
/// (ADR 0031): bundled base < per-Creator < per-clip, derived from the wav's
/// stream-folder location. Read-only here.
fn layered_lexicon(wav: &Path, lang: Language) -> DialectLexicon {
    let base_dir = PathBuf::from("assets/dialect");
    let lc = lang_token(lang);
    let mut overlays: Vec<PathBuf> = Vec::new();
    if let Some(stream_dir) = wav.parent().and_then(|d| d.parent()) {
        if let Some(creator_dir) = stream_dir.parent() {
            overlays.push(creator_dir.join(format!("{lc}.json")));
        }
        if let Ok(rd) = std::fs::read_dir(stream_dir) {
            let suffix = format!(".{lc}.json");
            for e in rd.flatten() {
                let p = e.path();
                if p.file_name().and_then(|n| n.to_str()).map(|n| n.ends_with(&suffix)).unwrap_or(false)
                {
                    overlays.push(p);
                }
            }
        }
    }
    DialectLexicon::load_layered(&base_dir, &overlays, lang)
}

/// Cut the fixture's exact range out of analysis.wav for the sidecar — same
/// 16 kHz mono content `read_range_samples` hands whisper (pcm wav seeks are
/// sample-exact; no resample happens).
fn cut_wav(ffmpeg: &Path, src: &Path, range: TimeRange, out: &Path) -> anyhow::Result<()> {
    let status = Command::new(ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .arg("-ss")
        .arg(format!("{}", range.start_s))
        .arg("-t")
        .arg(format!("{}", range.duration_s()))
        .arg("-i")
        .arg(src)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(out)
        .status()?;
    anyhow::ensure!(status.success(), "ffmpeg cut failed ({status})");
    Ok(())
}

/// Everything after Qwen3-ASR's `language ...<asr_text>` header, if present —
/// the transcript proper. Falls back to the whole trimmed output.
fn parse_qwen_text(stdout: &str) -> String {
    let t = stdout.trim();
    match t.find("<asr_text>") {
        Some(i) => t[i + "<asr_text>".len()..].trim().to_string(),
        None => t.to_string(),
    }
}

/// Crude timestamp-fidelity probe over the RAW sidecar output: anything that
/// looks like a time token (`<|12.34|>`, `[00:12]`, `12.3s`) is evidence.
/// Report-only (grill decision: not a gate).
fn timestamp_evidence(raw: &str) -> String {
    let mut hits: Vec<&str> = Vec::new();
    for pat in ["<|", "[0", "[1", "[2"] {
        if raw.contains(pat) {
            hits.push(pat);
        }
    }
    if hits.is_empty() {
        "none seen (no time-like tokens in the sidecar output)".to_string()
    } else {
        format!("candidate time-like tokens present ({}) - inspect the raw block", hits.join(", "))
    }
}

fn run_qwen(fixture: &Fixture, clip_wav: &Path) -> anyhow::Result<EngineTake> {
    let cli = PathBuf::from(MTMD_CLI);
    anyhow::ensure!(
        cli.is_file(),
        "llama-mtmd-cli sidecar missing: {} (run scripts\\fetch-llama-sidecar.ps1)",
        cli.display()
    );
    let t0 = Instant::now();
    let out = Command::new(&cli)
        .arg("-m")
        .arg(QWEN_MODEL)
        .arg("--mmproj")
        .arg(QWEN_MMPROJ)
        .arg("--audio")
        .arg(clip_wav)
        .args(["--temp", "0", "-ngl", "99", "-p", "Transcribe the audio."])
        .output()?;
    let wall_s = t0.elapsed().as_secs_f64();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // Forensics beside the report; stderr carries llama.cpp's own logs.
    let stem = clip_wav.file_stem().and_then(|s| s.to_str()).unwrap_or("clip");
    std::fs::write(Path::new(OUT_DIR).join(format!("qwen_{stem}.stdout.txt")), &stdout)?;
    std::fs::write(Path::new(OUT_DIR).join(format!("qwen_{stem}.stderr.txt")), &stderr)?;
    anyhow::ensure!(
        out.status.success(),
        "llama-mtmd-cli failed ({}): see {}/qwen_{stem}.stderr.txt",
        out.status,
        OUT_DIR
    );
    let text = parse_qwen_text(&stdout);
    let dur = fixture.range.duration_s();
    Ok(EngineTake {
        name: "Qwen3-ASR raw",
        units: vec![(0.0, dur, text)],
        raw: Some(stdout),
        wall_s,
    })
}

/// Render one engine's units, collapsing runs of identical text (the eh-pile
/// prints as one `eh xN` line instead of 115) — display only, counts stay real.
fn print_units(report: &mut String, take: &EngineTake) {
    let mut line = |s: String| {
        println!("{s}");
        report.push_str(&s);
        report.push('\n');
    };
    line(format!(
        "--- {} ({} unit(s), {:.1}s wall) ---",
        take.name,
        take.units.len(),
        take.wall_s
    ));
    let mut i = 0;
    while i < take.units.len() {
        let (s0, _, text) = &take.units[i];
        let mut j = i + 1;
        while j < take.units.len() && take.units[j].2.trim() == text.trim() {
            j += 1;
        }
        let (_, e_last, _) = &take.units[j - 1];
        if j - i > 2 {
            line(format!("  [{:7.2}-{:7.2}] {} x{}", s0, e_last, text.trim(), j - i));
        } else {
            for k in i..j {
                let (s, e, t) = &take.units[k];
                line(format!("  [{:7.2}-{:7.2}] {}", s, e, t.trim()));
            }
        }
        i = j;
    }
    if let Some(raw) = &take.raw {
        line(format!("  timestamp fidelity: {}", timestamp_evidence(raw)));
        line("  raw sidecar output:".to_string());
        for l in raw.trim().lines() {
            line(format!("    | {l}"));
        }
    }
}

fn main() -> anyhow::Result<()> {
    // Same subscriber as the app (ADR 0033 lesson): without it the dialect-store
    // line, the decode config, and whisper.cpp's hooked logs are silently
    // dropped. Logs to stderr; the report stays clean on stdout.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut args = std::env::args().skip(1);
    let fixtures = match args.next() {
        Some(wav) => {
            let start_s: f64 = args.next().expect("usage: asr_ab_diag [<wav> <start_s> <end_s> [lang]]").parse()?;
            let end_s: f64 = args.next().expect("end_s").parse()?;
            let lang = match args.next().as_deref() {
                Some("en") => Language::En,
                Some("ja") => Language::Ja,
                _ => Language::Id,
            };
            vec![Fixture {
                label: "custom clip",
                wav: PathBuf::from(wav),
                range: TimeRange { start_s, end_s },
                lang,
            }]
        }
        None => benchmark_fixtures()?,
    };

    let whisper_model = PathBuf::from(WHISPER_MODEL);
    anyhow::ensure!(whisper_model.is_file(), "whisper model missing: {}", whisper_model.display());
    anyhow::ensure!(
        Path::new(QWEN_MODEL).is_file() && Path::new(QWEN_MMPROJ).is_file(),
        "Qwen3-ASR GGUFs missing under models/ (need {QWEN_MODEL} + {QWEN_MMPROJ})"
    );
    std::fs::create_dir_all(OUT_DIR)?;
    let ffmpeg = PathBuf::from("sidecars/ffmpeg.exe");
    anyhow::ensure!(ffmpeg.is_file(), "ffmpeg sidecar missing: {}", ffmpeg.display());

    let mut report = String::from("# ASR A/B: whisper large-v3 vs Qwen3-ASR-1.7B-Q8_0\n\n");

    // ---- Phase 1: whisper (one resident model, two decodes per fixture) ----
    eprintln!("[asr_ab_diag] loading whisper large-v3 (GPU)...");
    let transcriber = Transcriber::load(&whisper_model)?;
    let mut whisper_takes: Vec<(Vec<(f64, f64, String)>, Vec<(f64, f64, String)>, f64, f64, usize)> =
        Vec::new();
    for f in &fixtures {
        let samples = read_range_samples(&f.wav, f.range)?;
        eprintln!(
            "[asr_ab_diag] whisper x2 on '{}' ({:.1}s of audio)...",
            f.label,
            samples.len() as f64 / WHISPER_SR as f64
        );
        let t0 = Instant::now();
        let raw = transcriber.transcribe(&samples, f.lang, &DialectLexicon::default(), || false)?;
        let raw_wall = t0.elapsed().as_secs_f64();
        let lex = layered_lexicon(&f.wav, f.lang);
        let n_corr = lex.corrections.iter().filter(|c| !c.right.is_empty()).count();
        let t1 = Instant::now();
        let prod = transcriber.transcribe(&samples, f.lang, &lex, || false)?;
        let prod_wall = t1.elapsed().as_secs_f64();
        whisper_takes.push((
            raw.units.iter().map(|u| (u.start_s, u.end_s, u.text.clone())).collect(),
            prod.units.iter().map(|u| (u.start_s, u.end_s, u.text.clone())).collect(),
            raw_wall,
            prod_wall,
            n_corr,
        ));
    }
    // ---- Phase 2: whisper OUT before Qwen in (strict sequential GPU rule) ----
    drop(transcriber);
    eprintln!("[asr_ab_diag] whisper unloaded; starting Qwen3-ASR sidecar passes...");

    // ---- Phase 3: Qwen3-ASR via llama-mtmd-cli, one spawn per fixture ----
    let mut qwen_takes: Vec<EngineTake> = Vec::new();
    for (i, f) in fixtures.iter().enumerate() {
        let clip_wav = Path::new(OUT_DIR).join(format!("clip_{i}_{}s.wav", f.range.start_s as u64));
        cut_wav(&ffmpeg, &f.wav, f.range, &clip_wav)?;
        eprintln!("[asr_ab_diag] qwen on '{}'...", f.label);
        qwen_takes.push(run_qwen(f, &clip_wav)?);
    }

    // ---- Phase 4: the side-by-side report ----
    for (i, f) in fixtures.iter().enumerate() {
        let (raw_units, prod_units, raw_wall, prod_wall, n_corr) = &whisper_takes[i];
        let header = format!(
            "\n=== {} ===\n    {} [{:.1}-{:.1}s] {}  ({} confirmed corrections in store)\n",
            f.label,
            f.wav.display(),
            f.range.start_s,
            f.range.end_s,
            lang_token(f.lang),
            n_corr
        );
        print!("{header}");
        report.push_str(&header);
        print_units(
            &mut report,
            &EngineTake { name: "whisper RAW (empty lexicon)", units: raw_units.clone(), raw: None, wall_s: *raw_wall },
        );
        print_units(
            &mut report,
            &EngineTake { name: "whisper PROD (layered store)", units: prod_units.clone(), raw: None, wall_s: *prod_wall },
        );
        print_units(&mut report, &qwen_takes[i]);

        // Harness ground-truth anchor (ADR 0033's documented Diskusi output):
        // PROD must reproduce the curated render — 23 units, "dicegat" present.
        // A drift here means the harness is NOT on the production path; the
        // whole report would be untrustworthy (WARN, not abort, so the evidence
        // still prints).
        if f.label.starts_with("Diskusi") {
            let has_dicegat = prod_units.iter().any(|(_, _, t)| t.to_lowercase().contains("dicegat"));
            let check = format!(
                "CHECK Diskusi PROD anchor: units={} (expect 23), dicegat={} (expect true) -> {}\n",
                prod_units.len(),
                has_dicegat,
                if prod_units.len() == 23 && has_dicegat { "PASS" } else { "WARN: harness off the production path?" }
            );
            print!("{check}");
            report.push_str(&check);
        }
    }

    let report_path = Path::new(OUT_DIR).join("report.md");
    std::fs::write(&report_path, &report)?;
    println!("\n(report written to {})", report_path.display());
    Ok(())
}
