//! Print the production-measured in-segment seek for a clip, so a manual re-burn
//! of a suppressed clip.ass reproduces the render's exact frame/caption
//! alignment (the HLS section download can snap seconds off the request, so the
//! seek is audio-anchored, not assumed — see `align.rs`). Throwaway gate helper.
//!
//!   cargo run -p yc-ingest --example segment_seek -- <segment.mp4> <analysis.wav> <clip_start_s> [pad_s=2]

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let segment = PathBuf::from(a.next().expect("segment.mp4"));
    let analysis = PathBuf::from(a.next().expect("analysis.wav"));
    let clip_start: f64 = a.next().expect("clip_start_s").parse()?;
    let pad: f64 = a.next().map(|s| s.parse().unwrap()).unwrap_or(2.0);
    let padded_start = (clip_start - pad).max(0.0);
    let ffmpeg = PathBuf::from("sidecars/ffmpeg.exe");
    let ffprobe = PathBuf::from("sidecars/ffprobe.exe");
    let cancel = yc_ingest::CancelToken::new();
    match yc_ingest::measure_segment_anchor(&ffmpeg, &ffprobe, &segment, &analysis, padded_start, &cancel)? {
        Some(anchor) => {
            let seek = (clip_start - anchor.vod_t0_s).max(0.0);
            println!("vod_t0={:.3}  corr={:.3}  SEEK={:.3}", anchor.vod_t0_s, anchor.corr, seek);
        }
        None => println!("NO LOCK (fall back to assumed seek = pad + segment.start_time)"),
    }
    Ok(())
}
