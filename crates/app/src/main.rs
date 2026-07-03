//! yt-clipper - pure-Rust desktop shell (egui/eframe, ADR 0005).
//! M2: YouTube ingest end-to-end - import a URL (audio + chat + metadata),
//! pick a range, promote it to a Clip (padded Segment -> whisper -> stacked
//! Layout -> rolling-pop ASS -> NVENC export). A local file is also importable
//! for offline iteration. The pipeline runs on a background worker thread; the
//! UI polls it and stays responsive, and Cancel kills an in-flight download.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod editor;
mod pipeline;
mod player;
mod presets;
mod review_queue;
mod theme;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use pipeline::{ImportSource, Job, Progress, Timeline};
use yc_core::{
    CaptionEngine, CaptionGenre, CaptionStyle, Language, LayoutPref, Moment, Signals, TimeRange,
};
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
        deep_filter: paths.deep_filter(),
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
        let language = parse_language(argv.get(i + 4).map(|s| s.as_str()));
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
                        .send(Job::Prepare { range, title: None, layout_pref, preview: false })
                        .expect("send prepare");
                }
                Ok(Progress::Prepared { layout, .. }) => {
                    // No GUI to nudge in: render the auto-detected Layout as-is,
                    // preserving the old one-shot promote behavior (ADR 0012).
                    // No editor means no Caption placement either (ADR 0036).
                    // Engine `None`: the Creator's saved engine decides, with
                    // YC_QWEN_ENS as the tri-state override (ADR 0035).
                    to_worker
                        .send(Job::Render {
                            layout,
                            style: CaptionStyle::for_genre(caption_genre),
                            correct: correct_from_env(),
                            placement: None,
                            caption_engine: None,
                            camera: None,
                            transcript_override: None,
                        })
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
                Ok(Progress::Captions { .. }) => {} // preview-only (ADR 0036); no editor headless
                Ok(Progress::Speakers { .. }) => {} // editor-only (podcast mode)
                Ok(Progress::JobDone) => {}
                Err(_) => std::process::exit(1),
            }
        }
    }

    // Headless detection for verification (drives the real worker, GPU and all):
    //   yt-clipper --detect <url-or-file> [en|id|ja]
    if let Some(i) = argv.iter().position(|a| a == "--detect") {
        let target = argv.get(i + 1).expect("--detect needs <url-or-file> [en|id|ja]").clone();
        let language = parse_language(argv.get(i + 2).map(|s| s.as_str()));
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
                    to_worker.send(Job::Detect { max_dur_s: max_clip_s_from_env() }).expect("send detect");
                }
                Ok(Progress::Detected { moments, .. }) => {
                    println!("detected {} moments:", moments.len());
                    for m in &moments {
                        println!(
                            "  #{:<2} {:>8}-{:<8} {:>4.0}s score {:5.2}  chat {} loud {} lex {} arou {} llm {}",
                            m.id,
                            fmt_clock(m.range.start_s),
                            fmt_clock(m.range.end_s),
                            m.range.duration_s(),
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
                Ok(Progress::Captions { .. }) => {}
                Ok(Progress::Speakers { .. }) => {}
                Ok(Progress::JobDone) => {}
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
        let language = parse_language(argv.get(i + 2).map(|s| s.as_str()));
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
                    to_worker.send(Job::Detect { max_dur_s: max_clip_s_from_env() }).expect("send detect");
                }
                Ok(Progress::Detected { moments, .. }) => {
                    queue = moments.iter().take(k).map(|m| (m.range, m.title.clone())).collect();
                    tracing::info!("batch: rendering {} of {} moments", queue.len(), moments.len());
                    match queue.first().cloned() {
                        Some((range, title)) => {
                            to_worker
                                .send(Job::Prepare { range, title, layout_pref, preview: false })
                                .expect("send prepare");
                        }
                        None => {
                            eprintln!("no moments to render");
                            std::process::exit(0);
                        }
                    }
                }
                Ok(Progress::Prepared { layout, .. }) => {
                    // Engine `None`: per-Creator resolution + env override, as
                    // in --headless above (ADR 0035).
                    to_worker
                        .send(Job::Render {
                            layout,
                            style: CaptionStyle::for_genre(caption_genre),
                            correct: correct_from_env(),
                            placement: None,
                            caption_engine: None,
                            camera: None,
                            transcript_override: None,
                        })
                        .expect("send render");
                }
                Ok(Progress::Done(p)) => {
                    println!("{}", p.display()); // one Short path per rendered Moment
                    rendered += 1;
                    match queue.get(rendered).cloned() {
                        Some((range, title)) => {
                            to_worker
                                .send(Job::Prepare { range, title, layout_pref, preview: false })
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
                Ok(Progress::Captions { .. }) => {} // preview-only (ADR 0036); no editor in batch
                Ok(Progress::Speakers { .. }) => {}
                Ok(Progress::JobDone) => {}
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
                language: None, // Auto: the Creator's saved language (ADR 0016)
                caption_genre: CaptionGenre::HugeWord,
                caption_engine: CaptionEngine::Whisper,
                saved_engine: None,
                correct_captions: false, // opt-in (ADR 0031); off until the operator ticks it
                layout_pref: LayoutPref::default(),
                max_clip_s: yc_detect::DetectParams::default().max_dur_s,
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
                pending_title: None,
                opening_editor: false,
                rendering: false,
                batch_selected: HashSet::new(),
                render_queue: Vec::new(),
                queue_idx: 0,
                review: None,
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

    /// The bundled `deep-filter` DeepFilterNet sidecar for Cleaned-voice captions
    /// (`enh`, ADR 0029) — a pinned sidecar like ffmpeg/yt-dlp (the model is baked
    /// into the binary). Absent unless downloaded; the export then captions the
    /// mixed analysis audio.
    fn deep_filter(&self) -> PathBuf {
        self.sidecars.join("deep-filter.exe")
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
    /// What the import's language resolved to (the explicit pick, or — on Auto —
    /// the Creator's saved default, ADR 0016), shown so the operator can see what
    /// the transcription will use.
    language: Language,
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
    /// Transcription language for the next import: `None` = **Auto** (apply the
    /// Creator's saved language from `creators.json`, ADR 0016); `Some` = the
    /// operator's explicit pick, which always wins. The import reports what it
    /// resolved to via `Progress::Imported`.
    language: Option<Language>,
    /// Caption animation for the next render (M7): huge-word / rolling-pop /
    /// karaoke-fill. A global selection for now; per-Clip override is later M7.
    caption_genre: CaptionGenre,
    /// Caption engine for the next render (ADR 0035): whisper or the Qwen
    /// ensemble. Seeded from the Creator's saved engine on import (reset to
    /// Whisper for an unknown Creator — no default flip); the render persists
    /// the selection back per Creator.
    caption_engine: CaptionEngine,
    /// The imported Creator's engine as saved in `creators.json` (`None` =
    /// unknown Creator), kept to detect a flip: when the rail selection
    /// differs, the import rail shows which curated corrections carry
    /// (ADR 0035's switch warn). Updated when a render persists the selection.
    saved_engine: Option<CaptionEngine>,
    /// Run the LLM caption-correction pass on the next render (ADR 0030/0031):
    /// applies the operator's curated slang/name overrides in context. Default off
    /// (opt-in) and only effective in a `correct` build with the sidecar present.
    correct_captions: bool,
    /// Explicit Layout preference for the next clip (ADR 0017): Auto runs M6
    /// auto-detect, the others force stacked / full-cam / full-gameplay. A global
    /// session selection (the nudge editor can still override per-Clip).
    layout_pref: LayoutPref,
    /// Ceiling for a detected Moment's adaptive length, seconds (30..=180, the
    /// YouTube-Shorts maximum). The detector picks each Moment's natural length
    /// below this.
    max_clip_s: f64,
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
    /// The Studio editor page, open from Prepare until the operator dismisses
    /// it or a new Prepare/import replaces it (ADR 0012); persists across
    /// re-renders.
    editor: Option<editor::EditorState>,
    /// The Moment title promoted into the editor (Prepare carries it to the
    /// worker; the editor toolbar shows it).
    pending_title: Option<String>,
    /// True from clicking "Open in editor" until the Prepare finishes: the
    /// detail pane shows a loading state (segment fetch + face detect +
    /// filmstrip take a few seconds; silence read as a hang).
    opening_editor: bool,
    /// True while an NVENC render is in flight — gates ONLY the editor's
    /// Export/Render actions. Transcribe / speaker analysis do NOT set this:
    /// the operator keeps editing (and can even queue the render) while the
    /// GPU pre-passes run.
    rendering: bool,
    /// Moment ids checked for a batch render (M8 job-queue): "Render selected"
    /// renders them sequentially, each auto-framed (no editor).
    batch_selected: HashSet<u64>,
    /// The active batch queue (range + title per Moment) and the index of the
    /// clip currently rendering; empty when no batch runs. The worker drain
    /// auto-renders each (Prepared -> Render) and advances on Done.
    render_queue: Vec<(TimeRange, Option<String>)>,
    queue_idx: usize,
    /// The Caption review queue for the imported VOD's Creator (ADR 0032): the
    /// per-Creator dialect store's harvested to-dos, curated in-app. `None` until a
    /// VOD is imported; reloaded fresh on each import.
    review: Option<review_queue::ReviewState>,
    to_worker: Sender<Job>,
    from_worker: Receiver<Progress>,
    cancel: CancelToken,
}

/// egui's `Spinner`, minus its `request_repaint()`. The stock widget requests an
/// **immediate** repaint every frame it is visible ("because it is animated"),
/// and egui honours the soonest request — so drawing it while a job runs
/// silently overrode the 10 fps GPU-job throttle and kept the wgpu loop
/// repainting flat-out against whisper/NVENC on the single 8 GB card. This
/// paints the same arc (radius/points/stroke copied from egui 0.34
/// `Spinner::paint_at`) and lets the throttle's `request_repaint_after` drive
/// the animation at ~10 fps instead.
pub(crate) fn throttled_spinner(ui: &mut egui::Ui) {
    let size = ui.style().spacing.interact_size.y;
    throttled_spinner_sized(ui, size);
}

/// [`throttled_spinner`] at an explicit size — the loading hero draws it big.
pub(crate) fn throttled_spinner_sized(ui: &mut egui::Ui, size: f32) {
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let color = ui.visuals().strong_text_color();
        let radius = (rect.height().min(rect.width()) / 2.0) - 2.0;
        let n_points = (radius.round() as u32).clamp(8, 128);
        let time = ui.input(|i| i.time);
        let start_angle = time * std::f64::consts::TAU;
        let end_angle = start_angle + 240f64.to_radians() * time.sin();
        let points: Vec<egui::Pos2> = (0..n_points)
            .map(|i| {
                let angle = egui::lerp(start_angle..=end_angle, f64::from(i) / f64::from(n_points));
                let (sin, cos) = angle.sin_cos();
                rect.center() + radius * egui::vec2(cos as f32, sin as f32)
            })
            .collect();
        ui.painter().add(egui::Shape::line(points, egui::Stroke::new(3.0, color)));
    }
}

/// Display label for a transcription language (the combo + the imported header).
fn lang_label(l: Language) -> &'static str {
    match l {
        Language::En => "English",
        Language::Id => "Bahasa Indonesia",
        Language::Ja => "Nihongo",
    }
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

/// Truncate `s` to at most `max` characters, appending an ellipsis when cut — for
/// the compact Moment-list labels in the rail (W4).
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/// Parse the headless / batch language arg: an explicit `en` / `id` / `ja` forces
/// that language; anything else (omitted, or a following positional like the
/// genre) is **Auto** — the import applies the Creator's saved language (ADR
/// 0016), falling back to Bahasa Indonesia. Matches the GUI's Auto default.
fn parse_language(arg: Option<&str>) -> Option<Language> {
    match arg {
        Some("en") => Some(Language::En),
        Some("id") => Some(Language::Id),
        Some("ja") => Some(Language::Ja),
        _ => None,
    }
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

/// Whether a worker stage string names GPU work (whisper / NVENC / the LLM
/// judge / the Qwen ensemble). GPU stages get the protective ~10 fps repaint
/// throttle (the detect-hang scar); everything else animates at 60 fps. The
/// stage strings are in-repo constants (pipeline.rs `Progress::Stage` sends),
/// so the substrings below are stable; a new GPU stage must mention its
/// engine ("whisper" / "GPU" / "NVENC" / "LLM" / "Ensemble") to be throttled.
fn stage_is_gpu(stage: &str) -> bool {
    ["whisper", "GPU", "NVENC", "LLM", "Ensemble"].iter().any(|k| stage.contains(k))
}

#[cfg(test)]
mod stage_tests {
    use super::stage_is_gpu;

    /// Pin the classification of every `Progress::Stage` string pipeline.rs
    /// sends: GPU stages must keep the protective 10 fps repaint throttle,
    /// CPU/network stages must animate at 60 fps. If a stage string changes
    /// in pipeline.rs, this list is the reminder to reclassify it.
    #[test]
    fn pipeline_stages_classify_correctly() {
        for gpu in [
            "Transcribing (whisper, GPU)",
            "Refining moments (whisper, GPU)",
            "Refining moments (LLM judgment, GPU)",
            "Ensemble captions (Qwen3-ASR)",
            "Correcting captions (LLM)",
            "Rendering (NVENC)",
        ] {
            assert!(stage_is_gpu(gpu), "{gpu} must throttle");
        }
        for cpu in [
            "Fetching metadata",
            "Downloading audio",
            "Fetching chat",
            "Extracting audio",
            "Detecting moments (chat + loudness)",
            "Refining moments (arousal, CPU)",
            "Fetching segment",
            "Framing (face detect)",
            "Extracting preview frames",
            "Analyzing speakers (faces + voice)",
            "Cleaning voice",
            "Separating vocal stem",
            "Generating captions",
            "Captions ready",
        ] {
            assert!(!stage_is_gpu(cpu), "{cpu} must stay smooth");
        }
    }
}

/// The max Moment length (seconds) for headless/batch detection: `YC_MAX_CLIP_S`
/// overrides, else the detector's default cap. Clamped downstream to the 180 s
/// Shorts ceiling. The GUI exposes the same knob as a slider.
fn max_clip_s_from_env() -> f64 {
    std::env::var("YC_MAX_CLIP_S")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(yc_detect::DetectParams::default().max_dur_s)
}

/// Whether headless/batch renders should run the LLM caption-correction pass (ADR
/// 0030/0031). Off by default (matching the GUI checkbox); opt in with `YC_CORRECT=1`
/// (or on/true/yes). Only effective in a `correct` build with the sidecar present.
fn correct_from_env() -> bool {
    matches!(
        std::env::var("YC_CORRECT").ok().as_deref().map(str::trim),
        Some("1") | Some("on") | Some("true") | Some("yes")
    )
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
                    language,
                    analysis_wav,
                    caption_genre,
                    caption_engine,
                    moments,
                    transcripts,
                    llm_reasons,
                    creator_store,
                    video_id,
                    clip_stores,
                } => {
                    self.imported = Some(ImportedInfo { title, duration_s, language });
                    self.analysis_wav = Some(analysis_wav);
                    // Load this Creator's caption review queue (ADR 0032): the
                    // per-Creator store's harvested to-dos plus any per-clip stores'
                    // fresh harvests, curated in the detail pane.
                    self.review = Some(review_queue::ReviewState::load(
                        creator_store,
                        &clip_stores,
                        video_id,
                    ));
                    // Seed the caption-style picker to this Creator's remembered
                    // choice (ADR 0016); the operator can still override it.
                    if let Some(genre) = caption_genre {
                        self.caption_genre = genre;
                    }
                    // Seed the engine picker likewise (ADR 0035) — but unlike
                    // genre, an UNKNOWN Creator resets it to Whisper: engine
                    // flips are deliberate per-Creator acts, so a previous
                    // session's ensemble pick must not leak onto a new Creator.
                    self.caption_engine = caption_engine.unwrap_or_default();
                    self.saved_engine = caption_engine;
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
                Progress::Prepared {
                    layout,
                    src_w,
                    src_h,
                    frames,
                    frame_w,
                    frame_h,
                    frame_fps,
                    range,
                    faces,
                    render_src,
                    seek_s,
                } => {
                    self.opening_editor = false;
                    // Batch render (M8): auto-render this clip with its auto-detected
                    // Layout (no editor); the Done handler advances the queue.
                    // `continue` skips the editor setup and drains the next message.
                    // No editor also means no Caption placement (ADR 0036).
                    if !self.render_queue.is_empty() {
                        let _ = self.to_worker.send(Job::Render {
                            layout,
                            style: CaptionStyle::for_genre(self.caption_genre),
                            correct: self.correct_captions,
                            placement: None,
                            caption_engine: Some(self.caption_engine),
                            camera: None,
                            transcript_override: None,
                        });
                        self.rendering = true;
                        continue;
                    }
                    // Single clip: upload the preview frames to textures and open the
                    // Studio editor seeded with the auto-detected Layout (ADR 0012).
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
                        let podcast_frame = faces.len() >= 2;
                        let mut ed = editor::EditorState::from_seed(
                            layout,
                            src_w,
                            src_h,
                            range,
                            self.pending_title.clone(),
                            textures,
                            frame_fps,
                            self.caption_genre,
                            self.caption_engine,
                            faces,
                            self.paths.ffmpeg(),
                            render_src,
                            seek_s,
                        );
                        // The editor workflow pre-passes (focus 2026-07): kick
                        // transcription now so captions are editable before any
                        // render (the render then reuses the cache, NVENC-only);
                        // a 2+-face frame reads as a podcast, so the speaker
                        // analysis queues right behind it (the worker is serial).
                        let _ = self.to_worker.send(Job::Transcribe {
                            correct: self.correct_captions,
                            caption_engine: Some(self.caption_engine),
                        });
                        self.status = Status::Working("Transcribing captions".into());
                        if podcast_frame {
                            let _ = self.to_worker.send(Job::AnalyzeSpeakers);
                            ed.speaker_job = editor::SpeakerJob::Running;
                        }
                        self.editor = Some(ed);
                    }
                }
                Progress::Captions { transcript } => {
                    // The refined transcript a Transcribe/Render produced (ADR
                    // 0036): hand it to the editor so the caption panel + overlay
                    // show the render's truth.
                    if let Some(ed) = &mut self.editor {
                        ed.set_captions(transcript);
                    }
                }
                Progress::Speakers { analysis, plan } => {
                    if let Some(ed) = &mut self.editor {
                        ed.set_speakers(analysis, plan);
                    }
                }
                Progress::JobDone => {
                    self.status = Status::Idle;
                }
                Progress::Done(p) => {
                    // The render just persisted the rail's engine selection per
                    // Creator (ADR 0035); track it so the switch warn clears.
                    self.saved_engine = Some(self.caption_engine);
                    self.rendering = false;
                    if self.render_queue.is_empty() {
                        self.status = Status::Done(p);
                    } else {
                        // Batch (M8): advance to the next queued Moment, or finish.
                        self.queue_idx += 1;
                        match self.render_queue.get(self.queue_idx).cloned() {
                            Some((range, title)) => {
                                let n = self.render_queue.len();
                                // Batch: no editor opens, so no filmstrip (ADR 0036).
                                let _ = self.to_worker.send(Job::Prepare {
                                    range,
                                    title,
                                    layout_pref: self.layout_pref,
                                    preview: false,
                                });
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
                    self.opening_editor = false;
                    self.rendering = false;
                    // A cancelled speaker analysis must not leave the Camera
                    // panel spinning (mirrors the Failed handler); NotRun
                    // re-offers the run button.
                    if let Some(ed) = &mut self.editor {
                        if ed.speaker_job == editor::SpeakerJob::Running {
                            ed.speaker_job = editor::SpeakerJob::NotRun;
                        }
                    }
                    self.status = Status::Cancelled;
                }
                Progress::Failed(e) => {
                    self.render_queue.clear();
                    self.queue_idx = 0;
                    self.opening_editor = false;
                    self.rendering = false;
                    // A failure while the speaker analysis was in flight lands in
                    // the editor's Camera panel (retryable) as well as the bar.
                    if let Some(ed) = &mut self.editor {
                        if ed.speaker_job == editor::SpeakerJob::Running {
                            ed.speaker_job = editor::SpeakerJob::Failed(e.clone());
                        }
                    }
                    self.status = Status::Failed(e);
                }
            }
        }
        let working = matches!(self.status, Status::Working(_));
        // Whether the CURRENT stage holds the GPU (whisper / NVENC / LLM /
        // ensemble): those get the protective 10 fps repaint throttle; CPU and
        // network stages animate at full rate.
        let gpu_busy = matches!(&self.status, Status::Working(s) if stage_is_gpu(s));

        // Top brand bar (W4 / ADR 0024 theme): the gold brand mark + an
        // always-visible status. Ink-filled so it reads as the app's chrome,
        // one step below the panels it caps.
        egui::Panel::top("brandbar")
            .frame(
                egui::Frame::new()
                    .fill(theme::INK)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(egui::RichText::new("yt-clipper").color(theme::GOLD).size(24.0));
                    ui.add_space(12.0);
                    ui.weak("Turn long VODs into vertical Shorts.");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.status_bar(ui);
                    });
                });
            });

        // --- Two pages: the Library (import + Moments + detail) and, once a
        // Clip is promoted, the full-window Studio editor (focus 2026-07). The
        // Studio owns the whole area below the brand bar — a promoted Clip is
        // the operator's entire context until they Export or go Back. ---
        let mut editor_action = editor::EditorAction::None;
        if self.editor.is_some() {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                if let Some(ed) = &mut self.editor {
                    editor_action = ed.show(ui, working, gpu_busy, self.rendering);
                }
            });
        } else {
            egui::Panel::left("rail")
                .resizable(true)
                .default_size(380.0)
                .min_size(300.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::SURFACE)
                        .inner_margin(egui::Margin::symmetric(12, 8)),
                )
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("rail").show(ui, |ui| {
                        self.ui_import(ui, working);
                        self.ui_moments(ui, working);
                        self.ui_preflight(ui);
                    });
                });
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(theme::WELL)
                        .inner_margin(egui::Margin::symmetric(18, 12)),
                )
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("detail").show(ui, |ui| {
                        self.ui_detail(ui, working);
                    });
                });
        }

        match editor_action {
            editor::EditorAction::Render(spec) => {
                // The editor's per-Clip genre pick becomes the session default
                // (and is what persists per Creator). Placement stays per-Clip
                // (ADR 0036) — nothing global to mirror.
                self.caption_genre = spec.style.genre;
                self.stop_audio();
                let _ = self.to_worker.send(Job::Render {
                    layout: spec.layout,
                    style: spec.style,
                    correct: self.correct_captions,
                    placement: spec.placement,
                    caption_engine: Some(self.caption_engine),
                    camera: spec.camera,
                    transcript_override: spec.transcript_override,
                });
                self.rendering = true;
                self.status = Status::Working("Rendering".into());
            }
            editor::EditorAction::AnalyzeSpeakers => {
                let _ = self.to_worker.send(Job::AnalyzeSpeakers);
                self.status = Status::Working("Analyzing speakers".into());
            }
            editor::EditorAction::Cancel => {
                self.stop_audio();
                // Leaving the Studio abandons its pre-pass: cancel the in-flight
                // auto job (Transcribe / AnalyzeSpeakers) instead of letting it
                // burn the GPU for a clip nobody is editing. A deliberate Render
                // is never touched - Back stays usable while one runs.
                if matches!(self.status, Status::Working(_)) && !self.rendering {
                    self.cancel.cancel();
                }
                self.editor = None;
            }
            // Editor playback (ADR 0036): the same sink the Moment review uses,
            // sliced from the whole-VOD analysis wav at the clip offset.
            editor::EditorAction::Play(range) => self.play_range(range),
            editor::EditorAction::StopAudio => self.stop_audio(),
            editor::EditorAction::None => {}
        }

        // (Status lives in the top brand bar now — see `status_bar`.)

        // Repaint cadence while a job runs. GPU stages (whisper / NVENC / the
        // LLM judge / the Qwen ensemble) keep the ~10 fps throttle: the
        // continuous wgpu render loop otherwise competes with them for the
        // single 8 GB card and starves them (the detect-hang scar). CPU/network
        // stages (import, segment fetch, face detect, speaker analysis) tick at
        // 60 fps so spinners and progress read smoothly — wgpu repaints cost
        // those stages nothing. This only holds because nothing else requests
        // an immediate repaint while working — egui's stock `ui.spinner()` does
        // exactly that every frame, which silently overrode this throttle until
        // 2026-07-02; the status bar draws [`throttled_spinner`] instead.
        if working {
            let ms = if gpu_busy { 100 } else { 16 };
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(ms));
        }
    }
}

