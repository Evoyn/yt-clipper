//! yt-clipper - pure-Rust desktop shell (egui/eframe, ADR 0005).
//! M2: YouTube ingest end-to-end - import a URL (audio + chat + metadata),
//! pick a range, promote it to a Clip (padded Segment -> whisper -> stacked
//! Layout -> rolling-pop ASS -> NVENC export). A local file is also importable
//! for offline iteration. The pipeline runs on a background worker thread; the
//! UI polls it and stays responsive, and Cancel kills an in-flight download.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod editor;
mod pipeline;
mod theme;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use pipeline::{ImportSource, Job, Progress, Timeline};
use yc_core::{CaptionGenre, Language, LayoutPref, Moment, Signals, TimeRange};
use yc_ingest::CancelToken;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let paths = AppPaths::resolve();
    let deno_dir = paths.deno_dir();
    let (to_worker, from_worker, cancel) = pipeline::spawn(pipeline::PipelinePaths {
        ffmpeg: paths.ffmpeg(),
        ffprobe: paths.ffprobe(),
        ytdlp: paths.ytdlp(),
        deno_dir: deno_dir.clone(),
        model: paths.model(),
        ser_model: paths.ser_model(),
        llm_model: paths.llm_model(),
        llm_judge: paths.llm_judge(),
        face_model: paths.face_model(),
        sep_model: paths.sep_model(),
        dialect_dir: paths.dialect_dir(),
        font: paths.font(),
        workspace: paths.workspace.clone(),
    });

    // Headless one-shot for testing / visual iteration (no GUI clicking):
    //   yt-clipper --headless <url-or-file> <start_s> <end_s> [en|id|ja] [huge|rolling|karaoke] [auto|stacked|cam|gameplay]
    // An http(s) target is imported as a YouTube URL, anything else as a local
    // file. The optional last arg picks the caption animation (M7). Drives the
    // same Import -> Promote worker path the GUI uses.
    let argv: Vec<String> = std::env::args().collect();
    if let Some(i) = argv.iter().position(|a| a == "--headless") {
        let target = argv
            .get(i + 1)
            .expect("--headless needs <url-or-file> <start_s> <end_s> [en|id|ja]")
            .clone();
        let start_s: f64 = argv.get(i + 2).and_then(|s| s.parse().ok()).expect("start_s");
        let end_s: f64 = argv.get(i + 3).and_then(|s| s.parse().ok()).expect("end_s");
        let language = match argv.get(i + 4).map(|s| s.as_str()) {
            Some("en") => Language::En,
            Some("ja") => Language::Ja,
            _ => Language::Id,
        };
        // Optional 6th arg picks the caption animation (M7): huge | rolling |
        // karaoke. Defaults to huge-word (the historical headless default).
        let caption_genre = parse_genre(argv.get(i + 5).map(|s| s.as_str()));
        // Optional 7th arg picks the Layout (ADR 0017): auto | stacked | cam |
        // gameplay. Defaults to auto (M6 auto-detect).
        let layout_pref = parse_layout_pref(argv.get(i + 6).map(|s| s.as_str()));
        let source = if target.starts_with("http") {
            ImportSource::YouTube(target)
        } else {
            ImportSource::Local(PathBuf::from(target))
        };
        to_worker.send(Job::Import { source, language }).expect("send import");
        let range = TimeRange { start_s, end_s };
        loop {
            match from_worker.recv() {
                Ok(Progress::Stage(s)) => tracing::info!("stage: {s}"),
                Ok(Progress::Imported { title, duration_s, .. }) => {
                    tracing::info!("imported: {title} ({})", fmt_duration(duration_s));
                    // No detection in --headless, so no generated title; the
                    // render names the Short by timestamp (ADR 0015).
                    to_worker
                        .send(Job::Prepare { range, title: None, layout_pref })
                        .expect("send prepare");
                }
                Ok(Progress::Prepared { layout, .. }) => {
                    // No GUI to nudge in: render the auto-detected Layout as-is,
                    // preserving the old one-shot promote behavior (ADR 0012).
                    to_worker
                        .send(Job::Render { layout, caption_genre })
                        .expect("send render");
                }
                Ok(Progress::Done(p)) => {
                    println!("{}", p.display());
                    std::process::exit(0);
                }
                Ok(Progress::Cancelled) => {
                    eprintln!("CANCELLED");
                    std::process::exit(1);
                }
                Ok(Progress::Failed(e)) => {
                    eprintln!("FAILED: {e}");
                    std::process::exit(1);
                }
                Ok(Progress::Detected { .. }) => {} // not reachable in promote-only mode
                Err(_) => std::process::exit(1),
            }
        }
    }

    // Headless detection for verification (drives the real worker, GPU and all):
    //   yt-clipper --detect <url-or-file> [en|id|ja]
    if let Some(i) = argv.iter().position(|a| a == "--detect") {
        let target = argv.get(i + 1).expect("--detect needs <url-or-file> [en|id|ja]").clone();
        let language = match argv.get(i + 2).map(|s| s.as_str()) {
            Some("en") => Language::En,
            Some("ja") => Language::Ja,
            _ => Language::Id,
        };
        let source = if target.starts_with("http") {
            ImportSource::YouTube(target)
        } else {
            ImportSource::Local(PathBuf::from(target))
        };
        to_worker.send(Job::Import { source, language }).expect("send import");
        loop {
            match from_worker.recv() {
                Ok(Progress::Stage(s)) => tracing::info!("stage: {s}"),
                Ok(Progress::Imported { title, duration_s, .. }) => {
                    tracing::info!("imported: {title} ({})", fmt_duration(duration_s));
                    to_worker.send(Job::Detect).expect("send detect");
                }
                Ok(Progress::Detected { moments, .. }) => {
                    println!("detected {} moments:", moments.len());
                    for m in &moments {
                        println!(
                            "  #{:<2} {:>8}-{:<8} score {:5.2}  chat {} loud {} lex {} arou {} llm {}",
                            m.id,
                            fmt_clock(m.range.start_s),
                            fmt_clock(m.range.end_s),
                            m.score,
                            fmt_sig(m.signals.chat_rate),
                            fmt_sig(m.signals.loudness),
                            fmt_sig(m.signals.lexicon),
                            fmt_sig(m.signals.arousal),
                            fmt_sig(m.signals.llm),
                        );
                        // The LLM-generated Shorts title (ADR 0015), if any.
                        if let Some(t) = m.title.as_deref().filter(|t| !t.is_empty()) {
                            println!("        title: {t}");
                        }
                    }
                    std::process::exit(0);
                }
                Ok(Progress::Cancelled) => {
                    eprintln!("CANCELLED");
                    std::process::exit(1);
                }
                Ok(Progress::Failed(e)) => {
                    eprintln!("FAILED: {e}");
                    std::process::exit(1);
                }
                Ok(Progress::Prepared { .. }) => {} // not reachable in detect-only mode
                Ok(Progress::Done(_)) => {}
                Err(_) => std::process::exit(1),
            }
        }
    }

    // Headless batch render (M8 — the job-queue core): import -> detect -> render
    // the top-k Moments sequentially, each auto-framed (no editor) with the chosen
    // Caption Style and named by its LLM title.
    //   yt-clipper --batch <url-or-file> [en|id|ja] [huge|rolling|karaoke] [k] [auto|stacked|cam|gameplay]
    // Drives the same worker the GUI does; one clip renders at a time, so the GPU
    // stages stay strictly sequential (a Prepare/Render pair per Moment, in turn).
    if let Some(i) = argv.iter().position(|a| a == "--batch") {
        let target =
            argv.get(i + 1).expect("--batch needs <url-or-file> [en|id|ja] [genre] [k]").clone();
        let language = match argv.get(i + 2).map(|s| s.as_str()) {
            Some("en") => Language::En,
            Some("ja") => Language::Ja,
            _ => Language::Id,
        };
        let caption_genre = parse_genre(argv.get(i + 3).map(|s| s.as_str()));
        let k: usize = argv.get(i + 4).and_then(|s| s.parse().ok()).unwrap_or(3);
        // Optional 6th arg picks the Layout for every clip (ADR 0017): auto |
        // stacked | cam | gameplay. Defaults to auto (M6 auto-detect).
        let layout_pref = parse_layout_pref(argv.get(i + 5).map(|s| s.as_str()));
        let source = if target.starts_with("http") {
            ImportSource::YouTube(target)
        } else {
            ImportSource::Local(PathBuf::from(target))
        };
        to_worker.send(Job::Import { source, language }).expect("send import");
        // The Moments to render (range + title) and how many have finished.
        let mut queue: Vec<(TimeRange, Option<String>)> = Vec::new();
        let mut rendered = 0usize;
        loop {
            match from_worker.recv() {
                Ok(Progress::Stage(s)) => tracing::info!("stage: {s}"),
                Ok(Progress::Imported { title, duration_s, .. }) => {
                    tracing::info!("imported: {title} ({})", fmt_duration(duration_s));
                    to_worker.send(Job::Detect).expect("send detect");
                }
                Ok(Progress::Detected { moments, .. }) => {
                    queue = moments.iter().take(k).map(|m| (m.range, m.title.clone())).collect();
                    tracing::info!("batch: rendering {} of {} moments", queue.len(), moments.len());
                    match queue.first().cloned() {
                        Some((range, title)) => {
                            to_worker
                                .send(Job::Prepare { range, title, layout_pref })
                                .expect("send prepare");
                        }
                        None => {
                            eprintln!("no moments to render");
                            std::process::exit(0);
                        }
                    }
                }
                Ok(Progress::Prepared { layout, .. }) => {
                    to_worker.send(Job::Render { layout, caption_genre }).expect("send render");
                }
                Ok(Progress::Done(p)) => {
                    println!("{}", p.display()); // one Short path per rendered Moment
                    rendered += 1;
                    match queue.get(rendered).cloned() {
                        Some((range, title)) => {
                            to_worker
                                .send(Job::Prepare { range, title, layout_pref })
                                .expect("send prepare");
                        }
                        None => {
                            tracing::info!("batch: rendered {rendered} clip(s)");
                            std::process::exit(0);
                        }
                    }
                }
                Ok(Progress::Cancelled) => {
                    eprintln!("CANCELLED");
                    std::process::exit(1);
                }
                Ok(Progress::Failed(e)) => {
                    eprintln!("FAILED: {e}");
                    std::process::exit(1);
                }
                Err(_) => std::process::exit(1),
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 860.0])
            .with_title("yt-clipper"),
        ..Default::default()
    };

    eframe::run_native(
        "yt-clipper",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx, &paths.font());
            Ok(Box::new(App {
                paths,
                deno_dir,
                url: String::new(),
                video_path: String::new(),
                start_s: 0.0,
                end_s: 30.0,
                language: Language::Id,
                caption_genre: CaptionGenre::HugeWord,
                layout_pref: LayoutPref::default(),
                imported: None,
                moments: Vec::new(),
                selected: None,
                transcripts: HashMap::new(),
                llm_reasons: HashMap::new(),
                timeline: None,
                analysis_wav: None,
                audio_out: None,
                sink: None,
                volume: 1.0,
                status: Status::Idle,
                editor: None,
                batch_selected: HashSet::new(),
                render_queue: Vec::new(),
                queue_idx: 0,
                to_worker,
                from_worker,
                cancel,
            }))
        }),
    )
}

