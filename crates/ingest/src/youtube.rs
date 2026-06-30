//! YouTube ingest (M2): two-phase, two-client (ADR 0006).
//!
//! - **Import** fetches the whole-VOD analysis audio (itag 140 via the
//!   `android_vr` client - a direct DASH audio-only stream downloaded whole, so
//!   no seek is involved), the raw `live_chat` replay JSON (tolerating its
//!   absence; parsed at M3), and metadata.
//! - **Promote** fetches the padded Segment as token-free **HLS via the
//!   `web_safari` client**: HLS is fragmented, so `--download-sections` pulls
//!   only the in-range fragments through yt-dlp's native downloader - no ffmpeg
//!   seek (the M1 "hang" was ffmpeg trying to range-seek a moov-not-at-front
//!   DASH file). The format is 1080p60 H.264, muxed (itag 301).
//!
//! Every child runs with deno on its PATH (nsig / anti-throttle) under a
//! killable [`CancelToken`]: yt-dlp shells out to ffmpeg, so a cancel kills the
//! whole **process tree** (`taskkill /T`) - killing only the direct child would
//! orphan the grandchild and re-create the hang.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use yc_core::{Language, NoConsole, TimeRange, Vod, VodSource};

/// Pinned sidecar locations + the resolved deno directory, captured once.
#[derive(Clone)]
pub struct Sidecars {
    pub ytdlp: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// Directory to prepend to a child's PATH so yt-dlp finds deno, or `None`
    /// if deno is already on PATH (or absent). See [`resolve_deno_dir`].
    pub deno_dir: Option<PathBuf>,
}

// --- cancellation -----------------------------------------------------------

#[derive(Default)]
struct CancelState {
    cancelled: bool,
    /// PID of the child currently being waited on, if any.
    pid: Option<u32>,
}

/// Cooperative cancellation for ingest child processes. Cloneable and shareable
/// across threads: one clone lives on the worker (which registers each child it
/// waits on) and one on the UI (which flips the flag and kills the registered
/// process tree). The shared `Mutex` makes register/cancel race-free - a cancel
/// that lands between spawn and register is seen by `register`, which then tells
/// the caller to kill immediately.
#[derive(Clone, Default)]
pub struct CancelToken {
    state: Arc<Mutex<CancelState>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.lock().unwrap().cancelled
    }

    /// Clear the flag and any registered PID. Call at the start of each job so a
    /// cancel of the previous job does not bleed into the next.
    pub fn reset(&self) {
        let mut s = self.state.lock().unwrap();
        s.cancelled = false;
        s.pid = None;
    }

    /// Request cancellation and kill any currently-registered child tree.
    pub fn cancel(&self) {
        let pid = {
            let mut s = self.state.lock().unwrap();
            s.cancelled = true;
            s.pid
        };
        if let Some(pid) = pid {
            kill_tree(pid);
        }
    }

    /// Register a freshly-spawned child. Returns `false` if cancellation already
    /// fired (the caller must then kill the child and bail).
    fn register(&self, pid: u32) -> bool {
        let mut s = self.state.lock().unwrap();
        if s.cancelled {
            return false;
        }
        s.pid = Some(pid);
        true
    }

    fn clear(&self) {
        self.state.lock().unwrap().pid = None;
    }
}

#[cfg(windows)]
fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .no_console()
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn kill_tree(pid: u32) {
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
}

// --- child process helpers --------------------------------------------------

/// Prepend `deno_dir` to this command's PATH so yt-dlp can spawn deno (nsig).
fn with_deno_path(cmd: &mut Command, deno_dir: Option<&Path>) {
    if let Some(dir) = deno_dir {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut joined = OsString::from(dir);
        joined.push(sep);
        joined.push(existing);
        cmd.env("PATH", joined);
    }
}

/// Spawn `program` with `args`, register it with `cancel`, and wait. stdout and
/// stderr are inherited (download progress shows in the console / log). A
/// cancelled run returns an error whose source is the cancel - callers detect
/// it via `cancel.is_cancelled()` rather than string matching.
fn run(program: &Path, args: &[String], deno_dir: Option<&Path>, cancel: &CancelToken) -> Result<()> {
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let mut cmd = Command::new(program);
    cmd.args(args);
    with_deno_path(&mut cmd, deno_dir);
    cmd.no_console();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", program.display()))?;
    if !cancel.register(child.id()) {
        kill_tree(child.id());
        let _ = child.wait();
        anyhow::bail!("cancelled");
    }
    let status = child.wait();
    cancel.clear();
    let status = status.with_context(|| format!("waiting on {}", program.display()))?;
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    anyhow::ensure!(status.success(), "{} failed ({status})", program.display());
    Ok(())
}

