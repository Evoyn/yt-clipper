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
use yc_core::{NoConsole, TimeRange};

pub mod youtube;
pub use youtube::{
    fetch_segment, in_segment_offset, pad_range, probe_segment, resolve_deno_dir, youtube_fetch_audio,
    youtube_fetch_chat, youtube_metadata, CancelToken, SegmentProbe, Sidecars, SEGMENT_PAD_S,
};

/// Sample rate (mono) whisper.cpp expects.
pub const WHISPER_SR: u32 = 16_000;

/// Sanitize `raw` into a single filesystem-safe path segment (a folder name or a
/// file stem), valid on Windows and POSIX — the output organization feature names
/// folders from VOD metadata and Shorts from LLM-generated titles (ADR 0015), and
/// both arrive as arbitrary text. The transform:
///
/// - replaces the Windows-reserved characters `< > : " / \ | ? *` and any control
///   character with `_` (so the result is always one segment, never a separator),
/// - collapses runs of whitespace to a single space,
/// - caps the length to `max_len` *characters* (not bytes, so multibyte titles
///   aren't split mid-codepoint),
/// - trims leading/trailing whitespace and dots (Windows silently strips trailing
///   dots and spaces from names, which would otherwise desync a created path from
///   the name we think we wrote),
/// - prefixes an `_` to the reserved DOS device names (`CON`, `PRN`, `AUX`, `NUL`,
///   `COM1`-`COM9`, `LPT1`-`LPT9`), which are unusable as filenames even with an
///   extension,
/// - and returns `fallback` when nothing usable survives (e.g. an all-symbol
///   title), so a segment is never empty.
///
/// Pure, so the rules are unit-tested without touching the filesystem.
pub fn sanitize_segment(raw: &str, fallback: &str, max_len: usize) -> String {
    // Map reserved/control characters to '_'; keep everything else.
    let mapped: String = raw
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    // Collapse internal whitespace runs to single spaces.
    let collapsed = mapped.split_whitespace().collect::<Vec<_>>().join(" ");
    // Cap by character count, then trim trailing dots/spaces (Windows-illegal).
    let capped: String = collapsed.chars().take(max_len).collect();
    let trimmed = capped.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    // Guard the reserved DOS device names (case-insensitive, base before any dot).
    let base = trimmed.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// ffmpeg args to extract a VOD's whole audio track to 16 kHz mono PCM wav —
/// the audio-first analysis artifact (ADR 0001). `-vn -map 0:a:0` decodes only
/// the first audio stream; no video is touched. `-f wav` is explicit so the
/// output name needn't end in `.wav` (the caller extracts to a temp name).
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
        "-f".into(),
        "wav".into(),
        "-y".into(),
        out_wav.display().to_string(),
    ]
}

