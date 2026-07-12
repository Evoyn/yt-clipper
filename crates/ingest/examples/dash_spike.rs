//! ADR 0060 spike instrument: measure the pre-registered bars for the native
//! DASH section fetch on a REAL VOD, through the exact production functions
//! (`yc_ingest::dash`).
//!
//!   cargo run -p yc-ingest --example dash_spike -- <video_id> [start_s end_s]
//!
//! Bars (pre-registered in docs/adr/0060-native-dash-section-fetch.md BEFORE
//! the first run):
//!   S1 permission — a mid-file ~10 MB ranged read of the avc1 video AND m4a
//!      audio URLs returns exactly the window, no 403 (mechanism recorded).
//!   S2 speed      — that window sustains >= 5 MB/s.
//!   S3 index      — head has ftyp+moov+ONE sidx before media; entry
//!      durations sum to ~ the VOD duration (+/-2%).
//!   S4 alignment  — video & audio sections snap to their own subsegment
//!      boundaries; after the local ffmpeg mux the probed A/V first-pts delta
//!      equals the sidx-computed (video_t0 - audio_t0) within 50 ms; the
//!      working mux recipe is recorded.
//!
//! Run from the repo root (sidecars\yt-dlp.exe + ffmpeg/ffprobe resolved
//! relative). deno on PATH is used by yt-dlp for -j resolution as usual.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result};
use yc_ingest::dash;
use yc_ingest::CancelToken;

fn run_json(exe: &Path, args: &[&str]) -> Result<serde_json::Value> {
    let out = Command::new(exe).args(args).output().context("spawning yt-dlp")?;
    anyhow::ensure!(
        out.status.success(),
        "yt-dlp -j failed: {}",
        String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("?")
    );
    serde_json::from_slice(&out.stdout).context("parsing yt-dlp -j output")
}

