//! yt-clipper - pure-Rust desktop shell (egui/eframe, ADR 0005).
//! M2: YouTube ingest end-to-end - import a URL (audio + chat + metadata),
//! pick a range, promote it to a Clip (padded Segment -> whisper -> stacked
//! Layout -> rolling-pop ASS -> NVENC export). A local file is also importable
//! for offline iteration. The pipeline runs on a background worker thread; the
//! UI polls it and stays responsive, and Cancel kills an in-flight download.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod pipeline;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use pipeline::{ImportSource, Job, Progress, Timeline};
use yc_core::{Language, Moment, Signals, TimeRange};
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
        font: paths.font(),
        workspace: paths.workspace.clone(),
    });

    // Headless one-shot for testing / visual iteration (no GUI clicking):
    //   yt-clipper --headless <url-or-file> <start_s> <end_s> [en|id|ja]
    // An http(s) target is imported as a YouTube URL, anything else as a local
    // file. Drives the same Import -> Promote worker path the GUI uses.
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
                    to_worker.send(Job::Promote { range }).expect("send promote");
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
                Ok(Progress::Done(_)) => {}
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
        Box::new(move |_cc| {
            Ok(Box::new(App {
                paths,
                deno_dir,
                url: String::new(),
                video_path: String::new(),
                start_s: 0.0,
                end_s: 30.0,
                language: Language::Id,
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

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Drain worker messages.
        while let Ok(msg) = self.from_worker.try_recv() {
            match msg {
                Progress::Stage(s) => self.status = Status::Working(s.to_string()),
                Progress::Imported { title, duration_s, analysis_wav } => {
                    self.imported = Some(ImportedInfo { title, duration_s });
                    self.analysis_wav = Some(analysis_wav);
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
                Progress::Done(p) => self.status = Status::Done(p),
                Progress::Cancelled => self.status = Status::Cancelled,
                Progress::Failed(e) => self.status = Status::Failed(e),
            }
        }
        let working = matches!(self.status, Status::Working(_));

        ui.heading("yt-clipper - M3 detection");

        // Preflight: are the sidecars / model / font present?
        ui.separator();
        let file_row = |ui: &mut egui::Ui, name: &str, path: &PathBuf| {
            let ok = path.exists();
            ui.horizontal(|ui| {
                ui.colored_label(
                    if ok { egui::Color32::GREEN } else { egui::Color32::RED },
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
                    let mut to_promote: Option<TimeRange> = None;
                    let mut to_select: Option<u64> = None;
                    let selected = self.selected;
                    let enabled = !working;
                    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                        egui::Grid::new("moments").striped(true).num_columns(9).show(ui, |ui| {
                            for h in ["#", "range", "score", "chat", "loud", "lex", "arou", "llm", ""] {
                                ui.label(h);
                            }
                            ui.end_row();
                            for m in &self.moments {
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
                                    to_promote = Some(m.range);
                                    to_select = Some(m.id);
                                }
                                ui.end_row();
                            }
                        });
                    });
                    if let Some(id) = to_select {
                        self.selected = Some(id);
                    }
                    if let Some(range) = to_promote {
                        let _ = self.to_worker.send(Job::Promote { range });
                        self.status = Status::Working("Starting".into());
                    }
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

        // --- Status + Cancel ---
        ui.separator();
        match &self.status {
            Status::Idle => {
                ui.label("Idle.");
            }
            Status::Working(stage) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(stage);
                    if ui.button("Cancel").clicked() {
                        self.cancel.cancel();
                    }
                });
            }
            Status::Done(path) => {
                ui.colored_label(egui::Color32::GREEN, format!("Done -> {}", path.display()));
            }
            Status::Cancelled => {
                ui.colored_label(egui::Color32::YELLOW, "Cancelled.");
            }
            Status::Failed(err) => {
                ui.colored_label(egui::Color32::RED, format!("Failed: {err}"));
            }
        }

        if working {
            ui.ctx().request_repaint();
        }
    }
}

impl App {
    fn start_import(&mut self, source: ImportSource) {
        self.imported = None;
        self.moments.clear();
        self.transcripts.clear();
        self.llm_reasons.clear();
        self.timeline = None;
        self.selected = None;
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