/// Like [`run`], but captures stdout (for metadata / ffprobe queries). Uses
/// `wait_with_output` so the stdout pipe is drained while the child runs - a
/// large `-J` dump cannot deadlock on a full pipe buffer.
fn run_capture(
    program: &Path,
    args: &[String],
    deno_dir: Option<&Path>,
    cancel: &CancelToken,
) -> Result<String> {
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let mut cmd = Command::new(program);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::inherit());
    with_deno_path(&mut cmd, deno_dir);
    cmd.no_console();
    let child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", program.display()))?;
    let pid = child.id();
    if !cancel.register(pid) {
        kill_tree(pid);
        let _ = child.wait_with_output();
        anyhow::bail!("cancelled");
    }
    let output = child.wait_with_output();
    cancel.clear();
    let output = output.with_context(|| format!("waiting on {}", program.display()))?;
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    anyhow::ensure!(
        output.status.success(),
        "{} failed ({})",
        program.display(),
        output.status
    );
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// --- deno discovery ---------------------------------------------------------

/// Directory to prepend to a child's PATH so yt-dlp finds deno (nsig). Priority:
/// the pinned sidecar, then the winget install (deno is installed but its exe is
/// not on PATH), then `None` (deno is already on PATH, or genuinely absent).
/// Mirrors `scripts/spike-segment.ps1`.
pub fn resolve_deno_dir(sidecars: &Path) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "deno.exe" } else { "deno" };
    if sidecars.join(exe).is_file() {
        return Some(sidecars.to_path_buf());
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let packages = PathBuf::from(local)
            .join("Microsoft")
            .join("WinGet")
            .join("Packages");
        if let Ok(entries) = std::fs::read_dir(&packages) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("DenoLand.Deno_") {
                    let candidate = entry.path().join("deno.exe");
                    if candidate.is_file() {
                        return candidate.parent().map(PathBuf::from);
                    }
                }
            }
        }
    }
    None
}

// --- workspace file helpers -------------------------------------------------

/// Delete any `dir` entries whose name starts with `prefix` (e.g. stale
/// `audio.m4a` / `audio.m4a.part` before a re-download), so the post-download
/// scan finds exactly one match.
fn clear_prefix(dir: &Path, prefix: &str) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// The single `dir` entry whose name starts with `prefix`, if any.
fn single_with_prefix(dir: &Path, prefix: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with(prefix))
                .unwrap_or(false)
        })
}

// --- yt-dlp / ffprobe argument builders (pure, unit-tested) -----------------

/// `-J` metadata dump. `android_vr` is a reliable extraction path in the 2026
/// SABR era; `--ignore-no-formats-error` keeps the info dict (id/title/duration)
/// coming back even when the client surfaces no downloadable formats.
fn meta_args(url: &str) -> Vec<String> {
    vec![
        "-J".into(),
        "--no-warnings".into(),
        "--ignore-no-formats-error".into(),
        "--extractor-args".into(),
        "youtube:player_client=android_vr".into(),
        "--no-playlist".into(),
        url.into(),
    ]
}

/// Whole-VOD analysis audio: itag 140 (m4a AAC) via `android_vr` (direct DASH,
/// downloaded whole - no seek). Output `<workdir>/audio.<ext>`.
fn audio_args(url: &str, workdir: &Path) -> Vec<String> {
    vec![
        "-f".into(),
        "140/bestaudio[ext=m4a]/bestaudio".into(),
        "--extractor-args".into(),
        "youtube:player_client=android_vr".into(),
        "--socket-timeout".into(),
        "30".into(),
        "--no-playlist".into(),
        "--newline".into(),
        "--no-warnings".into(),
        "-o".into(),
        workdir.join("audio.%(ext)s").display().to_string(),
        url.into(),
    ]
}

/// Raw `live_chat` replay JSON (no media). Best-effort: many VODs have none.
/// Output `<workdir>/chat.live_chat.json`.
fn chat_args(url: &str, workdir: &Path) -> Vec<String> {
    vec![
        "--skip-download".into(),
        "--write-subs".into(),
        "--sub-langs".into(),
        "live_chat".into(),
        "--no-playlist".into(),
        "--no-warnings".into(),
        "-o".into(),
        workdir.join("chat.%(ext)s").display().to_string(),
        url.into(),
    ]
}

