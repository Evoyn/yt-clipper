//! yt-clipper — pure-Rust desktop shell (egui/eframe, ADR 0005).
//! M0: window opens, paths resolve, nothing else.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("yt-clipper"),
        ..Default::default()
    };

    eframe::run_native(
        "yt-clipper",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}

/// Resolved locations of everything the app needs on disk.
struct AppPaths {
    /// Pinned ffmpeg.exe / yt-dlp.exe live here (fetched by scripts/fetch-sidecars.ps1).
    sidecars: PathBuf,
    /// Whisper GGML and LLM GGUF model files.
    models: PathBuf,
    /// Per-VOD project folders.
    workspace: PathBuf,
}

impl AppPaths {
    /// Everything resolves relative to the executable for now: this is a
    /// solo-operator tool distributed as exe-plus-folders (ADR 0005).
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
            workspace: root.join("workspace"),
        }
    }

    fn ffmpeg(&self) -> PathBuf {
        self.sidecars.join("ffmpeg.exe")
    }

    fn ytdlp(&self) -> PathBuf {
        self.sidecars.join("yt-dlp.exe")
    }
}

struct App {
    paths: AppPaths,
}

impl App {
    fn new() -> Self {
        Self { paths: AppPaths::resolve() }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.heading("yt-clipper");
        ui.label("M0 skeleton — pipeline crates wired, nothing implemented yet.");
        ui.separator();

        let status = |ui: &mut egui::Ui, name: &str, path: &PathBuf| {
            let found = path.exists();
            let icon = if found { "✔" } else { "✘" };
            ui.label(format!("{icon} {name}: {}", path.display()));
        };
        status(ui, "ffmpeg", &self.paths.ffmpeg());
        status(ui, "yt-dlp", &self.paths.ytdlp());
        status(ui, "models dir", &self.paths.models);
        status(ui, "workspace dir", &self.paths.workspace);
    }
}
