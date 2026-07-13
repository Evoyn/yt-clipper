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
mod settings;
mod theme;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

use pipeline::{ImportSource, Job, Progress, Timeline};
use yc_core::{
    CaptionEngine, CaptionGenre, CaptionStyle, Language, LayoutPref, Moment, NoConsole, Signals,
    TimeRange,
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
        voice_model: paths.voice_model(),
        yunet_model: paths.yunet_model(),
        sface_model: paths.sface_model(),
        tag_model: paths.tag_model(),
        tag_labels: paths.tag_labels(),
        sep_model: paths.sep_model(),
        deep_filter: paths.deep_filter(),
        mtmd_cli: paths.mtmd_cli(),
        qwen_model: paths.qwen_model(),
        qwen_mmproj: paths.qwen_mmproj(),
        align_model: paths.align_model(),
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
                Ok(Progress::Download { label, frac }) => {
                    tracing::info!("downloading {label}: {:.0}%", frac * 100.0)
                }
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
                            keep: None,
                            manual_captions: Vec::new(),
                            intro: None,
                            music: Vec::new(),
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
                Ok(Progress::Download { label, frac }) => {
                    tracing::info!("downloading {label}: {:.0}%", frac * 100.0)
                }
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
                Ok(Progress::Download { label, frac }) => {
                    tracing::info!("downloading {label}: {:.0}%", frac * 100.0)
                }
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
                            keep: None,
                            manual_captions: Vec::new(),
                            intro: None,
                            music: Vec::new(),
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
            .with_title("yt-clipper")
            .with_icon(window_icon()),
        ..Default::default()
    };

    // App-level operator preferences (workspace/settings.json): today the
    // master playback volume (feature plan #7) — playback only, never renders.
    let app_settings = settings::AppSettings::load(&paths.workspace);

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
                caption_engine: CaptionEngine::FOR_NEW_CREATORS,
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
                music_sinks: Vec::new(),
                prefs: app_settings,
                prefs_dirty_since: None,
                status: Status::Idle,
                download_frac: None,
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

    /// CAM++ speaker-embedding model for the voice lane (ADR 0042). Absent
    /// unless downloaded; the speaker analysis is then mouth-only.
    fn voice_model(&self) -> PathBuf {
        self.models.join("3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx")
    }

    /// YuNet landmark detector for the occupant map (ADR 0044). Absent unless
    /// downloaded; the voice join is then seat-scoped (ADR 0042 behavior).
    fn yunet_model(&self) -> PathBuf {
        self.models.join("face_detection_yunet_2023mar.onnx")
    }

    /// SFace face-recognition embedder for the occupant map (ADR 0044).
    /// Absent unless downloaded; the voice join is then seat-scoped.
    fn sface_model(&self) -> PathBuf {
        self.models.join("face_recognition_sface_2021dec.onnx")
    }

    /// Zipformer AudioSet event tagger for the shared-reaction mask (ADR
    /// 0045/0046). Absent unless downloaded; the split grammar then sees no
    /// laughter fuel (reaction splits simply never arm).
    fn tag_model(&self) -> PathBuf {
        self.models.join("sherpa-onnx-zipformer-audio-tagging-2024-04-09.onnx")
    }

    /// The tagger's AudioSet class-label CSV (index → display name) — the
    /// laughter family is looked up by display name, so the lane needs both
    /// files.
    fn tag_labels(&self) -> PathBuf {
        self.models.join("audioset_class_labels_indices.csv")
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

    /// llama.cpp's multimodal CLI for the Qwen ensemble Caption engine (ADR
    /// 0034), pinned under `sidecars/llama/` (fetch-llama-sidecar.ps1).
    fn mtmd_cli(&self) -> PathBuf {
        self.sidecars.join("llama").join("llama-mtmd-cli.exe")
    }

    /// Qwen3-ASR GGUF pair for the ensemble Caption engine (fetch-models.ps1).
    fn qwen_model(&self) -> PathBuf {
        self.models.join("Qwen3-ASR-1.7B-Q8_0.gguf")
    }

    fn qwen_mmproj(&self) -> PathBuf {
        self.models.join("mmproj-Qwen3-ASR-1.7B-Q8_0.gguf")
    }

    /// wav2vec2-CTC forced-alignment model DIR (`model.onnx` + `vocab.json`,
    /// fetch-models.ps1) — the ensemble's default timing skeleton on an
    /// `align` build (ADR 0054/0055; `YC_FORCED_ALIGN=0` is the off-switch).
    /// Absent: DTW timing stands.
    fn align_model(&self) -> PathBuf {
        self.models.join("w2v2-align-id")
    }

    /// Silero VAD ggml (whisper.cpp's pinned release) for the `YC_VAD=1` trial
    /// decode knob (ADR 0033). yc-transcribe resolves it beside the whisper
    /// model itself; this accessor exists for the Diagnostics registry.
    fn silero_model(&self) -> PathBuf {
        self.models.join("ggml-silero-v5.1.2.bin")
    }

    /// Directory of per-language dialect/slang stores (`assets/dialect/<lang>.json`).
    fn dialect_dir(&self) -> PathBuf {
        self.assets.join("dialect")
    }

    fn font(&self) -> PathBuf {
        self.assets.join("fonts").join("Anton-Regular.ttf")
    }
}

