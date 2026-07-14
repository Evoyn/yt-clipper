//! Diag: closing the app mid-render must not ORPHAN the ffmpeg/NVENC child
//! (review-fixes slice 1, 2026-07-14). Reproduces the exit path's kill chain on
//! the REAL production functions — `yc_render::run_export` driven by a
//! `yc_ingest::CancelToken`, exactly as `pipeline::do_render` wires them — and
//! then performs the EXACT sequence `App::on_exit` performs on a window close:
//! flip the token, then wait a bounded ~2 s for the worker to land its kill.
//!
//! Before the fix, `on_exit` did not exist: the process died with the token
//! never flipped, so the export child kept encoding headless. The bar here is
//! the one from the nextprompt: no `ffmpeg.exe` survivor afterwards.
//!
//! (The other half — that a titlebar-X actually REACHES `on_exit` — is proven
//! on the real GUI: closing the window logs "window closed: cancelling the
//! in-flight job before exit" and exits 0. This harness proves what that hook
//! then accomplishes against a live child.)
//!
//!   cargo run -p yt-clipper --example exit_kill_diag -- [<segment.mp4>]
//!
//! Run from the repo root (finds `sidecars\ffmpeg.exe`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Live `ffmpeg.exe` count (Windows `tasklist`; the survivor check the
/// nextprompt's bar names).
fn ffmpeg_count() -> usize {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq ffmpeg.exe", "/NH"])
        .output()
        .expect("tasklist");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.to_lowercase().contains("ffmpeg.exe"))
        .count()
}

fn main() -> anyhow::Result<()> {
    let ffmpeg = PathBuf::from("sidecars/ffmpeg.exe");
    anyhow::ensure!(ffmpeg.is_file(), "run from the repo root (sidecars\\ffmpeg.exe not found)");
    let src = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(
            "workspace/tvOneNews/Pakar Ungkap Skema IPO 'Pura-pura' di Indonesia _ IBF tvOne/data/segment.mp4",
        )
    });
    anyhow::ensure!(src.is_file(), "source segment missing: {}", src.display());
    // `run_export` runs ffmpeg with `workdir` as its cwd (the ASS/fontsdir
    // contract), so the input must be absolute.
    let src = src.canonicalize()?;
    let ffmpeg = ffmpeg.canonicalize()?;
    let workdir = std::env::temp_dir().join("yc-exit-kill-diag");
    std::fs::create_dir_all(&workdir)?;

    anyhow::ensure!(
        ffmpeg_count() == 0,
        "a stray ffmpeg.exe is already running — clear it first, or this check lies"
    );

    // A production-shaped export: the same `run_export` the render path calls
    // (pipeline.rs), on the real Segment, encoding to the vertical canvas — slow
    // enough (CPU x264 preset, no NVENC so it is comparable on any box) that it
    // is unmistakably mid-flight when the "window closes".
    let args: Vec<String> = [
        "-hide_banner",
        "-i",
        src.to_str().expect("utf-8 path"),
        "-vf",
        "scale=1080:1920:force_original_aspect_ratio=decrease,pad=1080:1920:-1:-1",
        "-c:v",
        "libx264",
        "-preset",
        "veryslow",
        "-c:a",
        "aac",
        "-y",
        "exit-kill-out.mp4",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    // The worker's cancel token — the SAME type and wiring the app holds
    // (`do_render` passes `&|| cancel.is_cancelled()` into `run_export`).
    let cancel = Arc::new(yc_ingest::CancelToken::new());
    let worker_cancel = cancel.clone();
    let ff = ffmpeg.clone();
    let wd = workdir.clone();
    let worker = std::thread::spawn(move || {
        yc_render::run_export(&ff, &wd, &args, &|| worker_cancel.is_cancelled())
    });

    // Let the encode get properly under way.
    std::thread::sleep(Duration::from_millis(2500));
    let during = ffmpeg_count();
    println!("[mid-render] ffmpeg.exe alive: {during}");
    anyhow::ensure!(during >= 1, "FAIL: the export child never started — nothing to orphan");

    // === exactly what App::on_exit does on a window close ===
    println!("[close] flipping the cancel token (App::on_exit)");
    let t0 = Instant::now();
    cancel.cancel();
    let deadline = t0 + Duration::from_secs(2);
    while !worker.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let joined = worker.is_finished();
    let outcome = if joined {
        match worker.join().expect("worker thread") {
            Ok(()) => "returned Ok (export finished before the kill?)".to_string(),
            Err(e) => format!("returned Err: {e:#}"),
        }
    } else {
        "STILL RUNNING at the 2 s bound".to_string()
    };
    println!("[close] worker {outcome} after {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);

    // The bar: no survivor. (Give the OS a beat to reap the killed child.)
    std::thread::sleep(Duration::from_millis(400));
    let after = ffmpeg_count();
    println!("[after] ffmpeg.exe alive: {after}");
    anyhow::ensure!(joined, "FAIL: the worker did not finish inside the exit hook's 2 s bound");
    anyhow::ensure!(after == 0, "FAIL: {after} orphaned ffmpeg.exe survived the close");

    // A partial output file is expected and fine — same as hitting Cancel.
    let partial = workdir.join("exit-kill-out.mp4");
    let bytes = partial.metadata().map(|m| m.len()).unwrap_or(0);
    println!("[after] partial output: {} ({bytes} bytes, expected — same as Cancel)", partial.display());
    println!("PASS: the exit hook killed the in-flight export; no orphan");
    Ok(())
}
