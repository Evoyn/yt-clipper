//! yt-clipper - pure-Rust desktop shell (egui/eframe, ADR 0005).
//! M2: YouTube ingest end-to-end - import a URL (audio + chat + metadata),
//! pick a range, promote it to a Clip (padded Segment -> whisper -> stacked
//! Layout -> rolling-pop ASS -> NVENC export). A local file is also importable
//! for offline iteration. The pipeline runs on a background worker thread; the
//! UI polls it and stays responsive, and Cancel kills an in-flight download.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod editor;
mod pipeline;
mod review_queue;
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
                    to_worker
                        .send(Job::Render {
                            layout,
                            caption_genre,
                            correct: correct_from_env(),
                            placement: None,
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
                Ok(Progress::Captions { .. }) => {}
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
                    to_worker.send(Job::Detect).expect("send detect");
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
                    to_worker
                        .send(Job::Render {
                            layout,
                            caption_genre,
                            correct: correct_from_env(),
                            placement: None,
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
                correct_captions: false, // opt-in (ADR 0031); off until the operator ticks it
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
    /// Run the LLM caption-correction pass on the next render (ADR 0030/0031):
    /// applies the operator's curated slang/name overrides in context. Default off
    /// (opt-in) and only effective in a `correct` build with the sidecar present.
    correct_captions: bool,
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
fn throttled_spinner(ui: &mut egui::Ui) {
    let size = ui.style().spacing.interact_size.y;
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
                } => {
                    // Batch render (M8): auto-render this clip with its auto-detected
                    // Layout (no editor); the Done handler advances the queue.
                    // `continue` skips the editor setup and drains the next message.
                    // No editor also means no Caption placement (ADR 0036).
                    if !self.render_queue.is_empty() {
                        let _ = self.to_worker.send(Job::Render {
                            layout,
                            caption_genre: self.caption_genre,
                            correct: self.correct_captions,
                            placement: None,
                        });
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
                            frame_fps,
                            self.caption_genre,
                        ));
                        self.status = Status::Idle;
                    }
                }
                Progress::Captions { transcript } => {
                    // The refined transcript this render burns (ADR 0036): hand it
                    // to the editor so the caption overlay previews the render's
                    // truth (arrives while NVENC still runs).
                    if let Some(ed) = &mut self.editor {
                        ed.set_captions(transcript);
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
        egui::Panel::top("brandbar").show_inside(ui, |ui| {
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

        // --- W4: two-pane clip workspace — a Moments rail (left) + a detail /
        // preview pane (right), on the ADR 0024 theme. Each section is its own
        // method so the panel structure stays legible. ---
        egui::Panel::left("rail")
            .resizable(true)
            .default_size(380.0)
            .min_size(300.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("rail").show(ui, |ui| {
                    self.ui_preflight(ui);
                    self.ui_import(ui, working);
                    self.ui_moments(ui, working);
                });
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("detail").show(ui, |ui| {
                self.ui_detail(ui, working);
            });
        });

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
            editor::EditorAction::Render(layout, caption_genre, placement) => {
                // The editor's per-Clip pick wins; mirror it back to the app's
                // selection so it stays the default for the next clip. Placement
                // stays per-Clip (ADR 0036) — nothing global to mirror.
                self.caption_genre = caption_genre;
                self.stop_audio();
                let _ = self.to_worker.send(Job::Render {
                    layout,
                    caption_genre,
                    correct: self.correct_captions,
                    placement,
                });
                self.status = Status::Working("Rendering".into());
            }
            editor::EditorAction::Cancel => {
                self.stop_audio();
                self.editor = None;
            }
            // Editor playback (ADR 0036): the same sink the Moment review uses,
            // sliced from the whole-VOD analysis wav at the clip offset.
            editor::EditorAction::Play(range) => self.play_range(range),
            editor::EditorAction::StopAudio => self.stop_audio(),
            editor::EditorAction::None => {}
        }

        // (Status lives in the top brand bar now — see `status_bar`.)

        // While a GPU job runs, repaint at ~10 fps instead of unbounded: the
        // continuous wgpu render loop otherwise competes with whisper for the
        // single 8 GB card and starves it (the detect-hang scar). 10 fps still
        // drains worker progress and animates the spinner smoothly. This only
        // holds because nothing else requests an immediate repaint while working —
        // egui's stock `ui.spinner()` does exactly that every frame (an animated
        // widget), which silently overrode this throttle until 2026-07-02; the
        // status bar draws [`throttled_spinner`] instead.
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
                throttled_spinner(ui);
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

    /// Left-rail section: the preflight tool/model check, collapsed once every
    /// sidecar + model is present (auto-expanded when something is missing).
    fn ui_preflight(&mut self, ui: &mut egui::Ui) {
        let row = |ui: &mut egui::Ui, ok: bool, text: String| {
            ui.horizontal(|ui| {
                ui.colored_label(
                    if ok { theme::OK } else { theme::ERR },
                    if ok { "ok" } else { "MISSING" },
                );
                ui.label(text);
            });
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
        egui::CollapsingHeader::new(header).default_open(!all_ok).show(ui, |ui| {
            for (name, p) in &files {
                row(ui, p.exists(), format!("{name}: {}", p.display()));
            }
            match &self.deno_dir {
                Some(d) => row(ui, true, format!("deno: {}", d.display())),
                None => row(ui, false, "deno: not found (run fetch-sidecars.ps1 / winget)".into()),
            }
        });
    }

    /// Left-rail section: import a VOD — the per-clip defaults (language / caption /
    /// layout) and the URL / local-file pickers.
    fn ui_import(&mut self, ui: &mut egui::Ui, working: bool) {
        ui.separator();
        ui.strong("1 · Import a VOD");
        ui.add_space(2.0);
        ui.horizontal(|ui| {
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
            ui.weak("(streamer, not the game)");
        });
        ui.horizontal(|ui| {
            ui.label("Caption");
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
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.correct_captions, "Correct captions (LLM)");
            ui.weak("(curated slang/name fixes; needs a 'correct' build + sidecar)");
        });
        ui.horizontal(|ui| {
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
        });
        ui.add_space(6.0);
        ui.add_enabled_ui(!working, |ui| {
            ui.label("YouTube URL");
            ui.add(
                egui::TextEdit::singleline(&mut self.url)
                    .desired_width(f32::INFINITY)
                    .hint_text("https://youtu.be/…"),
            );
            if ui.button("Import URL").clicked() && !self.url.trim().is_empty() {
                self.start_import(ImportSource::YouTube(self.url.trim().to_string()));
            }
            ui.add_space(6.0);
            ui.label("or a local file");
            ui.add(
                egui::TextEdit::singleline(&mut self.video_path)
                    .desired_width(f32::INFINITY)
                    .hint_text("path to a video / audio file"),
            );
            ui.horizontal(|ui| {
                if ui.button("Browse…").clicked() {
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
    }

    /// Left-rail section: detect / mark Moments + the ranked, selectable Moment
    /// list (pick one to see its detail on the right; check boxes for a batch
    /// render). The wide per-signal breakdown moved to the detail pane.
    fn ui_moments(&mut self, ui: &mut egui::Ui, working: bool) {
        ui.separator();
        let enabled = !working;
        let Some((title, duration_s, language)) =
            self.imported.as_ref().map(|i| (i.title.clone(), i.duration_s, i.language))
        else {
            ui.strong("2 · Moments");
            ui.weak("Import a VOD to detect Moments.");
            return;
        };
        ui.strong("2 · Moments");
        // Show the resolved language so an Auto import's Creator default is visible.
        ui.weak(format!("{title}  ({} · {})", fmt_duration(duration_s), lang_label(language)));
        ui.add_enabled_ui(enabled, |ui| {
            if ui.button("Detect Moments").clicked() {
                self.moments.clear();
                self.selected = None;
                let _ = self.to_worker.send(Job::Detect);
                self.status = Status::Working("Starting detection".into());
            }
            ui.horizontal(|ui| {
                ui.label("Manual:");
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
        egui::ScrollArea::vertical().id_salt("moments").max_height(360.0).show(ui, |ui| {
            for m in &self.moments {
                ui.horizontal(|ui| {
                    let mut checked = self.batch_selected.contains(&m.id);
                    if ui.add_enabled(enabled, egui::Checkbox::new(&mut checked, "")).changed() {
                        batch_toggles.push((m.id, checked));
                    }
                    let label = match m.title.as_deref().filter(|t| !t.is_empty()) {
                        Some(t) => format!("#{}  {}", m.id, ellipsize(t, 30)),
                        None => format!(
                            "#{}  {}–{}",
                            m.id,
                            fmt_clock(m.range.start_s),
                            fmt_clock(m.range.end_s)
                        ),
                    };
                    if ui.selectable_label(selected == Some(m.id), label).clicked() {
                        to_select = Some(m.id);
                    }
                    if m.score > 0.0 {
                        ui.weak(format!("{:.1}", m.score));
                    }
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
        let n = review_queue::todo_count(&review.lexicon.corrections);
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
                // Build the grouped view (owns its rows + source indices), then edit
                // corrections[idx] in place — the group holds no borrow into the store.
                let groups = review_queue::group_unverified(&review.lexicon.corrections);
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
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.weak("Select a Moment from the list to see its detail,");
                ui.weak("or Detect Moments to begin.");
            });
            return;
        };
        let Some(m) = self.moments.iter().find(|m| m.id == id).cloned() else {
            return;
        };

        ui.horizontal(|ui| {
            ui.heading(format!("Moment #{id}"));
            ui.add_space(6.0);
            ui.label(format!("{} – {}", fmt_clock(m.range.start_s), fmt_clock(m.range.end_s)));
        });
        if let Some(t) = m.title.as_deref().filter(|t| !t.is_empty()) {
            ui.horizontal_wrapped(|ui| {
                ui.strong("Title:");
                ui.label(egui::RichText::new(t).color(theme::GOLD));
            });
        }
        // Per-signal breakdown (z-scores; a dash means the signal is absent).
        ui.horizontal_wrapped(|ui| {
            let sig = |ui: &mut egui::Ui, name: &str, v: Option<f32>| {
                ui.label(format!(
                    "{name} {}",
                    v.map(|x| format!("{x:+.1}")).unwrap_or_else(|| "—".into())
                ));
            };
            ui.weak(format!("score {:.2}  ·", m.score));
            sig(ui, "chat", m.signals.chat_rate);
            sig(ui, "loud", m.signals.loudness);
            sig(ui, "lex", m.signals.lexicon);
            sig(ui, "arou", m.signals.arousal);
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
            if ui
                .add_enabled(!working, egui::Button::new("Promote → Frame & Render"))
                .clicked()
            {
                self.editor = None;
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