impl App {
    /// The always-visible status painted into the top brand bar: a spinner +
    /// stage + Cancel while a job runs, else a coloured chip (full path /
    /// error on hover). Right-to-left layout — the rightmost item adds first.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        match &self.status {
            Status::Idle => {
                theme::status_chip(ui, egui::Color32::from_gray(120), "Ready");
            }
            Status::Working(stage) => {
                if ui.button("Cancel").clicked() {
                    self.cancel.cancel();
                }
                ui.label(stage.clone());
                throttled_spinner(ui);
            }
            Status::Done(path) => {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let resp = ui.scope(|ui| {
                    theme::status_chip(ui, theme::OK, &format!("Done · {}", ellipsize(&name, 36)));
                });
                resp.response.on_hover_text(path.display().to_string());
            }
            Status::Cancelled => {
                theme::status_chip(ui, theme::GOLD, "Cancelled");
            }
            Status::Failed(err) => {
                let err = err.clone();
                let resp = ui.scope(|ui| {
                    theme::status_chip(ui, theme::ERR, "Failed");
                });
                resp.response.on_hover_text(err);
            }
        }
    }

    /// Left-rail section: the preflight tool/model check, collapsed once every
    /// sidecar + model is present (auto-expanded when something is missing).
    /// Rows show only the name + a status dot (the operator's ask) — the full
    /// path is a hover detail, not a wall of directories.
    fn ui_preflight(&mut self, ui: &mut egui::Ui) {
        let row = |ui: &mut egui::Ui, ok: bool, name: &str, detail: String| {
            let resp = ui
                .horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(
                        rect.center(),
                        4.0,
                        if ok { theme::OK } else { theme::ERR },
                    );
                    ui.label(name);
                    if !ok {
                        ui.colored_label(theme::ERR, "missing");
                    }
                })
                .response;
            resp.on_hover_text(detail);
        };
        let files = [
            ("ffmpeg", self.paths.ffmpeg()),
            ("ffprobe", self.paths.ffprobe()),
            ("yt-dlp", self.paths.ytdlp()),
            ("whisper model", self.paths.model()),
            ("caption font", self.paths.font()),
        ];
        let all_ok = self.deno_dir.is_some() && files.iter().all(|(_, p)| p.exists());
        let header = if all_ok {
            "Diagnostics — all tools ready"
        } else {
            "Diagnostics — something is missing"
        };
        // One flat row list (deno included) so every tool renders through the
        // same path.
        let mut rows: Vec<(bool, String, String)> = files
            .iter()
            .map(|(n, p)| (p.exists(), (*n).to_string(), p.display().to_string()))
            .collect();
        match &self.deno_dir {
            Some(d) => rows.push((true, "deno".into(), d.display().to_string())),
            None => rows.push((
                false,
                "deno".into(),
                "not found — run fetch-sidecars.ps1 or `winget install DenoLand.Deno`".into(),
            )),
        }
        let debug_open = std::env::var("YC_DIAG_OPEN").is_ok(); // capture aid
        egui::CollapsingHeader::new(header).default_open(!all_ok || debug_open).show(ui, |ui| {
            for (ok, name, detail) in rows {
                row(ui, ok, &name, detail);
            }
        });
    }

    /// The engine-switch warn (ADR 0035 §2): when the rail's engine selection
    /// differs from the imported Creator's saved one, quantify — from the
    /// per-Creator store the review queue already holds in memory — which of
    /// their confirmed corrections carry across the flip. Inline and
    /// non-blocking: nothing about a flip is destructive (corrections are never
    /// deleted), and the operator's ear stays ground truth.
    fn ui_engine_switch_warn(&self, ui: &mut egui::Ui) {
        let Some(saved) = self.saved_engine else { return };
        if saved == self.caption_engine {
            return;
        }
        let Some(review) = &self.review else { return };
        let t = yc_transcribe::ensemble::transfer_counts(&review.lexicon.corrections);
        if t.total() == 0 {
            return; // nothing curated yet -> nothing to quantify
        }
        let text = match self.caption_engine {
            CaptionEngine::QwenEnsemble => format!(
                "Engine flip: {} correction(s) carry to the ensemble ({} single-word + {} pinned); \
                 {} stay whisper-only ({} multi-word + {} context).",
                t.carries(),
                t.single_word,
                t.pinned,
                t.stays(),
                t.multi_word,
                t.context
            ),
            CaptionEngine::Whisper => format!(
                "Engine flip: {} pinned fix(es) go dormant (ensemble-only); \
                 the other {} correction(s) apply on whisper as before.",
                t.pinned,
                t.total() - t.pinned
            ),
        };
        ui.colored_label(theme::GOLD, text);
    }

    /// Left-rail section: import a VOD — the URL / local-file pickers up top
    /// (the first thing a new session needs), the per-import defaults below.
    fn ui_import(&mut self, ui: &mut egui::Ui, working: bool) {
        theme::section(ui, "Import a VOD");
        theme::card().show(ui, |ui| {
            ui.add_enabled_ui(!working, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.url)
                        .desired_width(f32::INFINITY)
                        .hint_text("https://youtu.be/…"),
                );
                ui.horizontal(|ui| {
                    if theme::primary_button(ui, "Import URL").clicked()
                        && !self.url.trim().is_empty()
                    {
                        self.start_import(ImportSource::YouTube(self.url.trim().to_string()));
                    }
                    ui.weak("or");
                    if ui.button("Open a local file…").clicked() {
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
                });
            });
        });

        theme::section(ui, "Defaults for this import");
        theme::card().show(ui, |ui| {
            egui::Grid::new("import-defaults").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Language");
                egui::ComboBox::from_id_salt("lang")
                    .selected_text(match self.language {
                        None => "Auto (Creator's saved)",
                        Some(l) => lang_label(l),
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.language, None, "Auto (Creator's saved)");
                        ui.selectable_value(&mut self.language, Some(Language::En), "English");
                        ui.selectable_value(&mut self.language, Some(Language::Id), "Bahasa Indonesia");
                        ui.selectable_value(&mut self.language, Some(Language::Ja), "Nihongo");
                    });
                ui.end_row();

                ui.label("Caption style");
                egui::ComboBox::from_id_salt("caption")
                    .selected_text(match self.caption_genre {
                        CaptionGenre::HugeWord => "Huge Word",
                        CaptionGenre::RollingPop => "Rolling Pop",
                        CaptionGenre::KaraokeFill => "Karaoke",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.caption_genre, CaptionGenre::HugeWord, "Huge Word");
                        ui.selectable_value(&mut self.caption_genre, CaptionGenre::RollingPop, "Rolling Pop");
                        ui.selectable_value(&mut self.caption_genre, CaptionGenre::KaraokeFill, "Karaoke");
                    });
                ui.end_row();

                ui.label("Engine");
                egui::ComboBox::from_id_salt("engine")
                    .selected_text(match self.caption_engine {
                        CaptionEngine::Whisper => "Whisper",
                        CaptionEngine::QwenEnsemble => "Qwen ensemble",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.caption_engine, CaptionEngine::Whisper, "Whisper");
                        ui.selectable_value(
                            &mut self.caption_engine,
                            CaptionEngine::QwenEnsemble,
                            "Qwen ensemble",
                        );
                    })
                    .response
                    .on_hover_text("Saved per Creator; the ensemble adds ~60-90 s per clip (ADR 0035)");
                ui.end_row();

                ui.label("Layout");
                egui::ComboBox::from_id_salt("layout")
                    .selected_text(match self.layout_pref {
                        LayoutPref::Auto => "Auto-detect",
                        LayoutPref::Stacked => "Stacked",
                        LayoutPref::FullCam => "Full cam",
                        LayoutPref::FullGameplay => "Full gameplay",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.layout_pref, LayoutPref::Auto, "Auto-detect");
                        ui.selectable_value(&mut self.layout_pref, LayoutPref::Stacked, "Stacked (game + cam)");
                        ui.selectable_value(&mut self.layout_pref, LayoutPref::FullCam, "Full cam");
                        ui.selectable_value(&mut self.layout_pref, LayoutPref::FullGameplay, "Full gameplay");
                    });
                ui.end_row();
            });
            ui.checkbox(&mut self.correct_captions, "LLM caption correction")
                .on_hover_text("Apply curated slang/name fixes in context (needs a 'correct' build + sidecar; ADR 0030)");
            self.ui_engine_switch_warn(ui);
        });
    }

    /// Left-rail section: detect / mark Moments + the ranked, selectable Moment
    /// list (pick one to see its detail on the right; check boxes for a batch
    /// render). The wide per-signal breakdown moved to the detail pane.
    fn ui_moments(&mut self, ui: &mut egui::Ui, working: bool) {
        let enabled = !working;
        theme::section(ui, "Moments");
        let Some((title, duration_s, language)) =
            self.imported.as_ref().map(|i| (i.title.clone(), i.duration_s, i.language))
        else {
            ui.weak("Import a VOD to detect Moments.");
            return;
        };
        // Show the resolved language so an Auto import's Creator default is visible.
        ui.label(egui::RichText::new(ellipsize(&title, 44)).strong());
        ui.weak(format!("{} · {}", fmt_duration(duration_s), lang_label(language)));
        ui.add_space(4.0);
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                if theme::primary_button(ui, "Detect Moments").clicked() {
                    self.moments.clear();
                    self.selected = None;
                    let _ = self.to_worker.send(Job::Detect { max_dur_s: self.max_clip_s });
                    self.status = Status::Working("Starting detection".into());
                }
                ui.label("max");
                ui.add(
                    egui::Slider::new(&mut self.max_clip_s, 30.0..=180.0)
                        .step_by(5.0)
                        .suffix(" s"),
                )
                .on_hover_text(
                    "Ceiling for a detected Moment. The detector picks the natural \
                     length per moment (a sustained arc grows, a sharp one stays tight); \
                     180 s is the YouTube Shorts maximum.",
                );
            });
            ui.horizontal(|ui| {
                ui.weak("Mark manually:");
                ui.add(egui::DragValue::new(&mut self.start_s).speed(0.5).suffix("s"));
                ui.label("→");
                ui.add(egui::DragValue::new(&mut self.end_s).speed(0.5).suffix("s"));
                if ui.button("Add").clicked() && self.end_s > self.start_s {
                    let id = self.moments.iter().map(|m| m.id).max().unwrap_or(0) + 1;
                    self.moments.push(Moment {
                        id,
                        range: TimeRange { start_s: self.start_s, end_s: self.end_s },
                        signals: Signals::default(),
                        score: 0.0,
                        title: None,
                    });
                    self.selected = Some(id);
                }
            });
        });
        ui.add_space(4.0);
        if self.moments.is_empty() {
            ui.weak("No Moments yet — Detect, or add one manually.");
            return;
        }
        let selected = self.selected;
        let mut to_select: Option<u64> = None;
        let mut batch_toggles: Vec<(u64, bool)> = Vec::new();
        egui::ScrollArea::vertical().id_salt("moments").max_height(380.0).show(ui, |ui| {
            for m in &self.moments {
                ui.horizontal(|ui| {
                    let mut checked = self.batch_selected.contains(&m.id);
                    if ui
                        .add_enabled(enabled, egui::Checkbox::new(&mut checked, ""))
                        .on_hover_text("Queue for a batch render")
                        .changed()
                    {
                        batch_toggles.push((m.id, checked));
                    }
                    let label = match m.title.as_deref().filter(|t| !t.is_empty()) {
                        Some(t) => ellipsize(t, 30),
                        None => format!(
                            "{}–{}",
                            fmt_clock(m.range.start_s),
                            fmt_clock(m.range.end_s)
                        ),
                    };
                    if ui.selectable_label(selected == Some(m.id), label).clicked() {
                        to_select = Some(m.id);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if m.score > 0.0 {
                            ui.weak(format!("{:.1}", m.score));
                        }
                        ui.weak(format!("{:.0}s", m.range.duration_s()));
                    });
                });
            }
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
        let n_sel = self.batch_selected.len();
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    enabled && n_sel > 0,
                    egui::Button::new(format!("Render {n_sel} selected")),
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
                    // Batch: no editor opens, so no filmstrip (ADR 0036).
                    let _ = self.to_worker.send(Job::Prepare {
                        range,
                        title,
                        layout_pref: self.layout_pref,
                        preview: false,
                    });
                    self.status =
                        Status::Working(format!("Rendering 1/{}", self.render_queue.len()));
                }
            }
            if n_sel > 0 && ui.add_enabled(enabled, egui::Button::new("Clear")).clicked() {
                self.batch_selected.clear();
            }
        });
    }

    /// Right pane: the VOD overview waveform (click a marker to select) + the
    /// selected Moment's detail — signals, title, LLM reason, transcript, audio
    /// scrub, and the Promote action. An empty state when nothing is selected.
    /// The Caption review queue (ADR 0032): surface the imported Creator's harvested
    /// caption to-dos (blank-`right` corrections, ADR 0014/0022) grouped by source
    /// clip, so the operator fills the correct word + Saves in-app instead of editing
    /// JSON. Edits the per-Creator store directly (the promote target), so a Save is
    /// durable for the Creator immediately. Save is disabled while a job runs, so it
    /// never races the worker's mid-render `promote_confirmed`.
    fn ui_review(&mut self, ui: &mut egui::Ui, working: bool) {
        let Some(review) = self.review.as_mut() else { return };
        // The SNAPSHOT queue, not a live blank-`right` filter: rows must stay
        // put while the operator types into them (they retire on Save).
        let n = review.queue.len();
        let header = if n == 0 {
            "Caption review queue".to_string()
        } else {
            format!("Caption review queue · {n} to-do{}", if n == 1 { "" } else { "s" })
        };
        egui::CollapsingHeader::new(egui::RichText::new(header).strong())
            .id_salt("review-queue")
            .default_open(n > 0)
            .show(ui, |ui| {
                if n == 0 {
                    ui.weak("No caption to-dos — this Creator's captions are clean, or none harvested yet.");
                    return;
                }
                ui.weak("Fill the word whisper should have written, then Save. Confirmed fixes apply to every future clip of this Creator (ADR 0031).");
                ui.add_space(4.0);
                // Build the grouped view from the snapshot (owns its rows + source
                // indices), then edit corrections[idx] in place — the group holds no
                // borrow into the store, and rows never vanish mid-edit.
                let groups =
                    review_queue::group_queue(&review.lexicon.corrections, &review.queue);
                egui::ScrollArea::vertical().id_salt("review-rows").max_height(320.0).show(ui, |ui| {
                    for g in &groups {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(&g.title).color(theme::GOLD));
                        for row in &g.rows {
                            ui.horizontal(|ui| {
                                ui.monospace(&row.wrong);
                                if let Some(conf) = row.note.confidence {
                                    ui.weak(format!("· conf {conf:.2}"));
                                }
                                if let Some(at) = row.note.at_s {
                                    match review.video_id.as_deref() {
                                        Some(vid) => {
                                            ui.hyperlink_to(
                                                format!("▶ {}", fmt_clock(at)),
                                                review_queue::youtube_jump_url(vid, at),
                                            );
                                        }
                                        None => {
                                            ui.weak(format!("@ {}", fmt_clock(at)));
                                        }
                                    }
                                }
                                ui.add_space(6.0);
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut review.lexicon.corrections[row.idx].right,
                                    )
                                    .desired_width(150.0)
                                    .hint_text("correct word"),
                                );
                                ui.checkbox(
                                    &mut review.lexicon.corrections[row.idx].context,
                                    "context",
                                )
                                .on_hover_text("Tick for a real word the streamer means as slang or a name — routes through the LLM pass in context, not the always-on global dict (ADR 0030).");
                            });
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!working, egui::Button::new("Save curation"))
                        .on_hover_text("Writes workspace/<creator>/<lang>.json (ADR 0031)")
                        .clicked()
                    {
                        review.status = match review.save() {
                            Ok(k) => format!("Saved — {k} confirmed correction(s)."),
                            Err(e) => format!("Save failed: {e}"),
                        };
                    }
                    ui.weak(&review.status);
                });
            });
        ui.add_space(8.0);
        ui.separator();
    }

    fn ui_detail(&mut self, ui: &mut egui::Ui, working: bool) {
        // Opening the editor: Prepare runs a few seconds (segment fetch, face
        // detect, filmstrip) — show a real loading state instead of a frozen
        // library (the operator read the silent gap as a hang).
        if self.opening_editor {
            ui.add_space(ui.available_height() * 0.30);
            ui.vertical_centered(|ui| {
                throttled_spinner_sized(ui, 44.0);
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("OPENING THE EDITOR")
                        .family(theme::display_family())
                        .size(20.0)
                        .color(egui::Color32::from_gray(210)),
                );
                ui.add_space(4.0);
                let stage = match &self.status {
                    Status::Working(s) => s.clone(),
                    _ => "Preparing clip".into(),
                };
                ui.weak(format!("{stage}…"));
                ui.weak("fetching the segment · detecting faces · building the preview");
            });
            // Smooth 60 fps spinner: Prepare is CPU/network only (segment
            // fetch, ffprobe, face detect, frame extraction) — the 10 fps
            // GPU-protection throttle is about whisper/NVENC, which never run
            // during this screen, so the loading animation can be fluid.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
            return;
        }

        // Caption review queue (ADR 0032): the Creator's harvested caption to-dos,
        // curated in-app. Creator-level, so it shows regardless of Moment selection.
        self.ui_review(ui, working);

        let mut tl_select: Option<u64> = None;
        if let Some(tl) = &self.timeline {
            let total_s = tl.loudness.len() as f64 * tl.bin_s;
            if total_s > 0.0 {
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 64.0),
                    egui::Sense::click(),
                );
                let p = ui.painter_at(rect);
                p.rect_filled(rect, egui::CornerRadius::same(4), egui::Color32::from_gray(18));
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
                        egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
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
                        egui::Color32::from_rgba_unmultiplied(255, 209, 0, 120)
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
                ui.add_space(8.0);
            }
        }
        if let Some(id) = tl_select {
            self.selected = Some(id);
        }

        let Some(id) = self.selected else {
            // Empty state: a proper landing hero instead of a lone sentence —
            // the pane is most of the window and must look intentional.
            ui.add_space(ui.available_height() * 0.24);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("FROM VOD TO SHORT IN MINUTES")
                        .family(theme::display_family())
                        .size(28.0)
                        .color(egui::Color32::from_gray(210)),
                );
                ui.add_space(14.0);
                let step = |ui: &mut egui::Ui, n: &str, text: &str| {
                    ui.horizontal(|ui| {
                        ui.add_space(ui.available_width() * 0.5 - 170.0);
                        ui.label(
                            egui::RichText::new(n)
                                .color(theme::GOLD)
                                .strong()
                                .size(15.0),
                        );
                        ui.label(egui::RichText::new(text).size(14.5).color(egui::Color32::from_gray(170)));
                    });
                };
                step(ui, "1", "Import a VOD — a YouTube URL or a local file");
                step(ui, "2", "Detect Moments — AI ranks the clip-worthy spans");
                step(ui, "3", "Open in editor — frame, caption, and export the Short");
                if self.imported.is_some() && !self.moments.is_empty() {
                    ui.add_space(12.0);
                    ui.weak("Select a Moment from the list on the left.");
                }
            });
            return;
        };
        let Some(m) = self.moments.iter().find(|m| m.id == id).cloned() else {
            return;
        };

        if let Some(t) = m.title.as_deref().filter(|t| !t.is_empty()) {
            ui.heading(egui::RichText::new(t).color(theme::GOLD));
        } else {
            ui.heading(format!("Moment #{id}"));
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "{} – {}",
                fmt_clock(m.range.start_s),
                fmt_clock(m.range.end_s)
            ));
            ui.weak(format!("· {:.0} s", m.range.duration_s()));
            ui.weak(format!("· score {:.2}", m.score));
        });
        // Per-signal breakdown (z-scores; a dash means the signal is absent).
        ui.horizontal_wrapped(|ui| {
            let sig = |ui: &mut egui::Ui, name: &str, v: Option<f32>| {
                let text = format!(
                    "{name} {}",
                    v.map(|x| format!("{x:+.1}")).unwrap_or_else(|| "—".into())
                );
                let color = match v {
                    Some(x) if x >= 1.0 => theme::OK,
                    Some(x) if x <= -1.0 => theme::ERR,
                    _ => egui::Color32::from_gray(150),
                };
                theme::status_chip(ui, color, &text);
            };
            sig(ui, "chat", m.signals.chat_rate);
            sig(ui, "loud", m.signals.loudness);
            sig(ui, "lex", m.signals.lexicon);
            sig(ui, "arousal", m.signals.arousal);
            sig(ui, "llm", m.signals.llm);
        });
        if let Some(reason) = self.llm_reasons.get(&id) {
            ui.horizontal_wrapped(|ui| {
                ui.strong("LLM:");
                ui.label(reason);
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("▶ Play").clicked() {
                self.play_range(m.range);
            }
            if ui.button("■ Stop").clicked() {
                self.stop_audio();
            }
            ui.label("Vol");
            if ui
                .add(egui::Slider::new(&mut self.volume, 0.0..=2.0).show_value(false))
                .changed()
            {
                if let Some(sink) = &self.sink {
                    sink.set_volume(self.volume);
                }
            }
            ui.add_space(10.0);
            if theme::primary_button(ui, "Open in editor")
                .on_hover_text("Frame, caption, and export this Moment as a Short")
                .clicked()
                && !working
            {
                // Review audio must not play over the Studio (Back and Render
                // already stop it; the promote path forgot).
                self.stop_audio();
                self.editor = None;
                self.pending_title = m.title.clone();
                self.opening_editor = true;
                let _ = self.to_worker.send(Job::Prepare {
                    range: m.range,
                    title: m.title.clone(),
                    layout_pref: self.layout_pref,
                    preview: true, // the editor opens on this Prepare
                });
                self.status = Status::Working("Preparing clip".into());
            }
        });
        ui.add_space(6.0);
        match self.transcripts.get(&id) {
            Some(t) if !t.trim().is_empty() => {
                ui.strong("Transcript");
                egui::ScrollArea::vertical().id_salt("transcript").max_height(240.0).show(ui, |ui| {
                    ui.label(t);
                });
            }
            Some(_) => {
                ui.weak("(no speech transcribed for this Moment)");
            }
            None => {
                ui.weak("(manual Moment — run Detect to transcribe)");
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
        self.review = None;
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