/// Resolved locations of everything the app needs on disk.
struct AppPaths {
    /// Directory of the running executable; sibling binaries (the yc-llm-judge
    /// sidecar) live here, even though sidecars/models/assets walk up to the root.
    exe_dir: PathBuf,
    /// Pinned ffmpeg.exe / ffprobe.exe / yt-dlp.exe / deno.exe (fetch-sidecars.ps1).
    sidecars: PathBuf,
    /// Whisper GGML and LLM GGUF model files (fetch-models.ps1).
    models: PathBuf,
    /// Bundled assets (fonts, ...) shipped with the app.
    assets: PathBuf,
    /// Per-VOD project folders.
    workspace: PathBuf,
}

impl AppPaths {
    /// Everything resolves relative to the executable: this is a solo-operator
    /// tool distributed as exe-plus-folders (ADR 0005).
    fn resolve() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        // In dev builds the exe sits in target/{debug,release}; walk up to the repo
        // root for sidecars/models/assets/workspace. The yc-llm-judge sidecar binary
        // stays beside the exe, so it resolves from exe_dir, not the walked-up root.
        let root = if exe_dir.ends_with("debug") || exe_dir.ends_with("release") {
            exe_dir.ancestors().nth(2).map(PathBuf::from).unwrap_or_else(|| exe_dir.clone())
        } else {
            exe_dir.clone()
        };
        Self {
            exe_dir,
            sidecars: root.join("sidecars"),
            models: root.join("models"),
            assets: root.join("assets"),
            workspace: root.join("workspace"),
        }
    }

    fn ffmpeg(&self) -> PathBuf {
        self.sidecars.join("ffmpeg.exe")
    }

    fn ffprobe(&self) -> PathBuf {
        self.sidecars.join("ffprobe.exe")
    }

    fn ytdlp(&self) -> PathBuf {
        self.sidecars.join("yt-dlp.exe")
    }

    /// Directory holding deno.exe (pinned sidecar or winget install), prepended
    /// to yt-dlp's PATH so it can solve nsig. `None` if deno is on PATH / absent.
    fn deno_dir(&self) -> Option<PathBuf> {
        yc_ingest::resolve_deno_dir(&self.sidecars)
    }

    fn model(&self) -> PathBuf {
        self.models.join("ggml-large-v3.bin")
    }

    /// CPU speech-emotion model for the arousal Signal (ADR 0008). Absent unless
    /// downloaded; detection runs without it. Operator extracts the audonnx zip
    /// here, giving `models/w2v2-emotion/model.onnx`.
    fn ser_model(&self) -> PathBuf {
        self.models.join("w2v2-emotion").join("model.onnx")
    }

    /// LLM judgment GGUF for the `llm` Signal (ADR 0010). Absent unless
    /// downloaded; detection runs without it. The operator-gated ~5.4 GB
    /// Qwen2.5-7B-Instruct Q5_K_M default.
    fn llm_model(&self) -> PathBuf {
        self.models.join("qwen2.5-7b-instruct-q5_k_m.gguf")
    }

    /// The `yc-llm-judge` sidecar binary, beside the app exe (ADR 0010): it links
    /// llama (the app does not), so the app shells out to it for the llm Signal.
    fn llm_judge(&self) -> PathBuf {
        self.exe_dir.join("yc-llm-judge.exe")
    }

    /// Ultraface RFB-320 face model for M6 auto-framing (ADR 0011). Absent unless
    /// downloaded; framing then falls back to full-frame gameplay.
    fn face_model(&self) -> PathBuf {
        self.models.join("version-RFB-320.onnx")
    }

    /// htdemucs vocals model for the Vocal-stem captions (`sep`). Absent unless
    /// downloaded; the export then captions the mixed analysis audio.
    fn sep_model(&self) -> PathBuf {
        self.models.join("htdemucs_ft_vocals.onnx")
    }

    /// Directory of per-language dialect/slang stores (`assets/dialect/<lang>.json`).
    fn dialect_dir(&self) -> PathBuf {
        self.assets.join("dialect")
    }

    fn font(&self) -> PathBuf {
        self.assets.join("fonts").join("Anton-Regular.ttf")
    }
}