/// Extract the whole-VOD analysis wav by shelling out to the pinned ffmpeg.
/// Writes to a `.tmp` sibling and renames on success: the import caches any
/// non-empty `analysis.wav` forever (re-import reuse), so a cancelled/killed
/// extract must never leave a truncated wav behind to be silently analysed on
/// every future import. `cancel` is polled while ffmpeg runs — a whole-VOD
/// extract takes minutes on a long stream, and before this a Cancel merely set
/// a flag the worker read after the extract finished.
pub fn extract_audio(
    ffmpeg: &Path,
    video: &Path,
    out_wav: &Path,
    cancel: &youtube::CancelToken,
) -> Result<()> {
    let mut name = out_wav.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    let tmp = out_wav.with_file_name(name);
    let args = extract_audio_args(video, &tmp);
    let mut child = std::process::Command::new(ffmpeg)
        .no_console()
        .args(&args)
        .spawn()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    let status = yc_core::wait_killable(&mut child, &|| cancel.is_cancelled())
        .context("waiting on ffmpeg audio extract")?;
    let Some(status) = status else {
        anyhow::bail!("cancelled");
    };
    anyhow::ensure!(status.success(), "ffmpeg audio extract failed ({status})");
    std::fs::rename(&tmp, out_wav)
        .with_context(|| format!("moving extracted audio into {}", out_wav.display()))?;
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
        .no_console()
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

/// Like [`extract_frames_rgb`] but **streaming**: each decoded frame is handed
/// to `on_frame` and its buffer reused, so memory stays one frame deep — the
/// speaker-analysis pass reads 5 fps over a up-to-3-minute clip (~900 frames),
/// which would be hundreds of MB collected. `-t dur_s` bounds the decode.
/// `on_frame` returning `false` stops the stream early (cancellation).
#[allow(clippy::too_many_arguments)]
pub fn stream_frames_rgb(
    ffmpeg: &Path,
    video: &Path,
    seek_s: f64,
    dur_s: f64,
    w: u32,
    h: u32,
    fps: f64,
    max_frames: usize,
    on_frame: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<usize> {
    use std::io::Read;
    let mut args = vec![
        "-ss".to_string(),
        format!("{seek_s:.3}"),
        "-t".into(),
        format!("{dur_s:.3}"),
    ];
    args.extend(extract_frames_args(video, 0.0, w, h, fps, max_frames).into_iter().skip(2));
    let mut child = std::process::Command::new(ffmpeg)
        .no_console()
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    let mut stdout = child.stdout.take().context("ffmpeg stdout unavailable")?;
    let frame_bytes = (w as usize) * (h as usize) * 3;
    anyhow::ensure!(frame_bytes > 0, "zero frame size");
    let mut buf = vec![0u8; frame_bytes];
    let mut n_frames = 0usize;
    let mut stopped = false;
    'read: loop {
        let mut filled = 0usize;
        while filled < frame_bytes {
            match stdout.read(&mut buf[filled..]) {
                Ok(0) => break 'read, // EOF: a partial trailing frame is dropped
                Ok(k) => filled += k,
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e).context("reading ffmpeg frame stream");
                }
            }
        }
        n_frames += 1;
        if !on_frame(&buf) {
            stopped = true;
            break;
        }
    }
    if stopped {
        // Early stop (cancel): kill the decoder rather than draining it.
        let _ = child.kill();
        let _ = child.wait();
        return Ok(n_frames);
    }
    drop(stdout);
    let status = child.wait().context("waiting on ffmpeg frame stream")?;
    anyhow::ensure!(status.success(), "ffmpeg frame stream failed ({status})");
    anyhow::ensure!(n_frames > 0, "ffmpeg produced no frames for {}", video.display());
    Ok(n_frames)
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
        // -f wav is explicit so the temp-name output (extract_audio's .tmp +
        // rename durability) still muxes as wav.
        let f = args.iter().position(|a| a == "-f").unwrap();
        assert_eq!(args[f + 1], "wav");
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
    fn sanitize_replaces_separators_and_reserved_chars() {
        // Path separators and Windows-reserved chars become '_', so the result is
        // always a single segment.
        assert_eq!(
            sanitize_segment(r#"a/b\c:d*e?"f|g"#, "x", 64),
            "a_b_c_d_e__f_g"
        );
    }

    #[test]
    fn sanitize_collapses_whitespace_and_trims_dots() {
        assert_eq!(sanitize_segment("  hello   world  ", "x", 64), "hello world");
        // Trailing dots/spaces are Windows-illegal on a name -> trimmed.
        assert_eq!(sanitize_segment("My Clip...", "x", 64), "My Clip");
        assert_eq!(sanitize_segment("....", "fallback", 64), "fallback");
    }

    #[test]
    fn sanitize_caps_length_by_chars_not_bytes() {
        let s = sanitize_segment("abcdefghij", "x", 4);
        assert_eq!(s, "abcd");
        // A multibyte title is capped on char boundaries, never split mid-codepoint.
        let multi = sanitize_segment("héllo wörld", "x", 5);
        assert_eq!(multi.chars().count(), 5);
        assert_eq!(multi, "héllo");
    }

    #[test]
    fn sanitize_guards_reserved_device_names_and_empties() {
        assert_eq!(sanitize_segment("CON", "x", 64), "_CON");
        assert_eq!(sanitize_segment("com1", "x", 64), "_com1"); // case-insensitive
        assert_eq!(sanitize_segment("nul.txt", "x", 64), "_nul.txt");
        // COM0 is not a reserved device -> left alone.
        assert_eq!(sanitize_segment("COM0", "x", 64), "COM0");
        // Empty / all-symbol input falls back.
        assert_eq!(sanitize_segment("", "untitled", 64), "untitled");
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