/// The window/taskbar icon (brand pass): the "Punch-out" mark as raw 64x64
/// RGBA, generated by `scripts/render-brand.ps1` and embedded raw so no image
/// decoder ships. The exe's own icon is the matching multi-res .ico
/// (`crates/app/build.rs`).
fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../../../assets/branding/window-icon-64.rgba").to_vec(),
        width: 64,
        height: 64,
    }
}

/// One Diagnostics dependency: a tool or model the pipeline resolves. Every
/// external binary/model the app can use is registered in
/// [`App::dependency_registry`] so the Diagnostics section can never drift
/// from what the code actually loads — add new AI tools/models THERE, nowhere
/// else. The planned per-row Download action hangs off this same table.
struct Dep {
    /// Core flows (import / detect / caption / render) fail without it;
    /// `false` = an optional capability degrades or is skipped instead.
    required: bool,
    name: &'static str,
    path: PathBuf,
    /// What it powers — the hover line beside the resolved path.
    role: &'static str,
    /// Ids into [`App::download_specs`] that materialize this row (ADR 0041).
    /// Usually one; the llama sidecar needs two archives; empty = not
    /// downloadable in-app (`yc-llm-judge.exe` ships beside the app exe).
    downloads: &'static [&'static str],
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
    /// The Music track's preview sinks (ADR 0068): one per cue, each with its
    /// clip's gain (sink volume = prefs.volume × gain, kept live under the
    /// slider). `stop_audio` clears them all with the voice.
    music_sinks: Vec<(rodio::Sink, f32)>,
    /// App-level operator preferences (workspace/settings.json): the master
    /// playback gain (Moment review + Studio playback share it; 1.0 =
    /// unmodified, up to 2.0 boosts a quiet streamer — playback only, never
    /// the export) and the operator's saved caption presets.
    prefs: settings::AppSettings,
    /// When the operator last changed a preference not yet written to
    /// settings.json — debounces the save off slider drags.
    prefs_dirty_since: Option<std::time::Instant>,
    status: Status,
    /// Fraction (0..=1) of the in-flight dependency download (ADR 0041);
    /// `None` outside a download. Drives the status bar's progress bar.
    download_frac: Option<f32>,
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
/// and egui honours the soonest request — drawing it would pin the wgpu loop
/// at flat-out repaint instead of the app's own 16 ms cadence. This paints the
/// same arc (radius/points/stroke copied from egui 0.34 `Spinner::paint_at`)
/// and lets the app's `request_repaint_after` drive the animation.
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

/// Open a file with its Windows association (the default player, for a Short):
/// `explorer <file>` is ShellExecute-equivalent and never shows a console.
fn open_in_shell(path: &Path) {
    let _ = std::process::Command::new("explorer").no_console().arg(path).spawn();
}