/// Padded Segment as HLS via `web_safari` (token-free m3u8). `--download-sections`
/// pulls only the in-range fragments natively (no ffmpeg seek). `best[...avc1]`
/// selects the muxed 1080p60 H.264 (itag 301). Output `<workdir>/segment.<ext>`.
fn segment_args(url: &str, padded: TimeRange, ffmpeg: &Path, workdir: &Path) -> Vec<String> {
    vec![
        "--download-sections".into(),
        format!("*{:.3}-{:.3}", padded.start_s, padded.end_s),
        "-f".into(),
        "best[height<=1080][vcodec^=avc1]/best[height<=1080]/best".into(),
        "--extractor-args".into(),
        "youtube:player_client=web_safari".into(),
        "--socket-timeout".into(),
        "30".into(),
        "--ffmpeg-location".into(),
        ffmpeg.display().to_string(),
        "--no-playlist".into(),
        "--newline".into(),
        "--no-warnings".into(),
        "-o".into(),
        workdir.join("segment.%(ext)s").display().to_string(),
        url.into(),
    ]
}

/// ffprobe the first video stream + container for the layout dimensions and the
/// timeline anchor (`format=duration,start_time`).
fn probe_args(media: &Path) -> Vec<String> {
    vec![
        "-v".into(),
        "error".into(),
        "-select_streams".into(),
        "v:0".into(),
        "-show_entries".into(),
        "stream=width,height:format=duration,start_time".into(),
        "-of".into(),
        "default=noprint_wrappers=1".into(),
        media.display().to_string(),
    ]
}

// --- import (phase 1) -------------------------------------------------------

#[derive(Deserialize)]
struct YtMeta {
    id: String,
    title: String,
    duration: Option<f64>,
    uploader: Option<String>,
    channel: Option<String>,
}

/// Fetch VOD metadata and build a [`Vod`]. The `creator` is seeded from the
/// uploader/channel (the Creator model proper lands at M5/M6).
pub fn youtube_metadata(
    sc: &Sidecars,
    url: &str,
    language: Language,
    cancel: &CancelToken,
) -> Result<Vod> {
    let json = run_capture(&sc.ytdlp, &meta_args(url), sc.deno_dir.as_deref(), cancel)?;
    let meta: YtMeta = serde_json::from_str(json.trim()).context("parsing yt-dlp -J metadata")?;
    let creator = meta
        .uploader
        .or(meta.channel)
        .unwrap_or_else(|| "unknown".into());
    Ok(Vod {
        creator,
        title: meta.title,
        source: VodSource::YouTube { video_id: meta.id },
        language,
        duration_s: meta.duration,
    })
}

/// Download the analysis audio and extract it to `<workdir>/analysis.wav`
/// (16 kHz mono PCM, reusing [`crate::extract_audio`]). Returns the wav path.
pub fn youtube_fetch_audio(
    sc: &Sidecars,
    url: &str,
    workdir: &Path,
    cancel: &CancelToken,
) -> Result<PathBuf> {
    // Re-importing the same VOD reuses the cached analysis audio (the slow part)
    // so promoting more ranges / iterating captions does not re-download it.
    let analysis = workdir.join("analysis.wav");
    if analysis.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        tracing::info!("reusing cached analysis audio at {}", analysis.display());
        return Ok(analysis);
    }
    clear_prefix(workdir, "audio.");
    run(&sc.ytdlp, &audio_args(url, workdir), sc.deno_dir.as_deref(), cancel)?;
    let audio = single_with_prefix(workdir, "audio.")
        .context("yt-dlp produced no audio.* (import audio download)")?;
    crate::extract_audio(&sc.ffmpeg, &audio, &analysis)?;
    Ok(analysis)
}

/// Best-effort raw chat replay. Returns the JSON path if the VOD had chat, or
/// `None` if it did not (logged, not an error). A cancel propagates as an error.
pub fn youtube_fetch_chat(
    sc: &Sidecars,
    url: &str,
    workdir: &Path,
    cancel: &CancelToken,
) -> Result<Option<PathBuf>> {
    clear_prefix(workdir, "chat.");
    match run(&sc.ytdlp, &chat_args(url, workdir), sc.deno_dir.as_deref(), cancel) {
        Ok(()) => Ok(single_with_prefix(workdir, "chat.")),
        Err(e) if cancel.is_cancelled() => Err(e),
        Err(e) => {
            tracing::warn!("live chat unavailable: {e:#}");
            Ok(None)
        }
    }
}

// --- promote (phase 2): segment fetch + probe -------------------------------

/// Padding applied to each side of a Clip's range before fetching the Segment,
/// so the frame-accurate export cut sits safely inside a keyframe-clean span
/// (CONTEXT.md: the Segment is always a little longer than the Clip).
pub const SEGMENT_PAD_S: f64 = 2.0;

