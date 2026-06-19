//! The M2 pipeline worker: one background thread (ADR 0005) running the
//! two-phase ingest off the UI thread, reporting progress over a channel.
//!
//! - **Import** resolves a VOD (YouTube URL or local file) to its whole-VOD
//!   analysis audio + raw chat + metadata, and persists `project.json` in the
//!   per-VOD workspace folder. It leaves a [`Session`] the worker remembers.
//! - **Promote** turns a picked range into a Clip: fetch the padded Segment
//!   (YouTube) or use the local file, probe it for the in-segment offset and
//!   layout dimensions, transcribe the range, generate captions, NVENC export.
//!
//! A [`CancelToken`] (shared with the UI) kills the yt-dlp/ffmpeg child tree
//! mid-download - the M1 "hang" scar. The Stacked Layout is still hardcoded
//! scaffolding here (the framing editor is M5), but now derives its Crops from
//! the *probed* source resolution so it works for 360p and 1080p alike.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result};
use yc_core::{
    CaptionGenre, CaptionStyle, Clip, Crop, Language, Layout, Moment, Project, Signals, TimeRange,
    Vod, VodSource, CANVAS_H, CANVAS_W,
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
    Promote { range: TimeRange },
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

/// How `Promote` obtains the video to render from, and where it seeks.
enum PromoteSource {
    /// Fetch a padded Segment per-promote (web_safari HLS); seek the offset.
    YouTube(String),
    /// Use the local file directly as the "Segment"; seek the range start.
    Local(PathBuf),
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
                Job::Promote { range } => match &session {
                    None => {
                        let _ = tx_prog
                            .send(Progress::Failed("import a VOD before making a clip".into()));
                    }
                    Some(s) => match do_promote(&paths, s, range, &worker_cancel, &tx_prog) {
                        Ok(out) => {
                            let _ = tx_prog.send(Progress::Done(out));
                        }
                        Err(e) => {
                            let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                        }
                    },
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
    let transcriber = yc_transcribe::Transcriber::load_text_only(&paths.model)?;
    let mut densities = Vec::with_capacity(moments.len());
    let mut texts = Vec::with_capacity(moments.len());
    for m in &moments {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let samples = yc_ingest::read_range_samples(&session.analysis_wav, m.range)?;
        let transcript = transcriber.transcribe(&samples, session.vod.language, {
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

// --- promote (phase 2) ------------------------------------------------------

fn do_promote(
    paths: &PipelinePaths,
    session: &Session,
    range: TimeRange,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<PathBuf> {
    anyhow::ensure!(paths.model.is_file(), "whisper model missing - run fetch-models.ps1");
    anyhow::ensure!(range.duration_s() > 0.0, "pick a range with end > start");
    let sc = paths.sidecars();

    // 1. Obtain the render source, the in-segment seek offset, and the source
    //    resolution (for the Layout) - all from one ffprobe of the media.
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

    // 2. Transcribe the picked range from the analysis audio (M1 path). Whisper
    //    captions the loudest voice in the mixed track; mic isolation is future
    //    work (see ROADMAP M1 known-limitation).
    let _ = tx.send(Progress::Stage("Transcribing (whisper, GPU)"));
    let samples = yc_ingest::read_range_samples(&session.analysis_wav, range)?;
    let transcript = yc_transcribe::transcribe_range(&paths.model, &samples, session.vod.language, {
        let c = cancel.clone();
        move || c.is_cancelled()
    })?;

    // 3. Captions: generate the ASS and copy the font beside it (libass finds it
    //    via fontsdir=., dodging Windows filtergraph path escaping).
    let _ = tx.send(Progress::Stage("Generating captions"));
    let style = caption_style();
    let ass = yc_render::generate_ass(&transcript, &style);
    fs::write(session.workdir.join("clip.ass"), ass).context("writing clip.ass")?;
    fs::copy(&paths.font, session.workdir.join("Anton-Regular.ttf"))
        .with_context(|| format!("copying font from {}", paths.font.display()))?;

    // 4. Record the promoted Clip in the project, then render.
    let clip = build_clip(range, layout_for(src_w, src_h), &style.name);
    persist_clip(&session.vod, &clip, &session.workdir)?;

    let _ = tx.send(Progress::Stage("Rendering (NVENC)"));
    let filtergraph = yc_render::build_filtergraph(&clip.layout, "clip.ass");
    let args = yc_render::export_args(&render_src, seek_s, range.duration_s(), &filtergraph, "export.mp4");
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
    project.clips.push(clip.clone());
    project
        .save(&workdir.join("project.json"))
        .with_context(|| format!("writing project.json in {}", workdir.display()))
}

// --- hardcoded Layout + Caption Style (scaffolding; framing editor is M5) ----

/// Gameplay Panel occupies the top `SEAM` fraction; facecam the rest.
const SEAM: f32 = 0.62;
/// Facecam inset as a fraction of the source frame (bottom-right corner). This
/// generalizes M1's fixed 130x110-of-640x360 inset to any resolution, so the
/// same scaffolding frames the 360p test VOD and a 1080p Segment. The operator
/// will drag the real facecam rectangle in the M5 editor.
const FACE_W_FRAC: f32 = 0.20;
const FACE_H_FRAC: f32 = 0.30;

/// The hardcoded stacked Layout, built from the *probed* source resolution:
/// gameplay above facecam (CONTEXT.md), each Crop aspect-fitted to its Panel so
/// neither stretches (Panel invariant).
fn layout_for(src_w: f32, src_h: f32) -> Layout {
    let gh = (CANVAS_H as f32 * SEAM).round();
    let fh = CANVAS_H as f32 - gh;
    let gameplay =
        Crop { x: 0.0, y: 0.0, w: src_w, h: src_h }.fit_to_aspect(CANVAS_W as f32 / gh);
    let fw = src_w * FACE_W_FRAC;
    let fhgt = src_h * FACE_H_FRAC;
    let facecam = Crop { x: src_w - fw, y: src_h - fhgt, w: fw, h: fhgt }
        .fit_to_aspect(CANVAS_W as f32 / fh);
    Layout::Stacked { seam: SEAM, gameplay, facecam }
}

/// The default Caption Style preset: one word per caption (huge-word), which
/// keeps a single word on screen at its own spoken onset — tighter perceived
/// sync than a multi-word line. The full selectable preset set lands at M6.
fn caption_style() -> CaptionStyle {
    CaptionStyle {
        name: "Huge Word".into(),
        genre: CaptionGenre::HugeWord,
        font_family: "Anton".into(),
        font_size: 96,
        primary_color: [255, 255, 255, 255],
        accent_color: [255, 209, 0, 255],
    }
}
