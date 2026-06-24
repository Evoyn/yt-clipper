//! The M2 pipeline worker: one background thread (ADR 0005) running the
//! two-phase ingest off the UI thread, reporting progress over a channel.
//!
//! - **Import** resolves a VOD (YouTube URL or local file) to its whole-VOD
//!   analysis audio + raw chat + metadata, and persists `project.json` in the
//!   per-VOD workspace folder. It leaves a [`Session`] the worker remembers.
//! - **Prepare** (ADR 0012) fetches the padded Segment, probes it for the
//!   in-segment offset and layout dimensions, auto-detects the seed Layout
//!   (Ultraface locates the Facecam; `build_layout` picks stacked / full-cam /
//!   full-frame, M6/ADR 0011), and extracts a handful of preview frames. It
//!   leaves a [`PreparedClip`] the worker holds for the matching Render.
//! - **Render** composites the operator's (possibly nudged in the editor) Layout
//!   over the prepared Segment: transcribe the range (once, then cached on the
//!   PreparedClip so re-renders are NVENC-only), generate captions, NVENC export.
//!
//! Splitting promote in two is what lets the nudge editor interject between the
//! auto-detected Layout and the render (ADR 0012); whisper is deferred to Render
//! so Prepare stays CPU/network and the editor opens fast.
//!
//! A [`CancelToken`] (shared with the UI) kills the yt-dlp/ffmpeg child tree
//! mid-download - the M1 "hang" scar.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result};
use yc_core::{
    CaptionGenre, CaptionStyle, Clip, Language, Layout, Moment, Project, Signals, TimeRange,
    Transcript, Vod, VodSource,
};
use yc_detect::DetectParams;
use yc_ingest::{CancelToken, Sidecars};

/// Resolved inputs the worker needs, captured once at spawn.
#[derive(Clone)]
pub struct PipelinePaths {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub ytdlp: PathBuf,
    pub deno_dir: Option<PathBuf>,
    pub model: PathBuf,
    /// CPU speech-emotion model for the arousal Signal (ADR 0008). May be
    /// absent: detection runs without it (combined_score renormalizes). Only
    /// read by the `ser`-gated refine pass.
    #[cfg_attr(not(feature = "ser"), allow(dead_code))]
    pub ser_model: PathBuf,
    /// LLM judgment GGUF for the `llm` Signal (ADR 0010). With `llm_judge` present
    /// the app runs the out-of-process judge over it; if either is missing the
    /// signal is omitted (combined_score renormalizes), like a missing SER model.
    pub llm_model: PathBuf,
    /// The `yc-llm-judge` sidecar binary (it links llama; the app does not).
    /// Absent where it was not built/shipped - detection then omits the llm signal.
    pub llm_judge: PathBuf,
    /// Ultraface face model for M6 auto-framing (ADR 0011). May be absent:
    /// framing falls back to full-frame gameplay (combined with a non-`face`
    /// build). Only read by the `face`-gated auto-frame pass.
    #[cfg_attr(not(feature = "face"), allow(dead_code))]
    pub face_model: PathBuf,
    /// htdemucs vocals model for the Vocal-stem captions (`sep`). May be absent:
    /// the export then captions the mixed analysis audio. Only read by the
    /// `sep`-gated caption pass.
    #[cfg_attr(not(feature = "sep"), allow(dead_code))]
    pub sep_model: PathBuf,
    /// Directory of per-language dialect/slang correction stores (`<lang>.json`,
    /// see `yc_transcribe::DialectLexicon`). Primes whisper + patches known
    /// mishears; a missing file just disables the fix-ups for that language.
    pub dialect_dir: PathBuf,
    pub font: PathBuf,
    pub workspace: PathBuf,
}

impl PipelinePaths {
    fn sidecars(&self) -> Sidecars {
        Sidecars {
            ytdlp: self.ytdlp.clone(),
            ffmpeg: self.ffmpeg.clone(),
            ffprobe: self.ffprobe.clone(),
            deno_dir: self.deno_dir.clone(),
        }
    }
}

/// Where a VOD is imported from.
pub enum ImportSource {
    YouTube(String),
    Local(PathBuf),
}

/// A unit of work requested by the UI. Cancel is out-of-band (the worker is
/// busy inside a job), so it travels via the [`CancelToken`], not this channel.
pub enum Job {
    Import { source: ImportSource, language: Language },
    /// Run detection over the imported VOD (discover + refine), surfacing
    /// ranked candidate Moments (ADR 0007). Operates on the current session.
    Detect,
    /// Phase-2a (ADR 0012): fetch the padded Segment, probe it, auto-detect the
    /// seed Layout, and extract preview frames for the nudge editor. Leaves a
    /// [`PreparedClip`] the worker holds for the matching [`Job::Render`].
    Prepare { range: TimeRange },
    /// Phase-2b (ADR 0012): render the operator's (possibly nudged) `layout`
    /// over the held [`PreparedClip`] - transcribe (once, then cached), caption,
    /// NVENC export.
    Render { layout: Layout },
}