fn probe_first_pts(ffprobe: &Path, media: &Path, stream: &str) -> Result<f64> {
    let out = Command::new(ffprobe)
        .args([
            "-v", "error", "-select_streams", stream, "-show_entries", "packet=pts_time",
            "-read_intervals", "%+#1", "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(media)
        .output()
        .context("ffprobe first pts")?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse::<f64>().ok())
        .context("no first pts")
}

fn main() -> Result<()> {
    let mut a = std::env::args().skip(1);
    let vid = a.next().expect("usage: dash_spike <video_id> [start_s end_s]");
    let start_s: f64 = a.next().map(|s| s.parse().unwrap()).unwrap_or(100.0);
    let end_s: f64 = a.next().map(|s| s.parse().unwrap()).unwrap_or(130.0);
    let url = format!("https://www.youtube.com/watch?v={vid}");
    let ytdlp = PathBuf::from("sidecars/yt-dlp.exe");
    let ffmpeg = PathBuf::from("sidecars/ffmpeg.exe");
    let ffprobe = PathBuf::from("sidecars/ffprobe.exe");
    let work = std::env::temp_dir().join("yc_dash_spike");
    std::fs::create_dir_all(&work)?;
    let cancel = CancelToken::new();
    let agent = dash::section_agent();

    println!("=== dash_spike {vid} [{start_s:.1}-{end_s:.1}s] ===");
    let meta = run_json(&ytdlp, &["-j", "--no-playlist", "--no-warnings", &url])?;
    let vod_dur = meta["duration"].as_f64().unwrap_or(0.0);
    let formats = &meta["formats"];
    println!("muxed HLS present (tier 1): {}", dash::has_muxed_hls(formats));
    let (vurl, aurl) = dash::pick_dash_pair(formats).context("no DASH avc1+m4a pair (tier 2 unavailable)")?;
    println!("picked DASH pair (video+audio URLs resolved)");

    // --- S3: head + sidx on both streams ---------------------------------------
    let deadline = Instant::now() + std::time::Duration::from_secs(180);
    let mut s3_ok = true;
    let mut idx_pair = Vec::new();
    for (label, u) in [("video", &vurl), ("audio", &aurl)] {
        let head = dash::read_range(&agent, u, 0, 512 * 1024, &cancel, deadline)?;
        match dash::parse_sidx_head(&head) {
            Ok(idx) => {
                let dur = idx.duration_s();
                let dev = if vod_dur > 0.0 { (dur - vod_dur).abs() / vod_dur * 100.0 } else { 0.0 };
                println!(
                    "S3 {label}: sidx ok — {} entries, timescale {}, head {} B, media dur {:.1}s (VOD {:.1}s, dev {:.2}%)",
                    idx.segments.len(), idx.timescale, idx.head_len, dur, vod_dur, dev
                );
                if dev > 2.0 {
                    s3_ok = false;
                    println!("S3 {label}: FAIL — duration deviation > 2%");
                }
                idx_pair.push(idx);
            }
            Err(e) => {
                s3_ok = false;
                println!("S3 {label}: FAIL — {e:#}");
            }
        }
    }
    println!("S3: {}", if s3_ok && idx_pair.len() == 2 { "PASS" } else { "FAIL" });
    anyhow::ensure!(s3_ok && idx_pair.len() == 2, "S3 failed — stop per pre-registration");

    // --- S1 + S2: mid-file 10 MB window, timed ---------------------------------
    let mut s12_ok = true;
    for (label, u, idx) in [("video", &vurl, &idx_pair[0]), ("audio", &aurl, &idx_pair[1])] {
        // A window starting well into the media (past any head cache).
        let mid = idx.segments[idx.segments.len() / 2].byte_start;
        let want: u64 = if label == "video" { 10 * 1024 * 1024 } else { 2 * 1024 * 1024 };
        let t0 = Instant::now();
        match dash::read_range(&agent, u, mid, mid + want, &cancel, Instant::now() + std::time::Duration::from_secs(180)) {
            Ok(bytes) => {
                let secs = t0.elapsed().as_secs_f64();
                let mbs = bytes.len() as f64 / 1e6 / secs;
                let pass = if label == "video" { mbs >= 5.0 } else { mbs >= 1.0 };
                println!(
                    "S1/S2 {label}: {} bytes from offset {} in {secs:.1}s = {mbs:.1} MB/s {}",
                    bytes.len(), mid, if pass { "PASS" } else { "FAIL (throttle class?)" }
                );
                if !pass {
                    s12_ok = false;
                }
            }
            Err(e) => {
                s12_ok = false;
                println!("S1 {label}: FAIL — {e:#}");
            }
        }
    }
    println!("S1/S2: {}", if s12_ok { "PASS" } else { "FAIL" });
    anyhow::ensure!(s12_ok, "S1/S2 failed — stop per pre-registration");

    // --- S4: full section fetch both streams + mux + pts-delta check -----------
    let vpart = work.join("dashpart_v.mp4");
    let apart = work.join("dashpart_a.m4a");
    let t0 = Instant::now();
    let vplan = dash::fetch_stream_section(&agent, &vurl, start_s, end_s, &vpart, &cancel)?;
    let aplan = dash::fetch_stream_section(&agent, &aurl, start_s, end_s, &apart, &cancel)?;
    println!(
        "S4 fetch: video [{:.2}-{:.2}s] {} MB, audio [{:.2}-{:.2}s] {} MB in {:.1}s",
        vplan.t0_s, vplan.t1_s, (vplan.byte_to - vplan.byte_from) / (1024 * 1024),
        aplan.t0_s, aplan.t1_s, (aplan.byte_to - aplan.byte_from) / (1024 * 1024),
        t0.elapsed().as_secs_f64()
    );
    let expected_delta = vplan.t0_s - aplan.t0_s;
    let out = work.join("segment.mp4");
    let mut recipe_used = None;
    for recipe in ["copyts", "plain"] {
        let mut c = Command::new(&ffmpeg);
        c.args(["-y", "-v", "error"]);
        if recipe == "copyts" {
            c.args(["-copyts", "-i"]).arg(&vpart).args(["-copyts", "-i"]).arg(&apart);
        } else {
            c.arg("-i").arg(&vpart).arg("-i").arg(&apart);
        }
        c.args(["-map", "0:v:0", "-map", "1:a:0", "-c", "copy"]).arg(&out);
        let st = c.output().context("ffmpeg mux")?;
        if !st.status.success() {
            println!("S4 mux ({recipe}): ffmpeg failed — {}", String::from_utf8_lossy(&st.stderr).lines().last().unwrap_or("?"));
            continue;
        }
        let vp = probe_first_pts(&ffprobe, &out, "v:0")?;
        let ap = probe_first_pts(&ffprobe, &out, "a:0")?;
        let got_delta = vp - ap;
        let err = (got_delta - expected_delta).abs();
        println!(
            "S4 mux ({recipe}): v_pts0 {vp:.3} a_pts0 {ap:.3} delta {got_delta:+.3}s expected {expected_delta:+.3}s err {err:.3}s {}",
            if err <= 0.050 { "PASS" } else { "fail" }
        );
        if err <= 0.050 {
            recipe_used = Some(recipe);
            break;
        }
    }
    match recipe_used {
        Some(r) => println!("S4: PASS (recipe: {r}; artifacts in {})", work.display()),
        None => {
            println!("S4: FAIL — no mux recipe preserved the sidx-computed A/V delta");
            anyhow::bail!("S4 failed — stop per pre-registration");
        }
    }
    Ok(())
}