/// VOD facts shown after a successful import.
struct ImportedInfo {
    title: String,
    duration_s: Option<f64>,
}

enum Status {
    Idle,
    Working(String),
    Done(PathBuf),
    Cancelled,
    Failed(String),
}

struct App {
    paths: AppPaths,
    deno_dir: Option<PathBuf>,
    url: String,
    video_path: String,
    start_s: f64,
    end_s: f64,
    language: Language,
    /// Caption animation for the next render (M7): huge-word / rolling-pop /
    /// karaoke-fill. A global selection for now; per-Clip override is later M7.
    caption_genre: CaptionGenre,
    /// Explicit Layout preference for the next clip (ADR 0017): Auto runs M6
    /// auto-detect, the others force stacked / full-cam / full-gameplay. A global
    /// session selection (the nudge editor can still override per-Clip).
    layout_pref: LayoutPref,
    imported: Option<ImportedInfo>,
    /// Candidate Moments from detection (and any manually-marked ones), ranked.
    moments: Vec<Moment>,
    /// The Moment id currently selected for review, if any.
    selected: Option<u64>,
    /// Transcript text per detected Moment id, for the review panel.
    transcripts: HashMap<u64, String>,
    /// LLM judgment reason per detected Moment id (ADR 0010), for the review
    /// panel; empty unless built `--features llm` with the GGUF present.
    llm_reasons: HashMap<u64, String>,
    /// Whole-VOD signal series for the review waveform (set on detect).
    timeline: Option<Timeline>,
    /// Whole-VOD analysis wav (set on import), source for Moment audio playback.
    analysis_wav: Option<PathBuf>,
    /// Default audio output, opened lazily on first playback (None if it fails).
    audio_out: Option<(rodio::OutputStream, rodio::OutputStreamHandle)>,
    /// The currently-playing sink; taking/replacing it stops playback.
    sink: Option<rodio::Sink>,
    /// Review playback gain applied to the sink; 1.0 = unmodified, >1.0 boosts
    /// a quiet streamer in the mixed track (rodio amplifies linearly).
    volume: f32,
    status: Status,
    /// The nudge editor, open from Prepare until the operator dismisses it or a
    /// new Prepare/import replaces it (ADR 0012); persists across re-renders.
    editor: Option<editor::EditorState>,
    /// Moment ids checked for a batch render (M8 job-queue): "Render selected"
    /// renders them sequentially, each auto-framed (no editor).
    batch_selected: HashSet<u64>,
    /// The active batch queue (range + title per Moment) and the index of the
    /// clip currently rendering; empty when no batch runs. The worker drain
    /// auto-renders each (Prepared -> Render) and advances on Done.
    render_queue: Vec<(TimeRange, Option<String>)>,
    queue_idx: usize,
    to_worker: Sender<Job>,
    from_worker: Receiver<Progress>,
    cancel: CancelToken,
}

