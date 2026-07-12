//! Native DASH **section** fetch (ADR 0060): 1080p Segments without tokens.
//!
//! YouTube's DASH https representations are single fragmented-mp4 files whose
//! `sidx` box (right after `ftyp`+`moov`, before any media) maps time → byte
//! ranges of self-contained subsegments (`moof`+`mdat`, IDR-aligned). ffmpeg
//! cannot range-seek these over HTTP (ADR 0006's M1 hang — a "section" pulls
//! the whole multi-GB file), but nothing needs it to: we fetch the head once,
//! parse the sidx, compute the byte window covering the padded Segment range,
//! and read exactly those bytes. `head + window` concatenated is a valid
//! fragmented mp4 starting at a subsegment boundary; the two streams (avc1
//! video + m4a audio) are muxed locally by the bundled ffmpeg. The pipeline's
//! measured-alignment pass owns absolute placement on the VOD timeline
//! (protocol-agnostic by design — ADR 0059), exactly as it did for HLS's own
//! boundary snapping.
//!
//! Discipline (the M1 scar, in-process form): every read is chunked with a
//! [`CancelToken`] poll and a hard wall-clock deadline; a ranged read whose
//! reply is not exactly the requested window is an error, never a silent
//! whole-file stream. The pure parsing/planning below is unit-tested; the
//! network path is exercised by `examples/dash_spike.rs` (the ADR 0060 gate
//! instrument) against the pre-registered bars.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::CancelToken;

/// Hard wall-clock budget for one stream's section fetch (head + window).
/// Expected reality is seconds (ADR 0060 spike); the budget exists so a
/// throttled/stalled read dies loudly instead of hanging a Promote.
const FETCH_BUDGET: Duration = Duration::from_secs(180);
/// First head probe; doubled once if the sidx is not inside (a fatter moov).
const HEAD_PROBE: u64 = 512 * 1024;
/// Sanity cap on a computed media window: a pathological sidx must fall back
/// to the 360p tier, not start a multi-GB pull.
const MAX_WINDOW: u64 = 400 * 1024 * 1024;

/// One `sidx` subsegment: `byte_start` is absolute in the stream file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SidxSegment {
    pub byte_start: u64,
    pub byte_len: u64,
    /// Duration in `SidxIndex::timescale` units.
    pub dur: u64,
}

/// The parsed index of one DASH representation.
#[derive(Debug, Clone)]
pub struct SidxIndex {
    pub timescale: u64,
    /// Presentation time of the first subsegment, in timescale units.
    pub earliest_pts: u64,
    /// Bytes `0..head_len` are `ftyp`+`moov`+`sidx` — the init prefix every
    /// section file starts with.
    pub head_len: u64,
    pub segments: Vec<SidxSegment>,
}

impl SidxIndex {
    /// Total indexed media duration in seconds.
    pub fn duration_s(&self) -> f64 {
        self.segments.iter().map(|s| s.dur).sum::<u64>() as f64 / self.timescale as f64
    }
}

/// The byte window covering a clip-time range, plus where it actually starts
/// on the stream's own timeline (subsegments snap like HLS fragments did).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DashPlan {
    pub byte_from: u64,
    /// Exclusive.
    pub byte_to: u64,
    /// Absolute stream time the window's first subsegment starts at.
    pub t0_s: f64,
    /// Absolute stream time the window's last subsegment ends at.
    pub t1_s: f64,
}

/// Walk top-level mp4 boxes: `(type, payload_offset, payload_len, box_end)`.
/// Tolerates a truncated tail (the head probe cuts mid-file) by stopping at
/// the first box that runs past `buf` — callers only need the boxes that fit.
fn boxes(buf: &[u8]) -> Vec<([u8; 4], u64, u64, u64)> {
    let mut out = Vec::new();
    let mut off: u64 = 0;
    let n = buf.len() as u64;
    while off + 8 <= n {
        let at = off as usize;
        let size32 = u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as u64;
        let typ = [buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]];
        let (hdr, size) = if size32 == 1 {
            if off + 16 > n {
                break;
            }
            let mut b = [0u8; 8];
            b.copy_from_slice(&buf[at + 8..at + 16]);
            (16u64, u64::from_be_bytes(b))
        } else {
            (8u64, size32)
        };
        if size < hdr {
            break; // size==0 ("to end of file") or corrupt — nothing indexable
        }
        out.push((typ, off + hdr, size - hdr, off + size));
        off += size;
    }
    out
}

