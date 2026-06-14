//! The M1 pipeline worker: one background thread (ADR 0005) that runs the full
//! tracer-bullet chain off the UI thread — ffmpeg audio extract -> whisper over
//! the picked range -> rolling-pop ASS -> NVENC export — reporting progress
//! over a channel. The hardcoded Stacked Layout lives here because it is M1
//! scaffolding, not domain logic; EDIT the source/facecam constants to match
//! the test VOD.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result};
use yc_core::{
    CaptionGenre, CaptionStyle, Clip, Crop, Language, Layout, Moment, Signals, TimeRange, CANVAS_H,
    CANVAS_W,
};

/// Resolved inputs the worker needs, captured once at spawn.
#[derive(Clone)]
pub struct PipelinePaths {
    pub ffmpeg: PathBuf,
    pub model: PathBuf,
    pub font: PathBuf,
    pub workspace: PathBuf,
}

/// A unit of work requested by the UI.
pub enum Job {
    Run { video: PathBuf, range: TimeRange },
}

/// Progress reported back to the UI thread.
pub enum Progress {
    Stage(&'static str),
    Done(PathBuf),
    Failed(String),
}

/// Spawn the worker thread and return the job sender + progress receiver.
pub fn spawn(paths: PipelinePaths) -> (Sender<Job>, Receiver<Progress>) {
    let (tx_job, rx_job) = mpsc::channel::<Job>();
    let (tx_prog, rx_prog) = mpsc::channel::<Progress>();
    thread::spawn(move || {
        while let Ok(job) = rx_job.recv() {
            match job {
                Job::Run { video, range } => {
                    let result = run_pipeline(&paths, &video, range, &tx_prog);
                    let msg = match result {
                        Ok(out) => Progress::Done(out),
                        Err(e) => Progress::Failed(format!("{e:#}")),
                    };
                    let _ = tx_prog.send(msg);
                }
            }
        }
    });
    (tx_job, rx_prog)
}

// --- M1 hardcoded Layout + Caption Style (EDIT to taste / to fit the VOD) ---

/// Assumed source resolution of the test VOD.
const SRC_W: f32 = 1920.0;
const SRC_H: f32 = 1080.0;
/// Gameplay Panel occupies the top `SEAM` fraction; facecam the rest.
const SEAM: f32 = 0.62;
/// Facecam inset rectangle within the source (bottom-right by default).
const FACE_W: f32 = 480.0;
const FACE_H: f32 = 270.0;
const FACE_X: f32 = SRC_W - FACE_W;
const FACE_Y: f32 = SRC_H - FACE_H;

/// The hardcoded stacked Layout: gameplay above facecam (CONTEXT.md), each
/// Crop aspect-fitted to its Panel so neither stretches (Panel invariant).
fn m1_layout() -> Layout {
    let gh = (CANVAS_H as f32 * SEAM).round();
    let fh = CANVAS_H as f32 - gh;
    let gameplay = Crop { x: 0.0, y: 0.0, w: SRC_W, h: SRC_H }.fit_to_aspect(CANVAS_W as f32 / gh);
    let facecam =
        Crop { x: FACE_X, y: FACE_Y, w: FACE_W, h: FACE_H }.fit_to_aspect(CANVAS_W as f32 / fh);
    Layout::Stacked { seam: SEAM, gameplay, facecam }
}

/// The single M1 Caption Style preset.
fn m1_caption_style() -> CaptionStyle {
    CaptionStyle {
        name: "Rolling Pop".into(),
        genre: CaptionGenre::RollingPop,
        font_family: "Anton".into(),
        font_size: 96,
        primary_color: [255, 255, 255, 255],
        accent_color: [255, 209, 0, 255],
    }
}

fn run_pipeline(
    paths: &PipelinePaths,
    video: &Path,
    range: TimeRange,
    tx: &Sender<Progress>,
) -> Result<PathBuf> {
    anyhow::ensure!(video.is_file(), "video not found: {}", video.display());
    anyhow::ensure!(paths.ffmpeg.is_file(), "ffmpeg sidecar missing — run fetch-sidecars.ps1");
    anyhow::ensure!(paths.model.is_file(), "whisper model missing — run fetch-models.ps1");
    anyhow::ensure!(range.duration_s() > 0.0, "pick a range with end > start");

    // Manual pick -> a manually-marked Moment promoted to a Clip (CONTEXT.md).
    let moment = Moment { id: 1, range, signals: Signals::default(), score: 0.0 };
    let style = m1_caption_style();
    let clip = Clip {
        id: 1,
        moment_id: moment.id,
        range: moment.range,
        layout: m1_layout(),
        caption_style: style.name.clone(),
        segment_path: None,
        export_path: None,
    };

    let stem = video.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "clip".into());
    let workdir = paths.workspace.join(&stem);
    fs::create_dir_all(&workdir).with_context(|| format!("creating {}", workdir.display()))?;

    let _ = tx.send(Progress::Stage("Extracting audio"));
    let analysis = workdir.join("analysis.wav");
    yc_ingest::extract_audio(&paths.ffmpeg, video, &analysis)?;

    let _ = tx.send(Progress::Stage("Transcribing (whisper, GPU)"));
    let samples = yc_ingest::read_range_samples(&analysis, clip.range)?;
    let transcript = yc_transcribe::transcribe_range(&paths.model, &samples, Language::En)?;

    let _ = tx.send(Progress::Stage("Generating captions"));
    let ass = yc_render::generate_ass(&transcript, &style);
    fs::write(workdir.join("clip.ass"), ass).context("writing clip.ass")?;
    // Copy the font beside the ASS so libass finds it via fontsdir=. (dodges
    // Windows filtergraph path escaping).
    fs::copy(&paths.font, workdir.join("Anton-Regular.ttf"))
        .with_context(|| format!("copying font from {}", paths.font.display()))?;

    let _ = tx.send(Progress::Stage("Rendering (NVENC)"));
    let filtergraph = yc_render::build_filtergraph(&clip.layout, "clip.ass");
    let args = yc_render::export_args(video, clip.range, &filtergraph, "export.mp4");
    yc_render::run_export(&paths.ffmpeg, &workdir, &args)?;

    Ok(workdir.join("export.mp4"))
}
