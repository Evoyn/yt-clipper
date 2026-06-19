//! Ingestion: gets a VOD's audio (and later, padded video segments) into the
//! per-VOD workspace folder. Audio-first, two-phase (ADR 0001):
//! audio + chat replay at import; video segments only when a Moment is
//! promoted to a Clip. YouTube fetches shell out to the pinned yt-dlp
//! sidecar; local files go through ffmpeg audio extraction.
//!
//! M1 scope: local-file audio extraction. M2 scope: YouTube audio + chat +
//! segment downloads.

use anyhow::{Context, Result};
use std::path::Path;
use yc_core::TimeRange;

pub mod youtube;
pub use youtube::{
    fetch_segment, in_segment_offset, pad_range, probe_segment, resolve_deno_dir, youtube_fetch_audio,
    youtube_fetch_chat, youtube_metadata, CancelToken, SegmentProbe, Sidecars, SEGMENT_PAD_S,
};

/// Sample rate (mono) whisper.cpp expects.
pub const WHISPER_SR: u32 = 16_000;

/// ffmpeg args to extract a VOD's whole audio track to 16 kHz mono PCM wav —
/// the audio-first analysis artifact (ADR 0001). `-vn -map 0:a:0` decodes only
/// the first audio stream; no video is touched.
pub fn extract_audio_args(video: &Path, out_wav: &Path) -> Vec<String> {
    vec![
        "-i".into(),
        video.display().to_string(),
        "-vn".into(),
        "-map".into(),
        "0:a:0".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        WHISPER_SR.to_string(),
        "-c:a".into(),
        "pcm_s16le".into(),
        "-y".into(),
        out_wav.display().to_string(),
    ]
}

/// Extract the whole-VOD analysis wav by shelling out to the pinned ffmpeg.
pub fn extract_audio(ffmpeg: &Path, video: &Path, out_wav: &Path) -> Result<()> {
    let args = extract_audio_args(video, out_wav);
    let status = std::process::Command::new(ffmpeg)
        .args(&args)
        .status()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    anyhow::ensure!(status.success(), "ffmpeg audio extract failed ({status})");
    Ok(())
}

/// ffmpeg args to sample `max_frames` frames at `fps` per second from `video`,
/// each scaled to `w` x `h` and emitted as raw rgb24 on stdout. `-ss` before
/// `-i` fast-seeks `seek_s` in (the Segment is padded, the local file is the
/// whole VOD), so sampling starts at the Clip's moment, not the media start.
/// Used by M6 auto-framing (ADR 0011): a handful of frames is enough to locate
/// the static Facecam.
pub fn extract_frames_args(
    video: &Path,
    seek_s: f64,
    w: u32,
    h: u32,
    fps: f64,
    max_frames: usize,
) -> Vec<String> {
    vec![
        "-ss".into(),
        format!("{seek_s:.3}"),
        "-i".into(),
        video.display().to_string(),
        "-vf".into(),
        format!("fps={fps},scale={w}:{h}"),
        "-frames:v".into(),
        max_frames.to_string(),
        "-pix_fmt".into(),
        "rgb24".into(),
        "-f".into(),
        "rawvideo".into(),
        "-".into(),
    ]
}

/// Sample frames from `video` as raw rgb24 (`w` x `h`), returning one
/// `w*h*3`-byte buffer per frame. stderr is inherited so ffmpeg's diagnostics
/// reach the log; stdout is drained via `wait_with_output` so a large rawvideo
/// stream cannot deadlock on a full pipe.
pub fn extract_frames_rgb(
    ffmpeg: &Path,
    video: &Path,
    seek_s: f64,
    w: u32,
    h: u32,
    fps: f64,
    max_frames: usize,
) -> Result<Vec<Vec<u8>>> {
    let args = extract_frames_args(video, seek_s, w, h, fps, max_frames);
    let child = std::process::Command::new(ffmpeg)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    let output = child.wait_with_output().context("waiting on ffmpeg frame extract")?;
    anyhow::ensure!(output.status.success(), "ffmpeg frame extract failed ({})", output.status);

    let frame_bytes = (w as usize) * (h as usize) * 3;
    anyhow::ensure!(frame_bytes > 0, "zero frame size");
    let frames: Vec<Vec<u8>> =
        output.stdout.chunks_exact(frame_bytes).map(|c| c.to_vec()).collect();
    anyhow::ensure!(!frames.is_empty(), "ffmpeg produced no frames for {}", video.display());
    Ok(frames)
}