/// Parse the head of a DASH representation: require `ftyp`+`moov`+ exactly
/// one top-level `sidx` BEFORE any media box (`moof`/`mdat`), per bar S3.
/// `buf` is the head probe; boxes past its end are ignored (media).
pub fn parse_sidx_head(buf: &[u8]) -> Result<SidxIndex> {
    let list = boxes(buf);
    let mut sidx: Option<(u64, u64, u64)> = None; // payload off/len, box end
    for (typ, poff, plen, end) in &list {
        match typ {
            b"sidx" => {
                anyhow::ensure!(sidx.is_none(), "multiple top-level sidx boxes");
                sidx = Some((*poff, *plen, *end));
            }
            b"moof" | b"mdat" => break, // media begins — stop scanning
            _ => {}
        }
    }
    let (poff, plen, box_end) = sidx.context("no sidx box in head probe")?;
    anyhow::ensure!(
        list.iter().any(|(t, ..)| t == b"moov"),
        "no moov before sidx (not an initialized representation)"
    );
    let p = buf
        .get(poff as usize..(poff + plen) as usize)
        .context("sidx payload truncated by head probe")?;
    anyhow::ensure!(p.len() >= 4, "sidx payload too short");
    let version = p[0];
    let be32 = |at: usize| -> Result<u64> {
        let b = p.get(at..at + 4).context("sidx truncated")?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64)
    };
    let be64 = |at: usize| -> Result<u64> {
        let b = p.get(at..at + 8).context("sidx truncated")?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_be_bytes(a))
    };
    // fullbox(4) + reference_ID(4) + timescale(4)
    let timescale = be32(8)?;
    anyhow::ensure!(timescale > 0, "sidx timescale is zero");
    let (earliest_pts, first_offset, mut at) = if version == 0 {
        (be32(12)?, be32(16)?, 20usize)
    } else {
        (be64(12)?, be64(20)?, 28usize)
    };
    at += 2; // reserved
    let count = {
        let b = p.get(at..at + 2).context("sidx truncated")?;
        u16::from_be_bytes([b[0], b[1]]) as usize
    };
    at += 2;
    anyhow::ensure!(count > 0, "sidx has no entries");
    let mut segments = Vec::with_capacity(count);
    let mut byte_at = box_end + first_offset;
    for i in 0..count {
        let e = at + i * 12;
        let word = be32(e)?;
        anyhow::ensure!(
            word & 0x8000_0000 == 0,
            "hierarchical sidx (reference_type=1) — unsupported"
        );
        let len = word & 0x7fff_ffff;
        let dur = be32(e + 4)?;
        segments.push(SidxSegment { byte_start: byte_at, byte_len: len as u64, dur });
        byte_at += len as u64;
    }
    Ok(SidxIndex { timescale, earliest_pts, head_len: box_end, segments })
}

/// Pure: the byte window whose subsegments cover `[start_s, end_s]`.
/// Times before the first subsegment clamp to it; times past the end clamp
/// to the last (a padded range at the VOD tail). Entirely out-of-range is an
/// error.
pub fn plan_window(idx: &SidxIndex, start_s: f64, end_s: f64) -> Result<DashPlan> {
    anyhow::ensure!(!idx.segments.is_empty(), "empty sidx");
    let ts = idx.timescale as f64;
    let mut t = idx.earliest_pts as f64 / ts;
    anyhow::ensure!(
        start_s <= t + idx.duration_s(),
        "requested window starts past the indexed media ({start_s:.1}s > {:.1}s)",
        t + idx.duration_s()
    );
    let (mut a, mut b) = (None, idx.segments.len() - 1);
    let mut t0_s = t;
    let mut t1_s = t;
    for (i, seg) in idx.segments.iter().enumerate() {
        let t_next = t + seg.dur as f64 / ts;
        if a.is_none() && (start_s < t_next || i == idx.segments.len() - 1) {
            a = Some(i);
            t0_s = t;
        }
        if a.is_some() {
            b = i;
            t1_s = t_next;
            if end_s <= t_next {
                break;
            }
        }
        t = t_next;
    }
    let a = a.expect("loop always selects a start segment");
    let from = idx.segments[a].byte_start;
    let to = idx.segments[b].byte_start + idx.segments[b].byte_len;
    Ok(DashPlan { byte_from: from, byte_to: to, t0_s, t1_s })
}

