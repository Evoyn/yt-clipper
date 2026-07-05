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

use std::io::Read;
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
}

/// Content time (seconds) of the MIDDLE of the newest delivered frame —
/// frames `0..n` delivered, so the one on screen spans `[(n-1)/fps, n/fps)`.
/// The midpoint is the robust instant to pick the camera shot with: a cut
/// boundary is an exact frame pts, so comparing at mid-frame tolerates up to
/// half a frame of seek/grid phase in either direction.
pub(crate) fn frame_mid_s(frames_seen: u64, fps: f64) -> f64 {
    (frames_seen as f64 - 0.5) / fps.max(1e-6)
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
        let mut child = Command::new(ffmpeg)
            .no_console()
            .args([
                "-ss",
                &format!("{seek_s:.3}"),
                "-re",
                "-i",
                &src.display().to_string(),
                "-t",
                &format!("{:.3}", dur_s.max(0.05)),
                "-an",
                "-vf",
                &format!("fps={fps},scale={w}:{h}:flags=fast_bilinear"),
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = child.stdout.take().expect("piped stdout");
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
        Ok(Self { child, rx, w, h, fps, texture: None, got_frame: false, frames_seen: 0 })
    }

    /// The video's elapsed **content** time (seconds) — frames delivered so far
    /// over the decode grid — or `None` before the first frame. The caller adds
    /// it to the play offset for the playhead (the audio/caption clock).
    pub fn video_secs(&self) -> Option<f64> {
        (self.frames_seen > 0).then(|| self.frames_seen as f64 / self.fps.max(1e-6))
    }

    /// Content time of the midpoint of the frame currently ON SCREEN, or
    /// `None` before the first frame — what the camera crop must be picked
    /// with (see [`frame_mid_s`]).
    pub fn shown_frame_mid_s(&self) -> Option<f64> {
        (self.frames_seen > 0).then(|| frame_mid_s(self.frames_seen, self.fps))
    }

    /// Drain to the newest decoded frame and return the live texture to draw,
    /// or `None` until the first frame lands (caller shows the filmstrip).
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<egui::TextureId> {
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