/// How many times to attempt a Segment fetch. yt-dlp's `web_safari` HLS
/// extraction intermittently fails format selection ("Requested format is not
/// available") on an unlucky webpage response - bailing before the m3u8 stage,
/// before any bytes download (ADR 0006's SABR-era flakiness). A fresh invocation
/// almost always re-resolves itag 301, so the fetch is retried; a genuinely
/// unfetchable range still errors after the final attempt. Tune-from-use.
const SEGMENT_FETCH_ATTEMPTS: u32 = 4;
/// Backoff between Segment-fetch attempts. Sleeps on the pipeline worker thread,
/// never the UI thread.
const SEGMENT_RETRY_BACKOFF_S: u64 = 3;

/// Pad a Clip's range on both sides and clamp to the VOD (`[0, duration]`).
pub fn pad_range(range: TimeRange, duration_s: Option<f64>) -> TimeRange {
    let start_s = (range.start_s - SEGMENT_PAD_S).max(0.0);
    let end_s = match duration_s {
        Some(d) => (range.end_s + SEGMENT_PAD_S).min(d),
        None => range.end_s + SEGMENT_PAD_S,
    };
    TimeRange { start_s, end_s }
}

/// Fetch the padded Segment (HLS section download). Returns the file path.
///
/// Retries transient yt-dlp format-resolution failures up to
/// [`SEGMENT_FETCH_ATTEMPTS`] times (one bad webpage response would otherwise
/// abort a whole batch render); a user cancel is terminal and never retried.
pub fn fetch_segment(
    sc: &Sidecars,
    url: &str,
    padded: TimeRange,
    workdir: &Path,
    cancel: &CancelToken,
) -> Result<PathBuf> {
    let args = segment_args(url, padded, &sc.ffmpeg, workdir);
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=SEGMENT_FETCH_ATTEMPTS {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        // Start each attempt from a clean slate (a failed try may leave a .part).
        clear_prefix(workdir, "segment.");
        match run(&sc.ytdlp, &args, sc.deno_dir.as_deref(), cancel) {
            Ok(()) => {
                return single_with_prefix(workdir, "segment.")
                    .context("yt-dlp produced no segment.* (segment fetch)");
            }
            // A cancel surfaces as a run error; it is terminal, not transient.
            Err(e) if cancel.is_cancelled() => return Err(e),
            Err(e) => {
                tracing::warn!(
                    "segment fetch attempt {attempt}/{SEGMENT_FETCH_ATTEMPTS} failed: {e:#}"
                );
                last_err = Some(e);
                if attempt < SEGMENT_FETCH_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_secs(SEGMENT_RETRY_BACKOFF_S));
                }
            }
        }
    }
    Err(last_err
        .expect("loop body runs at least once")
        .context(format!("segment fetch failed after {SEGMENT_FETCH_ATTEMPTS} attempts")))
}

/// What ffprobe tells us about a Segment (or any local media): the layout
/// dimensions, the playable duration, and the container timeline anchor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentProbe {
    pub width: u32,
    pub height: u32,
    pub duration_s: f64,
    /// Container `start_time`. The spike showed yt-dlp resets a section's
    /// timeline so pts=0 maps to the requested section start (start_time = 0,
    /// with a negative-PTS keyframe lead-in for clean decode). We still read it
    /// so the offset self-corrects if that ever changes.
    pub start_time_s: f64,
}

/// ffprobe `media` for [`SegmentProbe`].
pub fn probe_segment(ffprobe: &Path, media: &Path, cancel: &CancelToken) -> Result<SegmentProbe> {
    let out = run_capture(ffprobe, &probe_args(media), None, cancel)?;
    parse_probe(&out).with_context(|| format!("parsing ffprobe output for {}", media.display()))
}

/// Parse ffprobe `key=value` lines into a [`SegmentProbe`]. Pure, so the parser
/// is unit-tested without ffprobe.
fn parse_probe(out: &str) -> Result<SegmentProbe> {
    let value = |key: &str| -> Option<&str> {
        out.lines()
            .find_map(|l| l.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v.trim()))
    };
    let width = value("width")
        .and_then(|v| v.parse().ok())
        .context("ffprobe: no video width")?;
    let height = value("height")
        .and_then(|v| v.parse().ok())
        .context("ffprobe: no video height")?;
    // duration / start_time can be "N/A" on some containers; treat as 0.
    let duration_s = value("duration").and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let start_time_s = value("start_time").and_then(|v| v.parse().ok()).unwrap_or(0.0);
    Ok(SegmentProbe { width, height, duration_s, start_time_s })
}