/// One ranged read, verified: the reply must be EXACTLY `[from, to)` bytes.
/// Tries the `Range:` header first; a server that ignores it (HTTP 200 with
/// the full length) is aborted before the body streams and retried via
/// googlevideo's `&range=a-b` URL parameter. Chunked, cancel-polled,
/// deadline-bounded — a throttled read errors out instead of hanging.
pub fn read_range(
    agent: &ureq::Agent,
    url: &str,
    from: u64,
    to: u64,
    cancel: &CancelToken,
    deadline: Instant,
) -> Result<Vec<u8>> {
    anyhow::ensure!(to > from, "empty range");
    let want = to - from;
    let via_header = || -> Result<ureq::http::Response<ureq::Body>> {
        agent
            .get(url)
            .header("Range", format!("bytes={}-{}", from, to - 1))
            .call()
            .context("ranged request (header) failed")
    };
    let via_param = || -> Result<ureq::http::Response<ureq::Body>> {
        let sep = if url.contains('?') { '&' } else { '?' };
        agent
            .get(format!("{url}{sep}range={}-{}", from, to - 1))
            .call()
            .context("ranged request (param) failed")
    };
    let content_len = |r: &ureq::http::Response<ureq::Body>| -> Option<u64> {
        r.headers().get("content-length")?.to_str().ok()?.parse().ok()
    };
    let mut resp = via_header()?;
    if resp.status().as_u16() != 206 && content_len(&resp) != Some(want) {
        // Header ignored (200 + full file, or an error page): do NOT stream
        // it — that is the whole-file pull this module exists to avoid.
        resp = via_param()?;
        let got = content_len(&resp);
        anyhow::ensure!(
            resp.status().as_u16() < 300 && got == Some(want),
            "server refused the byte window (status {}, content-length {:?}, want {want})",
            resp.status(),
            got
        );
    }
    let mut reader = resp.body_mut().as_reader();
    let mut out = Vec::with_capacity(want.min(64 * 1024 * 1024) as usize);
    let mut buf = [0u8; 256 * 1024];
    loop {
        anyhow::ensure!(!cancel.is_cancelled(), "cancelled");
        anyhow::ensure!(
            Instant::now() < deadline,
            "section fetch exceeded its {}s budget (throttled?)",
            FETCH_BUDGET.as_secs()
        );
        let n = reader.read(&mut buf).context("reading ranged body")?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        anyhow::ensure!(
            out.len() as u64 <= want,
            "server sent more than the requested window"
        );
    }
    anyhow::ensure!(
        out.len() as u64 == want,
        "short ranged read: got {} of {want} bytes",
        out.len()
    );
    Ok(out)
}

/// Fetch one representation's section: head probe → sidx → plan → window
/// read → `head + window` written to `out`. Returns the plan (its `t0_s` is
/// the section's own start time — the mux uses the video/audio delta).
pub fn fetch_stream_section(
    agent: &ureq::Agent,
    url: &str,
    start_s: f64,
    end_s: f64,
    out: &Path,
    cancel: &CancelToken,
) -> Result<DashPlan> {
    let deadline = Instant::now() + FETCH_BUDGET;
    let mut head = read_range(agent, url, 0, HEAD_PROBE, cancel, deadline)?;
    let idx = match parse_sidx_head(&head) {
        Ok(i) => i,
        Err(_) => {
            // One fatter probe (bigger moov), then give up to the caller.
            head = read_range(agent, url, 0, HEAD_PROBE * 4, cancel, deadline)?;
            parse_sidx_head(&head).context("no parseable sidx in stream head")?
        }
    };
    let plan = plan_window(&idx, start_s, end_s)?;
    anyhow::ensure!(
        plan.byte_to - plan.byte_from <= MAX_WINDOW,
        "computed window {} MB exceeds the sanity cap",
        (plan.byte_to - plan.byte_from) / (1024 * 1024)
    );
    let media = read_range(agent, url, plan.byte_from, plan.byte_to, cancel, deadline)?;
    let mut file = Vec::with_capacity(idx.head_len as usize + media.len());
    file.extend_from_slice(&head[..idx.head_len as usize]);
    file.extend_from_slice(&media);
    std::fs::write(out, &file).with_context(|| format!("writing {}", out.display()))?;
    Ok(plan)
}