/// Reveal a file in Explorer with the file pre-selected. `/select,` and the
/// path travel as ONE argument (Explorer's own parsing).
fn reveal_in_explorer(path: &Path) {
    let mut select = std::ffi::OsString::from("/select,");
    select.push(path);
    let _ = std::process::Command::new("explorer").no_console().arg(select).spawn();
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
                Progress::Stage(s) => {
                    self.status = Status::Working(s.to_string());
                    self.download_frac = None;
                }
                Progress::Download { label, frac } => {
                    self.status = Status::Working(format!("Downloading {label}"));
                    self.download_frac = Some(frac);
                }
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
                    // genre, an UNKNOWN Creator always RESETS the picker (a
                    // previous session's pick must not leak onto a new
                    // Creator): to the ensemble, the new-Creator default the
                    // operator ruled (ADR 0061). A saved engine still wins.
                    self.caption_engine = caption_engine.unwrap_or(CaptionEngine::FOR_NEW_CREATORS);
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
                    src_fps,
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
                            keep: None,
                            manual_captions: Vec::new(),
                            intro: None,
                            music: Vec::new(),
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
                            self.paths.ffprobe(),
                            render_src,
                            seek_s,
                            src_fps,
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
                Progress::Speakers { analysis, plan, voice_note } => {
                    if let Some(ed) = &mut self.editor {
                        ed.set_speakers(analysis, plan, voice_note);
                    }
                }
                Progress::JobDone => {
                    self.status = Status::Idle;
                    self.download_frac = None;
                    // A finished download can materialize the deno sidecar; the
                    // row reads this resolved dir, so refresh it (cheap).
                    self.deno_dir = self.paths.deno_dir();
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
                    self.download_frac = None;
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
                    self.download_frac = None;
                }
            }
        }
        let working = matches!(self.status, Status::Working(_));

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
                    theme::brand_mark(ui, 27.0);
                    ui.add_space(4.0);
                    ui.heading(egui::RichText::new("YT CLIPPER").color(theme::GOLD).size(24.0));
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
        let prefs_before = self.prefs.clone();
        if self.editor.is_some() {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                if let Some(ed) = &mut self.editor {
                    editor_action = ed.show(ui, working, self.rendering, &mut self.prefs);
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
                        self.ui_preflight(ui, working);
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
                    keep: spec.keep,
                    manual_captions: spec.manual_captions,
                    intro: spec.intro,
                    music: spec.music,
                });
                self.rendering = true;
                // While a pre-pass job still runs, this Render only QUEUES
                // behind it on the serial worker — say so instead of claiming
                // NVENC is running (the 2026-07-03 report: "Rendering" shown
                // over a crawling ensemble pre-pass). The render's own Stage
                // messages take the label over once it actually starts.
                self.status = Status::Working(if matches!(self.status, Status::Working(_)) {
                    "Render queued — waiting for captions".into()
                } else {
                    "Rendering".into()
                });
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
            // Editor playback (ADR 0036): the voice is the same sink the
            // Moment review uses, sliced from the whole-VOD analysis wav at
            // the clip offset; music cues each get their own sink (ADR 0068).
            // ONE restart message stops everything, then starts what the
            // editor computed — never two sinks of the same clip.
            editor::EditorAction::Play { voice, music } => {
                self.stop_audio();
                if let Some(range) = voice {
                    self.play_range(range);
                }
                self.play_music_cues(music);
            }
            editor::EditorAction::StopAudio => self.stop_audio(),
            editor::EditorAction::None => {}
        }

        // The Studio changed a preference (volume slider, saved caption
        // presets): apply volume to the live sink immediately (real-time
        // adjustment) and schedule the settings write.
        if self.prefs != prefs_before {
            if let Some(sink) = &self.sink {
                sink.set_volume(self.prefs.volume);
            }
            // Music sinks track the same master, scaled by their clip gain
            // (ADR 0068) — real-time, playback only, never the export.
            for (sink, gain) in &self.music_sinks {
                sink.set_volume(self.prefs.volume * gain);
            }
            self.prefs_dirty_since = Some(std::time::Instant::now());
        }
        // Debounced settings write: once the prefs have rested ~0.7 s (a write
        // per slider frame would hammer the disk for nothing). The repaint
        // request guarantees the flush runs even if the app then idles;
        // `save` (below) flushes on exit for the close-immediately case.
        if let Some(t0) = self.prefs_dirty_since {
            if t0.elapsed().as_secs_f32() > 0.7 {
                self.prefs.save(&self.paths.workspace);
                self.prefs_dirty_since = None;
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(750));
            }
        }

        // (Status lives in the top brand bar now — see `status_bar`.)

        // Repaint cadence while a job runs: a steady 60 fps tick (16 ms,
        // vsync-capped) for EVERY stage, GPU ones included - the operator
        // chose fluid spinners over the June-19 contention protection
        // (2026-07-03; git history keeps the 10 fps `stage_is_gpu` throttle
        // if whisper ever crawls again). This only holds because nothing else
        // requests an immediate repaint while working - egui's stock
        // `ui.spinner()` does exactly that every frame; the status bar draws
        // [`throttled_spinner`] instead.
        if working {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    /// eframe's periodic/exit persistence hook: flush a preference change the
    /// debounce hasn't written yet (the operator drags the slider and closes
    /// the window inside the debounce window).
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        if self.prefs_dirty_since.take().is_some() {
            self.prefs.save(&self.paths.workspace);
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
                // A dependency download knows its size (the pin), so it gets a
                // real bar (ADR 0041); pipeline stages keep the spinner.
                if let Some(frac) = self.download_frac {
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .desired_width(140.0)
                            .show_percentage(),
                    );
                }
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
                // The post-export step is always "watch it": the Short is one
                // click (or Ctrl+O) away, its folder one more.
                let open = ui
                    .button("Open")
                    .on_hover_text("Play the exported Short (Ctrl+O)")
                    .clicked()
                    || ui.input_mut(|i| {
                        i.consume_shortcut(&egui::KeyboardShortcut::new(
                            egui::Modifiers::CTRL,
                            egui::Key::O,
                        ))
                    });
                if open {
                    open_in_shell(path);
                }
                if ui
                    .button("Folder")
                    .on_hover_text("Reveal the exported Short in Explorer")
                    .clicked()
                {
                    reveal_in_explorer(path);
                }
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

    /// Every tool/model the pipeline can resolve, required-first — THE single
    /// registration point for the Diagnostics section (operator's ask,
    /// 2026-07-03: the page must show new AI tools without a separate list to
    /// remember). deno is the one non-file dependency and is handled beside
    /// these in [`Self::ui_preflight`].
    fn dependency_registry(&self) -> Vec<Dep> {
        let p = &self.paths;
        let dep = |required, name, path, role, downloads| Dep { required, name, path, role, downloads };
        vec![
            dep(true, "ffmpeg", p.ffmpeg(), "audio/video decode + every render", &["ffmpeg"][..]),
            dep(true, "ffprobe", p.ffprobe(), "stream probing at import/promote", &["ffmpeg"]),
            dep(true, "yt-dlp", p.ytdlp(), "YouTube VOD audio + Segment fetch", &["yt-dlp"]),
            dep(
                true,
                "whisper model",
                p.model(),
                "transcription (captions + detect refine) (~3 GB)",
                &["whisper-model"],
            ),
            dep(true, "caption font", p.font(), "the burned caption face (Anton)", &["caption-font"]),
            dep(
                false,
                "LLM judge",
                p.llm_judge(),
                "LLM judgment Signal + caption correction; detect renormalizes without it \
                 (built with the app - reinstall to restore)",
                &[],
            ),
            dep(
                false,
                "judge model",
                p.llm_model(),
                "the judge's Qwen2.5-7B GGUF (~5.4 GB)",
                &["judge-model"],
            ),
            dep(
                false,
                "Qwen3-ASR model",
                p.qwen_model(),
                "ensemble Caption engine voter (ADR 0034) (~2.2 GB)",
                &["qwen3-asr-model"],
            ),
            dep(
                false,
                "Qwen3-ASR mmproj",
                p.qwen_mmproj(),
                "the ensemble voter's audio projector (~360 MB)",
                &["qwen3-asr-mmproj"],
            ),
            dep(
                false,
                "llama-mtmd-cli",
                p.mtmd_cli(),
                "ensemble Caption engine decoder (pinned llama.cpp sidecar, ~550 MB with CUDA runtime)",
                &["llama-bin", "llama-cudart"],
            ),
            dep(
                false,
                "deep-filter",
                p.deep_filter(),
                "ensemble denoise variants + Cleaned-voice captions (ADR 0029)",
                &["deep-filter"],
            ),
            dep(
                false,
                "Silero VAD",
                p.silero_model(),
                "YC_VAD=1 trial decode knob (ADR 0033)",
                &["silero-vad"],
            ),
            dep(
                false,
                "face model",
                p.face_model(),
                "facecam auto-framing + podcast Speaker tracks (face builds)",
                &["face-model"],
            ),
            dep(
                false,
                "voice model",
                p.voice_model(),
                "voice lane: speaker diarization beside the mouth lane (ADR 0042)",
                &["voice-model"],
            ),
            dep(
                false,
                "YuNet landmarks",
                p.yunet_model(),
                "occupant map: face landmarks for the person-scoped voice join (ADR 0044)",
                &["yunet-model"],
            ),
            dep(
                false,
                "SFace embedder",
                p.sface_model(),
                "occupant map: face identity per (camera, seat) (ADR 0044) (~37 MB)",
                &["sface-model"],
            ),
            dep(
                false,
                "reaction tagger",
                p.tag_model(),
                "Shared reaction: laughter-class mask feeding the split grammar (ADR 0045/0046) (~260 MB)",
                &["tagger-model"],
            ),
            dep(
                false,
                "AudioSet labels",
                p.tag_labels(),
                "the reaction tagger's class names (laughter family lookup)",
                &["tagger-labels"],
            ),
            dep(
                false,
                "SER model",
                p.ser_model(),
                "arousal Signal (ser builds, ADR 0008) (~610 MB)",
                &["ser-model"],
            ),
        ]
    }

    /// The pinned download table (ADR 0041): every fetchable dependency's
    /// official source, version-stable URL, expected SHA-256, and install
    /// shape. Pins mirror `scripts/fetch-*.ps1`; the hashes were taken from
    /// the upstream release/LFS metadata and verified byte-identical against
    /// the operator's working set on 2026-07-03. A pin that rots (gyan.dev
    /// rotates versioned packages out; HF revisions never rot) fails the
    /// SHA/404 check loudly — bump the URL *and* hash together, here only.
    fn download_specs(&self) -> Vec<pipeline::DownloadSpec> {
        use pipeline::{DownloadSpec, Install};
        let p = &self.paths;
        vec![
            DownloadSpec {
                id: "ffmpeg",
                label: "ffmpeg + ffprobe",
                url: "https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-8.1.2-essentials_build.zip",
                sha256: "db580001caa24ac104c8cb856cd113a87b0a443f7bdf47d8c12b1d740584a2ec",
                total_bytes: 109_728_040,
                install: Install::Unzip {
                    picks: vec![
                        ("/bin/ffmpeg.exe", p.ffmpeg()),
                        ("/bin/ffprobe.exe", p.ffprobe()),
                    ],
                    dll_sweep_to: None,
                },
            },
            DownloadSpec {
                id: "yt-dlp",
                label: "yt-dlp",
                // 2026.07.04: bumped the day YouTube cut off web_safari HLS
                // (ADR 0059) — in the SABR arms race a month-old extractor is
                // a liability; the segment fetch was re-validated on this
                // exact build.
                url: "https://github.com/yt-dlp/yt-dlp/releases/download/2026.07.04/yt-dlp.exe",
                sha256: "52fe3c26dcf71fbdc85b528589020bb0b8e383155cfa81b64dd447bbe35e24b8",
                total_bytes: 18_226_085,
                install: Install::File(p.ytdlp()),
            },
            DownloadSpec {
                id: "deno",
                label: "deno (yt-dlp's JS runtime)",
                url: "https://github.com/denoland/deno/releases/download/v2.9.1/deno-x86_64-pc-windows-msvc.zip",
                sha256: "ab310b4232cca207d40ffa41867e93aaf9f893802bc76756e74f486a6b21b371",
                total_bytes: 42_707_941,
                install: Install::Unzip {
                    picks: vec![("deno.exe", p.sidecars.join("deno.exe"))],
                    dll_sweep_to: None,
                },
            },
            DownloadSpec {
                id: "whisper-model",
                label: "whisper large-v3 model",
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-large-v3.bin",
                sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
                total_bytes: 3_095_033_483,
                install: Install::File(p.model()),
            },
            DownloadSpec {
                id: "caption-font",
                label: "Anton caption font",
                url: "https://raw.githubusercontent.com/google/fonts/e0a8124cf36bb7c32ca68e5d46d6acdbc3df866a/ofl/anton/Anton-Regular.ttf",
                sha256: "a4ba3a92350ebb031da0cb47630ac49eb265082ca1bc0450442f4a83ab947cab",
                total_bytes: 170_812,
                install: Install::File(p.font()),
            },
            DownloadSpec {
                id: "judge-model",
                label: "judge model (Qwen2.5-7B Q5_K_M)",
                url: "https://huggingface.co/bartowski/Qwen2.5-7B-Instruct-GGUF/resolve/8911e8a47f92bac19d6f5c64a2e2095bd2f7d031/Qwen2.5-7B-Instruct-Q5_K_M.gguf",
                sha256: "2e998d7e181c8756c5ffc55231b9ee1cdc9d3acec4245d6e27d32bd8e738c474",
                total_bytes: 5_444_831_936,
                install: Install::File(p.llm_model()),
            },
            DownloadSpec {
                id: "qwen3-asr-model",
                label: "Qwen3-ASR model",
                url: "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/36a678687ba7d07a74ca70ccb0e36902e005fb80/Qwen3-ASR-1.7B-Q8_0.gguf",
                sha256: "58e22d0532d4eacaf034cfac17a6fed159f37c41390c710186783be439d1fc57",
                total_bytes: 2_165_034_944,
                install: Install::File(p.qwen_model()),
            },
            DownloadSpec {
                id: "qwen3-asr-mmproj",
                label: "Qwen3-ASR mmproj",
                url: "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/36a678687ba7d07a74ca70ccb0e36902e005fb80/mmproj-Qwen3-ASR-1.7B-Q8_0.gguf",
                sha256: "46c1d533af3f354ceb37ce855dbceff7da7fa7cf1e6a523df3b13440bd164c0d",
                total_bytes: 355_709_344,
                install: Install::File(p.qwen_mmproj()),
            },
            DownloadSpec {
                id: "llama-bin",
                label: "llama.cpp b9859 binaries",
                url: "https://github.com/ggml-org/llama.cpp/releases/download/b9859/llama-b9859-bin-win-cuda-13.3-x64.zip",
                sha256: "5bab577d1ac05f049b80489ec3db37ec544bbf9b50793a7842b1f4fed1f9bbd1",
                total_bytes: 161_363_100,
                install: Install::Unzip {
                    picks: vec![
                        ("llama-mtmd-cli.exe", p.mtmd_cli()),
                        ("llama-server.exe", p.sidecars.join("llama").join("llama-server.exe")),
                    ],
                    dll_sweep_to: Some(p.sidecars.join("llama")),
                },
            },
            DownloadSpec {
                id: "llama-cudart",
                label: "llama.cpp CUDA runtime",
                url: "https://github.com/ggml-org/llama.cpp/releases/download/b9859/cudart-llama-bin-win-cuda-13.3-x64.zip",
                sha256: "1462a050eb4c684921ba51dcc4cc488a036674c3e73e9945ee705b854808d03e",
                total_bytes: 390_970_417,
                install: Install::Unzip {
                    picks: Vec::new(),
                    dll_sweep_to: Some(p.sidecars.join("llama")),
                },
            },
            DownloadSpec {
                id: "deep-filter",
                label: "deep-filter",
                url: "https://github.com/Rikorose/DeepFilterNet/releases/download/v0.5.6/deep-filter-0.5.6-x86_64-pc-windows-msvc.exe",
                sha256: "75e11fa16445f560cb6b021521ddb89e89270d13b83089705d98776f58fd7915",
                total_bytes: 26_912_256,
                install: Install::File(p.deep_filter()),
            },
            DownloadSpec {
                id: "silero-vad",
                label: "Silero VAD model",
                url: "https://huggingface.co/ggml-org/whisper-vad/resolve/9ffd54a1e1ee413ddf265af9913beaf518d1639b/ggml-silero-v5.1.2.bin",
                sha256: "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf",
                total_bytes: 885_098,
                install: Install::File(p.silero_model()),
            },
            DownloadSpec {
                id: "face-model",
                label: "face model (Ultraface RFB-320)",
                url: "https://raw.githubusercontent.com/Linzaer/Ultra-Light-Fast-Generic-Face-Detector-1MB/0f9ca4a9fc80170fd505168fd1132b837141f7df/models/onnx/version-RFB-320.onnx",
                sha256: "34cd7e60aeff28744c657de7a3dc64e872d506741de66987f3426f2b79f88017",
                total_bytes: 1_270_727,
                install: Install::File(p.face_model()),
            },
            DownloadSpec {
                id: "voice-model",
                label: "voice model (CAM++ speaker embedding)",
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx",
                sha256: "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2",
                total_bytes: 28_281_164,
                install: Install::File(p.voice_model()),
            },
            // The OpenCV-zoo face-id pair for the occupant map (ADR 0043/0044):
            // pinned at the zoo commits the spike verified; the SHA-256s are the
            // repo's own Git-LFS oids, re-verified against these exact URLs
            // 2026-07-06.
            DownloadSpec {
                id: "yunet-model",
                label: "YuNet face landmarks (occupant map)",
                url: "https://media.githubusercontent.com/media/opencv/opencv_zoo/f12e12798e8314f7c074a6656816c048dcc95b7a/models/face_detection_yunet/face_detection_yunet_2023mar.onnx",
                sha256: "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
                total_bytes: 232_589,
                install: Install::File(p.yunet_model()),
            },
            DownloadSpec {
                id: "sface-model",
                label: "SFace face embedder (occupant map)",
                url: "https://media.githubusercontent.com/media/opencv/opencv_zoo/ba91a3b91d00d76e86540d4013f944bd6b514e39/models/face_recognition_sface/face_recognition_sface_2021dec.onnx",
                sha256: "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79",
                total_bytes: 38_696_353,
                install: Install::File(p.sface_model()),
            },
            // The shared-reaction tagger pair (ADR 0045/0046): k2-fsa's OWN
            // HuggingFace mirror at a pinned revision, chosen over the GitHub
            // release asset because that tag is a LIVING release (assets
            // re-uploaded in place — its checksum.txt was already stale once,
            // ADR 0045) and ships only .tar.bz2; the HF revision URL is
            // immutable and serves the bare files. Both hashes verified
            // byte-identical to the gate-passing install on 2026-07-07.
            DownloadSpec {
                id: "tagger-model",
                label: "reaction tagger (Zipformer audio tags)",
                url: "https://huggingface.co/k2-fsa/sherpa-onnx-zipformer-audio-tagging-2024-04-09/resolve/3c795f58cd1fe15a42cee103519e7a5cbbd93415/model.onnx",
                sha256: "a8f11014905fbaab81644514b79e719f3fcfa3ad45d29a25b46e34eb03c48ed8",
                total_bytes: 259_079_136,
                install: Install::File(p.tag_model()),
            },
            DownloadSpec {
                id: "tagger-labels",
                label: "AudioSet class labels",
                url: "https://huggingface.co/k2-fsa/sherpa-onnx-zipformer-audio-tagging-2024-04-09/resolve/3c795f58cd1fe15a42cee103519e7a5cbbd93415/class_labels_indices.csv",
                sha256: "cdd1049833c4b86127c2773ac0d14a2754b6a6d0d1798002ed5c66e699708429",
                total_bytes: 14_675,
                install: Install::File(p.tag_labels()),
            },
            DownloadSpec {
                id: "ser-model",
                label: "SER model (audeering w2v2)",
                url: "https://zenodo.org/records/6221127/files/w2v2-L-robust-12.6bc4a7fd-1.1.0.zip?download=1",
                sha256: "01813761eff4a74f9b809ca966224d26b3bd6064878bf4f6ade4918177b5e777",
                total_bytes: 609_889_780,
                install: Install::Unzip {
                    picks: vec![
                        ("model.onnx", p.ser_model()),
                        ("model.yaml", p.models.join("w2v2-emotion").join("model.yaml")),
                    ],
                    dll_sweep_to: None,
                },
            },
        ]
    }

    /// Left-rail section: the preflight tool/model check, collapsed once every
    /// REQUIRED sidecar + model is present (auto-expanded when one is missing).
    /// Rows show only the name + a status dot (the operator's ask) — the
    /// resolved path + role are hover detail, not a wall of directories.
    /// Optional rows (feature models, alternate engines) show a neutral dot
    /// when absent: nothing is wrong, the capability just isn't installed.
    fn ui_preflight(&mut self, ui: &mut egui::Ui, working: bool) {
        // Per-row Download clicks accumulate spec ids here (ADR 0041); the job
        // is sent once, after the section closes.
        let mut clicked: Vec<&'static str> = Vec::new();
        let row = |ui: &mut egui::Ui,
                   clicked: &mut Vec<&'static str>,
                   ok: bool,
                   required: bool,
                   name: &str,
                   detail: String,
                   downloads: &'static [&'static str]| {
            let resp = ui
                .horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    let color = match (ok, required) {
                        (true, _) => theme::OK,
                        (false, true) => theme::ERR,
                        (false, false) => egui::Color32::from_gray(0x60),
                    };
                    ui.painter().circle_filled(rect.center(), 4.0, color);
                    ui.label(name);
                    if !ok {
                        if required {
                            ui.colored_label(theme::ERR, "missing");
                        } else {
                            ui.weak("not installed");
                        }
                        if !downloads.is_empty()
                            && ui
                                .add_enabled(!working, egui::Button::new("Download").small())
                                .on_hover_text("Fetch from the pinned official source (ADR 0041)")
                                .clicked()
                        {
                            clicked.extend_from_slice(downloads);
                        }
                    }
                })
                .response;
            resp.on_hover_text(detail);
        };
        let deps = self.dependency_registry();
        let deno_ok = self.deno_dir.is_some();
        let all_ok =
            deno_ok && deps.iter().filter(|d| d.required).all(|d| d.path.exists());
        let header = if all_ok {
            "Diagnostics - all tools ready"
        } else {
            "Diagnostics - something is missing"
        };
        let debug_open = std::env::var("YC_DIAG_OPEN").is_ok(); // capture aid
        egui::CollapsingHeader::new(header).default_open(!all_ok || debug_open).show(ui, |ui| {
            for d in deps.iter().filter(|d| d.required) {
                row(
                    ui,
                    &mut clicked,
                    d.path.exists(),
                    true,
                    d.name,
                    format!("{} · {}", d.role, d.path.display()),
                    d.downloads,
                );
            }
            // deno resolves as a directory (sidecar or winget), not a file.
            match &self.deno_dir {
                Some(dir) => row(
                    ui,
                    &mut clicked,
                    true,
                    true,
                    "deno",
                    format!("yt-dlp's JS runtime (nsig) · {}", dir.display()),
                    &[],
                ),
                None => row(
                    ui,
                    &mut clicked,
                    false,
                    true,
                    "deno",
                    "yt-dlp's JS runtime (nsig) - or `winget install DenoLand.Deno`".into(),
                    &["deno"],
                ),
            }
            ui.add_space(4.0);
            ui.weak("Optional (a capability degrades gracefully when absent):");
            for d in deps.iter().filter(|d| !d.required) {
                row(
                    ui,
                    &mut clicked,
                    d.path.exists(),
                    false,
                    d.name,
                    format!("{} · {}", d.role, d.path.display()),
                    d.downloads,
                );
            }
            // Everything missing in one go, sized so the operator knows what
            // they are agreeing to before a multi-GB fetch starts.
            let mut missing: Vec<&'static str> = deps
                .iter()
                .filter(|d| !d.path.exists())
                .flat_map(|d| d.downloads.iter().copied())
                .collect();
            if !deno_ok {
                missing.push("deno");
            }
            missing.dedup();
            if !missing.is_empty() {
                let total: u64 = self
                    .download_specs()
                    .iter()
                    .filter(|s| missing.contains(&s.id))
                    .map(|s| s.total_bytes)
                    .sum();
                ui.add_space(4.0);
                if ui
                    .add_enabled(
                        !working,
                        egui::Button::new(format!(
                            "Download all missing ({:.1} GB)",
                            total as f64 / 1e9
                        )),
                    )
                    .clicked()
                {
                    clicked.extend(missing);
                }
            }
        });
        if !clicked.is_empty() {
            clicked.sort();
            clicked.dedup();
            let specs: Vec<pipeline::DownloadSpec> = self
                .download_specs()
                .into_iter()
                .filter(|s| clicked.contains(&s.id))
                .collect();
            let _ = self.to_worker.send(Job::Download { specs });
            self.status = Status::Working("Starting downloads".into());
        }
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
                    egui::Slider::new(&mut self.max_clip_s, 45.0..=180.0)
                        .step_by(5.0)
                        .suffix(" s"),
                )
                .on_hover_text(
                    "Ceiling for a detected Moment. Clips start and end on sentence \
                     boundaries and aim for at least 45 s of real speech (shorter when \
                     speech runs dry - never padded with dead air); 180 s is the \
                     YouTube Shorts maximum. Manually marked Moments are untouched.",
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
            ui.weak("No Moments yet - Detect, or add one manually.");
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
                    ui.weak("No caption to-dos - this Creator's captions are clean, or none harvested yet.");
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
                                .on_hover_text("Tick for a real word the streamer means as slang or a name - routes through the LLM pass in context, not the always-on global dict (ADR 0030).");
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
                            Ok(k) => format!("Saved - {k} confirmed correction(s)."),
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
                step(ui, "1", "Import a VOD - a YouTube URL or a local file");
                step(ui, "2", "Detect Moments - AI ranks the clip-worthy spans");
                step(ui, "3", "Open in editor - frame, caption, and export the Short");
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
                    v.map(|x| format!("{x:+.1}")).unwrap_or_else(|| "-".into())
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
                .add(egui::Slider::new(&mut self.prefs.volume, 0.0..=2.0).show_value(false))
                .on_hover_text(format!(
                    "Playback volume {:.0}% — persists; never affects the export",
                    self.prefs.volume * 100.0
                ))
                .changed()
            {
                if let Some(sink) = &self.sink {
                    sink.set_volume(self.prefs.volume);
                }
                self.prefs_dirty_since = Some(std::time::Instant::now());
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
                ui.weak("(manual Moment - run Detect to transcribe)");
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
                sink.set_volume(self.prefs.volume);
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
        for (sink, _) in self.music_sinks.drain(..) {
            sink.stop();
        }
    }

    /// Spawn one sink per music cue (ADR 0068): a slice of the editor's
    /// cached PCM, delayed to its entrance (`Source::delay`), at
    /// `prefs.volume × gain`. The caller stopped every sink first — the
    /// restart contract, so a scrub can never double-start a clip.
    fn play_music_cues(&mut self, cues: Vec<editor::MusicCue>) {
        if cues.is_empty() {
            return;
        }
        let Some(handle) = self.ensure_audio().cloned() else { return };
        for cue in cues {
            let end = (cue.first + cue.len).min(cue.samples.len());
            if cue.first >= end {
                continue;
            }
            match rodio::Sink::try_new(&handle) {
                Ok(sink) => {
                    use rodio::Source;
                    let buf = rodio::buffer::SamplesBuffer::new(
                        cue.ch,
                        cue.sr,
                        cue.samples[cue.first..end].to_vec(),
                    );
                    sink.append(buf.delay(std::time::Duration::from_secs_f64(
                        cue.delay_s.max(0.0),
                    )));
                    sink.set_volume(self.prefs.volume * cue.gain);
                    sink.play();
                    self.music_sinks.push((sink, cue.gain));
                }
                Err(e) => tracing::warn!("music sink: {e}"),
            }
        }
    }
}
