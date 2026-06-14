//! yt-clipper — pure-Rust desktop shell (egui/eframe, ADR 0005).
//! M1 tracer bullet: local file -> audio extract -> whisper (picked range) ->
//! hardcoded stacked Layout -> rolling-pop ASS -> NVENC export. The pipeline
//! runs on a background worker thread; the UI polls it and stays responsive.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod pipeline;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use yc_core::TimeRange;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let paths = AppPaths::resolve();
    let (to_worker, from_worker) = pipeline::spawn(pipeline::PipelinePaths {
        ffmpeg: paths.ffmpeg(),
        model: paths.model(),
        font: paths.font(),
        workspace: paths.workspace.clone(),
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("yt-clipper"),
        ..Default::default()
    };

    eframe::run_native(
        "yt-clipper",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(App {
                paths,
                video_path: String::new(),
                start_s: 0.0,
                end_s: 30.0,
                status: Status::Idle,
                to_worker,
                from_worker,
            }))
        }),
    )
}

/// Resolved locations of everything the app needs on disk.
struct AppPaths {
    /// Pinned ffmpeg.exe / yt-dlp.exe (fetched by scripts/fetch-sidecars.ps1).
    sidecars: PathBuf,
    /// Whisper GGML and LLM GGUF model files (fetched by scripts/fetch-models.ps1).
    models: PathBuf,
    /// Bundled assets (fonts, …) shipped with the app.
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

    fn model(&self) -> PathBuf {
        self.models.join("ggml-large-v3.bin")
    }

    fn font(&self) -> PathBuf {
        self.assets.join("fonts").join("Anton-Regular.ttf")
    }
}

enum Status {
    Idle,
    Running(String),
    Done(PathBuf),
    Failed(String),
}

struct App {
    paths: AppPaths,
    video_path: String,
    start_s: f64,
    end_s: f64,
    status: Status,
    to_worker: Sender<pipeline::Job>,
    from_worker: Receiver<pipeline::Progress>,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Drain worker messages.
        while let Ok(msg) = self.from_worker.try_recv() {
            self.status = match msg {
                pipeline::Progress::Stage(s) => Status::Running(s.to_string()),
                pipeline::Progress::Done(p) => Status::Done(p),
                pipeline::Progress::Failed(e) => Status::Failed(e),
            };
        }

        ui.heading("yt-clipper — M1 tracer bullet");

        // Preflight: are the sidecar / model / font present?
        ui.separator();
        let row = |ui: &mut egui::Ui, name: &str, path: &PathBuf| {
            let ok = path.exists();
            ui.horizontal(|ui| {
                ui.colored_label(
                    if ok { egui::Color32::GREEN } else { egui::Color32::RED },
                    if ok { "✔" } else { "✘" },
                );
                ui.label(format!("{name}: {}", path.display()));
            });
        };
        row(ui, "ffmpeg", &self.paths.ffmpeg());
        row(ui, "whisper model", &self.paths.model());
        row(ui, "caption font", &self.paths.font());

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Video file:");
            ui.add(egui::TextEdit::singleline(&mut self.video_path).desired_width(640.0));
        });
        ui.horizontal(|ui| {
            ui.label("Start");
            ui.add(egui::DragValue::new(&mut self.start_s).speed(0.5).suffix(" s"));
            ui.label("End");
            ui.add(egui::DragValue::new(&mut self.end_s).speed(0.5).suffix(" s"));
            if self.end_s < self.start_s {
                ui.colored_label(egui::Color32::RED, "end < start");
            }
        });

        let running = matches!(self.status, Status::Running(_));
        ui.add_enabled_ui(!running, |ui| {
            if ui.button("Make clip").clicked() && !self.video_path.trim().is_empty() {
                let range = TimeRange { start_s: self.start_s, end_s: self.end_s };
                let _ = self.to_worker.send(pipeline::Job::Run {
                    video: PathBuf::from(self.video_path.trim()),
                    range,
                });
                self.status = Status::Running("Starting".into());
            }
        });

        ui.separator();
        match &self.status {
            Status::Idle => {
                ui.label("Idle — point at a local video, pick a range, Make clip.");
            }
            Status::Running(stage) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(stage);
                });
            }
            Status::Done(path) => {
                ui.colored_label(egui::Color32::GREEN, format!("Done → {}", path.display()));
            }
            Status::Failed(err) => {
                ui.colored_label(egui::Color32::RED, format!("Failed: {err}"));
            }
        }

        if running {
            ui.ctx().request_repaint();
        }
    }
}
