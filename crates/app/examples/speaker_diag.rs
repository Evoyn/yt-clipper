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
//!
//! The **voice lane** (ADR 0042, integrated) runs the PRODUCTION functions —
//! `yc_frame::voice::{embed_windows, build_lane, fuse_attribution}` — per
//! candidate CAM++ model, prints the evidence trail (CV-scored sweep, angle
//! joins, off-screen suspects, disagreements), fuses the first model into
//! the analysis exactly as `Job::AnalyzeSpeakers` does, and diffs the
//! integrated plan against the mouth-only baseline. Lanes append to the CSV;
//! `YC_INTEG_RENDER=1` renders the integrated plan to
//! `../diar_integration.mp4` (the ADR 0042 integration gate artifact). Also:
//!
//!   … speaker_diag --features face -- selftest <wav> <wav> [<wav>…]
//!
//! embeds whole 16 kHz wavs and prints their pairwise cosine similarity —
//! run it on known same/different-speaker recordings to validate the fbank +
//! embedding path end-to-end before trusting fixture numbers.

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
    let first = args.next().expect("data dir (…/<stream>/data), or: selftest <wav> <wav>…");
    if first == "selftest" {
        let wavs: Vec<String> = args.collect();
        return voice_selftest(&wavs);
    }
    let data_dir = PathBuf::from(first);
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
    let mut analysis =
        SpeakerAnalysis { bin_s, tracks, voiced, speaking, confidence, voice: None };

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

    // The MOUTH-ONLY plan — the pre-integration camera, kept as the printed
    // baseline the integrated plan is diffed against below.
    let baseline_plan = speaker::plan_shots(&analysis, src_w, src_h, dur, &cuts);

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

    // --- voice lanes (ADR 0042, INTEGRATED): the PRODUCTION functions —
    // yc_frame::voice::{embed_windows, build_lane, fuse_attribution} — run
    // here on the same inputs, so what this harness measures IS the analysis
    // Job::AnalyzeSpeakers ships. Candidate models A/B beside the production
    // one; a missing file skips its lane so the visual forensics still print.
    println!("\n== voice lanes (production path):");
    let attribution = speaker::attribution_regime(&analysis);
    let mut lanes: Vec<(&'static str, yc_frame::voice::VoiceLane)> = Vec::new();
    for (i, &(tag, path, scale, cmn)) in VOICE_MODELS.iter().enumerate() {
        let model = Path::new(path);
        if !model.is_file() {
            println!("  [{tag}] missing {} — lane skipped", model.display());
            continue;
        }
        let t0 = std::time::Instant::now();
        let (embs, kept) = match yc_frame::voice::embed_windows(
            model,
            scale,
            cmn,
            &samples,
            &analysis.voiced,
            bin_s,
        ) {
            Ok(v) => v,
            Err(e) => {
                println!("  [{tag}] FAILED: {e:#}");
                continue;
            }
        };
        println!(
            "  [{tag}] {} windows ({:.2}s / {:.2}s hop) embedded in {:.1}s",
            embs.len(),
            yc_frame::voice::WIN_S,
            yc_frame::voice::HOP_S,
            t0.elapsed().as_secs_f32()
        );
        let Some((mut lane, diag)) =
            yc_frame::voice::build_lane(&embs, &kept, &analysis, &cuts, dur, attribution)
        else {
            println!("  [{tag}] only {} embeddable windows — lane skipped", embs.len());
            continue;
        };
        for l in &diag.lines {
            println!("  [{tag}] {l}");
        }
        // Window dump for offline digging (audio snippets, transcript overlay).
        if std::env::var_os("YC_VOICE_WINDOWS").is_some() {
            let mut wtxt = String::from("start_s,end_s,cluster,joined_seat\n");
            for &(s, e, c, j) in &diag.windows {
                wtxt.push_str(&format!("{s:.3},{e:.3},{c},{j}\n"));
            }
            let p = format!("{tag}_windows.csv");
            std::fs::write(&p, wtxt)?;
            println!("  [{tag}] window dump: {p}");
        }
        // The FIRST present model is the production lane: fuse it into the
        // analysis exactly as Job::AnalyzeSpeakers does — the plan below is
        // then the integrated production camera.
        if i == 0 {
            let (fspeak, fconf, overridden) =
                yc_frame::voice::fuse_attribution(&analysis, &lane.seat);
            print!("  [{tag}] fused attribution (voice tiebreak) switches:");
            let mut last: Option<usize> = None;
            for b in 0..n_bins {
                if let Some(id) = fspeak[b] {
                    if last != Some(id) {
                        print!(" {:.1}s->{}", b as f64 * bin_s, speaker::track_label(id));
                        last = Some(id);
                    }
                }
            }
            println!();
            println!(
                "  [{tag}] overridden: {:.1}s | off-screen: {:.1}s | lane {:.1}s @ {:.0}%",
                overridden.iter().filter(|o| **o).count() as f64 * bin_s,
                diag.offscreen_s,
                diag.claimed_s,
                100.0 * diag.agreement
            );
            lane.overridden = overridden;
            analysis.speaking = fspeak;
            analysis.confidence = fconf;
            analysis.voice = Some(lane.clone());
        }
        lanes.push((tag, lane));
    }

    // The INTEGRATED production plan: plan_shots itself applies the
    // off-screen splits and interjection rescues from analysis.voice. Every
    // forensic below — the shot list, the camera audit, the crop-stability
    // blocks, camera_diag.fg — runs on THIS plan.
    let plan = speaker::plan_shots(&analysis, src_w, src_h, dur, &cuts);
    let changed = plan
        .shots
        .iter()
        .filter(|s| {
            !baseline_plan.shots.iter().any(|p| {
                (p.start_s - s.start_s).abs() < 0.02
                    && (p.end_s - s.end_s).abs() < 0.02
                    && p.track == s.track
            })
        })
        .count();
    println!(
        "\n== integrated plan: {} shots ({} differ from the {}-shot mouth-only baseline)",
        plan.shots.len(),
        changed,
        baseline_plan.shots.len()
    );

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

    // --- camera plan audit: the jitter-class defect detector (camera creep /
    // re-frame without cause / subject adrift). ZERO findings on the
    // production fixtures is the regression bar; a finding means this plan
    // would render with a visible camera defect.
    let audit = speaker::audit_camera_plan(&analysis, &plan);
    if audit.is_empty() {
        println!("\n== camera audit: clean (0 findings)");
    } else {
        println!("\n== camera audit: {} FINDING(S) — this plan renders with visible camera defects:", audit.len());
        for f in &audit {
            println!("  ! {f}");
        }
    }

    // --- same-subject piece pairs (crop stability forensics): each solo shot
    // vs the PREVIOUS solo shot of the same track (adjacent or across runs) —
    // exactly the pairs a framing memory would act on. Δsubj is the span-median
    // face center/height; Δcrop is the planner's current output; sig compares
    // the spans' 60px seat-geometry signatures (reference only — leans are
    // known to split angles, ADR 0042). PAN pairs carry real motion and are
    // excluded from the summary distributions.
    println!("\n== same-subject piece pairs (crop stability forensics):");
    let span_bins = |s: &yc_core::Shot| -> (usize, usize) {
        let b0 = ((s.start_s / bin_s).round() as usize).min(n_bins);
        let b1 = ((s.end_s / bin_s).round() as usize).clamp(b0, n_bins);
        (b0, b1)
    };
    let med_of = |v: &mut Vec<f32>| -> f32 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    };
    let shot_subj = |tr: &SpeakerTrack, b0: usize, b1: usize| -> Option<(f32, f32, f32)> {
        let (mut xs, mut ys, mut hs) = (Vec::new(), Vec::new(), Vec::new());
        for b in b0..b1.min(n_bins) {
            if let Some(f) = tr.path.get(b).and_then(|p| p.as_ref()) {
                xs.push(f.cx());
                ys.push(f.cy());
                hs.push(f.h);
            }
        }
        if xs.is_empty() {
            return None;
        }
        Some((med_of(&mut xs), med_of(&mut ys), med_of(&mut hs)))
    };
    let span_sig = |b0: usize, b1: usize| -> String {
        let len = b1.saturating_sub(b0).max(1);
        let mut sig = String::new();
        for t in &analysis.tracks {
            let (mut xs, mut ys, mut hs) = (Vec::new(), Vec::new(), Vec::new());
            for b in b0..b1.min(n_bins) {
                if let Some(f) = t.path.get(b).and_then(|p| p.as_ref()) {
                    xs.push(f.cx());
                    ys.push(f.cy());
                    hs.push(f.h);
                }
            }
            if xs.len() * 5 < len * 2 {
                continue; // seat absent from this span (<40%)
            }
            sig.push_str(&format!(
                "{}:{},{},{};",
                t.id,
                (med_of(&mut xs) / 60.0).round() as i32,
                (med_of(&mut ys) / 60.0).round() as i32,
                (med_of(&mut hs) / 40.0).round() as i32
            ));
        }
        sig
    };
    // (Δcenter px, |Δface h| %, Δcrop pos px, |Δcrop h| %) per pair, grouped
    // by the signature verdict.
    let mut same_sig: Vec<(f32, f32, f32, f32)> = Vec::new();
    let mut diff_sig: Vec<(f32, f32, f32, f32)> = Vec::new();
    let mut last_solo: std::collections::HashMap<usize, usize> = Default::default();
    for (i, s) in plan.shots.iter().enumerate() {
        let (Some(id), yc_core::Layout::FullFrame { crop }) = (s.track, &s.layout) else {
            continue;
        };
        let Some(j) = last_solo.insert(id, i) else { continue };
        let p = &plan.shots[j];
        let yc_core::Layout::FullFrame { crop: pcrop } = &p.layout else { continue };
        let Some(tr) = analysis.tracks.iter().find(|t| t.id == id) else { continue };
        let (a0, a1) = span_bins(p);
        let (b0, b1) = span_bins(s);
        let (Some((ax, ay, ah)), Some((bx, by, bh))) =
            (shot_subj(tr, a0, a1), shot_subj(tr, b0, b1))
        else {
            continue;
        };
        let d_center = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
        let d_h_pct = 100.0 * (bh - ah) / ah.max(1.0);
        let d_crop_pos = ((crop.x - pcrop.x).powi(2) + (crop.y - pcrop.y).powi(2)).sqrt();
        let d_crop_h_pct = 100.0 * (crop.h - pcrop.h) / pcrop.h.max(1.0);
        let sig_same = span_sig(a0, a1) == span_sig(b0, b1);
        let pan = p.pan_to.is_some() || s.pan_to.is_some();
        println!(
            "  #{j}->#{i} {} gap {:>4.1}s | subj d({:+5.0},{:+5.0})={:>4.0}px dh {:+5.1}% | crop dpos {:>4.0}px dh {:+6.1}% | sig {}{}",
            speaker::track_label(id),
            s.start_s - p.end_s,
            bx - ax,
            by - ay,
            d_center,
            d_h_pct,
            d_crop_pos,
            d_crop_h_pct,
            if sig_same { "same" } else { "DIFF" },
            if pan { " | PAN" } else { "" }
        );
        if !pan {
            let row = (d_center, d_h_pct.abs(), d_crop_pos, d_crop_h_pct.abs());
            if sig_same {
                same_sig.push(row);
            } else {
                diff_sig.push(row);
            }
        }
    }
    let stats = |v: &[(f32, f32, f32, f32)], pick: fn(&(f32, f32, f32, f32)) -> f32| -> String {
        if v.is_empty() {
            return "-".into();
        }
        let mut xs: Vec<f32> = v.iter().map(pick).collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        format!("med {:.1} max {:.1}", xs[xs.len() / 2], xs[xs.len() - 1])
    };
    for (name, v) in [("same-sig", &same_sig), ("diff-sig", &diff_sig)] {
        println!(
            "  {name} pairs ({}): subj dcenter px [{}] |dh|% [{}] | crop dpos px [{}] |dh|% [{}]",
            v.len(),
            stats(v, |r| r.0),
            stats(v, |r| r.1),
            stats(v, |r| r.2),
            stats(v, |r| r.3)
        );
    }

    // --- best prior anchor per solo piece: what the framing memory would
    // actually find. For each solo shot, the prior same-track solo shot whose
    // subject geometry is closest (center px + 4x |dh| px) — small deltas mark
    // a RETURN to an already-framed camera (the reuse candidates), large ones
    // a first visit to a new angle. Fractions are of the anchor's face height
    // (fh) and crop width, the scale-free units a dead-zone would use.
    println!("\n== best prior anchor per solo piece (the memory's view):");
    let mut solos: Vec<(usize, usize, f32, f32, f32, &yc_core::Crop)> = Vec::new(); // (shot idx, track, cx, cy, fh, crop)
    for (i, s) in plan.shots.iter().enumerate() {
        let (Some(id), yc_core::Layout::FullFrame { crop }) = (s.track, &s.layout) else {
            continue;
        };
        let Some(tr) = analysis.tracks.iter().find(|t| t.id == id) else { continue };
        let (b0, b1) = span_bins(s);
        let Some((cx, cy, fh)) = shot_subj(tr, b0, b1) else { continue };
        let best = solos
            .iter()
            .filter(|(_, tid, ..)| *tid == id)
            .map(|(j, _, ax, ay, ah, ac)| {
                let dc = ((cx - ax).powi(2) + (cy - ay).powi(2)).sqrt();
                let dh = fh - ah;
                (dc + 4.0 * dh.abs(), *j, dc, dh, *ah, *ac)
            })
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((_, j, dc, dh, ah, ac)) = best {
            let dcrop_pos = ((crop.x - ac.x).powi(2) + (crop.y - ac.y).powi(2)).sqrt();
            println!(
                "  #{i} {} <- #{j}: subj dcenter {:>4.0}px ({:.2} fh, {:.2} crop-w) dh {:+5.1}% | crop would jump dpos {:>4.0}px dh {:+6.1}%{}",
                speaker::track_label(id),
                dc,
                dc / ah.max(1.0),
                dc / ac.w.max(1.0),
                100.0 * dh / ah.max(1.0),
                dcrop_pos,
                100.0 * (crop.h - ac.h) / ac.h.max(1.0),
                if plan.shots[i].pan_to.is_some() { " | PAN" } else { "" }
            );
        } else {
            println!(
                "  #{i} {}: first solo piece of its track (no prior anchor)",
                speaker::track_label(id)
            );
        }
        // A panning piece anchors its CLOSING crop (where the subject ended),
        // matching the production memory.
        solos.push((i, id, cx, cy, fh, plan.shots[i].pan_to.as_ref().unwrap_or(crop)));
    }

    // --- the real filtergraph (render it with export_args-style ffmpeg flags
    // to SEE this plan; clip.ass + fonts live in the data dir) ----------------
    let fg = data_dir.join("camera_diag.fg");
    std::fs::write(&fg, yc_render::build_camera_filtergraph(&plan, "clip.ass"))?;
    println!("\nfiltergraph: {}", fg.display());

    // Render THIS plan with the production export command. TWO env vars, two
    // output names, because past gate artifacts must never be clobbered
    // (their gates PASSED; the files are the record):
    //  - YC_SMOOTH_RENDER  -> ../camera_smoothing.mp4  (the camera-smoothing
    //    gate's name — NOTE: the plan now includes the voice behaviors, so
    //    re-rendering under this name overwrites the signed-off artifact;
    //    prefer YC_INTEG_RENDER unless reproducing that old gate on purpose)
    //  - YC_INTEG_RENDER   -> ../diar_integration.mp4  (the ADR 0042
    //    integration gate artifact, watched against diar_baseline.mp4)
    if std::env::var_os("YC_SMOOTH_RENDER").is_some() {
        let ffabs = std::fs::canonicalize(ffmpeg)?;
        let args = yc_render::export_args_script(
            Path::new("segment.mp4"),
            seek_s,
            dur,
            "camera_diag.fg",
            "../camera_smoothing.mp4",
        );
        println!("rendering ../camera_smoothing.mp4 ...");
        yc_render::run_export(&ffabs, &data_dir, &args, &|| false)?;
    }
    if std::env::var_os("YC_INTEG_RENDER").is_some() {
        let ffabs = std::fs::canonicalize(ffmpeg)?;
        let args = yc_render::export_args_script(
            Path::new("segment.mp4"),
            seek_s,
            dur,
            "camera_diag.fg",
            "../diar_integration.mp4",
        );
        println!("rendering ../diar_integration.mp4 ...");
        yc_render::run_export(&ffabs, &data_dir, &args, &|| false)?;
    }

    // --- per-bin CSV for deeper digging ---------------------------------------
    if let Some(csv) = csv_out {
        let mut w = String::from("t,voiced,speaker,conf");
        for t in &analysis.tracks {
            w.push_str(&format!(",{}x,{}y,{}act", t.id, t.id, t.id));
        }
        for (tag, _) in &lanes {
            w.push_str(&format!(",{0}_cluster,{0}_seat", tag));
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
            for (_, l) in &lanes {
                let c = l.cluster[b].map(|v| v as i64).unwrap_or(-1);
                let s = l.seat[b].map(|v| v as i64).unwrap_or(-1);
                w.push_str(&format!(",{c},{s}"));
            }
            w.push('\n');
        }
        std::fs::write(&csv, w)?;
        println!("\nper-bin csv: {}", csv.display());
    }
    Ok(())
}

