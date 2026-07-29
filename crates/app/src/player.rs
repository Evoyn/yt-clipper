//! Streaming preview playback (the ADR 0036 "natural later upgrade", shipped):
//! during Play, ffmpeg decodes the prepared render source at a low preview
//! resolution and real-time pace (`-re`) and pipes raw rgb24 frames to a
//! reader thread; the UI drains to the NEWEST frame each repaint and uploads
//! it into ONE reused texture. Full-rate motion (~24 fps) at preview quality,
//! one frame of memory, zero GPU decode.
//!
//! The 4 fps filmstrip (ADR 0036) stays for everything else — paused frames,
//! scrubbing, the instant after a seek — so pausing/seeking never waits on a
//! decoder. The player is spawned on Play / seek-while-playing and killed on
//! Pause / end / drop.

use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, TryRecvError};

use yc_core::NoConsole;

/// Fallback preview frame rate when the source's own rate is unknown (a probe
/// without `r_frame_rate`). When the real rate IS known the decode runs on the
/// SOURCE's grid instead: `fps=<source rate>` maps source frames 1:1 onto
/// delivered frames, so `frames_seen / fps` is exact content time and the
/// camera crop can bind to the precise frame on screen. A hardcoded 24 over a
/// 23.976 source re-quantizes every cut by up to a frame (a per-cut coin flip
/// — the preview twin of the render's trim-rounding flash).
pub const PLAY_FPS: f64 = 24.0;
/// Seek-target back-off, in frames, for every preview decode (ADR 0076): on
/// these DASH muxes `ffmpeg -ss` can KEEP the frame before the target and
/// emit it with its rebased pts CLAMPED TO ZERO — content one frame earlier
/// than every label derived from it. Measured on the eyegate fixture: the
/// linear scan puts the cut frame at 18.125, while `-ss <cut>` delivered the
/// PRE-cut frame labeled `pts_time:0`; the burn and analysis decode linearly
/// and never see this. Seeking a quarter-frame early makes a genuine
/// on-target frame rebase to a POSITIVE pts, so a zero-stamped frame is
/// provably the clamped impostor — and `select=gt(t,0)` drops it. Verified:
/// with the bias every probed seek lands on the true grid and the cut seek
/// delivers the actual cut frame.
const SEEK_BIAS_FRAMES: f64 = 0.25;
/// Longest edge of the live playback frame — quality is explicitly secondary
/// to motion here (the render always reads the full-res source).
const PLAY_LONG_EDGE: f32 = 640.0;

/// A live playback stream: the ffmpeg child, the frame receiver, and the one
/// texture the frames upload into.
pub struct PreviewPlayer {
    child: Child,
    rx: Receiver<Vec<u8>>,
    w: usize,
    h: usize,
    /// The decode grid (the source's probed rate, or [`PLAY_FPS`] fallback):
    /// delivered frame n spans `[n/fps, (n+1)/fps)` of content time.
    fps: f64,
    texture: Option<egui::TextureHandle>,
    /// Whether any frame has arrived yet (until then callers should keep
    /// showing the filmstrip so Play never flashes black).
    got_frame: bool,
    /// Total frames the decoder has delivered (including any coalesced when the
    /// UI drains to newest). Over [`Self::fps`] this **is** the video's elapsed
    /// content time — the caller drives the playhead by it so the crop can
    /// never run ahead of the frame actually on screen (the "blank before the
    /// cut": a wall-clock playhead outpaces a decoder that isn't perfectly
    /// real-time, switching the crop before the new shot is visible).
    frames_seen: u64,
    /// Every delivered frame's pts on the decode's own grid (`showinfo`
    /// after the fps filter, seconds from the seek point), keyed by the
    /// REPORTED output frame number — the shown frame binds to its reported
    /// pts, never an inferred one: `-ss` discards up to a frame, the fps
    /// filter can slot the first frame late, and mid-stream drop/duplicate
    /// wobble moves a count off the grid by another frame — each error put
    /// the crop 1-3 frames on the wrong side of a cut while playing
    /// (ADR 0076 follow-up, 2026-07-29). Consumers fall back to a count
    /// estimate for the few ms a frame can outrun its stderr line.
    pts_rx: Receiver<(usize, f64)>,
    /// Drained [`Self::pts_rx`] — slot k = delivered frame k's pts.
    pts_seen: Vec<Option<f64>>,
    /// The seek back-off applied at spawn ([`SEEK_BIAS_FRAMES`]): delivered
    /// times are relative to the biased target; subtracting this rebases
    /// them to the caller's requested origin.
    bias: f64,
}