/// The section-fetch HTTP agent: rustls, bounded connect (the read loop owns
/// the wall budget — mirrors the app's download agent, ADR 0041).
pub fn section_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .build()
        .into()
}

/// Fetch the padded Segment natively: both representations' sections, then a
/// local `-c copy` mux with timestamps normalized to SECTION-LOCAL — the
/// yt-dlp-sections shape every downstream consumer already expects
/// (`probe_segment` start_time ≈ 0; the measured-alignment pass owns absolute
/// placement, and its ±30 s search dwarfs the ~10 s subsegment snap).
///
/// The mux recipe is spike-pinned (ADR 0060 bar S4, ≤1 ms on both fixture
/// VODs): `-copyts` on both inputs preserves the streams' RELATIVE offset —
/// their fMP4 `tfdt` times ARE the sidx times — and `-output_ts_offset
/// -min_t0` shifts the pair so the earlier stream starts at 0. Do not "fix"
/// this to a plain mux: ffmpeg then zeroes each input separately and the A/V
/// desyncs by the video-vs-audio boundary snap (measured +6.4 s on the VIOR
/// fixture).
///
/// Part files are `dashpart_*` (never `segment.*` — the caller's stale-sweep
/// prefix must not match them) and are removed on success.
pub fn fetch_dash_section(
    video_url: &str,
    audio_url: &str,
    padded_start_s: f64,
    padded_end_s: f64,
    ffmpeg: &Path,
    workdir: &Path,
    cancel: &CancelToken,
) -> Result<std::path::PathBuf> {
    use yc_core::NoConsole as _;
    let agent = section_agent();
    let vpart = workdir.join("dashpart_v.mp4");
    let apart = workdir.join("dashpart_a.m4a");
    let out = workdir.join("segment.mp4");
    let vplan =
        fetch_stream_section(&agent, video_url, padded_start_s, padded_end_s, &vpart, cancel)?;
    let aplan =
        fetch_stream_section(&agent, audio_url, padded_start_s, padded_end_s, &apart, cancel)?;
    anyhow::ensure!(!cancel.is_cancelled(), "cancelled");
    let base = vplan.t0_s.min(aplan.t0_s);
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-y", "-v", "error", "-copyts", "-i"])
        .arg(&vpart)
        .args(["-copyts", "-i"])
        .arg(&apart)
        .args(["-map", "0:v:0", "-map", "1:a:0", "-c", "copy", "-output_ts_offset"])
        .arg(format!("{:.6}", -base))
        .arg(&out);
    cmd.no_console();
    // Local files, stream copy: sub-second. Cancel is checked around it; the
    // network reads above are the cancellable long poles.
    let res = cmd.output().context("running ffmpeg mux")?;
    anyhow::ensure!(
        res.status.success(),
        "ffmpeg mux failed: {}",
        String::from_utf8_lossy(&res.stderr).lines().last().unwrap_or("?")
    );
    let _ = std::fs::remove_file(&vpart);
    let _ = std::fs::remove_file(&apart);
    anyhow::ensure!(!cancel.is_cancelled(), "cancelled");
    tracing::info!(
        "native DASH section: video [{:.2}-{:.2}s] + audio [{:.2}-{:.2}s] -> segment.mp4",
        vplan.t0_s,
        vplan.t1_s,
        aplan.t0_s,
        aplan.t1_s
    );
    Ok(out)
}

/// Format pickers over yt-dlp `-j` output — shared by production tiering and
/// the spike so they cannot diverge (the ADR 0033 discipline, HTTP edition).
///
/// Tier 1: any MUXED HLS ≤1080p (avc1 preferred by the yt-dlp selector
/// itself — this only detects availability).
pub fn has_muxed_hls(formats: &serde_json::Value) -> bool {
    formats.as_array().is_some_and(|a| {
        a.iter().any(|f| {
            f["protocol"].as_str().is_some_and(|p| p.starts_with("m3u8"))
                && f["vcodec"].as_str().is_some_and(|v| v != "none")
                && f["acodec"].as_str().is_some_and(|v| v != "none")
                && f["height"].as_u64().is_some_and(|h| h <= 1080)
        })
    })
}