/// Half-open sample bounds for `range` in a clip of `total` samples at `sr`.
/// Clamped so `start <= end <= total`.
fn sample_bounds(range: TimeRange, sr: u32, total: usize) -> (usize, usize) {
    let start = ((range.start_s.max(0.0) * sr as f64) as usize).min(total);
    let end = ((range.end_s.max(0.0) * sr as f64) as usize).clamp(start, total);
    (start, end)
}

/// Read just the samples for `range` from a 16 kHz mono PCM wav, as f32 in
/// roughly [-1, 1] — the input to the whisper pass over the picked range
/// (transcribe-range-only, M1). Seeks rather than reading the whole file.
pub fn read_range_samples(wav: &Path, range: TimeRange) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(wav).with_context(|| format!("opening {}", wav.display()))?;
    let spec = reader.spec();
    anyhow::ensure!(
        spec.channels == 1,
        "expected mono analysis wav, got {} channels",
        spec.channels
    );
    anyhow::ensure!(
        spec.sample_rate == WHISPER_SR,
        "expected {WHISPER_SR} Hz analysis wav, got {} Hz",
        spec.sample_rate
    );

    let total = reader.len() as usize;
    let (start, end) = sample_bounds(range, spec.sample_rate, total);
    reader.seek(start as u32).context("seeking wav")?;
    let samples = reader
        .samples::<i16>()
        .take(end - start)
        .map(|r| r.map(|s| s as f32 / 32768.0))
        .collect::<Result<Vec<f32>, _>>()
        .context("decoding wav samples")?;
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_args_force_16k_mono_pcm() {
        let args = extract_audio_args(Path::new("F:/in.mkv"), Path::new("F:/out.wav"));
        // -ar 16000 and pcm_s16le present, in that flag->value order.
        let ar = args.iter().position(|a| a == "-ar").unwrap();
        assert_eq!(args[ar + 1], "16000");
        let ac = args.iter().position(|a| a == "-ac").unwrap();
        assert_eq!(args[ac + 1], "1");
        assert!(args.contains(&"pcm_s16le".to_string()));
        assert!(args.contains(&"F:/in.mkv".to_string()));
    }

    #[test]
    fn frame_args_sample_rawvideo_rgb_at_size() {
        let args = extract_frames_args(Path::new("F:/seg.mp4"), 12.5, 320, 240, 1.5, 40);
        // fast-seek before input
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        assert_eq!(args[ss + 1], "12.500");
        assert!(args.iter().position(|a| a == "-i").unwrap() > ss);
        // scale + fps in the filter, rawvideo rgb24 out to stdout
        let vf = args.iter().position(|a| a == "-vf").unwrap();
        assert_eq!(args[vf + 1], "fps=1.5,scale=320:240");
        assert!(args.contains(&"rawvideo".to_string()));
        let pf = args.iter().position(|a| a == "-pix_fmt").unwrap();
        assert_eq!(args[pf + 1], "rgb24");
        assert_eq!(args.last().unwrap(), "-");
    }

    #[test]
    fn bounds_are_clamped_half_open() {
        // 10 s clip at 16 kHz = 160_000 samples.
        let total = 160_000;
        let r = TimeRange { start_s: 1.0, end_s: 2.0 };
        assert_eq!(sample_bounds(r, WHISPER_SR, total), (16_000, 32_000));
        // end past the file is clamped to total.
        let over = TimeRange { start_s: 9.0, end_s: 99.0 };
        assert_eq!(sample_bounds(over, WHISPER_SR, total), (144_000, 160_000));
        // start past end yields an empty range, never a panic.
        let inverted = TimeRange { start_s: 5.0, end_s: 1.0 };
        let (s, e) = sample_bounds(inverted, WHISPER_SR, total);
        assert!(s >= e);
    }
}