/// Content time (seconds) of the MIDDLE of the newest delivered frame:
/// `phase` is the first frame's measured pts on the decode grid (0 until it
/// parses), frames `0..n` delivered, so the one on screen spans
/// `[phase + (n-1)/fps, phase + n/fps)`. The midpoint is the robust instant
/// to pick the camera shot with: a cut boundary is an exact frame pts, so
/// comparing at mid-frame tolerates the remaining sub-frame wobble.
pub(crate) fn frame_mid_s(phase: f64, frames_seen: u64, fps: f64) -> f64 {
    phase + (frames_seen as f64 - 0.5) / fps.max(1e-6)
}

/// Aspect-preserving even dimensions with the longest edge capped.
fn play_dims(src_w: f32, src_h: f32) -> (usize, usize) {
    let even = |v: f32| ((v.round().max(2.0) as usize) / 2) * 2;
    if src_w >= src_h {
        let w = src_w.min(PLAY_LONG_EDGE);
        (even(w), even(w * src_h / src_w))
    } else {
        let h = src_h.min(PLAY_LONG_EDGE);
        (even(h * src_w / src_h), even(h))
    }
}

impl PreviewPlayer {
    /// Start decoding `src` from `seek_s` (in-source seconds) for `dur_s`,
    /// on the source's own frame grid (`src_fps`, probed; 0 falls back to
    /// [`PLAY_FPS`]). `-re` paces delivery at real time, so draining to the
    /// newest frame keeps video within ~a frame of the app's wall-clock
    /// playhead (the audio runs beside it in rodio, same clock).
    pub fn spawn(
        ffmpeg: &Path,
        src: &PathBuf,
        seek_s: f64,
        dur_s: f64,
        src_w: f32,
        src_h: f32,
        src_fps: f64,
    ) -> std::io::Result<Self> {
        let (w, h) = play_dims(src_w, src_h);
        let fps = if src_fps.is_finite() && src_fps > 0.0 { src_fps } else { PLAY_FPS };
        // Quarter-frame-early target + drop-zero-pts: the clamped-leader
        // guard (see SEEK_BIAS_FRAMES). Delivered times are relative to the
        // BIASED target; `bias` converts them back to the caller's origin.
        let bias = SEEK_BIAS_FRAMES / fps;
        let target = (seek_s - bias).max(0.0);
        let mut child = Command::new(ffmpeg)
            .no_console()
            .args([
                "-ss",
                &format!("{target:.4}"),
                "-re",
                "-i",
                &src.display().to_string(),
                "-t",
                &format!("{:.3}", dur_s.max(0.05) + bias),
                "-an",
                "-vf",
                // select drops the clamped leader; showinfo AFTER the fps
                // filter reports each OUTPUT frame's slotted pts — the times
                // that bind delivered frames to true content time.
                &format!(
                    "select='gt(t,0)',fps={fps},showinfo,scale={w}:{h}:flags=fast_bilinear"
                ),
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        // Parse EVERY showinfo pts in order (one line per output frame) and
        // keep draining to EOF — an undrained stderr pipe would block ffmpeg.
        // The channel is effectively unbounded: a preview clip is minutes,
        // thousands of f64s at most.
        let (tx_pts, pts_rx) = std::sync::mpsc::channel::<(usize, f64)>();
        std::thread::spawn(move || {
            let mut r = std::io::BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if let Some(np) = parse_showinfo_frame(&line) {
                            if tx_pts.send(np).is_err() {
                                break; // player dropped (child killed with it)
                            }
                        }
                    }
                }
            }
        });
        // A shallow channel: the UI drains to newest, so anything deeper is
        // just latency between the decoder and the screen.
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(2);
        let frame_bytes = w * h * 3;
        std::thread::spawn(move || {
            let mut buf = vec![0u8; frame_bytes];
            'read: loop {
                let mut filled = 0;
                while filled < frame_bytes {
                    match stdout.read(&mut buf[filled..]) {
                        Ok(0) => break 'read, // EOF (clip end / kill)
                        Ok(k) => filled += k,
                        Err(_) => break 'read,
                    }
                }
                if tx.send(buf.clone()).is_err() {
                    break; // player dropped
                }
            }
        });
        Ok(Self {
            child,
            rx,
            w,
            h,
            fps,
            texture: None,
            got_frame: false,
            frames_seen: 0,
            pts_rx,
            pts_seen: Vec::new(),
            bias,
        })
    }

    /// Drain newly reported frame timestamps into their slots (slot k =
    /// frame k's pts, keyed by showinfo's own frame number).
    fn drain_pts(&mut self) {
        while let Ok((n, p)) = self.pts_rx.try_recv() {
            if n >= self.pts_seen.len() {
                self.pts_seen.resize(n + 1, None);
            }
            self.pts_seen[n] = Some(p);
        }
    }

    /// START time of the frame currently ON SCREEN, on the decode grid:
    /// its REPORTED pts when the stderr line has arrived, else the nearest
    /// earlier measured frame plus the grid steps since — an estimate that
    /// can only be off by the frames since the last measurement (≈0), never
    /// by drift accumulated from the stream start (which re-exposed a
    /// 1-frame early box/crop flip on exactly the fallback paints near a
    /// cut — the operator's "green box before the picture switches",
    /// 2026-07-29). `None` before any frame.
    fn shown_frame_start(&self) -> Option<f64> {
        if self.frames_seen == 0 {
            return None;
        }
        let idx = (self.frames_seen - 1) as usize;
        let step = 1.0 / self.fps.max(1e-6);
        // The shown frame's own line, or the nearest measured one before it —
        // rebased from the biased seek target to the caller's origin.
        for k in (0..=idx.min(self.pts_seen.len().saturating_sub(1))).rev() {
            if let Some(p) = self.pts_seen.get(k).copied().flatten() {
                return Some(p + (idx - k) as f64 * step - self.bias);
            }
        }
        // No line has arrived at all yet (first paints after spawn).
        Some(idx as f64 * step - self.bias)
    }

    /// The video's elapsed **content** time (seconds) — the shown frame's
    /// end — or `None` before the first frame. The caller adds it to the
    /// play offset for the playhead (the audio/caption clock).
    pub fn video_secs(&self) -> Option<f64> {
        Some(self.shown_frame_start()? + 1.0 / self.fps.max(1e-6))
    }

    /// One-line debug state for the Studio's time HUD (ADR 0076): delivered
    /// frame count, the shown frame's start time, and whether it is the
    /// frame's REPORTED pts or the nearest-anchor estimate.
    pub fn hud(&self) -> String {
        let idx = self.frames_seen.saturating_sub(1) as usize;
        let measured = self.pts_seen.get(idx).copied().flatten().is_some();
        match self.shown_frame_start() {
            Some(s) => format!(
                "live n={} start={s:.3} {}",
                self.frames_seen,
                if measured { "meas" } else { "est" }
            ),
            None => format!("live n={} (no frame yet)", self.frames_seen),
        }
    }

    /// Content time of the midpoint of the frame currently ON SCREEN, or
    /// `None` before the first frame — what the camera crop must be picked
    /// with (see [`frame_mid_s`]).
    pub fn shown_frame_mid_s(&self) -> Option<f64> {
        Some(self.shown_frame_start()? + 0.5 / self.fps.max(1e-6))
    }

    /// Drain to the newest decoded frame and return the live texture to draw,
    /// or `None` until the first frame lands (caller shows the filmstrip).
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<egui::TextureId> {
        self.drain_pts();
        let mut newest: Option<Vec<u8>> = None;
        loop {
            match self.rx.try_recv() {
                Ok(f) => {
                    newest = Some(f);
                    self.frames_seen += 1;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        if let Some(frame) = newest {
            if frame.len() == self.w * self.h * 3 {
                let img = egui::ColorImage::from_rgb([self.w, self.h], &frame);
                match &mut self.texture {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => {
                        self.texture =
                            Some(ctx.load_texture("live-preview", img, egui::TextureOptions::LINEAR));
                    }
                }
                self.got_frame = true;
            }
        }
        if self.got_frame {
            self.texture.as_ref().map(|t| t.id())
        } else {
            None
        }
    }
}

impl Drop for PreviewPlayer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A **marker-exact paused frame** (ADR 0075): while the Studio is paused,
/// one background ffmpeg decode fetches the TRUE source frame at the
/// playhead and replaces the ~2-4 fps filmstrip thumbnail (whose slot
/// granularity makes a camera cut appear only at the next strip frame, up
/// to ~0.4 s after its marker). `showinfo` reports the delivered frame's
/// actual pts, so `display_time` can bind overlays to the exact frame shown
/// — never to the requested time, which may fall a sub-frame on the other
/// side of a cut boundary.
///
/// The decode discards frames before the seek target, so the delivered
/// frame is the FIRST one with pts at or after the playhead — parking
/// exactly on a cut marker shows the cut frame, the same side the render's
/// trim keeps.
pub struct PausedExact {
    child: Child,
    rx: Receiver<(f64, Vec<u8>)>,
    /// The clip-relative source time this fetch is FOR (the caller matches
    /// it against the current playhead before trusting the result).
    pub src_t: f64,
    w: usize,
    h: usize,
    texture: Option<egui::TextureHandle>,
    /// Clip-relative pts of the delivered frame, once it arrived.
    shown_pts: Option<f64>,
}

/// The `pts_time:` of the LAST `showinfo` line in an ffmpeg stderr dump —
/// the delivered frame's pts on the seek-rebased timeline (0 = the seek
/// point, so the frame's clip time is `src_t + this`).
pub(crate) fn parse_showinfo_pts(stderr: &str) -> Option<f64> {
    stderr
        .lines()
        .filter(|l| l.contains("Parsed_showinfo"))
        .filter_map(|l| l.split("pts_time:").nth(1))
        .filter_map(|s| {
            s.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                .next()
                .and_then(|v| v.parse::<f64>().ok())
        })
        .last()
}

/// One `showinfo` frame line → `(n, pts_time)`: the OUTPUT frame number and
/// its pts on the seek-rebased timeline. Pairing by the REPORTED `n` (not
/// arrival order) survives any extra showinfo/log lines interleaved in the
/// stream.
pub(crate) fn parse_showinfo_frame(line: &str) -> Option<(usize, f64)> {
    if !line.contains("Parsed_showinfo") {
        return None;
    }
    let n = line
        .split(" n:")
        .nth(1)?
        .trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse::<usize>()
        .ok()?;
    let pts = line
        .split("pts_time:")
        .nth(1)?
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .next()?
        .parse::<f64>()
        .ok()?;
    Some((n, pts))
}

impl PausedExact {
    /// Fetch the frame at clip-relative `src_t` (in-source seek =
    /// `seek_s + src_t`), at the live player's preview dimensions. Returns
    /// immediately; the decode (~0.1-0.2 s) lands via [`Self::poll`].
    pub fn spawn(
        ffmpeg: &Path,
        src: &PathBuf,
        seek_s: f64,
        src_t: f64,
        src_w: f32,
        src_h: f32,
        src_fps: f64,
    ) -> std::io::Result<Self> {
        let (w, h) = play_dims(src_w, src_h);
        let fps = if src_fps.is_finite() && src_fps > 0.0 { src_fps } else { PLAY_FPS };
        // Quarter-frame-early target + drop-zero-pts: the clamped-leader
        // guard (see SEEK_BIAS_FRAMES) — without it this decode delivered
        // the frame BEFORE a cut relabeled with the cut's own time.
        let bias = SEEK_BIAS_FRAMES / fps;
        let target = (seek_s + src_t - bias).max(0.0);
        let origin = src_t - bias;
        let mut child = Command::new(ffmpeg)
            .no_console()
            .args([
                "-ss",
                &format!("{target:.4}"),
                "-i",
                &src.display().to_string(),
                "-frames:v",
                "1",
                "-an",
                "-vf",
                &format!("select='gt(t,0)',showinfo,scale={w}:{h}:flags=fast_bilinear"),
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (tx, rx) = std::sync::mpsc::sync_channel::<(f64, Vec<u8>)>(1);
        let frame_bytes = w * h * 3;
        // Drain both pipes concurrently (a one-frame decode, but stderr must
        // not be left to fill while stdout blocks, or vice versa).
        let err_h = std::thread::spawn(move || {
            let mut s = String::new();
            let mut r = stderr;
            let _ = std::io::Read::read_to_string(&mut r, &mut s);
            s
        });
        std::thread::spawn(move || {
            let mut frame = Vec::with_capacity(frame_bytes);
            let mut r = stdout;
            let _ = std::io::Read::read_to_end(&mut r, &mut frame);
            let text = err_h.join().unwrap_or_default();
            let delta = parse_showinfo_pts(&text).unwrap_or(0.0);
            if frame.len() >= frame_bytes {
                frame.truncate(frame_bytes);
                let _ = tx.send((origin + delta, frame));
            }
        });
        Ok(Self { child, rx, src_t, w, h, texture: None, shown_pts: None })
    }

    /// The delivered frame's clip-relative pts, without polling — for
    /// `display_time`, which reads immutably after `poll` ran this paint.
    pub fn ready_pts(&self) -> Option<f64> {
        self.shown_pts
    }

    /// One-line debug state for the Studio's time HUD (ADR 0076).
    pub fn hud(&self) -> String {
        match self.shown_pts {
            Some(p) => format!("exact want={:.3} pts={p:.3}", self.src_t),
            None => format!("exact want={:.3} decoding…", self.src_t),
        }
    }

    /// The exact frame's texture and its clip-relative pts, once the decode
    /// lands (`None` until then — the caller keeps showing the filmstrip).
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<(egui::TextureId, f64)> {
        if self.shown_pts.is_none() {
            if let Ok((pts, frame)) = self.rx.try_recv() {
                if frame.len() == self.w * self.h * 3 {
                    let img = egui::ColorImage::from_rgb([self.w, self.h], &frame);
                    match &mut self.texture {
                        Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                        None => {
                            self.texture = Some(ctx.load_texture(
                                "paused-exact",
                                img,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                    }
                    self.shown_pts = Some(pts);
                }
            }
        }
        match (&self.texture, self.shown_pts) {
            (Some(t), Some(pts)) => Some((t.id(), pts)),
            _ => None,
        }
    }
}

impl Drop for PausedExact {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod paused_exact_tests {
    use super::parse_showinfo_pts;

    #[test]
    fn showinfo_pts_parses_the_delivered_frames_line() {
        let stderr = "\
[Parsed_showinfo_0 @ 0x1] config in time_base: 1/24000, frame_rate: 24000/1001\n\
[Parsed_showinfo_0 @ 0x1] n:   0 pts:    501 pts_time:0.020875 duration:1001 fmt:yuv420p\n\
frame=    1 fps=0.0 q=-0.0 size=  1350kB time=00:00:00.02\n";
        let pts = parse_showinfo_pts(stderr).unwrap();
        assert!((pts - 0.020875).abs() < 1e-9);
        assert!(parse_showinfo_pts("no such line").is_none(), "absent showinfo parses to None");
    }

    #[test]
    fn showinfo_frame_lines_pair_by_reported_number() {
        // The live binding pairs pts to frames by showinfo's OWN `n:` field —
        // arrival order can carry extra lines (a config header, side-data
        // rows) that index-pairing would drift on.
        let l = "[Parsed_showinfo_1 @ 0x2] n:  17 pts: 17017 pts_time:0.709042 duration:1001";
        let (n, pts) = super::parse_showinfo_frame(l).unwrap();
        assert_eq!(n, 17);
        assert!((pts - 0.709042).abs() < 1e-9);
        assert!(
            super::parse_showinfo_frame(
                "[Parsed_showinfo_1 @ 0x2] config in time_base: 1/24000"
            )
            .is_none(),
            "a non-frame showinfo line pairs to nothing"
        );
        assert!(super::parse_showinfo_frame("frame=  1 fps=0.0").is_none());
    }
}