/// Whole-VOD signal series for the review waveform, one value per `bin_s` bin
/// (raw RMS loudness and, when present, viewer-message counts). The UI
/// normalizes and downsamples to pixel width when painting.
pub struct Timeline {
    pub bin_s: f64,
    pub loudness: Vec<f32>,
    pub chat: Option<Vec<f32>>,
}

/// Progress reported back to the UI thread.
pub enum Progress {
    Stage(&'static str),
    /// Import finished; the VOD is ready to detect / promote ranges from.
    /// `analysis_wav` lets the review UI play a Moment's audio range.
    Imported { title: String, duration_s: Option<f64>, analysis_wav: PathBuf },
    /// Detection finished; ranked candidate Moments, each one's transcript text
    /// (keyed by Moment id), the LLM judgment Signal's one-line reason per Moment
    /// id (`llm_reasons`, empty unless built `--features llm` with the GGUF
    /// present; ADR 0010), and the whole-VOD signal timeline for the waveform.
    Detected {
        moments: Vec<Moment>,
        transcripts: HashMap<u64, String>,
        llm_reasons: HashMap<u64, String>,
        timeline: Timeline,
    },
    /// Prepare finished (ADR 0012): the auto-detected seed Layout, the Segment's
    /// source dimensions, and a handful of preview frames (raw rgb24, each
    /// `frame_w` x `frame_h`) sampled across the clip range. The UI uploads the
    /// frames to textures and opens the nudge editor seeded with `layout`;
    /// headless echoes `layout` straight back as a `Render` (no nudging).
    Prepared {
        layout: Layout,
        src_w: f32,
        src_h: f32,
        frames: Vec<Vec<u8>>,
        frame_w: u32,
        frame_h: u32,
        range: TimeRange,
    },
    /// A Clip rendered to this path.
    Done(PathBuf),
    Cancelled,
    Failed(String),
}

/// What an import leaves ready for promotion. The worker holds one between jobs.
struct Session {
    vod: Vod,
    promote: PromoteSource,
    workdir: PathBuf,
    analysis_wav: PathBuf,
}

/// How `Prepare` obtains the video to render from, and where it seeks.
enum PromoteSource {
    /// Fetch a padded Segment per-promote (web_safari HLS); seek the offset.
    YouTube(String),
    /// Use the local file directly as the "Segment"; seek the range start.
    Local(PathBuf),
}

/// What `Prepare` leaves ready for the nudge editor and the matching `Render`
/// (ADR 0012): the resolved render source + in-segment seek + source
/// dimensions, the picked range, the auto-detected seed Layout, and - once the
/// first Render has run - the cached transcript, so re-renders skip whisper and
/// are NVENC-only. The worker holds one until the next Prepare or Import.
struct PreparedClip {
    render_src: PathBuf,
    seek_s: f64,
    src_w: f32,
    src_h: f32,
    range: TimeRange,
    auto_layout: Layout,
    transcript: Option<Transcript>,
}

/// Spawn the worker thread. Returns the job sender, the progress receiver, and a
/// [`CancelToken`] the UI flips to kill an in-flight download.
pub fn spawn(paths: PipelinePaths) -> (Sender<Job>, Receiver<Progress>, CancelToken) {
    let (tx_job, rx_job) = mpsc::channel::<Job>();
    let (tx_prog, rx_prog) = mpsc::channel::<Progress>();
    let cancel = CancelToken::new();
    let worker_cancel = cancel.clone();
    thread::spawn(move || {
        let mut session: Option<Session> = None;
        // The clip prepared for the nudge editor, held between Prepare and its
        // Render(s) (ADR 0012). Invalidated by a new Import or Prepare.
        let mut prepared: Option<PreparedClip> = None;
        while let Ok(job) = rx_job.recv() {
            // A cancel of the previous job must not bleed into this one.
            worker_cancel.reset();
            match job {
                Job::Import { source, language } => {
                    match do_import(&paths, source, language, &worker_cancel, &tx_prog) {
                        Ok(s) => {
                            let _ = tx_prog.send(Progress::Imported {
                                title: s.vod.title.clone(),
                                duration_s: s.vod.duration_s,
                                analysis_wav: s.analysis_wav.clone(),
                            });
                            session = Some(s);
                            prepared = None; // a new VOD invalidates any prepared clip
                        }
                        Err(e) => {
                            let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                        }
                    }
                }
                Job::Detect => match &session {
                    None => {
                        let _ = tx_prog
                            .send(Progress::Failed("import a VOD before detecting".into()));
                    }
                    Some(s) => match do_detect(&paths, s, &worker_cancel, &tx_prog) {
                        Ok((moments, transcripts, llm_reasons, timeline)) => {
                            let _ = tx_prog.send(Progress::Detected {
                                moments,
                                transcripts,
                                llm_reasons,
                                timeline,
                            });
                        }
                        Err(e) => {
                            let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                        }
                    },
                },
                Job::Prepare { range } => match &session {
                    None => {
                        let _ = tx_prog
                            .send(Progress::Failed("import a VOD before making a clip".into()));
                    }
                    Some(s) => match do_prepare(&paths, s, range, &worker_cancel, &tx_prog) {
                        Ok((pc, frames, frame_w, frame_h)) => {
                            let _ = tx_prog.send(Progress::Prepared {
                                layout: pc.auto_layout.clone(),
                                src_w: pc.src_w,
                                src_h: pc.src_h,
                                frames,
                                frame_w,
                                frame_h,
                                range: pc.range,
                            });
                            prepared = Some(pc);
                        }
                        Err(e) => {
                            let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                        }
                    },
                },
                Job::Render { layout } => match (&session, &mut prepared) {
                    (Some(s), Some(pc)) => {
                        match do_render(&paths, s, pc, layout, &worker_cancel, &tx_prog) {
                            Ok(out) => {
                                let _ = tx_prog.send(Progress::Done(out));
                            }
                            Err(e) => {
                                let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                            }
                        }
                    }
                    _ => {
                        let _ = tx_prog
                            .send(Progress::Failed("prepare a clip before rendering".into()));
                    }
                },
            }
        }
    });
    (tx_job, rx_prog, cancel)
}

/// A job that ended in error reports as `Cancelled` if the token was flipped
/// (the child was tree-killed), else as a genuine `Failed`.
fn fail_or_cancel(e: anyhow::Error, cancel: &CancelToken) -> Progress {
    if cancel.is_cancelled() {
        Progress::Cancelled
    } else {
        Progress::Failed(format!("{e:#}"))
    }
}

// --- import (phase 1) -------------------------------------------------------

fn do_import(
    paths: &PipelinePaths,
    source: ImportSource,
    language: Language,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<Session> {
    anyhow::ensure!(paths.ffmpeg.is_file(), "ffmpeg sidecar missing - run fetch-sidecars.ps1");
    match source {
        ImportSource::YouTube(url) => import_youtube(paths, url, language, cancel, tx),
        ImportSource::Local(path) => import_local(paths, path, language, tx),
    }
}

fn import_youtube(
    paths: &PipelinePaths,
    url: String,
    language: Language,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<Session> {
    anyhow::ensure!(paths.ytdlp.is_file(), "yt-dlp sidecar missing - run fetch-sidecars.ps1");
    let sc = paths.sidecars();

    let _ = tx.send(Progress::Stage("Fetching metadata"));
    let vod = yc_ingest::youtube_metadata(&sc, &url, language, cancel)?;
    let video_id = match &vod.source {
        VodSource::YouTube { video_id } => video_id.clone(),
        // youtube_metadata always builds a YouTube source.
        VodSource::LocalFile { .. } => anyhow::bail!("expected a YouTube VOD"),
    };

    // Folder-per-VOD, keyed by video_id (Creator-scoped folders arrive at M5/M6).
    let workdir = paths.workspace.join(&video_id);
    fs::create_dir_all(&workdir).with_context(|| format!("creating {}", workdir.display()))?;

    let _ = tx.send(Progress::Stage("Downloading audio"));
    let analysis_wav = yc_ingest::youtube_fetch_audio(&sc, &url, &workdir, cancel)?;

    let _ = tx.send(Progress::Stage("Fetching chat"));
    let _chat = yc_ingest::youtube_fetch_chat(&sc, &url, &workdir, cancel)?;

    save_project(&vod, &workdir)?;
    Ok(Session { vod, promote: PromoteSource::YouTube(url), workdir, analysis_wav })
}

fn import_local(
    paths: &PipelinePaths,
    path: PathBuf,
    language: Language,
    tx: &Sender<Progress>,
) -> Result<Session> {
    // Absolutize: the export runs ffmpeg in the clip folder (so libass resolves
    // clip.ass + the font by relative name), so a relative source path would
    // resolve against the wrong cwd there. YouTube Segments are already absolute.
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().context("resolving current dir")?.join(path)
    };
    anyhow::ensure!(path.is_file(), "video not found: {}", path.display());
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".into());
    let workdir = paths.workspace.join(&stem);
    fs::create_dir_all(&workdir).with_context(|| format!("creating {}", workdir.display()))?;

    let _ = tx.send(Progress::Stage("Extracting audio"));
    let analysis_wav = workdir.join("analysis.wav");
    yc_ingest::extract_audio(&paths.ffmpeg, &path, &analysis_wav)?;

    let vod = Vod {
        creator: "local".into(),
        title: stem,
        source: VodSource::LocalFile { path: path.clone() },
        language,
        duration_s: None,
    };
    save_project(&vod, &workdir)?;
    Ok(Session { vod, promote: PromoteSource::Local(path), workdir, analysis_wav })
}

fn save_project(vod: &Vod, workdir: &Path) -> Result<()> {
    Project::new(vod.clone())
        .save(&workdir.join("project.json"))
        .with_context(|| format!("writing project.json in {}", workdir.display()))
}

/// Load the VOD's persisted project, or start a fresh one if none exists / it is
/// unreadable. Lets detect and promote update `project.json` without clobbering
/// each other's records.
fn load_or_new_project(vod: &Vod, workdir: &Path) -> Project {
    Project::load(&workdir.join("project.json")).unwrap_or_else(|_| Project::new(vod.clone()))
}

// --- detect (M3) ------------------------------------------------------------

/// Detect candidate Moments over the imported VOD (ADR 0007). Discover with the
/// cheap whole-VOD signals (chat-rate + loudness, no whisper), then refine:
/// transcribe each candidate once with a *resident* whisper model and score the
/// excitement lexicon. Returns Moments ranked by final score; persists them to
/// `project.json`.
fn do_detect(
    paths: &PipelinePaths,
    session: &Session,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<(Vec<Moment>, HashMap<u64, String>, HashMap<u64, String>, Timeline)> {
    anyhow::ensure!(paths.model.is_file(), "whisper model missing - run fetch-models.ps1");
    let params = DetectParams::default();

    // Discover: cheap, whole-VOD, no GPU. Read the bins here (rather than via
    // yc_detect::discover) so the same series feeds both the ranking and the
    // review timeline - no second wav read.
    let _ = tx.send(Progress::Stage("Detecting moments (chat + loudness)"));
    let chat = session.workdir.join("chat.live_chat.json");
    let loud_bins = yc_detect::loudness::read_rms_bins(&session.analysis_wav, params.bin_s)?;
    let chat_bins = if chat.exists() {
        let offsets = yc_detect::chat::message_offsets(&chat)?;
        Some(yc_detect::score::bin_counts(&offsets, params.bin_s, loud_bins.len()))
    } else {
        None
    };
    let mut moments = yc_detect::rank_moments(&loud_bins, chat_bins.as_deref(), &params);
    let timeline = Timeline { bin_s: params.bin_s, loudness: loud_bins, chat: chat_bins };
    if moments.is_empty() {
        tracing::info!("no moments discovered");
        return Ok((moments, HashMap::new(), HashMap::new(), timeline));
    }

    // Refine: one model load for the whole candidate batch (the resident
    // Transcriber), then lexicon-score each transcript. Keep each transcript's
    // text for the review UI (and, later, M4's LLM).
    let _ = tx.send(Progress::Stage("Refining moments (whisper, GPU)"));
    // Text-only (no DTW): the lexicon needs words, not word timing, and DTW
    // aborts on sparse music/SFX windows (see Transcriber::load_text_only).
    let lexicon = yc_transcribe::DialectLexicon::load(&paths.dialect_dir, session.vod.language);
    let transcriber = yc_transcribe::Transcriber::load_text_only(&paths.model)?;
    let mut densities = Vec::with_capacity(moments.len());
    let mut texts = Vec::with_capacity(moments.len());
    for m in &moments {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let samples = yc_ingest::read_range_samples(&session.analysis_wav, m.range)?;
        let transcript = transcriber.transcribe(&samples, session.vod.language, &lexicon, {
            let c = cancel.clone();
            move || c.is_cancelled()
        })?;
        densities.push(yc_detect::lexicon::density(&transcript, session.vod.language));
        texts.push(
            transcript.units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>().join(" "),
        );
    }
    drop(transcriber); // free VRAM before any later GPU stage (M5 LLM)
    yc_detect::lexicon::apply(&mut moments, &densities, &params.weights);

    // Arousal (ADR 0008): a CPU speech-emotion model scores how emotionally
    // *activated* the streamer's voice is, demoting loud-but-flat moments
    // (game explosions, music, cutscenes). Runs after the whisper drop (CPU, no
    // VRAM). Only when built `--features ser` and the model is present;
    // combined_score renormalizes when arousal is absent, so detection still
    // ranks fine without it.
    #[cfg(feature = "ser")]
    if paths.ser_model.is_file() {
        let _ = tx.send(Progress::Stage("Refining moments (arousal, CPU)"));
        let sr = yc_ingest::WHISPER_SR as f64;
        let win = (yc_detect::arousal::WINDOW_S * sr) as usize;
        let hop = (yc_detect::arousal::HOP_S * sr) as usize;
        let mut ser = yc_detect::arousal::Ser::load(&paths.ser_model)?;
        let mut arousals = Vec::with_capacity(moments.len());
        for m in &moments {
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }
            let samples = yc_ingest::read_range_samples(&session.analysis_wav, m.range)?;
            arousals.push(ser.arousal_max(&samples, win, hop)?);
        }
        yc_detect::arousal::apply(&mut moments, &arousals, &params.weights);
    }

    // LLM judgment (ADR 0010): a local GGUF model reads each candidate's
    // transcript and scores clip-worthiness, instructed to discount scripted
    // game narration (the load-bearing mitigation - ADR 0009's #11 cutscene at
    // arousal 1.107, streamer silent). Runs on the GPU after the whisper `drop`
    // (VRAM free) and after the CPU arousal pass, so the prompt can corroborate
    // against the z-scored chat/loudness/arousal now on each Moment. Only when
    // built `--features llm` with the GGUF present; combined_score renormalizes
    // when the llm signal is absent, so detection still ranks fine without it.
    // LLM judgment (ADR 0010): runs in the separate `yc-llm-judge` process
    // (whisper.cpp and llama.cpp can't co-link - duplicate ggml). We hand the
    // whole candidate batch to it as JSON over stdin and read back one verdict per
    // candidate. The child loads the GGUF after our whisper `drop` freed VRAM,
    // scores, and frees that VRAM on exit - sequential GPU staging across the
    // process boundary. Skipped (signal omitted, combined_score renormalizes) when
    // the judge binary or the GGUF is absent, exactly like a missing SER model.
    let mut llm_reasons_vec: Vec<String> = Vec::new();
    if paths.llm_judge.is_file() && paths.llm_model.is_file() {
        let _ = tx.send(Progress::Stage("Refining moments (LLM judgment, GPU)"));
        let request = yc_detect::llm::JudgeRequest {
            model_path: paths.llm_model.to_string_lossy().into_owned(),
            language: session.vod.language,
            candidates: moments
                .iter()
                .enumerate()
                .map(|(i, m)| yc_detect::llm::JudgeCandidate {
                    transcript: texts[i].clone(),
                    chat_z: m.signals.chat_rate,
                    loudness_z: m.signals.loudness,
                    arousal_z: m.signals.arousal,
                })
                .collect(),
        };
        match run_llm_judge(&paths.llm_judge, &request, cancel) {
            Ok(verdicts) if verdicts.len() == moments.len() => {
                let scores: Vec<f32> = verdicts.iter().map(|v| v.score).collect();
                llm_reasons_vec = verdicts.into_iter().map(|v| v.reason).collect();
                yc_detect::llm::apply(&mut moments, &scores, &params.weights);
            }
            Ok(v) => tracing::warn!(
                "llm-judge returned {} verdicts for {} moments; omitting llm signal",
                v.len(),
                moments.len()
            ),
            Err(e) if cancel.is_cancelled() => return Err(e),
            Err(e) => tracing::warn!("llm-judge failed: {e:#}; omitting llm signal"),
        }
    }

    // Rank by final score; carry each Moment's transcript + LLM reason through
    // the reorder, then renumber so ids read as the review rank.
    let mut order: Vec<usize> = (0..moments.len()).collect();
    order.sort_by(|&a, &b| {
        moments[b].score.partial_cmp(&moments[a].score).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut ranked = Vec::with_capacity(moments.len());
    let mut transcripts = HashMap::with_capacity(moments.len());
    let mut llm_reasons = HashMap::new();
    for (rank, &i) in order.iter().enumerate() {
        let mut m = moments[i].clone();
        m.id = (rank + 1) as u64;
        transcripts.insert(m.id, texts[i].clone());
        if let Some(r) = llm_reasons_vec.get(i).filter(|r| !r.trim().is_empty()) {
            llm_reasons.insert(m.id, r.clone());
        }
        ranked.push(m);
    }

    let mut project = load_or_new_project(&session.vod, &session.workdir);
    project.moments = ranked.clone();
    project
        .save(&session.workdir.join("project.json"))
        .with_context(|| format!("writing project.json in {}", session.workdir.display()))?;
    Ok((ranked, transcripts, llm_reasons, timeline))
}

/// Run the out-of-process LLM judge over the whole candidate batch (ADR 0010):
/// write the [`yc_detect::llm::JudgeRequest`] to its stdin, read the verdicts from
/// its stdout. stderr is inherited so llama.cpp's load logs reach the operator's
/// terminal. The child is killed if the detect is cancelled mid-run (releasing its
/// VRAM). The payload is a few KB (well under the pipe buffer) and the response is
/// small, so writing the request fully before reading the reply can't deadlock.
fn run_llm_judge(
    bin: &Path,
    request: &yc_detect::llm::JudgeRequest,
    cancel: &CancelToken,
) -> Result<Vec<yc_detect::llm::JudgeVerdict>> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    let payload = serde_json::to_vec(request).context("serializing llm-judge request")?;
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning {}", bin.display()))?;

    child
        .stdin
        .take()
        .context("llm-judge stdin unavailable")?
        .write_all(&payload)
        .context("writing llm-judge request")?;

    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("cancelled");
        }
        match child.try_wait().context("waiting on llm-judge")? {
            Some(status) => {
                anyhow::ensure!(status.success(), "llm-judge exited with {status}");
                break;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }

    let mut out = String::new();
    child
        .stdout
        .take()
        .context("llm-judge stdout unavailable")?
        .read_to_string(&mut out)
        .context("reading llm-judge output")?;
    serde_json::from_str(&out).context("parsing llm-judge verdicts")
}

// --- prepare + render (phase 2, split for the nudge editor - ADR 0012) ------

/// Number of preview frames sampled across the clip range for the editor's
/// scrub slider (ADR 0012). A handful is enough to see whether a static crop
/// holds across a moving face.
const PREVIEW_FRAMES: usize = 7;
/// Longest preview-frame edge, in pixels. Crisp enough to place a face without
/// the texture memory of a full-resolution frame; the editor stores Crops in
/// source pixels and only normalizes at draw time, so this resolution is purely
/// preview fidelity.
const PREVIEW_LONG_EDGE: f32 = 1280.0;

/// Aspect-preserving preview-frame dimensions (even, >= 2) with the longest edge
/// at most [`PREVIEW_LONG_EDGE`]. Unlike the 320x240 detection pass (ADR 0011,
/// which tolerates aspect distortion), the editor preview must not distort.
fn preview_dims(src_w: f32, src_h: f32) -> (u32, u32) {
    let even = |v: f32| (((v.round().max(2.0)) as u32) / 2) * 2;
    if src_w >= src_h {
        let w = src_w.min(PREVIEW_LONG_EDGE);
        (even(w), even(w * src_h / src_w))
    } else {
        let h = src_h.min(PREVIEW_LONG_EDGE);
        (even(h * src_w / src_h), even(h))
    }
}

/// Phase-2a (ADR 0012): resolve the render source (fetch the padded Segment for
/// YouTube, or use the local file), probe it for the in-segment seek offset and
/// source resolution, auto-detect the seed Layout (M6/ADR 0011), and sample
/// preview frames across the clip range. CPU/network only - whisper is deferred
/// to `do_render` so the editor opens fast. Returns the [`PreparedClip`] plus
/// the preview frames and their dimensions for the UI to texture.
fn do_prepare(
    paths: &PipelinePaths,
    session: &Session,
    range: TimeRange,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<(PreparedClip, Vec<Vec<u8>>, u32, u32)> {
    anyhow::ensure!(range.duration_s() > 0.0, "pick a range with end > start");
    let sc = paths.sidecars();

    // Obtain the render source, the in-segment seek offset, and the source
    // resolution (for the Layout) - all from one ffprobe of the media.
    let (render_src, seek_s, src_w, src_h) = match &session.promote {
        PromoteSource::Local(path) => {
            let p = yc_ingest::probe_segment(&paths.ffprobe, path, cancel)?;
            (path.clone(), range.start_s, p.width as f32, p.height as f32)
        }
        PromoteSource::YouTube(url) => {
            let _ = tx.send(Progress::Stage("Fetching segment"));
            let padded = yc_ingest::pad_range(range, session.vod.duration_s);
            let segment = yc_ingest::fetch_segment(&sc, url, padded, &session.workdir, cancel)?;
            let p = yc_ingest::probe_segment(&paths.ffprobe, &segment, cancel)?;
            let offset = yc_ingest::in_segment_offset(range.start_s, padded.start_s, &p);
            (segment, offset, p.width as f32, p.height as f32)
        }
    };

    // Auto-detect the seed Layout: Ultraface locates the Facecam (CPU; no GPU
    // contention), and build_layout picks stacked / full-cam / full-frame, or
    // falls back to full-frame gameplay (no `face` feature/model, or no face).
    // The editor opens seeded with this and the operator nudges from there.
    let _ = tx.send(Progress::Stage("Framing (face detect)"));
    let auto_layout = build_layout(paths, &render_src, seek_s, src_w, src_h);

    // Sample preview frames across the clip range for the editor's scrub slider.
    let _ = tx.send(Progress::Stage("Extracting preview frames"));
    let (frame_w, frame_h) = preview_dims(src_w, src_h);
    let fps = (PREVIEW_FRAMES as f64 / range.duration_s()).max(0.1);
    let frames = yc_ingest::extract_frames_rgb(
        &paths.ffmpeg,
        &render_src,
        seek_s,
        frame_w,
        frame_h,
        fps,
        PREVIEW_FRAMES,
    )?;

    let prepared = PreparedClip {
        render_src,
        seek_s,
        src_w,
        src_h,
        range,
        auto_layout,
        transcript: None,
    };
    Ok((prepared, frames, frame_w, frame_h))
}

/// The 16 kHz-mono samples whisper captions from. With the `sep` feature and the
/// htdemucs model present, this is the **Vocal stem** of the Segment's range —
/// the streamer's voice split from music/SFX (CONTEXT.md), so whisper reads the
/// voice rather than the loudest sound in the mix. Otherwise it is the mixed
/// analysis-wav range (the historical path). Either way the rendered clip's
/// audible audio stays the mix — only what whisper *hears* changes.
#[cfg_attr(not(feature = "sep"), allow(unused_variables))]
fn caption_samples(
    paths: &PipelinePaths,
    session: &Session,
    prepared: &PreparedClip,
    range: TimeRange,
    tx: &Sender<Progress>,
) -> Result<Vec<f32>> {
    #[cfg(feature = "sep")]
    {
        if paths.sep_model.is_file() {
            let _ = tx.send(Progress::Stage("Separating vocal stem"));
            let wd = &session.workdir;
            let in44 = wd.join("_sep_in44k.wav");
            let voc44 = wd.join("_sep_voc44k.wav");
            let voc16 = wd.join("_sep_voc16k.wav");
            // 44.1 kHz stereo for the clip range, from the render source at its
            // in-segment offset — the same window the mix would caption.
            ffmpeg_extract_stereo_44k(
                &paths.ffmpeg,
                &prepared.render_src,
                prepared.seek_s,
                range.duration_s(),
                &in44,
            )?;
            yc_detect::sep::separate_vocals_wav(&paths.sep_model, &in44, &voc44)?;
            // Back to whisper's 16 kHz mono; the temp file spans exactly the range.
            ffmpeg_resample_16k_mono(&paths.ffmpeg, &voc44, &voc16)?;
            return yc_ingest::read_range_samples(
                &voc16,
                TimeRange { start_s: 0.0, end_s: range.duration_s() + 1.0 },
            );
        }
    }
    yc_ingest::read_range_samples(&session.analysis_wav, range)
}

/// Extract `dur_s` of `src` from `seek_s` as a 44.1 kHz **stereo** PCM wav — the
/// htdemucs vocal-sep input. `-ss` before `-i` fast-seeks (the render source is
/// the padded Segment, or the whole local file).
#[cfg(feature = "sep")]
fn ffmpeg_extract_stereo_44k(
    ffmpeg: &Path,
    src: &Path,
    seek_s: f64,
    dur_s: f64,
    out: &Path,
) -> Result<()> {
    let args: Vec<String> = vec![
        "-ss".into(),
        format!("{seek_s:.3}"),
        "-t".into(),
        format!("{dur_s:.3}"),
        "-i".into(),
        src.display().to_string(),
        "-map".into(),
        "0:a:0".into(),
        "-ac".into(),
        "2".into(),
        "-ar".into(),
        "44100".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        "-y".into(),
        out.display().to_string(),
    ];
    let status = std::process::Command::new(ffmpeg)
        .args(&args)
        .status()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    anyhow::ensure!(status.success(), "ffmpeg vocal-sep extract failed ({status})");
    Ok(())
}

/// Resample a wav to whisper's 16 kHz mono PCM (the vocal stem -> caption input).
#[cfg(feature = "sep")]
fn ffmpeg_resample_16k_mono(ffmpeg: &Path, src: &Path, out: &Path) -> Result<()> {
    let args: Vec<String> = vec![
        "-i".into(),
        src.display().to_string(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        yc_ingest::WHISPER_SR.to_string(),
        "-c:a".into(),
        "pcm_s16le".into(),
        "-y".into(),
        out.display().to_string(),
    ];
    let status = std::process::Command::new(ffmpeg)
        .args(&args)
        .status()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    anyhow::ensure!(status.success(), "ffmpeg vocal-sep resample failed ({status})");
    Ok(())
}

/// Phase-2b (ADR 0012): render the operator's `layout` over the prepared
/// Segment. Transcribes the range once (whisper, GPU) and caches it on the
/// `PreparedClip`, so a re-render after another nudge is NVENC-only. The
/// transcript captions the Vocal stem when `sep` is built, else the mixed track
/// (the loudest voice in it); see `caption_samples`.
fn do_render(
    paths: &PipelinePaths,
    session: &Session,
    prepared: &mut PreparedClip,
    layout: Layout,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<PathBuf> {
    anyhow::ensure!(paths.model.is_file(), "whisper model missing - run fetch-models.ps1");
    let range = prepared.range;

    // Transcribe once, then reuse: re-rendering a nudged Layout skips whisper.
    if prepared.transcript.is_none() {
        // Captions read the Vocal stem when `sep` is built (music/SFX split off
        // the streamer's voice); otherwise the mixed analysis audio, as before.
        let samples = caption_samples(paths, session, prepared, range, tx)?;
        let _ = tx.send(Progress::Stage("Transcribing (whisper, GPU)"));
        let lexicon =
            yc_transcribe::DialectLexicon::load(&paths.dialect_dir, session.vod.language);
        let (transcript, harvest) = yc_transcribe::transcribe_range_harvesting(
            &paths.model,
            &samples,
            session.vod.language,
            &lexicon,
            {
                let c = cancel.clone();
                move || c.is_cancelled()
            },
        )?;
        // Self-populate the store's review queue with words whisper was unsure
        // about (auto-harvest), unless the store froze it. Best-effort: a write
        // failure logs and never sinks the render.
        if lexicon.harvest {
            let n = yc_transcribe::DialectLexicon::harvest_to_store(
                &paths.dialect_dir,
                session.vod.language,
                &harvest,
            );
            if n > 0 {
                tracing::info!("dialect: harvested {n} low-confidence word(s) to review");
            }
        }
        // Refine caption end-times to the streamer's actual vocalization: a
        // screamed / drawn-out word holds for its full sound and a normal word
        // clears when the sound drops, instead of huge-word's fixed hold.
        let transcript =
            yc_render::refine_caption_timing(transcript, &samples, yc_ingest::WHISPER_SR);
        prepared.transcript = Some(transcript);
    }
    let transcript = prepared.transcript.as_ref().expect("transcript set above");

    // Captions: generate the ASS and copy the font beside it (libass finds it
    // via fontsdir=., dodging Windows filtergraph path escaping).
    let _ = tx.send(Progress::Stage("Generating captions"));
    let style = caption_style();
    let ass = yc_render::generate_ass(transcript, &style);
    fs::write(session.workdir.join("clip.ass"), ass).context("writing clip.ass")?;
    fs::copy(&paths.font, session.workdir.join("Anton-Regular.ttf"))
        .with_context(|| format!("copying font from {}", paths.font.display()))?;

    // Record the promoted Clip with the operator's Layout, then render it.
    let clip = build_clip(range, layout, &style.name);
    persist_clip(&session.vod, &clip, &session.workdir)?;

    let _ = tx.send(Progress::Stage("Rendering (NVENC)"));
    let filtergraph = yc_render::build_filtergraph(&clip.layout, "clip.ass");
    let args = yc_render::export_args(
        &prepared.render_src,
        prepared.seek_s,
        range.duration_s(),
        &filtergraph,
        "export.mp4",
    );
    yc_render::run_export(&paths.ffmpeg, &session.workdir, &args)?;

    Ok(session.workdir.join("export.mp4"))
}

/// A manually-picked range -> a marked Moment promoted to a Clip (CONTEXT.md).
fn build_clip(range: TimeRange, layout: Layout, caption_style: &str) -> Clip {
    Clip {
        id: 1,
        moment_id: 1,
        range,
        layout,
        caption_style: caption_style.to_string(),
        segment_path: None,
        export_path: None,
    }
}

/// Re-save `project.json` with the promoted Clip recorded, preserving any
/// detected Moments. Records the source Moment only if it isn't already known
/// (e.g. a directly promoted range that never went through detection).
fn persist_clip(vod: &Vod, clip: &Clip, workdir: &Path) -> Result<()> {
    let mut project = load_or_new_project(vod, workdir);
    if !project.moments.iter().any(|m| m.id == clip.moment_id) {
        project.moments.push(Moment {
            id: clip.moment_id,
            range: clip.range,
            signals: Signals::default(),
            score: 0.0,
        });
    }
    // Replace any existing record of this Clip (a re-render after a nudge) so
    // project.json holds one entry per Clip, not one per render (ADR 0012).
    project.clips.retain(|c| c.id != clip.id);
    project.clips.push(clip.clone());
    project
        .save(&workdir.join("project.json"))
        .with_context(|| format!("writing project.json in {}", workdir.display()))
}

// --- auto-detect framing (M6, ADR 0011) -------------------------------------

/// Choose the Clip's Layout by detecting the Facecam in the Segment's frames.
/// With the `face` feature and the model present, sample frames, run Ultraface,
/// cluster the static Facecam, and pick stacked / full-cam / full-frame
/// gameplay. Otherwise (no feature, missing model, or a detection error) fall
/// back to full-frame gameplay - a safe default that never misplaces a facecam,
/// unlike the old hardcoded bottom-right Stacked scaffolding it replaces.
#[cfg_attr(not(feature = "face"), allow(unused_variables))]
fn build_layout(
    paths: &PipelinePaths,
    render_src: &Path,
    seek_s: f64,
    src_w: f32,
    src_h: f32,
) -> Layout {
    #[cfg(feature = "face")]
    {
        if paths.face_model.is_file() {
            match detect_layout(paths, render_src, seek_s, src_w, src_h) {
                Ok(layout) => return layout,
                Err(e) => tracing::warn!("auto-frame failed: {e:#}; full-frame fallback"),
            }
        } else {
            tracing::info!("face model absent; full-frame gameplay fallback");
        }
    }
    yc_frame::decide_layout(None, src_w, src_h, yc_frame::SEAM_DEFAULT)
}

/// Sample frames from the Segment, run Ultraface per frame, cluster the static
/// Facecam, and decide the Layout (ADR 0011). Frames are scaled to the model's
/// fixed input; its normalized detections map straight to source pixels.
#[cfg(feature = "face")]
fn detect_layout(
    paths: &PipelinePaths,
    render_src: &Path,
    seek_s: f64,
    src_w: f32,
    src_h: f32,
) -> Result<Layout> {
    let frames = yc_ingest::extract_frames_rgb(
        &paths.ffmpeg,
        render_src,
        seek_s,
        yc_frame::infer::DET_W as u32,
        yc_frame::infer::DET_H as u32,
        yc_frame::SAMPLE_FPS,
        yc_frame::MAX_FRAMES,
    )?;
    let mut detector = yc_frame::Detector::load(&paths.face_model)?;
    let mut per_frame = Vec::with_capacity(frames.len());
    for f in &frames {
        per_frame.push(detector.detect(f, src_w, src_h)?);
    }
    let faces: usize = per_frame.iter().map(|f| f.len()).sum();
    let cluster = yc_frame::cluster_static_face(&per_frame, src_w, src_h);
    tracing::info!(
        frames = frames.len(),
        faces,
        cam = cluster.is_some(),
        persistence = cluster.map(|c| c.persistence),
        "auto-frame detection"
    );
    Ok(yc_frame::decide_layout(cluster.as_ref(), src_w, src_h, yc_frame::SEAM_DEFAULT))
}

/// The default Caption Style preset: one word per caption (huge-word), which
/// keeps a single word on screen at its own spoken onset — tighter perceived
/// sync than a multi-word line. The full selectable preset set lands at M6.
fn caption_style() -> CaptionStyle {
    CaptionStyle {
        name: "Huge Word".into(),
        genre: CaptionGenre::HugeWord,
        font_family: "Anton".into(),
        // Large: one word at a time, meant to read on a phone. ~15 Anton chars fit
        // the 1080-wide canvas at this size; longer words are rare (tune freely).
        font_size: 150,
        primary_color: [255, 255, 255, 255],
        accent_color: [255, 209, 0, 255],
    }
}