/// Seconds to seek into the Segment to reach the Clip's frame-accurate start.
/// pts=0 in the Segment maps to the padded request start (plus any residual
/// container `start_time`), so the offset is the left pad - self-correcting on
/// `start_time`. (See [`SegmentProbe::start_time_s`].)
pub fn in_segment_offset(clip_start_s: f64, padded_start_s: f64, probe: &SegmentProbe) -> f64 {
    ((clip_start_s - padded_start_s) + probe.start_time_s).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_uses_a_concrete_client_and_dumps_json() {
        let a = meta_args("URL");
        assert!(a.contains(&"-J".to_string()));
        let x = a.iter().position(|s| s == "--extractor-args").unwrap();
        assert_eq!(a[x + 1], "youtube:player_client=android_vr");
        assert_eq!(a.last().unwrap(), "URL");
    }

    #[test]
    fn audio_selects_itag_140_audio_only() {
        let a = audio_args("URL", Path::new("F:/ws"));
        let f = a.iter().position(|s| s == "-f").unwrap();
        assert!(a[f + 1].starts_with("140/"), "selector: {}", a[f + 1]);
        let x = a.iter().position(|s| s == "--extractor-args").unwrap();
        assert_eq!(a[x + 1], "youtube:player_client=android_vr");
        // no section flag: the analysis audio is the whole file.
        assert!(!a.iter().any(|s| s == "--download-sections"));
    }

    #[test]
    fn segment_uses_web_safari_hls_section_download() {
        let padded = TimeRange { start_s: 598.0, end_s: 662.5 };
        let a = segment_args("URL", padded, Path::new("F:/sc/ffmpeg.exe"), Path::new("F:/ws"));
        let ds = a.iter().position(|s| s == "--download-sections").unwrap();
        assert_eq!(a[ds + 1], "*598.000-662.500");
        let x = a.iter().position(|s| s == "--extractor-args").unwrap();
        assert_eq!(a[x + 1], "youtube:player_client=web_safari");
        let f = a.iter().position(|s| s == "-f").unwrap();
        assert!(a[f + 1].contains("vcodec^=avc1"));
    }

    #[test]
    fn pad_clamps_to_the_vod_bounds() {
        // Mid-VOD: padded by SEGMENT_PAD_S on each side.
        let r = TimeRange { start_s: 600.0, end_s: 660.0 };
        let p = pad_range(r, Some(3600.0));
        assert_eq!(p.start_s, 600.0 - SEGMENT_PAD_S);
        assert_eq!(p.end_s, 660.0 + SEGMENT_PAD_S);
        // Near the start: left side clamps to 0, never negative.
        let head = pad_range(TimeRange { start_s: 1.0, end_s: 5.0 }, Some(3600.0));
        assert_eq!(head.start_s, 0.0);
        // Near the end: right side clamps to duration.
        let tail = pad_range(TimeRange { start_s: 3590.0, end_s: 3599.0 }, Some(3600.0));
        assert_eq!(tail.end_s, 3600.0);
    }

    #[test]
    fn offset_is_the_left_pad_and_self_corrects_on_start_time() {
        let probe = |st| SegmentProbe { width: 1920, height: 1080, duration_s: 64.0, start_time_s: st };
        // Observed case: start_time = 0 -> offset is exactly the left pad.
        assert_eq!(in_segment_offset(600.0, 598.0, &probe(0.0)), 2.0);
        // If the container ever carried a residual start_time, add it.
        assert!((in_segment_offset(600.0, 598.0, &probe(0.5)) - 2.5).abs() < 1e-9);
        // Never negative.
        assert_eq!(in_segment_offset(600.0, 600.0, &probe(0.0)), 0.0);
    }

    #[test]
    fn probe_parses_dims_duration_and_start_time() {
        // Shape of real ffprobe output for the spike segment.
        let out = "width=1920\nheight=1080\nduration=60.016000\nstart_time=0.000000\n";
        let p = parse_probe(out).unwrap();
        assert_eq!((p.width, p.height), (1920, 1080));
        assert!((p.duration_s - 60.016).abs() < 1e-6);
        assert_eq!(p.start_time_s, 0.0);
    }

    #[test]
    fn probe_tolerates_na_duration() {
        let out = "width=640\nheight=360\nduration=N/A\nstart_time=N/A\n";
        let p = parse_probe(out).unwrap();
        assert_eq!((p.width, p.height), (640, 360));
        assert_eq!(p.duration_s, 0.0);
        assert_eq!(p.start_time_s, 0.0);
    }
}