/// Tier 2: the best avc1 ≤1080p video-only + best m4a audio-only https pair.
/// Returns `(video_url, audio_url)`.
pub fn pick_dash_pair(formats: &serde_json::Value) -> Option<(String, String)> {
    let arr = formats.as_array()?;
    let https = |f: &&serde_json::Value| {
        f["protocol"].as_str().is_some_and(|p| p == "https" || p == "http")
            && f["url"].as_str().is_some()
    };
    let video = arr
        .iter()
        .filter(https)
        .filter(|f| {
            f["vcodec"].as_str().is_some_and(|v| v.starts_with("avc1"))
                && f["acodec"].as_str().is_some_and(|a| a == "none")
                && f["height"].as_u64().is_some_and(|h| h <= 1080)
                && f["container"].as_str().is_none_or(|c| c.contains("mp4"))
        })
        .max_by(|x, y| {
            let k = |f: &serde_json::Value| {
                (f["height"].as_u64().unwrap_or(0), f["tbr"].as_f64().unwrap_or(0.0) as u64)
            };
            k(x).cmp(&k(y))
        })?;
    let audio = arr
        .iter()
        .filter(https)
        .filter(|f| {
            f["acodec"].as_str().is_some_and(|a| a.starts_with("mp4a"))
                && f["vcodec"].as_str().is_some_and(|v| v == "none")
        })
        .max_by(|x, y| {
            let k = |f: &serde_json::Value| f["abr"].as_f64().unwrap_or(0.0) as u64;
            k(x).cmp(&k(y))
        })?;
    Some((video["url"].as_str()?.to_owned(), audio["url"].as_str()?.to_owned()))
}