/// The speaker-embedding models this harness runs (first = the PRODUCTION
/// lane, fused into the analysis; the rest are A/B candidates). Production
/// today: 3D-Speaker CAM++ zh_en
/// "advanced" — CAM++ is the fastest surveyed architecture on CPU — from the
/// sherpa-onnx `speaker-recongition-models` release (that typo is real),
/// SHA-256-verified against the release's published checksum.txt, Apache-2.0.
/// On the known-speaker selftest it separates cleanly (same speaker +0.60,
/// different +0.05). The other candidate, `wespeaker_en_voxceleb_CAM++_LM`,
/// FAILED that selftest under every documented convention (int16/unit scale,
/// CMN on/off — best case: same speaker +0.26 vs different +0.66) with the
/// identical fbank code, so it is rejected rather than A/B'd; if this lane
/// ever underwhelms, retry it with WeSpeaker's exact torchaudio frontend
/// (snip_edges=true, high_freq=nyquist) before blaming the weights.
#[cfg(feature = "face")]
const VOICE_MODELS: [(&str, &str, yc_frame::voice::SampleScale, bool); 1] = [
    // (tag, path, sample scale, apply per-window CMN) — the model's
    // reference convention, validated on the known-speaker selftest.
    (
        "3ds",
        "models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx",
        yc_frame::voice::SampleScale::Unit,
        true,
    ),
];