fn fmt_duration(duration_s: Option<f64>) -> String {
    match duration_s {
        Some(d) => {
            let total = d.round() as u64;
            format!("{}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
        }
        None => "?".into(),
    }
}

/// Compact m:ss (or h:mm:ss past an hour) for a Moment timestamp.
fn fmt_clock(t_s: f64) -> String {
    let s = t_s.round().max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// A signal cell: a z-scored value, or a dash when the signal is absent.
fn fmt_sig(v: Option<f32>) -> String {
    v.map(|x| format!("{x:5.2}")).unwrap_or_else(|| "    -".into())
}

/// Parse the headless caption-genre arg (M7): `rolling` / `karaoke`, else the
/// huge-word default.
fn parse_genre(arg: Option<&str>) -> CaptionGenre {
    match arg {
        Some("rolling") | Some("rolling-pop") => CaptionGenre::RollingPop,
        Some("karaoke") | Some("karaoke-fill") => CaptionGenre::KaraokeFill,
        _ => CaptionGenre::HugeWord,
    }
}

/// Parse the headless / batch Layout-preference arg (ADR 0017): `stacked` /
/// `cam` / `gameplay`, else the `auto` (M6 auto-detect) default. Accepts a few
/// spellings so the operator needn't remember the exact token.
fn parse_layout_pref(arg: Option<&str>) -> LayoutPref {
    match arg {
        Some("stacked") | Some("stack") => LayoutPref::Stacked,
        Some("cam") | Some("fullcam") | Some("full-cam") => LayoutPref::FullCam,
        Some("gameplay") | Some("fullgameplay") | Some("full-gameplay") => {
            LayoutPref::FullGameplay
        }
        _ => LayoutPref::Auto,
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Drain worker messages.
        while let Ok(msg) = self.from_worker.try_recv() {
            match msg {
                Progress::Stage(s) => self.status = Status::Working(s.to_string()),
                Progress::Imported {
                    title,
                    duration_s,
                    analysis_wav,
                    caption_genre,
                    moments,
                    transcripts,
                    llm_reasons,
                } => {
                    self.imported = Some(ImportedInfo { title, duration_s });
                    self.analysis_wav = Some(analysis_wav);
                    // Seed the caption-style picker to this Creator's remembered
                    // choice (ADR 0016); the operator can still override it.
                    if let Some(genre) = caption_genre {
                        self.caption_genre = genre;
                    }
                    // Restore a prior session's detected Moments + their review text
                    // from project.json / review.json (M8) so a re-import shows the
                    // full review (list + transcript panel + LLM reason, playable +
                    // promotable) without re-detecting. The waveform isn't persisted;
                    // a fresh Detect rebuilds it and replaces all of this.
                    self.selected = moments.first().map(|m| m.id);
                    self.moments = moments;
                    self.transcripts = transcripts;
                    self.llm_reasons = llm_reasons;
                    self.status = Status::Idle;
                }
                Progress::Detected { moments, transcripts, llm_reasons, timeline } => {
                    self.selected = moments.first().map(|m| m.id);
                    self.moments = moments;
                    self.transcripts = transcripts;
                    self.llm_reasons = llm_reasons;
                    self.timeline = Some(timeline);
                    self.status = Status::Idle;
                }
                Progress::Prepared { layout, src_w, src_h, frames, frame_w, frame_h, range } => {
                    // Batch render (M8): auto-render this clip with its auto-detected
                    // Layout (no editor); the Done handler advances the queue.
                    // `continue` skips the editor setup and drains the next message.
                    if !self.render_queue.is_empty() {
                        let _ = self
                            .to_worker
                            .send(Job::Render { layout, caption_genre: self.caption_genre });
                        continue;
                    }
                    // Single clip: upload the preview frames to textures and open the
                    // nudge editor seeded with the auto-detected Layout (ADR 0012).
                    let ctx = ui.ctx().clone();
                    let expected = frame_w as usize * frame_h as usize * 3;
                    let textures: Vec<egui::TextureHandle> = frames
                        .iter()
                        .enumerate()
                        .filter(|(_, b)| b.len() == expected)
                        .map(|(i, b)| {
                            let img =
                                egui::ColorImage::from_rgb([frame_w as usize, frame_h as usize], b);
                            ctx.load_texture(
                                format!("preview-{i}"),
                                img,
                                egui::TextureOptions::LINEAR,
                            )
                        })
                        .collect();
                    if textures.is_empty() {
                        self.status = Status::Failed("no preview frames extracted".into());
                    } else {
                        self.editor = Some(editor::EditorState::from_seed(
                            layout,
                            src_w,
                            src_h,
                            range,
                            textures,
                            self.caption_genre,
                        ));
                        self.status = Status::Idle;
                    }
                }
                Progress::Done(p) => {
                    if self.render_queue.is_empty() {
                        self.status = Status::Done(p);
                    } else {
                        // Batch (M8): advance to the next queued Moment, or finish.
                        self.queue_idx += 1;
                        match self.render_queue.get(self.queue_idx).cloned() {
                            Some((range, title)) => {
                                let n = self.render_queue.len();
                                let _ = self.to_worker.send(Job::Prepare { range, title, layout_pref: self.layout_pref });
                                self.status =
                                    Status::Working(format!("Rendering {}/{n}", self.queue_idx + 1));
                            }
                            None => {
                                let n = self.render_queue.len();
                                self.render_queue.clear();
                                self.queue_idx = 0;
                                tracing::info!("batch: rendered {n} clip(s)");
                                self.status = Status::Done(p); // last Short; all N in the folder
                            }
                        }
                    }
                }
                Progress::Cancelled => {
                    // A cancel stops the whole batch, not just the in-flight clip.
                    self.render_queue.clear();
                    self.queue_idx = 0;
                    self.status = Status::Cancelled;
                }
                Progress::Failed(e) => {
                    self.render_queue.clear();
                    self.queue_idx = 0;
                    self.status = Status::Failed(e);
                }
            }
        }
        let working = matches!(self.status, Status::Working(_));

        // Top brand bar (W4 / ADR 0024 theme): the gold brand mark + an
        // always-visible status, instead of a heading buried in the scroll and a
        // status pinned to the very bottom. A proper app bar — the first step of the
        // SaaS shell; the moments-rail / detail-pane split is the next slice.
        egui::TopBottomPanel::top("brandbar").show_inside(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("yt-clipper").color(theme::GOLD));
                ui.add_space(12.0);
                ui.weak("Turn long gaming VODs into vertical Shorts.");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.status_bar(ui);
                });
            });
            ui.add_space(4.0);
        });

        // Preflight: are the sidecars / model / font present?
        ui.separator();
        let file_row = |ui: &mut egui::Ui, name: &str, path: &PathBuf| {
            let ok = path.exists();
            ui.horizontal(|ui| {
                ui.colored_label(
                    if ok { theme::OK } else { theme::ERR },
                    if ok { "ok" } else { "MISSING" },
                );
                ui.label(format!("{name}: {}", path.display()));
            });
        };
        file_row(ui, "ffmpeg", &self.paths.ffmpeg());
        file_row(ui, "ffprobe", &self.paths.ffprobe());
        file_row(ui, "yt-dlp", &self.paths.ytdlp());
        ui.horizontal(|ui| {
            let ok = self.deno_dir.is_some();
            ui.colored_label(
                if ok { egui::Color32::GREEN } else { egui::Color32::RED },
                if ok { "ok" } else { "MISSING" },
            );
            match &self.deno_dir {
                Some(d) => ui.label(format!("deno: {}", d.display())),
                None => ui.label("deno: not found (run fetch-sidecars.ps1 or install via winget)"),
            };
        });
        file_row(ui, "whisper model", &self.paths.model());
        file_row(ui, "caption font", &self.paths.font());

        // --- Import (phase 1) ---
        ui.separator();
        ui.label("1. Import a VOD");
        ui.horizontal(|ui| {
            ui.label("Spoken language");
            egui::ComboBox::from_label("(the streamer, not the game)")
                .selected_text(match self.language {
                    Language::En => "English",
                    Language::Id => "Bahasa Indonesia",
                    Language::Ja => "Nihongo",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.language, Language::En, "English");
                    ui.selectable_value(&mut self.language, Language::Id, "Bahasa Indonesia");
                    ui.selectable_value(&mut self.language, Language::Ja, "Nihongo");
                });
        });
        ui.horizontal(|ui| {
            ui.label("Caption style");
            egui::ComboBox::from_label("(applied on render)")
                .selected_text(match self.caption_genre {
                    CaptionGenre::HugeWord => "Huge Word",
                    CaptionGenre::RollingPop => "Rolling Pop",
                    CaptionGenre::KaraokeFill => "Karaoke Fill",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.caption_genre, CaptionGenre::HugeWord, "Huge Word");
                    ui.selectable_value(&mut self.caption_genre, CaptionGenre::RollingPop, "Rolling Pop");
                    ui.selectable_value(
                        &mut self.caption_genre,
                        CaptionGenre::KaraokeFill,
                        "Karaoke Fill",
                    );
                });
        });
        // Layout preference (ADR 0017): the operator's explicit framing for the
        // next clip. Auto = M6 auto-detect; the others force it (so the preferred
        // stacked / game-on-top, cam-below framing is one click away, and works in
        // batch where the nudge editor never opens).
        ui.horizontal(|ui| {
            ui.label("Layout");
            egui::ComboBox::from_label("(framing for the next clip)")
                .selected_text(match self.layout_pref {
                    LayoutPref::Auto => "Auto-detect",
                    LayoutPref::Stacked => "Stacked (game + cam)",
                    LayoutPref::FullCam => "Full cam",
                    LayoutPref::FullGameplay => "Full gameplay",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.layout_pref, LayoutPref::Auto, "Auto-detect");
                    ui.selectable_value(
                        &mut self.layout_pref,
                        LayoutPref::Stacked,
                        "Stacked (game + cam)",
                    );
                    ui.selectable_value(&mut self.layout_pref, LayoutPref::FullCam, "Full cam");
                    ui.selectable_value(
                        &mut self.layout_pref,
                        LayoutPref::FullGameplay,
                        "Full gameplay",
                    );
                });
        });
        ui.add_enabled_ui(!working, |ui| {
            ui.horizontal(|ui| {
                ui.label("YouTube URL:");
                ui.add(egui::TextEdit::singleline(&mut self.url).desired_width(560.0));
                if ui.button("Import URL").clicked() && !self.url.trim().is_empty() {
                    self.start_import(ImportSource::YouTube(self.url.trim().to_string()));
                }
            });
            ui.horizontal(|ui| {
                ui.label("or local file:");
                ui.add(egui::TextEdit::singleline(&mut self.video_path).desired_width(440.0));
                if ui.button("Browse...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter(
                            "video / audio",
                            &["mp4", "mkv", "webm", "mov", "avi", "m4a", "mp3", "wav", "opus"],
                        )
                        .pick_file()
                    {
                        self.video_path = path.display().to_string();
                        self.start_import(ImportSource::Local(path));
                    }
                }
                if ui.button("Import file").clicked() && !self.video_path.trim().is_empty() {
                    self.start_import(ImportSource::Local(PathBuf::from(self.video_path.trim())));
                }
            });
        });

        // --- Detect & review Moments (phase 2) ---
        // Read the imported facts into an owned Option first, so the interactive
        // widgets below can borrow `self` mutably without fighting a borrow of
        // `self.imported` held across the match.
        ui.separator();
        let imported = self.imported.as_ref().map(|i| (i.title.clone(), i.duration_s));
        match imported {
            None => {
                ui.label("2. Detect Moments - import a VOD first.");
            }
            Some((title, duration_s)) => {
                ui.label(format!("2. Review Moments from: {}  ({})", title, fmt_duration(duration_s)));
                ui.add_enabled_ui(!working, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Detect Moments").clicked() {
                            self.moments.clear();
                            self.selected = None;
                            let _ = self.to_worker.send(Job::Detect);
                            self.status = Status::Working("Starting detection".into());
                        }
                        ui.separator();
                        ui.label("Mark manually:  start");
                        ui.add(egui::DragValue::new(&mut self.start_s).speed(0.5).suffix(" s"));
                        ui.label("end");
                        ui.add(egui::DragValue::new(&mut self.end_s).speed(0.5).suffix(" s"));
                        if ui.button("Add Moment").clicked() && self.end_s > self.start_s {
                            let id = self.moments.iter().map(|m| m.id).max().unwrap_or(0) + 1;
                            self.moments.push(Moment {
                                id,
                                range: TimeRange { start_s: self.start_s, end_s: self.end_s },
                                signals: Signals::default(),
                                score: 0.0,
                                title: None, // manual Moment: render names the Short by timestamp
                            });
                            self.selected = Some(id);
                        }
                    });
                });

                // VOD overview: loudness waveform + chat-rate overlay + Moment
                // markers (ADR 0001: review by waveform, not video scrubbing).
                // Click a marker to select that Moment.
                let mut tl_select: Option<u64> = None;
                if let Some(tl) = &self.timeline {
                    let total_s = tl.loudness.len() as f64 * tl.bin_s;
                    if total_s > 0.0 {
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 70.0),
                            egui::Sense::click(),
                        );
                        let p = ui.painter_at(rect);
                        p.rect_filled(rect, egui::CornerRadius::ZERO, egui::Color32::from_gray(18));
                        let n = tl.loudness.len();
                        let cols = rect.width().max(1.0) as usize;
                        let lmax = tl.loudness.iter().copied().fold(1e-6_f32, f32::max);
                        for c in 0..cols {
                            let i0 = c * n / cols;
                            let i1 = ((c + 1) * n / cols).clamp(i0 + 1, n);
                            let peak = tl.loudness[i0..i1].iter().copied().fold(0.0, f32::max) / lmax;
                            let x = rect.left() + c as f32;
                            p.line_segment(
                                [egui::pos2(x, rect.bottom()), egui::pos2(x, rect.bottom() - peak * rect.height())],
                                egui::Stroke::new(1.0, egui::Color32::from_gray(85)),
                            );
                        }
                        if let Some(chat) = &tl.chat {
                            let cmax = chat.iter().copied().fold(1e-6_f32, f32::max);
                            let mut prev: Option<egui::Pos2> = None;
                            for c in 0..cols {
                                let i0 = c * n / cols;
                                let i1 = ((c + 1) * n / cols).clamp(i0 + 1, n);
                                let avg = chat[i0..i1].iter().copied().sum::<f32>() / (i1 - i0) as f32 / cmax;
                                let pt = egui::pos2(rect.left() + c as f32, rect.bottom() - avg * rect.height());
                                if let Some(pp) = prev {
                                    p.line_segment([pp, pt], egui::Stroke::new(1.0, egui::Color32::from_rgb(90, 170, 255)));
                                }
                                prev = Some(pt);
                            }
                        }
                        let t_to_x = |t: f64| rect.left() + (t / total_s) as f32 * rect.width();
                        for m in &self.moments {
                            let x0 = t_to_x(m.range.start_s);
                            let x1 = t_to_x(m.range.end_s).max(x0 + 1.0);
                            let col = if self.selected == Some(m.id) {
                                egui::Color32::from_rgba_unmultiplied(255, 209, 0, 110)
                            } else {
                                egui::Color32::from_rgba_unmultiplied(255, 80, 80, 70)
                            };
                            p.rect_filled(
                                egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
                                egui::CornerRadius::ZERO,
                                col,
                            );
                        }
                        if resp.clicked() {
                            if let Some(pos) = resp.interact_pointer_pos() {
                                let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * total_s;
                                let mid = |m: &Moment| (m.range.start_s + m.range.end_s) / 2.0;
                                tl_select = self
                                    .moments
                                    .iter()
                                    .min_by(|a, b| {
                                        (mid(a) - t).abs().partial_cmp(&(mid(b) - t).abs()).unwrap_or(std::cmp::Ordering::Equal)
                                    })
                                    .map(|m| m.id);
                            }
                        }
                    }
                }
                if let Some(id) = tl_select {
                    self.selected = Some(id);
                }

                // Ranked Moment list with per-signal breakdown; select + promote.
                // Signals are z-scores (sigmas above the VOD baseline); a dash
                // means the signal is absent (e.g. no chat, or a manual Moment).
                if self.moments.is_empty() {
                    ui.label("No Moments yet - click Detect Moments, or mark one manually.");
                } else {
                    // The promoted range plus that Moment's generated title (ADR
                    // 0015), threaded to Prepare so the render names the Short.
                    let mut to_promote: Option<(TimeRange, Option<String>)> = None;
                    let mut to_select: Option<u64> = None;
                    // Batch-select checkbox toggles (M8): (Moment id, new checked).
                    let mut batch_toggles: Vec<(u64, bool)> = Vec::new();
                    let selected = self.selected;
                    let enabled = !working;
                    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                        egui::Grid::new("moments").striped(true).num_columns(10).show(ui, |ui| {
                            for h in ["", "#", "range", "score", "chat", "loud", "lex", "arou", "llm", ""] {
                                ui.label(h);
                            }
                            ui.end_row();
                            for m in &self.moments {
                                // Batch-render select (M8): render these together.
                                let mut checked = self.batch_selected.contains(&m.id);
                                if ui.add_enabled(enabled, egui::Checkbox::new(&mut checked, "")).changed() {
                                    batch_toggles.push((m.id, checked));
                                }
                                let sel = selected == Some(m.id);
                                if ui.add(egui::Button::selectable(sel, m.id.to_string())).clicked() {
                                    to_select = Some(m.id);
                                }
                                ui.label(format!(
                                    "{}-{}",
                                    fmt_clock(m.range.start_s),
                                    fmt_clock(m.range.end_s)
                                ));
                                ui.label(format!("{:.2}", m.score));
                                ui.label(fmt_sig(m.signals.chat_rate));
                                ui.label(fmt_sig(m.signals.loudness));
                                ui.label(fmt_sig(m.signals.lexicon));
                                ui.label(fmt_sig(m.signals.arousal));
                                ui.label(fmt_sig(m.signals.llm));
                                if ui.add_enabled(enabled, egui::Button::new("Promote")).clicked() {
                                    to_promote = Some((m.range, m.title.clone()));
                                    to_select = Some(m.id);
                                }
                                ui.end_row();
                            }
                        });
                    });
                    for (id, on) in batch_toggles {
                        if on {
                            self.batch_selected.insert(id);
                        } else {
                            self.batch_selected.remove(&id);
                        }
                    }
                    if let Some(id) = to_select {
                        self.selected = Some(id);
                    }
                    if let Some((range, title)) = to_promote {
                        self.editor = None; // a new clip replaces any open editor
                        let _ = self.to_worker.send(Job::Prepare { range, title, layout_pref: self.layout_pref });
                        self.status = Status::Working("Preparing clip".into());
                    }

                    // Batch render (M8 job-queue): render every checked Moment
                    // sequentially, each auto-framed (no editor), in rank order. The
                    // Prepared/Done worker-drain handlers above drive the queue.
                    let n_sel = self.batch_selected.len();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                enabled && n_sel > 0,
                                egui::Button::new(format!("Render {n_sel} selected (auto-framed)")),
                            )
                            .clicked()
                        {
                            let queue: Vec<(TimeRange, Option<String>)> = self
                                .moments
                                .iter()
                                .filter(|m| self.batch_selected.contains(&m.id))
                                .map(|m| (m.range, m.title.clone()))
                                .collect();
                            if let Some((range, title)) = queue.first().cloned() {
                                self.editor = None;
                                self.render_queue = queue;
                                self.queue_idx = 0;
                                let _ = self.to_worker.send(Job::Prepare { range, title, layout_pref: self.layout_pref });
                                self.status = Status::Working(format!(
                                    "Rendering 1/{}",
                                    self.render_queue.len()
                                ));
                            }
                        }
                        if n_sel > 0
                            && ui.add_enabled(enabled, egui::Button::new("Clear")).clicked()
                        {
                            self.batch_selected.clear();
                        }
                    });
                }
            }
        }

        // --- Selected Moment: transcript + audio playback (ADR 0001) ---
        let mut play: Option<TimeRange> = None;
        let mut stop = false;
        let mut vol_changed = false;
        if let Some(id) = self.selected {
            if let Some(m) = self.moments.iter().find(|m| m.id == id).cloned() {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "Moment #{}   {} - {}",
                        id,
                        fmt_clock(m.range.start_s),
                        fmt_clock(m.range.end_s)
                    ));
                    play = ui.button("Play").clicked().then_some(m.range);
                    stop = ui.button("Stop").clicked();
                    ui.label("Vol");
                    vol_changed = ui
                        .add(egui::Slider::new(&mut self.volume, 0.0..=2.0).show_value(false))
                        .changed();
                });
                // The LLM-generated Shorts title (ADR 0015) — this is what the
                // rendered Short will be named, so the operator sees it pre-promote.
                if let Some(title) = m.title.as_deref().filter(|t| !t.is_empty()) {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong("Title:");
                        ui.label(title);
                    });
                }
                if let Some(reason) = self.llm_reasons.get(&id) {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong("LLM:");
                        ui.label(reason);
                    });
                }
                match self.transcripts.get(&id) {
                    Some(t) if !t.trim().is_empty() => {
                        egui::ScrollArea::vertical()
                            .id_salt("transcript")
                            .max_height(120.0)
                            .show(ui, |ui| {
                                ui.label(t);
                            });
                    }
                    Some(_) => {
                        ui.weak("(no speech transcribed for this Moment)");
                    }
                    None => {
                        ui.weak("(manual Moment - run Detect to transcribe)");
                    }
                }
            }
        }
        if let Some(range) = play {
            self.play_range(range);
        }
        if stop {
            self.stop_audio();
        }
        // Live volume: nudge the playing sink without restarting it.
        if vol_changed {
            if let Some(sink) = &self.sink {
                sink.set_volume(self.volume);
            }
        }

        // --- Nudge editor (ADR 0012): frame the prepared Clip before render ---
        // A floating, scrollable Window so the tall 9:16 composite and its
        // Render button stay reachable over the central content (eframe's `ui`
        // gives a non-scrolling central Ui).
        let mut editor_action = editor::EditorAction::None;
        let mut keep_open = true;
        if let Some(ed) = &mut self.editor {
            egui::Window::new("Frame the Clip")
                .open(&mut keep_open)
                .resizable(true)
                .vscroll(true)
                .default_size(egui::vec2(380.0, 720.0))
                .show(ui.ctx(), |ui| {
                    editor_action = ed.show(ui, !working);
                });
        }
        if !keep_open {
            self.editor = None;
        }
        match editor_action {
            editor::EditorAction::Render(layout, caption_genre) => {
                // The editor's per-Clip pick wins; mirror it back to the app's
                // selection so it stays the default for the next clip.
                self.caption_genre = caption_genre;
                let _ = self.to_worker.send(Job::Render { layout, caption_genre });
                self.status = Status::Working("Rendering".into());
            }
            editor::EditorAction::Cancel => self.editor = None,
            editor::EditorAction::None => {}
        }

        // (Status lives in the top brand bar now — see `status_bar`.)

        // While a GPU job runs, repaint at ~10 fps instead of unbounded: the
        // continuous wgpu render loop otherwise competes with whisper for the
        // single 8 GB card and starves it (the detect-hang scar). 10 fps still
        // drains worker progress and animates the spinner smoothly.
        if working {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

impl App {
    /// The always-visible status painted into the top brand bar: a spinner + stage
    /// + Cancel while a job runs, else a coloured outcome (full path / error on
    /// hover). Rendered in a right-to-left layout, so the rightmost item is added
    /// first.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        match &self.status {
            Status::Idle => {
                ui.weak("Ready");
            }
            Status::Working(stage) => {
                if ui.button("Cancel").clicked() {
                    self.cancel.cancel();
                }
                ui.label(stage.clone());
                ui.spinner();
            }
            Status::Done(path) => {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                ui.colored_label(theme::OK, format!("Done: {name}"))
                    .on_hover_text(path.display().to_string());
            }
            Status::Cancelled => {
                ui.colored_label(theme::GOLD, "Cancelled");
            }
            Status::Failed(err) => {
                ui.colored_label(theme::ERR, "Failed").on_hover_text(err.clone());
            }
        }
    }

    fn start_import(&mut self, source: ImportSource) {
        self.imported = None;
        self.moments.clear();
        self.transcripts.clear();
        self.llm_reasons.clear();
        self.timeline = None;
        self.selected = None;
        self.editor = None;
        let _ = self.to_worker.send(Job::Import { source, language: self.language });
        self.status = Status::Working("Starting import".into());
    }

    /// Open the default audio output once; `None` if the system has none.
    fn ensure_audio(&mut self) -> Option<&rodio::OutputStreamHandle> {
        if self.audio_out.is_none() {
            match rodio::OutputStream::try_default() {
                Ok(pair) => self.audio_out = Some(pair),
                Err(e) => {
                    tracing::warn!("audio output unavailable: {e}");
                    return None;
                }
            }
        }
        self.audio_out.as_ref().map(|(_, handle)| handle)
    }

    /// Play a Moment's audio range from the analysis wav (review-by-ear; ADR
    /// 0001 rules out scrubbing, so this just plays the one range).
    fn play_range(&mut self, range: TimeRange) {
        let Some(wav) = self.analysis_wav.clone() else { return };
        let samples = match yc_ingest::read_range_samples(&wav, range) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("reading audio range: {e:#}");
                return;
            }
        };
        self.stop_audio();
        // Clone the handle so the borrow of `self.audio_out` ends before we
        // assign `self.sink`.
        let handle = match self.ensure_audio() {
            Some(h) => h.clone(),
            None => return,
        };
        match rodio::Sink::try_new(&handle) {
            Ok(sink) => {
                sink.append(rodio::buffer::SamplesBuffer::new(1, yc_ingest::WHISPER_SR, samples));
                sink.set_volume(self.volume);
                sink.play();
                self.sink = Some(sink);
            }
            Err(e) => tracing::warn!("audio sink: {e}"),
        }
    }

    fn stop_audio(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
    }
}