/// Build a synthetic single-`sidx` head for tests: `ftyp`+`moov` stubs, then
/// a version-0 sidx with the given timescale/entries.
#[cfg(test)]
fn synth_head(timescale: u32, earliest: u32, first_offset: u32, entries: &[(u32, u32)]) -> Vec<u8> {
    let mut b = Vec::new();
    let mut push_box = |typ: &[u8; 4], payload: &[u8]| {
        b.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
        b.extend_from_slice(typ);
        b.extend_from_slice(payload);
    };
    push_box(b"ftyp", b"isom\x00\x00\x00\x00");
    push_box(b"moov", &[0u8; 32]);
    let mut p = Vec::new();
    p.extend_from_slice(&[0, 0, 0, 0]); // version 0 + flags
    p.extend_from_slice(&1u32.to_be_bytes()); // reference_ID
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&earliest.to_be_bytes());
    p.extend_from_slice(&first_offset.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved
    p.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for (len, dur) in entries {
        p.extend_from_slice(&(len & 0x7fff_ffff).to_be_bytes());
        p.extend_from_slice(&dur.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes()); // SAP
    }
    push_box(b"sidx", &p);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_v0_sidx_and_maps_byte_offsets() {
        // 3 subsegments of 5 s (timescale 1000), 100/200/300 bytes,
        // media starting 8 bytes after the sidx box (first_offset).
        let head = synth_head(1000, 2000, 8, &[(100, 5000), (200, 5000), (300, 5000)]);
        let idx = parse_sidx_head(&head).unwrap();
        assert_eq!(idx.timescale, 1000);
        assert_eq!(idx.earliest_pts, 2000);
        assert_eq!(idx.head_len, head.len() as u64);
        assert_eq!(idx.duration_s(), 15.0);
        let base = head.len() as u64 + 8;
        assert_eq!(idx.segments[0], SidxSegment { byte_start: base, byte_len: 100, dur: 5000 });
        assert_eq!(idx.segments[1].byte_start, base + 100);
        assert_eq!(idx.segments[2].byte_start, base + 300);
    }

    #[test]
    fn plan_covers_the_requested_range_and_snaps_to_subsegments() {
        let head = synth_head(1000, 0, 0, &[(100, 5000), (200, 5000), (300, 5000), (400, 5000)]);
        let idx = parse_sidx_head(&head).unwrap();
        // 6..11 s lives in subsegments 1..=2 (5-10 and 10-15).
        let p = plan_window(&idx, 6.0, 11.0).unwrap();
        assert_eq!(p.byte_from, idx.segments[1].byte_start);
        assert_eq!(p.byte_to, idx.segments[2].byte_start + 300);
        assert_eq!((p.t0_s, p.t1_s), (5.0, 15.0));
        // A range past the tail clamps to the last subsegment.
        let tail = plan_window(&idx, 18.0, 25.0).unwrap();
        assert_eq!(tail.byte_from, idx.segments[3].byte_start);
        assert_eq!(tail.t1_s, 20.0);
        // Entirely past the media errors.
        assert!(plan_window(&idx, 21.0, 30.0).is_err());
        // earliest_pts offsets the whole timeline.
        let head2 = synth_head(1000, 100_000, 0, &[(100, 5000), (200, 5000)]);
        let idx2 = parse_sidx_head(&head2).unwrap();
        let p2 = plan_window(&idx2, 100.0, 101.0).unwrap();
        assert_eq!(p2.byte_from, idx2.segments[0].byte_start);
        assert_eq!(p2.t0_s, 100.0);
    }

    #[test]
    fn refuses_hierarchical_and_multiple_sidx() {
        let mut head = synth_head(1000, 0, 0, &[(100, 5000)]);
        // Flip the reference_type bit of entry 0 (offset: last 12-byte entry).
        let e = head.len() - 12;
        head[e] |= 0x80;
        assert!(parse_sidx_head(&head).unwrap_err().to_string().contains("hierarchical"));

        let dup = synth_head(1000, 0, 0, &[(100, 5000)]);
        let sidx_box_start = boxes(&dup)
            .iter()
            .find(|(t, ..)| t == b"sidx")
            .map(|(_, poff, ..)| (poff - 8) as usize)
            .unwrap();
        let mut two = dup.clone();
        two.extend_from_slice(&dup[sidx_box_start..]); // second sidx box verbatim
        assert!(parse_sidx_head(&two).is_err());
    }

    #[test]
    fn head_probe_truncation_is_tolerated_by_the_box_walk() {
        let head = synth_head(1000, 0, 0, &[(100, 5000), (200, 5000)]);
        // Media follows the head; the probe may cut it anywhere.
        let mut probe = head.clone();
        probe.extend_from_slice(&[0, 0, 1, 0]); // half a moof header
        let idx = parse_sidx_head(&probe).unwrap();
        assert_eq!(idx.head_len, head.len() as u64);
    }

    #[test]
    fn picks_the_best_avc1_dash_pair_and_detects_muxed_hls() {
        let formats: serde_json::Value = serde_json::json!([
            {"protocol":"https","url":"v720","vcodec":"avc1.64001f","acodec":"none","height":720,"tbr":1400.0},
            {"protocol":"https","url":"v1080","vcodec":"avc1.640028","acodec":"none","height":1080,"tbr":2741.0},
            {"protocol":"https","url":"v1440","vcodec":"avc1.640033","acodec":"none","height":1440,"tbr":9000.0},
            {"protocol":"https","url":"vp9","vcodec":"vp9","acodec":"none","height":1080,"tbr":1678.0},
            {"protocol":"https","url":"a128","vcodec":"none","acodec":"mp4a.40.2","abr":129.5},
            {"protocol":"https","url":"a48","vcodec":"none","acodec":"mp4a.40.5","abr":48.0},
            {"protocol":"https","url":"prog18","vcodec":"avc1.42001E","acodec":"mp4a.40.2","height":360}
        ]);
        let (v, a) = pick_dash_pair(&formats).unwrap();
        assert_eq!((v.as_str(), a.as_str()), ("v1080", "a128"));
        assert!(!has_muxed_hls(&formats));
        let hls: serde_json::Value = serde_json::json!([
            {"protocol":"m3u8_native","vcodec":"avc1","acodec":"mp4a","height":1080}
        ]);
        assert!(has_muxed_hls(&hls));
    }
}
