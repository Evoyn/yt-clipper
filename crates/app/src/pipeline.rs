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
    CaptionGenre, CaptionStyle, Clip, Creator, CreatorStore, Language, Layout, LayoutPref, Moment,
    NoConsole, Project, ReviewCache, Signals, TimeRange, Transcript, Vod, VodSource,
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
    /// The bundled `deep-filter` DeepFilterNet sidecar for Cleaned-voice captions
    /// (`enh`, ADR 0029): a gentle denoise of the caption audio before whisper, so
    /// the streamer's voice reads above game SFX. May be absent: the export then
    /// captions the mixed analysis audio. Only read by the `enh`-gated caption pass.
    #[cfg_attr(not(feature = "enh"), allow(dead_code))]
    pub deep_filter: PathBuf,
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
    /// Phase-2a (ADR 0012): fetch the padded Segment, probe it, choose the seed
    /// Layout, and extract preview frames for the nudge editor. Leaves a
    /// [`PreparedClip`] the worker holds for the matching [`Job::Render`]. `title`
    /// is the promoted Moment's LLM-generated title (ADR 0015), carried to the
    /// render to name the Short; `None` for a manually-marked / headless clip.
    /// `layout_pref` is the operator's explicit framing choice (ADR 0017): `Auto`
    /// runs M6 auto-detect, the others force a Layout.
    Prepare { range: TimeRange, title: Option<String>, layout_pref: LayoutPref },
    /// Phase-2b (ADR 0012): render the operator's (possibly nudged) `layout`
    /// over the held [`PreparedClip`] - transcribe (once, then cached), caption,
    /// NVENC export. `caption_genre` selects the Caption Style animation (M7):
    /// huge-word / rolling-pop / karaoke-fill; the rest of the style is data.
    Render { layout: Layout, caption_genre: CaptionGenre },
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
    /// `caption_genre` is this Creator's remembered Caption Style (ADR 0016), if
    /// known, so the UI seeds its caption-style picker to the operator's usual
    /// choice for this streamer; `None` for an unknown/new Creator. `moments` are
    /// the Moments persisted from a prior session (M8), so a re-import restores the
    /// review list without re-detecting (empty for a never-detected VOD), and
    /// `transcripts`/`llm_reasons` restore the review panel text from `review.json`.
    Imported {
        title: String,
        duration_s: Option<f64>,
        analysis_wav: PathBuf,
        caption_genre: Option<CaptionGenre>,
        moments: Vec<Moment>,
        transcripts: HashMap<u64, String>,
        llm_reasons: HashMap<u64, String>,
    },
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
///
/// Output organization (ADR 0015): each VOD owns a **stream folder**
/// `workspace/<creator>/<stream-title>/` (sanitized from VOD metadata, realizing
/// Creator scoping without a `creators.json` defaults store yet). Rendered Shorts
/// land at its root; every intermediate (analysis.wav, audio.m4a, segment*,
/// chat, project.json, clip.ass, font) lives under its `data/` subfolder.
struct Session {
    vod: Vod,
    promote: PromoteSource,
    /// `workspace/<creator>/<stream-title>/` — the rendered Shorts go here.
    stream_dir: PathBuf,
    /// `<stream_dir>/data/` — analysis audio, segments, project.json, captions.
    data_dir: PathBuf,
    analysis_wav: PathBuf,
    /// Moments persisted from a prior session (M8), surfaced to the review UI on
    /// import so a re-import doesn't lose detection work. Empty for a never-detected
    /// VOD. Read once (for `Progress::Imported`); a later Detect supersedes them.
    moments: Vec<Moment>,
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
    /// The promoted Moment's LLM-generated title (ADR 0015), used to name the
    /// rendered Short. `None` for a manual / headless clip → render falls back to
    /// a timestamp name. Held here so a re-render after a nudge keeps the name.
    title: Option<String>,
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
                            let (transcripts, llm_reasons) = load_review(&s.data_dir);
                            let _ = tx_prog.send(Progress::Imported {
                                title: s.vod.title.clone(),
                                duration_s: s.vod.duration_s,
                                analysis_wav: s.analysis_wav.clone(),
                                caption_genre: remembered_caption_genre(&paths.workspace, &s.vod),
                                moments: s.moments.clone(),
                                transcripts,
                                llm_reasons,
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
                Job::Prepare { range, title, layout_pref } => match &session {
                    None => {
                        let _ = tx_prog
                            .send(Progress::Failed("import a VOD before making a clip".into()));
                    }
                    Some(s) => match do_prepare(&paths, s, range, title, layout_pref, &worker_cancel, &tx_prog) {
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
                Job::Render { layout, caption_genre } => match (&session, &mut prepared) {
                    (Some(s), Some(pc)) => {
                        match do_render(&paths, s, pc, layout, caption_genre, &worker_cancel, &tx_prog) {
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
    anyhow::ensure!(
        matches!(vod.source, VodSource::YouTube { .. }),
        "expected a YouTube VOD"
    );

    // Output organization (ADR 0015): `workspace/<creator>/<stream-title>/`, with
    // intermediates under `data/`. The folder names come from VOD metadata
    // (creator from yt-dlp uploader/channel, title from `title`), sanitized to
    // safe path segments. Keyed by metadata (not the opaque video_id), so a
    // re-import of the same VOD lands in the same folder and reuses cached audio.
    let (stream_dir, data_dir) = stream_dirs(&paths.workspace, &vod);
    fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;

    let _ = tx.send(Progress::Stage("Downloading audio"));
    let analysis_wav = yc_ingest::youtube_fetch_audio(&sc, &url, &data_dir, cancel)?;

    let _ = tx.send(Progress::Stage("Fetching chat"));
    let _chat = yc_ingest::youtube_fetch_chat(&sc, &url, &data_dir, cancel)?;

    let project = save_project(&vod, &data_dir)?;
    Ok(Session {
        vod,
        promote: PromoteSource::YouTube(url),
        stream_dir,
        data_dir,
        analysis_wav,
        moments: project.moments,
    })
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
    // A local file has no Creator: scope it under `workspace/local/<file-stem>/`
    // (ADR 0015), matching the YouTube hierarchy. The fallback creator is "local".
    let vod = Vod {
        creator: "local".into(),
        title: stem,
        source: VodSource::LocalFile { path: path.clone() },
        language,
        duration_s: None,
    };
    let (stream_dir, data_dir) = stream_dirs(&paths.workspace, &vod);
    fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;

    let _ = tx.send(Progress::Stage("Extracting audio"));
    let analysis_wav = data_dir.join("analysis.wav");
    yc_ingest::extract_audio(&paths.ffmpeg, &path, &analysis_wav)?;

    let project = save_project(&vod, &data_dir)?;
    Ok(Session {
        vod,
        promote: PromoteSource::Local(path),
        stream_dir,
        data_dir,
        analysis_wav,
        moments: project.moments,
    })
}

/// Max chars for a sanitized folder segment (`<creator>` / `<stream-title>`).
/// Bounded well under Windows MAX_PATH so `workspace/<creator>/<title>/data/
/// segment.mp4` and the Short at the stream root both stay short enough.
const DIR_SEGMENT_MAX: usize = 64;
/// Max chars for a generated Short's filename stem — the Shorts-title ceiling
/// (the LLM is asked for <=60; this enforces it after sanitizing).
const TITLE_STEM_MAX: usize = 60;

/// The `(stream_dir, data_dir)` for a VOD under `workspace` (ADR 0015):
/// `workspace/<creator>/<stream-title>/` and its `data/` subfolder, both names
/// sanitized from the VOD's metadata to safe path segments. Pure (no I/O), so the
/// layout is unit-testable; the caller creates the directories.
fn stream_dirs(workspace: &Path, vod: &Vod) -> (PathBuf, PathBuf) {
    let creator = yc_ingest::sanitize_segment(&vod.creator, "unknown", DIR_SEGMENT_MAX);
    let title = yc_ingest::sanitize_segment(&vod.title, "untitled", DIR_SEGMENT_MAX);
    let stream_dir = workspace.join(creator).join(title);
    let data_dir = stream_dir.join("data");
    (stream_dir, data_dir)
}

/// Persist `project.json` at import, **preserving** any Moments/Clips a prior
/// session detected/promoted (M8): a re-import must not wipe detection work. Only
/// the Vod metadata is refreshed. Returns the (possibly pre-existing) Project so
/// the worker can surface its Moments back to the review UI.
fn save_project(vod: &Vod, data_dir: &Path) -> Result<Project> {
    let mut project = load_or_new_project(vod, data_dir);
    project.vod = vod.clone();
    project
        .save(&data_dir.join("project.json"))
        .with_context(|| format!("writing project.json in {}", data_dir.display()))?;
    Ok(project)
}

/// `workspace/creators.json` — the global per-Creator defaults store (ADR 0016).
fn creators_path(workspace: &Path) -> PathBuf {
    workspace.join("creators.json")
}

/// This VOD's Creator's remembered Caption Style genre (ADR 0016), if the store
/// knows this Creator — used to seed the render's caption style on import. `None`
/// for an unknown/new Creator (the operator's current selection then stands, and
/// is saved to the store on the first render).
fn remembered_caption_genre(workspace: &Path, vod: &Vod) -> Option<CaptionGenre> {
    CreatorStore::load(&creators_path(workspace))
        .get(&vod.creator)
        .and_then(|c| c.default_caption_genre)
}

/// Remember the Caption Style the operator just rendered with for this Creator
/// (ADR 0016): upsert the Creator record (creating it if new), keeping its
/// language current and recording `genre` as the new default. Best-effort — a
/// store read/write failure logs and never fails the render.
fn remember_creator_genre(workspace: &Path, vod: &Vod, genre: CaptionGenre) {
    let path = creators_path(workspace);
    let mut store = CreatorStore::load(&path);
    let mut creator = store
        .get(&vod.creator)
        .cloned()
        .unwrap_or_else(|| Creator::new(vod.creator.clone(), vod.language));
    creator.language = vod.language; // keep the recorded language current
    creator.default_caption_genre = Some(genre);
    store.upsert(creator);
    if let Err(e) = store.save(&path) {
        tracing::warn!("creators.json save failed ({e})");
    }
}

/// Load the VOD's persisted project, or start a fresh one if none exists / it is
/// unreadable. Lets detect and promote update `project.json` without clobbering
/// each other's records.
fn load_or_new_project(vod: &Vod, data_dir: &Path) -> Project {
    Project::load(&data_dir.join("project.json")).unwrap_or_else(|_| Project::new(vod.clone()))
}

/// Persist the per-Moment review notes (transcript + LLM reason) as
/// `data/review.json` (M8), so a re-import restores the review panel without
/// re-transcribing. Best-effort — a write failure logs and never fails Detect.
fn save_review(
    transcripts: &HashMap<u64, String>,
    llm_reasons: &HashMap<u64, String>,
    data_dir: &Path,
) {
    let mut cache = ReviewCache::default();
    for (&id, t) in transcripts {
        cache.notes.entry(id).or_default().transcript = t.clone();
    }
    for (&id, r) in llm_reasons {
        cache.notes.entry(id).or_default().llm_reason = r.clone();
    }
    if let Err(e) = cache.save(&data_dir.join("review.json")) {
        tracing::warn!("review.json save failed ({e})");
    }
}

/// Load the persisted review notes as the two `Moment id -> text` maps the review
/// UI consumes (empty when no `review.json` — a re-Detect refills it). Skips empty
/// strings so a Moment with no LLM reason doesn't get a blank entry.
fn load_review(data_dir: &Path) -> (HashMap<u64, String>, HashMap<u64, String>) {
    let cache = ReviewCache::load(&data_dir.join("review.json"));
    let mut transcripts = HashMap::new();
    let mut llm_reasons = HashMap::new();
    for (id, note) in cache.notes {
        if !note.transcript.is_empty() {
            transcripts.insert(id, note.transcript);
        }
        if !note.llm_reason.is_empty() {
            llm_reasons.insert(id, note.llm_reason);
        }
    }
    (transcripts, llm_reasons)
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
    let chat = session.data_dir.join("chat.live_chat.json");
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
    // LLM-generated Shorts titles, aligned to `moments` (ADR 0015). Used to name
    // each rendered Short when the Moment is promoted; carried through the rank
    // reorder below onto `Moment.title`.
    let mut llm_titles_vec: Vec<String> = Vec::new();
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
                llm_titles_vec = verdicts.iter().map(|v| v.title.clone()).collect();
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
        if let Some(t) = llm_titles_vec.get(i).map(|t| t.trim()).filter(|t| !t.is_empty()) {
            m.title = Some(t.to_string());
        }
        transcripts.insert(m.id, texts[i].clone());
        if let Some(r) = llm_reasons_vec.get(i).filter(|r| !r.trim().is_empty()) {
            llm_reasons.insert(m.id, r.clone());
        }
        ranked.push(m);
    }

    let mut project = load_or_new_project(&session.vod, &session.data_dir);
    project.moments = ranked.clone();
    project
        .save(&session.data_dir.join("project.json"))
        .with_context(|| format!("writing project.json in {}", session.data_dir.display()))?;
    // Persist the review notes alongside (M8) so a re-import restores the panel.
    save_review(&transcripts, &llm_reasons, &session.data_dir);
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
        .no_console()
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

/// LLM caption-correction pass (ADR 0030): hand whisper's caption units + per-unit
/// confidence + the dialect store's context overrides + the clip's topic to the
/// out-of-process `yc-llm-judge --correct`, then map its reply back onto the units
/// under the confidence/curated guardrails (a confident word is changed only by a
/// curated override; a spurious unsure word may be dropped; words are never added).
/// Runs on the units **before** caption timing so a dropped word's slot is absorbed
/// by the gap-fill. Best-effort: a missing sidecar/GGUF, a build-request no-op, or a
/// sidecar failure leaves captions uncorrected — it never sinks a render. Off by
/// default (compiled only under `--features correct`), pending operator sign-off on
/// the real render (the enh over-claim lesson).
#[cfg(feature = "correct")]
fn correct_captions(
    paths: &PipelinePaths,
    session: &Session,
    prepared: &PreparedClip,
    transcript: &mut Transcript,
    conf: &mut Vec<f32>,
    lexicon: &yc_transcribe::DialectLexicon,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) {
    if !paths.llm_judge.is_file() || !paths.llm_model.is_file() {
        tracing::info!("correct: sidecar or GGUF absent; captions left uncorrected");
        return;
    }
    // Collapse whisper's repeated-filler hallucinations first (deterministic): cleans
    // the "eh eh eh..." spam AND keeps the list short so the model's per-line index
    // numbering stays aligned with the units (a long run made it miscount).
    let removed = yc_transcribe::collapse_adjacent_duplicates(&mut transcript.units, conf);
    if removed > 0 {
        tracing::info!("correct: collapsed {removed} repeated filler unit(s)");
    }
    // Topic for the corrector: the clip's generated title + the store's note. Safe
    // to feed a chat model (unlike whisper's initial_prompt, which it never reaches).
    let mut topic = prepared.title.clone().unwrap_or_default();
    if !lexicon.note.is_empty() {
        if !topic.is_empty() {
            topic.push_str("; ");
        }
        topic.push_str(&lexicon.note);
    }
    let ctx = yc_transcribe::CorrectionContext { language: session.vod.language, topic };
    let Some(req) = yc_transcribe::build_correction_request(&transcript.units, conf, lexicon, &ctx)
    else {
        return; // a clean clip with no overrides — nothing the model could safely do
    };
    let _ = tx.send(Progress::Stage("Correcting captions (LLM)"));
    match run_llm_correct(&paths.llm_judge, &paths.llm_model, &req.system, &req.user, cancel) {
        Ok(raw) => {
            let stats = yc_transcribe::apply_correction(&mut transcript.units, conf, lexicon, &raw);
            tracing::info!("correct: {}", stats.summary());
        }
        Err(e) if cancel.is_cancelled() => tracing::info!("correct: cancelled ({e:#})"),
        Err(e) => tracing::warn!("correct: failed ({e:#}); captions left uncorrected"),
    }
}

/// Run `yc-llm-judge --correct` over one caption (ADR 0030): write the
/// `{model_path, system, user}` request to its stdin and read back the raw
/// free-form completion (the corrected `N: word` list) from its stdout. Mirrors
/// [`run_llm_judge`] — stderr inherited for the load logs, killed on cancel to free
/// VRAM, request small enough that writing it before reading can't deadlock.
#[cfg(feature = "correct")]
fn run_llm_correct(
    bin: &Path,
    model: &Path,
    system: &str,
    user: &str,
    cancel: &CancelToken,
) -> Result<String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    let payload = serde_json::to_vec(&serde_json::json!({
        "model_path": model.to_string_lossy(),
        "system": system,
        "user": user,
    }))
    .context("serializing correct request")?;
    let mut child = Command::new(bin)
        .arg("--correct")
        .no_console()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning {}", bin.display()))?;

    child
        .stdin
        .take()
        .context("correct stdin unavailable")?
        .write_all(&payload)
        .context("writing correct request")?;

    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("cancelled");
        }
        match child.try_wait().context("waiting on correct")? {
            Some(status) => {
                anyhow::ensure!(status.success(), "yc-llm-judge --correct exited with {status}");
                break;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }

    let mut out = String::new();
    child
        .stdout
        .take()
        .context("correct stdout unavailable")?
        .read_to_string(&mut out)
        .context("reading correct output")?;
    Ok(out)
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
    title: Option<String>,
    layout_pref: LayoutPref,
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
            let segment = yc_ingest::fetch_segment(&sc, url, padded, &session.data_dir, cancel)?;
            let p = yc_ingest::probe_segment(&paths.ffprobe, &segment, cancel)?;
            let offset = yc_ingest::in_segment_offset(range.start_s, padded.start_s, &p);
            (segment, offset, p.width as f32, p.height as f32)
        }
    };

    // Choose the seed Layout: Ultraface locates the Facecam (CPU; no GPU
    // contention), then build_layout applies the operator's Layout preference
    // (ADR 0017) - Auto runs M6's stacked / full-cam / full-frame decision, a
    // forced kind overrides it (still using the detected Facecam when found).
    // The editor opens seeded with this and the operator nudges from there.
    let _ = tx.send(Progress::Stage("Framing (face detect)"));
    let auto_layout = build_layout(paths, &render_src, seek_s, src_w, src_h, layout_pref);

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
        title,
    };
    Ok((prepared, frames, frame_w, frame_h))
}

/// The 16 kHz-mono samples whisper captions from. With the `sep` feature and the
/// htdemucs model present, this is the **Vocal stem** of the Segment's range —
/// the streamer's voice split from music/SFX (CONTEXT.md), so whisper reads the
/// voice rather than the loudest sound in the mix. Otherwise it is the mixed
/// analysis-wav range (the historical path). Either way the rendered clip's
/// audible audio stays the mix — only what whisper *hears* changes.
#[cfg_attr(not(any(feature = "sep", feature = "enh")), allow(unused_variables))]
fn caption_samples(
    paths: &PipelinePaths,
    session: &Session,
    prepared: &PreparedClip,
    range: TimeRange,
    tx: &Sender<Progress>,
) -> Result<Vec<f32>> {
    // Cleaned-voice captions (ADR 0029): a gentle DeepFilterNet denoise of the
    // caption audio before whisper, so the streamer's voice reads above game SFX.
    // Measured to recover masked speech while leaving clean clips intact; full
    // denoise, by contrast, over-suppressed quiet speech to silence and whisper
    // hallucinated. Takes precedence over `sep`; the clip's audible audio is the mix.
    #[cfg(feature = "enh")]
    {
        if paths.deep_filter.is_file() {
            let _ = tx.send(Progress::Stage("Cleaning voice"));
            let wd = &session.data_dir;
            let in48 = wd.join("_enh_in48k.wav");
            let out_dir = wd.join("_enh_out");
            let clean48 = out_dir.join("_enh_in48k.wav"); // deep-filter keeps the basename
            let clean16 = wd.join("_enh_clean16k.wav");
            // 48 kHz mono for the clip range, from the render source at its
            // in-segment offset — the same window the mix would caption.
            ffmpeg_extract_mono_48k(
                &paths.ffmpeg,
                &prepared.render_src,
                prepared.seek_s,
                range.duration_s(),
                &in48,
            )?;
            run_deep_filter(&paths.deep_filter, &in48, &out_dir)?;
            // Back to whisper's 16 kHz mono; the temp file spans exactly the range.
            ffmpeg_resample_16k_mono(&paths.ffmpeg, &clean48, &clean16)?;
            return yc_ingest::read_range_samples(
                &clean16,
                TimeRange { start_s: 0.0, end_s: range.duration_s() + 1.0 },
            );
        }
    }
    #[cfg(feature = "sep")]
    {
        if paths.sep_model.is_file() {
            let _ = tx.send(Progress::Stage("Separating vocal stem"));
            let wd = &session.data_dir;
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
        .no_console()
        .args(&args)
        .status()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    anyhow::ensure!(status.success(), "ffmpeg vocal-sep extract failed ({status})");
    Ok(())
}

/// Resample a wav to whisper's 16 kHz mono PCM (the cleaned/stem -> caption input).
#[cfg(any(feature = "sep", feature = "enh"))]
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
        .no_console()
        .args(&args)
        .status()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    anyhow::ensure!(status.success(), "ffmpeg vocal-sep resample failed ({status})");
    Ok(())
}

/// Gentle DeepFilterNet attenuation limit (dB) for Cleaned-voice captions (ADR
/// 0029): mixes ~12 dB of the original back, so quiet speech buried in game noise
/// is denoised but never gated to the dead silence that makes whisper hallucinate
/// repetitions. The full default (100 dB) was *measured* to destroy masked speech
/// (40 real units -> "eh" x211). Tune-from-use, like the caption-timing consts.
#[cfg(feature = "enh")]
const ENH_ATTEN_LIM_DB: &str = "12";

/// Extract `dur_s` of `src` from `seek_s` as a 48 kHz **mono** PCM wav — the
/// DeepFilterNet input (the model is full-band 48 kHz). `-ss` before `-i`
/// fast-seeks (the render source is the padded Segment, or the whole local file).
#[cfg(feature = "enh")]
fn ffmpeg_extract_mono_48k(
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
        "1".into(),
        "-ar".into(),
        "48000".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        "-y".into(),
        out.display().to_string(),
    ];
    let status = std::process::Command::new(ffmpeg)
        .no_console()
        .args(&args)
        .status()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    anyhow::ensure!(status.success(), "ffmpeg cleaned-voice extract failed ({status})");
    Ok(())
}

/// Run the bundled `deep-filter` DeepFilterNet sidecar over `in_wav`, writing the
/// cleaned wav into `out_dir` under the same file name. Gentle attenuation limit
/// (`-a`) plus delay compensation (`-D`) so the cleaned audio stays time-aligned
/// with the input for caption timing. The model is baked into the binary (no `-m`).
#[cfg(feature = "enh")]
fn run_deep_filter(deep_filter: &Path, in_wav: &Path, out_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let args: Vec<String> = vec![
        "-a".into(),
        ENH_ATTEN_LIM_DB.into(),
        "-D".into(),
        "-o".into(),
        out_dir.display().to_string(),
        in_wav.display().to_string(),
    ];
    let status = std::process::Command::new(deep_filter)
        .no_console()
        .args(&args)
        .status()
        .with_context(|| format!("spawning deep-filter at {}", deep_filter.display()))?;
    anyhow::ensure!(status.success(), "deep-filter (cleaned voice) failed ({status})");
    Ok(())
}

/// Peak sample magnitude (|s|, 0..1) below which a promoted clip is treated as
/// having no speech, so transcription is skipped (M8): a near-silent window trips
/// whisper.cpp's DTW assertion and aborts the process. Real speech peaks far above
/// this; only a genuinely silent range (an accidental promote of a quiet gap)
/// falls below. Tune-from-use.
const SILENT_CLIP_PEAK: f32 = 0.01;

/// Whether a clip's caption samples carry effectively no audio (peak below
/// [`SILENT_CLIP_PEAK`]) — the no-speech guard for the whisper DTW abort (M8).
/// Pure, so the threshold is unit-tested without a model.
fn is_silent_clip(samples: &[f32]) -> bool {
    samples.iter().fold(0.0f32, |m, &s| m.max(s.abs())) < SILENT_CLIP_PEAK
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
    caption_genre: CaptionGenre,
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
        // Guard whisper's DTW process-abort on a token-starved window (M8): a
        // (near-)silent promote feeds whisper almost no speech and trips the
        // whisper.cpp DTW assertion (filter_width < ne[2]), which *aborts the
        // whole process* (uncatchable from Rust; detection dodges it with the
        // no-DTW `load_text_only`, ADR 0007). If the clip has effectively no
        // audio, skip transcription -> empty captions and still render the video.
        // Real speech peaks far above this floor. (The harder music-but-no-speech
        // case wants whisper out-of-process, like the judge; noted, deferred.)
        let transcript = if is_silent_clip(&samples) {
            tracing::warn!(
                "caption: clip below silence floor {SILENT_CLIP_PEAK}; \
                 skipping transcription (no speech)"
            );
            Transcript { language: session.vod.language, units: Vec::new() }
        } else {
            let _ = tx.send(Progress::Stage("Transcribing (whisper, GPU)"));
            let lexicon =
                yc_transcribe::DialectLexicon::load(&paths.dialect_dir, session.vod.language);
            #[cfg_attr(not(feature = "correct"), allow(unused_mut, unused_variables))]
            let (mut transcript, mut conf, harvest) = yc_transcribe::transcribe_range_full(
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
                // Record each harvested word's source (ADR 0022): the generated
                // Short title + the absolute VOD timestamp (clip start + the word's
                // clip-relative onset), so the operator can find and curate it.
                let n = yc_transcribe::DialectLexicon::harvest_to_store(
                    &paths.dialect_dir,
                    session.vod.language,
                    &harvest,
                    range.start_s,
                    prepared.title.as_deref(),
                );
                if n > 0 {
                    tracing::info!("dialect: harvested {n} low-confidence word(s) to review");
                }
            }
            // LLM caption-correction pass (ADR 0030): repair the linguistic errors
            // (garble / slang / names) the audio path can't, on the units BEFORE
            // timing so a dropped spurious word's slot rides the gap-fill. Gated
            // behind the `correct` feature + the sidecar, off by default until the
            // operator A/Bs the real render (no repeat of the enh over-claim).
            #[cfg(feature = "correct")]
            correct_captions(paths, session, prepared, &mut transcript, &mut conf, &lexicon, cancel, tx);
            // Refine caption end-times to the streamer's actual vocalization: a
            // screamed / drawn-out word holds for its full sound and a normal word
            // clears when the sound drops, instead of huge-word's fixed hold.
            yc_render::refine_caption_timing(transcript, &samples, yc_ingest::WHISPER_SR)
        };
        prepared.transcript = Some(transcript);
    }
    let transcript = prepared.transcript.as_ref().expect("transcript set above");

    // Captions: write the ASS into the data folder and the font into a fonts-only
    // `data/fonts/` subdir. ffmpeg runs in the data folder, so the relative
    // `subtitles=clip.ass:fontsdir=fonts` resolves both (dodging Windows
    // filtergraph path escaping) while libass scans only fonts — not the sibling
    // analysis.wav / project.json a flat fontsdir tried (and failed) to open.
    let _ = tx.send(Progress::Stage("Generating captions"));
    let style = caption_style(caption_genre);
    let ass = yc_render::generate_ass(transcript, &style);
    fs::write(session.data_dir.join("clip.ass"), ass).context("writing clip.ass")?;
    let fonts_dir = session.data_dir.join("fonts");
    fs::create_dir_all(&fonts_dir)
        .with_context(|| format!("creating {}", fonts_dir.display()))?;
    fs::copy(&paths.font, fonts_dir.join("Anton-Regular.ttf"))
        .with_context(|| format!("copying font from {}", paths.font.display()))?;

    // Name the Short from the LLM-generated title (ADR 0015), de-collided so a
    // re-promote never clobbers a previous Short. It lands at the stream-folder
    // root; intermediates (clip.ass, font, segment) stay under data/.
    let stem = clip_title_stem(prepared.title.as_deref(), range);
    let out_path = unique_path(&session.stream_dir, &stem, "mp4");

    // Record the promoted Clip with the operator's Layout + its export path, then
    // render it.
    let clip = build_clip(range, layout, &style.name, &out_path);
    persist_clip(&session.vod, &clip, &session.data_dir)?;

    let _ = tx.send(Progress::Stage("Rendering (NVENC)"));
    let filtergraph = yc_render::build_filtergraph(&clip.layout, "clip.ass");
    // The output is an absolute path (only the `subtitles=clip.ass` filter must
    // stay relative for libass); ffmpeg runs in data/ so the relative ASS + font
    // resolve, and writes the Short up at the stream-folder root.
    let out_name = out_path.to_string_lossy();
    let args = yc_render::export_args(
        &prepared.render_src,
        prepared.seek_s,
        range.duration_s(),
        &filtergraph,
        &out_name,
    );
    yc_render::run_export(&paths.ffmpeg, &session.data_dir, &args)?;

    // Remember this Creator's Caption Style for the next import (ADR 0016).
    remember_creator_genre(&paths.workspace, &session.vod, caption_genre);

    Ok(out_path)
}

/// The filename stem for a rendered Short (no extension): the promoted Moment's
/// LLM-generated title sanitized to a safe segment, or — when there is no title
/// (manual / headless clip) or it sanitizes to nothing — a deterministic
/// timestamp name `clip-<m-ss>` from the clip's start, so every Short still gets
/// a meaningful, collision-resistant name (ADR 0015).
fn clip_title_stem(title: Option<&str>, range: TimeRange) -> String {
    title
        .map(|t| yc_ingest::sanitize_segment(t, "", TITLE_STEM_MAX))
        // Require a real word: an all-symbol / emoji title (sanitizes to e.g.
        // "___" or stays punctuation) is not a meaningful filename — fall back.
        .filter(|s| s.chars().any(|c| c.is_alphanumeric()))
        .unwrap_or_else(|| format!("clip-{}", clock_stem(range.start_s)))
}

/// `m-ss` (or `h-mm-ss` past an hour) for a timestamp — a filesystem-safe clock
/// (no `:`), used in the fallback Short name.
fn clock_stem(t_s: f64) -> String {
    let s = t_s.round().max(0.0) as u64;
    if s >= 3600 {
        format!("{}-{:02}-{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}-{:02}", s / 60, s % 60)
    }
}

/// A non-colliding `dir/<stem>.<ext>`: returned as-is when free, else
/// `dir/<stem> (2).<ext>`, `(3)`, … so a re-promote of the same Moment never
/// overwrites a previous Short (ADR 0015). Bounded so a pathological directory
/// can't loop forever (falls back to the base name after the cap).
fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    for n in 2..10_000 {
        let candidate = dir.join(format!("{stem} ({n}).{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

/// A manually-picked range -> a marked Moment promoted to a Clip (CONTEXT.md).
fn build_clip(range: TimeRange, layout: Layout, caption_style: &str, export_path: &Path) -> Clip {
    Clip {
        id: 1,
        moment_id: 1,
        range,
        layout,
        caption_style: caption_style.to_string(),
        segment_path: None,
        export_path: Some(export_path.to_path_buf()),
    }
}

/// Re-save `project.json` with the promoted Clip recorded, preserving any
/// detected Moments. Records the source Moment only if it isn't already known
/// (e.g. a directly promoted range that never went through detection).
fn persist_clip(vod: &Vod, clip: &Clip, data_dir: &Path) -> Result<()> {
    let mut project = load_or_new_project(vod, data_dir);
    if !project.moments.iter().any(|m| m.id == clip.moment_id) {
        project.moments.push(Moment {
            id: clip.moment_id,
            range: clip.range,
            signals: Signals::default(),
            score: 0.0,
            title: None,
        });
    }
    // Replace any existing record of this Clip (a re-render after a nudge) so
    // project.json holds one entry per Clip, not one per render (ADR 0012).
    project.clips.retain(|c| c.id != clip.id);
    project.clips.push(clip.clone());
    project
        .save(&data_dir.join("project.json"))
        .with_context(|| format!("writing project.json in {}", data_dir.display()))
}

// --- auto-detect framing (M6, ADR 0011) -------------------------------------

/// Choose the Clip's Layout (ADR 0017): detect the Facecam in the Segment's
/// frames (M6/ADR 0011), then apply the operator's `pref`. `Auto` runs the
/// three-way auto-decision (stacked / full-cam / full-frame gameplay); a forced
/// kind overrides it, still using the detected Facecam Crop when one was found.
/// Detection is best-effort - with no `face` feature, a missing model, or a
/// detection error the Facecam is `None`, and the forced seeds (or Auto's
/// full-frame fallback) apply.
fn build_layout(
    paths: &PipelinePaths,
    render_src: &Path,
    seek_s: f64,
    src_w: f32,
    src_h: f32,
    pref: LayoutPref,
) -> Layout {
    let faces = detect_facecam(paths, render_src, seek_s, src_w, src_h);
    yc_frame::decide_layout_with_pref(pref, &faces, src_w, src_h, yc_frame::SEAM_DEFAULT)
}

/// Detect the static Facecam(s) in the Segment (ADR 0011, multi-face ext) — one
/// per cam, two for a co-stream cam — or an empty Vec when the `face` feature /
/// model is absent, detection errors, or no face persists.
#[cfg(feature = "face")]
fn detect_facecam(
    paths: &PipelinePaths,
    render_src: &Path,
    seek_s: f64,
    src_w: f32,
    src_h: f32,
) -> Vec<yc_frame::FaceCluster> {
    if !paths.face_model.is_file() {
        tracing::info!("face model absent; no Facecam (Layout uses pref seeds / fallback)");
        return Vec::new();
    }
    match detect_facecam_inner(paths, render_src, seek_s, src_w, src_h) {
        Ok(faces) => faces,
        Err(e) => {
            tracing::warn!("auto-frame failed: {e:#}; no Facecam (pref seeds / fallback)");
            Vec::new()
        }
    }
}

/// Without the `face` feature there is no detector: no Facecam, and the Layout
/// comes entirely from the operator's preference seeds (ADR 0017) or Auto's
/// full-frame fallback.
#[cfg(not(feature = "face"))]
fn detect_facecam(
    _paths: &PipelinePaths,
    _render_src: &Path,
    _seek_s: f64,
    _src_w: f32,
    _src_h: f32,
) -> Vec<yc_frame::FaceCluster> {
    Vec::new()
}

/// Sample frames from the Segment, run Ultraface per frame, and cluster the
/// static Facecam(s) (ADR 0011). Frames are scaled to the model's fixed input; its
/// normalized detections map straight to source pixels.
#[cfg(feature = "face")]
fn detect_facecam_inner(
    paths: &PipelinePaths,
    render_src: &Path,
    seek_s: f64,
    src_w: f32,
    src_h: f32,
) -> Result<Vec<yc_frame::FaceCluster>> {
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
    let clusters = yc_frame::cluster_static_faces(&per_frame, src_w, src_h);
    tracing::info!(
        frames = frames.len(),
        faces,
        cams = clusters.len(),
        persistence = clusters.first().map(|c| c.persistence),
        "auto-frame detection"
    );
    Ok(clusters)
}

/// The default Caption Style preset: one word per caption (huge-word), which
/// keeps a single word on screen at its own spoken onset — tighter perceived
/// The Caption Style for a chosen animation `genre` (M7). The genre is the only
/// thing that varies the build; the rest is data (ADR 0004) — Anton, white text +
/// gold accent. Font size is the one size-sensitive datum: huge-word is one big
/// word filling the width, the multi-word genres (rolling-pop / karaoke-fill) need
/// a smaller size so a ~22-char line fits the 1080-wide canvas. The full per-Clip
/// preset editor + per-Creator defaults are the rest of M7.
fn caption_style(genre: CaptionGenre) -> CaptionStyle {
    let (name, font_size) = match genre {
        // Large: one word at a time, meant to read on a phone. ~15 Anton chars fit
        // the 1080-wide canvas at 150; longer words are rare (tune freely).
        CaptionGenre::HugeWord => ("Huge Word", 150),
        CaptionGenre::RollingPop => ("Rolling Pop", 96),
        CaptionGenre::KaraokeFill => ("Karaoke Fill", 96),
    };
    CaptionStyle {
        name: name.into(),
        genre,
        font_family: "Anton".into(),
        font_size,
        primary_color: [255, 255, 255, 255],
        accent_color: [255, 209, 0, 255],
    }
}

// --- output organization (ADR 0015) -----------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use yc_core::VodSource;

    fn vod(creator: &str, title: &str) -> Vod {
        Vod {
            creator: creator.into(),
            title: title.into(),
            source: VodSource::YouTube { video_id: "abc".into() },
            language: Language::Id,
            duration_s: None,
        }
    }

    #[test]
    fn stream_dirs_lay_out_creator_title_and_data_subfolder() {
        let ws = Path::new("F:/ws");
        let (stream, data) = stream_dirs(ws, &vod("Joddy Barat", "Main Game / Live!"));
        // `<creator>/<title>` with the path-illegal '/' sanitized to '_'.
        assert_eq!(stream, ws.join("Joddy Barat").join("Main Game _ Live!"));
        // Intermediates live under a `data/` child of the stream folder.
        assert_eq!(data, stream.join("data"));
    }

    #[test]
    fn stream_dirs_fall_back_on_empty_metadata() {
        let ws = Path::new("F:/ws");
        let (stream, _) = stream_dirs(ws, &vod("", "..."));
        assert_eq!(stream, ws.join("unknown").join("untitled"));
    }

    #[test]
    fn clip_title_stem_prefers_the_title_else_a_timestamp_name() {
        let r = TimeRange { start_s: 754.0, end_s: 784.0 }; // 12:34
        assert_eq!(clip_title_stem(Some("He LOST it on the boss"), r), "He LOST it on the boss");
        // No title (manual / headless clip) -> deterministic timestamp name.
        assert_eq!(clip_title_stem(None, r), "clip-12-34");
        // A title that sanitizes to nothing also falls back to the timestamp.
        assert_eq!(clip_title_stem(Some("///"), r), "clip-12-34");
    }

    #[test]
    fn clock_stem_is_filesafe_and_handles_hours() {
        assert_eq!(clock_stem(0.0), "0-00");
        assert_eq!(clock_stem(754.0), "12-34");
        assert_eq!(clock_stem(3661.0), "1-01-01"); // past an hour
        assert!(!clock_stem(754.0).contains(':')); // no colon -> filesystem-safe
    }

    #[test]
    fn unique_path_dedupes_collisions_with_a_counter() {
        let dir = std::env::temp_dir().join("yc_unique_path_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // First is the bare name; each existing file bumps the counter.
        let p1 = unique_path(&dir, "My Clip", "mp4");
        assert_eq!(p1, dir.join("My Clip.mp4"));
        fs::write(&p1, b"x").unwrap();
        let p2 = unique_path(&dir, "My Clip", "mp4");
        assert_eq!(p2, dir.join("My Clip (2).mp4"));
        fs::write(&p2, b"x").unwrap();
        assert_eq!(unique_path(&dir, "My Clip", "mp4"), dir.join("My Clip (3).mp4"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_silent_clip_flags_only_near_silence() {
        // Pure silence and a tiny-noise floor read as silent (would crash whisper's
        // DTW); a clip with any real speech-level energy does not.
        assert!(is_silent_clip(&[0.0; 16_000]));
        assert!(is_silent_clip(&[0.001, -0.002, 0.003])); // dither / noise floor
        assert!(!is_silent_clip(&[0.0, 0.0, 0.2, 0.0])); // one speech-level peak is enough
        assert!(!is_silent_clip(&[0.5; 100])); // loud
        assert!(is_silent_clip(&[])); // empty -> nothing to transcribe, treat as silent
    }

    #[test]
    fn review_notes_save_and_load_roundtrip() {
        // M8: Detect's transcript + llm-reason maps persist to review.json and a
        // re-import loads them back, so the review panel restores without re-detect.
        let dir = std::env::temp_dir().join("yc_review_roundtrip");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let transcripts = HashMap::from([
            (1u64, "kaget banget gua".to_string()),
            (2u64, "baca menu doang".to_string()),
        ]);
        // Only Moment 1 got an LLM reason (Moment 2's judge ran without one).
        let llm_reasons = HashMap::from([(1u64, "shock reaction".to_string())]);

        save_review(&transcripts, &llm_reasons, &dir);
        assert!(dir.join("review.json").is_file(), "wrote the sidecar");
        let (t, r) = load_review(&dir);
        assert_eq!(t.len(), 2);
        assert_eq!(t[&1], "kaget banget gua");
        assert_eq!(t[&2], "baca menu doang");
        assert_eq!(r.len(), 1); // empty reasons are skipped, not stored blank
        assert_eq!(r[&1], "shock reaction");
        assert!(!r.contains_key(&2));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_project_preserves_prior_moments_on_reimport() {
        // M8: a re-import must not wipe a prior session's detected Moments.
        let dir = std::env::temp_dir().join("yc_save_project_preserve");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let v = vod("local", "stream");

        // Seed project.json as if a prior Detect had run.
        let mut prior = Project::new(v.clone());
        prior.moments.push(Moment {
            id: 3,
            range: TimeRange { start_s: 10.0, end_s: 40.0 },
            signals: Signals::default(),
            score: 1.5,
            title: Some("Big play".into()),
        });
        prior.save(&dir.join("project.json")).unwrap();

        // save_project (the import-time persist) must keep the Moment and return it.
        let returned = save_project(&v, &dir).unwrap();
        assert_eq!(returned.moments.len(), 1, "returned the preserved Moment");
        assert_eq!(returned.moments[0].id, 3);
        // And it's still on disk (not clobbered to an empty Project).
        let reloaded = Project::load(&dir.join("project.json")).unwrap();
        assert_eq!(reloaded.moments.len(), 1);
        assert_eq!(reloaded.moments[0].title.as_deref(), Some("Big play"));

        let _ = fs::remove_dir_all(&dir);
    }
}
