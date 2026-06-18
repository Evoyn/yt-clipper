//! yt-clipper - pure-Rust desktop shell (egui/eframe, ADR 0005).
//! M2: YouTube ingest end-to-end - import a URL (audio + chat + metadata),
//! pick a range, promote it to a Clip (padded Segment -> whisper -> stacked
//! Layout -> rolling-pop ASS -> NVENC export). A local file is also importable
//! for offline iteration. The pipeline runs on a background worker thread; the
//! UI polls it and stays responsive, and Cancel kills an in-flight download.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod pipeline;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use pipeline::{ImportSource, Job, Progress};
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
                Ok(Progress::Imported { title, duration_s }) => {
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
                Ok(Progress::Imported { title, duration_s }) => {
                    tracing::info!("imported: {title} ({})", fmt_duration(duration_s));
                    to_worker.send(Job::Detect).expect("send detect");
                }
                Ok(Progress::Detected { moments }) => {
                    println!("detected {} moments:", moments.len());
                    for m in &moments {
                        println!(
                            "  #{:<2} {:>8}-{:<8} score {:5.2}  chat {} loud {} lex {}",
                            m.id,
                            fmt_clock(m.range.start_s),
                            fmt_clock(m.range.end_s),
                            m.score,
                            fmt_sig(m.signals.chat_rate),
                            fmt_sig(m.signals.loudness),
                            fmt_sig(m.signals.lexicon),
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
        let root = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        // In dev builds the exe sits in target/{debug,release}; walk up to the repo root.
        let root = if root.ends_with("debug") || root.ends_with("release") {
            root.ancestors().nth(2).map(PathBuf::from).unwrap_or(root)
        } else {
            root
        };
        Self {
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
                Progress::Imported { title, duration_s } => {
                    self.imported = Some(ImportedInfo { title, duration_s });
                    self.status = Status::Idle;
                }
                Progress::Detected { moments } => {
                    self.selected = moments.first().map(|m| m.id);
                    self.moments = moments;
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
                ui.add(egui::TextEdit::singleline(&mut self.video_path).desired_width(560.0));
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
                        egui::Grid::new("moments").striped(true).num_columns(7).show(ui, |ui| {
                            for h in ["#", "range", "score", "chat", "loud", "lex", ""] {
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
        self.selected = None;
        let _ = self.to_worker.send(Job::Import { source, language: self.language });
        self.status = Status::Working("Starting import".into());
    }
}