/// `selftest <wav> <wav> [<wav>…]`: embed whole 16 kHz mono wavs with every
/// present candidate model and print pairwise cosine similarity — ground
/// truth for the fbank + embedding path on known same/different speakers.
#[cfg(feature = "face")]
fn voice_selftest(wavs: &[String]) -> anyhow::Result<()> {
    use std::path::Path;
    use yc_core::TimeRange;
    use yc_frame::voice::VoiceEmbedder;
    anyhow::ensure!(wavs.len() >= 2, "selftest needs >= 2 wav paths (16 kHz mono)");
    let names: Vec<&str> =
        wavs.iter().map(|p| Path::new(p).file_stem().and_then(|s| s.to_str()).unwrap_or(p)).collect();
    for (tag, model, scale, cmn) in VOICE_MODELS {
        let model = Path::new(model);
        if !model.is_file() {
            println!("[{tag}] missing {} — skipped", model.display());
            continue;
        }
        let mut emb = VoiceEmbedder::load(model, scale, cmn)?;
        let mut embs: Vec<Vec<f32>> = Vec::new();
        for p in wavs {
            let s = yc_ingest::read_range_samples(
                Path::new(p),
                TimeRange { start_s: 0.0, end_s: 36_000.0 },
            )?;
            embs.push(emb.embed(&s)?);
        }
        println!("[{tag}] pairwise cosine similarity:");
        for i in 0..embs.len() {
            for j in i + 1..embs.len() {
                let cos: f32 = embs[i].iter().zip(embs[j].iter()).map(|(a, b)| a * b).sum();
                println!("  {} <-> {}: {cos:+.3}", names[i], names[j]);
            }
        }
    }
    Ok(())
}
