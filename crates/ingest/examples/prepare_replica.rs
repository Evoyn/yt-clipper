//! Throwaway gate helper (ADR 0074): re-fetch a Prepare segment for a clip
//! range into a REPLICA directory — never a live data dir — so a harness
//! window whose `segment.mp4` a later Prepare overwrote can be re-run
//! against today's code. Uses the production fetch verbatim
//! (`yc_ingest::fetch_segment`: muxed HLS → native DASH → yt-dlp floor).
//!
//!   cargo run --release -p yc-ingest --example prepare_replica -- \
//!     <video_id> <start_s> <end_s> <vod_duration_s> <out_dir>

use std::path::{Path, PathBuf};

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let video_id = a.next().expect("video_id");
    let start_s: f64 = a.next().expect("start_s").parse()?;
    let end_s: f64 = a.next().expect("end_s").parse()?;
    let vod_dur: f64 = a.next().expect("vod_duration_s").parse()?;
    let out_dir = PathBuf::from(a.next().expect("out_dir"));
    std::fs::create_dir_all(&out_dir)?;

    let sidecars = Path::new("sidecars");
    let sc = yc_ingest::Sidecars {
        ytdlp: sidecars.join("yt-dlp.exe"),
        ffmpeg: sidecars.join("ffmpeg.exe"),
        ffprobe: sidecars.join("ffprobe.exe"),
        deno_dir: yc_ingest::resolve_deno_dir(sidecars),
    };
    let url = format!("https://www.youtube.com/watch?v={video_id}");
    let range = yc_core::TimeRange { start_s, end_s };
    let padded = yc_ingest::pad_range(range, Some(vod_dur));
    let cancel = yc_ingest::CancelToken::new();
    println!("fetching {url} padded {:.2}-{:.2}s -> {}", padded.start_s, padded.end_s, out_dir.display());
    let seg = yc_ingest::fetch_segment(&sc, &url, padded, &out_dir, &cancel)?;
    let probe = yc_ingest::probe_segment(&sc.ffprobe, &seg, &cancel)?;
    println!(
        "fetched {}: {}x{} @ {:.3} fps, {:.2}s",
        seg.display(),
        probe.width,
        probe.height,
        probe.fps,
        probe.duration_s
    );
    Ok(())
}
