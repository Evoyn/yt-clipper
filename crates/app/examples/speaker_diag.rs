//! Speaker-analysis inspector: replicate `do_analyze_speakers` 1:1 on a clip's
//! workspace data and dump everything the Active-Speaker camera plan is built
//! from — tracks (with split/adjacency forensics), the attribution timeline,
//! and the plan (with per-shot subject-motion forensics explaining any
//! `pan_to`). The production-path twin of `caption_diag`: same segment, same
//! measured seek (audio anchor), same constants, same pure functions, so what
//! it prints IS what the editor and render saw.
//!
//!   cargo run --release -p yt-clipper --example speaker_diag --features face -- \
//!     "<stream_dir>/data" <clip_start_s> <clip_end_s> [per-bin.csv]
//!
//! `<data dir>` must hold segment.mp4 + analysis.wav (the app's own
//! intermediates); the range is the clip's VOD-absolute range from
//! project.json. Without `--features face` this prints a hint and exits.

#[cfg(not(feature = "face"))]
fn main() {
    eprintln!("speaker_diag needs the face feature: cargo run --release -p yt-clipper --example speaker_diag --features face -- <data_dir> <start_s> <end_s>");
}

#[cfg(feature = "face")]
fn main() -> anyhow::Result<()> {
    use std::path::{Path, PathBuf};
    use yc_core::TimeRange;
    use yc_frame::speaker::{self, SpeakerAnalysis, SpeakerTrack};
    use yc_ingest::CancelToken;

    let mut args = std::env::args().skip(1);
    let data_dir = PathBuf::from(args.next().expect("data dir (…/<stream>/data)"));
    let start_s: f64 = args.next().expect("clip start_s").parse()?;
    let end_s: f64 = args.next().expect("clip end_s").parse()?;
    let csv_out: Option<PathBuf> = args.next().map(PathBuf::from);
    let range = TimeRange { start_s, end_s };
    let dur = range.duration_s();

    let ffmpeg = Path::new("sidecars/ffmpeg.exe");
    let ffprobe = Path::new("sidecars/ffprobe.exe");
    let face_model = Path::new("models/version-RFB-320.onnx");
    let segment = data_dir.join("segment.mp4");
    let analysis_wav = data_dir.join("analysis.wav");
    anyhow::ensure!(segment.is_file(), "missing {}", segment.display());
    anyhow::ensure!(analysis_wav.is_file(), "missing {}", analysis_wav.display());
    anyhow::ensure!(face_model.is_file(), "missing {} (fetch-models.ps1)", face_model.display());
    let cancel = CancelToken::new();

    // --- the production seek: measured audio anchor, assumed-start fallback --
    let padded_start = (start_s - yc_ingest::SEGMENT_PAD_S).max(0.0);
    let probe = yc_ingest::probe_segment(ffprobe, &segment, &cancel)?;
    let assumed = yc_ingest::in_segment_offset(start_s, padded_start, &probe);
    let seek_s = match yc_ingest::measure_segment_anchor(
        ffmpeg,
        ffprobe,
        &segment,
        &analysis_wav,
        padded_start,
        &cancel,
    ) {
        Ok(Some(a)) => {
            let s = (start_s - a.vod_t0_s).max(0.0);
            println!("seek: measured {:.3}s (corr {:.3}; assumed {:.3}s)", s, a.corr, assumed);
            s
        }
        Ok(None) | Err(_) => {
            println!("seek: no anchor lock; assumed {assumed:.3}s");
            assumed
        }
    };
    println!(
        "segment: {}x{} @ {:.3} fps, {:.3}s; clip {:.3}..{:.3} ({dur:.2}s)\n",
        probe.width, probe.height, probe.fps, probe.duration_s, start_s, end_s
    );
    let (src_w, src_h) = (probe.width as f32, probe.height as f32);

    // --- track building: the exact do_analyze_speakers glue ------------------
    let fps = speaker::SPEAKER_FPS;
    let max_frames = (dur * fps).ceil() as usize + 4;
    let long = 640.0f32;
    let even = |v: f32| (((v.round().max(2.0)) as u32) / 2) * 2;
    let (tw, th) = if src_w >= src_h {
        (even(src_w.min(long)), even(src_w.min(long) * src_h / src_w))
    } else {
        (even(src_h.min(long) * src_w / src_h), even(src_h.min(long)))
    };
    let mut detector = yc_frame::Detector::load(face_model)?;
    let mut builder = speaker::TrackBuilder::new(src_w, src_h, tw as usize, th as usize);
    let mut det_buf = vec![0u8; yc_frame::infer::DET_W * yc_frame::infer::DET_H * 3];
    let mut detect_err: Option<anyhow::Error> = None;
    yc_ingest::stream_frames_rgb(ffmpeg, &segment, seek_s, dur, tw, th, fps, max_frames, &mut |rgb| {
        speaker::downscale_rgb(
            rgb,
            tw as usize,
            th as usize,
            &mut det_buf,
            yc_frame::infer::DET_W,
            yc_frame::infer::DET_H,
        );
        match detector.detect(&det_buf, src_w, src_h) {
            Ok(faces) => {
                builder.observe(&faces, rgb);
                true
            }
            Err(e) => {
                detect_err = Some(e);
                false
            }
        }
    })?;
    if let Some(e) = detect_err {
        return Err(e.context("face detection"));
    }
    let tracks = builder.finish();

    let samples = yc_ingest::read_range_samples(&analysis_wav, range)?;
    let bin_s = 1.0 / fps;
    let n_bins = (dur * fps).ceil().max(1.0) as usize;
    let voiced = speaker::voiced_bins(&samples, yc_ingest::WHISPER_SR, bin_s, n_bins);
    let (speaking, confidence) = speaker::attribute_speakers(&tracks, &voiced);
    let analysis =
        SpeakerAnalysis { bin_s, tracks, voiced, speaking, confidence };

    // --- scene cuts: detect_scene_cuts replica (same command, same parse) ----
    let out = std::process::Command::new(ffmpeg)
        .args([
            "-v",
            "info",
            "-ss",
            &format!("{seek_s:.3}"),
            "-t",
            &format!("{dur:.3}"),
            "-i",
            &segment.display().to_string(),
            "-vf",
            "select='gt(scene,0.2)',metadata=print",
            "-an",
            "-f",
            "null",
            "-",
        ])
        .output()?;
    let text = String::from_utf8_lossy(&out.stderr);
    let mut cuts: Vec<f64> = text
        .lines()
        .filter_map(|l| l.split("pts_time:").nth(1))
        .filter_map(|s| s.split_whitespace().next())
        .filter_map(|s| s.parse::<f64>().ok())
        .collect();
    cuts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    cuts.dedup_by(|a, b| (*a - *b).abs() < 0.02);

    let plan = speaker::plan_shots(&analysis, src_w, src_h, dur, &cuts);

    // --- forensics ------------------------------------------------------------
    let center = |t: &SpeakerTrack, b: usize| t.path.get(b).and_then(|p| p.as_ref()).map(|f| (f.cx(), f.cy()));
    let vis_bins = |t: &SpeakerTrack| t.path.iter().filter(|p| p.is_some()).count();
    let mean_faces = (0..n_bins)
        .map(|b| analysis.tracks.iter().filter(|t| center(t, b).is_some()).count())
        .sum::<usize>() as f32
        / n_bins.max(1) as f32;
    println!(
        "regime: mean visible faces {mean_faces:.2} -> {} | scene cuts {}",
        if mean_faces < 1.5 { "FOLLOW-VISIBLE (multicam)" } else { "ATTRIBUTION (static wide)" },
        cuts.len()
    );

    println!("\n== tracks ({}):", analysis.tracks.len());
    for t in &analysis.tracks {
        let first = t.path.iter().position(|p| p.is_some()).unwrap_or(0);
        let last = t.path.iter().rposition(|p| p.is_some()).unwrap_or(0);
        // Longest detection gap and largest single-bin center jump.
        let (mut gap, mut cur_gap, mut jump) = (0usize, 0usize, 0.0f32);
        let mut prev: Option<(f32, f32)> = None;
        for b in first..=last {
            match center(t, b) {
                Some(c) => {
                    if let Some(p) = prev {
                        let d = ((c.0 - p.0).powi(2) + (c.1 - p.1).powi(2)).sqrt();
                        jump = jump.max(d);
                    }
                    prev = Some(c);
                    gap = gap.max(cur_gap);
                    cur_gap = 0;
                }
                None => cur_gap += 1,
            }
        }
        println!(
            "  {}: presence {:>4.0}% | box ({:>4.0},{:>4.0}) {:>3.0}x{:>3.0} | seen {:>5.1}s..{:>5.1}s ({:>4} bins) | max gap {:.1}s | max jump {:.0}px",
            speaker::track_label(t.id),
            t.presence * 100.0,
            t.bbox.x,
            t.bbox.y,
            t.bbox.w,
            t.bbox.h,
            first as f64 * bin_s,
            last as f64 * bin_s,
            vis_bins(t),
            gap as f64 * bin_s,
            jump
        );
    }
    println!("\n== track pairs (split / adjacency suspects):");
    for i in 0..analysis.tracks.len() {
        for j in i + 1..analysis.tracks.len() {
            let (a, b) = (&analysis.tracks[i], &analysis.tracks[j]);
            let d = ((a.bbox.cx() - b.bbox.cx()).powi(2) + (a.bbox.cy() - b.bbox.cy()).powi(2)).sqrt();
            let both = (0..n_bins).filter(|&k| center(a, k).is_some() && center(b, k).is_some()).count();
            let near = d < 1.5 * a.bbox.w.max(b.bbox.w);
            println!(
                "  {}-{}: center dist {:>4.0}px ({}), co-visible {:>4.1}s{}",
                speaker::track_label(a.id),
                speaker::track_label(b.id),
                d,
                if near { "ADJACENT" } else { "apart" },
                both as f64 * bin_s,
                if near && both as f64 * bin_s < 1.0 { "  << SPLIT-TRACK SUSPECT (near + rarely co-visible)" } else { "" }
            );
        }
    }

    let voiced_n = analysis.voiced.iter().filter(|v| **v).count();
    let attr_n = (0..n_bins).filter(|&b| analysis.voiced[b] && analysis.speaking[b].is_some()).count();
    println!(
        "\n== attribution: voiced {:.0}% of clip; attributed {:.0}% of voiced (mean conf {:.2})",
        100.0 * voiced_n as f64 / n_bins as f64,
        100.0 * attr_n as f64 / voiced_n.max(1) as f64,
        (0..n_bins).map(|b| analysis.confidence[b] as f64).sum::<f64>() / n_bins as f64
    );
    // Unattributed-while-voiced runs (the "doesn't detect who is speaking").
    let mut b = 0;
    println!("  voiced-but-unattributed runs (>0.5s):");
    while b < n_bins {
        if analysis.voiced[b] && analysis.speaking[b].is_none() {
            let s = b;
            while b < n_bins && analysis.speaking[b].is_none() {
                b += 1;
            }
            if (b - s) as f64 * bin_s > 0.5 {
                println!("    {:>5.1}s..{:>5.1}s ({:.1}s)", s as f64 * bin_s, b as f64 * bin_s, (b - s) as f64 * bin_s);
            }
        } else {
            b += 1;
        }
    }
    println!("  speaker switches:");
    let mut last_sp: Option<usize> = None;
    for b in 0..n_bins {
        if let Some(id) = analysis.speaking[b] {
            if last_sp != Some(id) {
                println!(
                    "    {:>5.1}s -> {} (conf {:.2})",
                    b as f64 * bin_s,
                    speaker::track_label(id),
                    analysis.confidence[b]
                );
                last_sp = Some(id);
            }
        }
    }

    println!("\n== plan ({} shots):", plan.shots.len());
    for (i, s) in plan.shots.iter().enumerate() {
        let who = s.track.map(speaker::track_label).unwrap_or_else(|| "group".into());
        print!("  #{i}: {:>5.1}s..{:>5.1}s  {:<9}", s.start_s, s.end_s, who);
        if let yc_core::Layout::FullFrame { crop } = &s.layout {
            print!(" crop ({:>4.0},{:>4.0}) {:.0}x{:.0}", crop.x, crop.y, crop.w, crop.h);
        }
        match &s.pan_to {
            Some(p) => println!("  PAN to ({:>4.0},{:>4.0})  d=({:+.0},{:+.0})", p.x, p.y, p.x - match &s.layout { yc_core::Layout::FullFrame { crop } => crop.x, _ => 0.0 }, p.y - match &s.layout { yc_core::Layout::FullFrame { crop } => crop.y, _ => 0.0 }),
            None => println!(),
        }
        // Why a pan: the subject's head vs tail median center over the shot.
        if let Some(id) = s.track {
            if let Some(tr) = analysis.tracks.iter().find(|t| t.id == id) {
                let b0 = (s.start_s / bin_s) as usize;
                let b1 = ((s.end_s / bin_s) as usize).min(n_bins);
                let edge = (1.6 / bin_s) as usize;
                let med = |lo: usize, hi: usize| {
                    let mut xs: Vec<f32> = Vec::new();
                    let mut ys: Vec<f32> = Vec::new();
                    for k in lo..hi.min(n_bins) {
                        if let Some(c) = center(tr, k) {
                            xs.push(c.0);
                            ys.push(c.1);
                        }
                    }
                    if xs.is_empty() {
                        return None;
                    }
                    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    Some((xs[xs.len() / 2], ys[ys.len() / 2]))
                };
                if let (Some(h), Some(t2)) = (med(b0, b0 + edge), med(b1.saturating_sub(edge), b1)) {
                    println!(
                        "        subject head ({:>4.0},{:>4.0}) -> tail ({:>4.0},{:>4.0})  moved ({:+.0},{:+.0})",
                        h.0, h.1, t2.0, t2.1, t2.0 - h.0, t2.1 - h.1
                    );
                }
            }
        }
    }

    // --- the real filtergraph (render it with export_args-style ffmpeg flags
    // to SEE this plan; clip.ass + fonts live in the data dir) ----------------
    let fg = data_dir.join("camera_diag.fg");
    std::fs::write(&fg, yc_render::build_camera_filtergraph(&plan, "clip.ass"))?;
    println!("\nfiltergraph: {}", fg.display());

    // --- per-bin CSV for deeper digging ---------------------------------------
    if let Some(csv) = csv_out {
        let mut w = String::from("t,voiced,speaker,conf");
        for t in &analysis.tracks {
            w.push_str(&format!(",{}x,{}y,{}act", t.id, t.id, t.id));
        }
        w.push('\n');
        for b in 0..n_bins {
            w.push_str(&format!(
                "{:.3},{},{},{:.3}",
                b as f64 * bin_s,
                analysis.voiced[b] as u8,
                analysis.speaking[b].map(|i| i as i64).unwrap_or(-1),
                analysis.confidence[b]
            ));
            for t in &analysis.tracks {
                match center(t, b) {
                    Some((x, y)) => w.push_str(&format!(",{x:.0},{y:.0},{:.4}", t.activity.get(b).copied().unwrap_or(0.0))),
                    None => w.push_str(",,,"),
                }
            }
            w.push('\n');
        }
        std::fs::write(&csv, w)?;
        println!("\nper-bin csv: {}", csv.display());
    }
    Ok(())
}
