//! Diag: subprocess failures must REPORT the child's stderr tail (review-fixes
//! slice 1, 2026-07-14). Before the fix, `run_export` and ingest's `run`
//! inherited stderr — under the GUI's `no_console()` it went nowhere, so a
//! field failure reported only an exit status. Drives the two REAL production
//! paths with deliberate failures — a bad-arg ffmpeg export
//! (`yc_render::run_export`) and a bogus-URL yt-dlp download
//! (`yc_ingest::youtube_fetch_audio`, which reaches the shared `run` first) —
//! and PASSes when both errors contain the child's own stderr text. Cheap:
//! two child spawns, no network fetch, no GPU.
//!
//!   cargo run -p yt-clipper --example stderr_tail_diag
//!
//! Run from the repo root (finds `sidecars\ffmpeg.exe` / `sidecars\yt-dlp.exe`).

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let ffmpeg = PathBuf::from("sidecars/ffmpeg.exe");
    let ytdlp = PathBuf::from("sidecars/yt-dlp.exe");
    anyhow::ensure!(
        ffmpeg.is_file() && ytdlp.is_file(),
        "run from the repo root (sidecars\\ not found)"
    );
    let work = std::env::temp_dir().join("yc-stderr-tail-diag");
    std::fs::create_dir_all(&work)?;

    // 1) ffmpeg export with a nonsense flag: the error must carry ffmpeg's own
    //    complaint, not only the exit status.
    let args: Vec<String> = vec!["-hide_banner".into(), "-nonsense-flag".into()];
    let err = yc_render::run_export(&ffmpeg, &work, &args, &|| false)
        .expect_err("bad-arg export must fail");
    let msg = format!("{err:#}");
    println!("[ffmpeg]  {msg}");
    anyhow::ensure!(
        msg.contains("nonsense-flag"),
        "FAIL: ffmpeg error lacks its stderr tail"
    );

    // 2) yt-dlp download of a non-URL. Clear the analysis cache first —
    //    youtube_fetch_audio returns early on a cached analysis.wav.
    let _ = std::fs::remove_file(work.join("analysis.wav"));
    let sc = yc_ingest::Sidecars {
        ytdlp,
        ffmpeg: ffmpeg.clone(),
        ffprobe: PathBuf::from("sidecars/ffprobe.exe"),
        deno_dir: None,
    };
    let cancel = yc_ingest::CancelToken::new();
    let err = yc_ingest::youtube_fetch_audio(&sc, "not-a-url", &work, &cancel)
        .expect_err("bogus-url fetch must fail");
    let msg = format!("{err:#}");
    println!("[yt-dlp]  {msg}");
    anyhow::ensure!(
        msg.contains("not-a-url") && msg.to_uppercase().contains("ERROR"),
        "FAIL: yt-dlp error lacks its stderr tail"
    );

    println!("PASS: both failures report the child's stderr tail");
    Ok(())
}
