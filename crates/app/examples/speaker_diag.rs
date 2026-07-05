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
//! The diarization spike (ADR 0042 gate) adds a **voice lane** per candidate
//! CAM++ model: voiced windows embedded and cosine-clustered, clusters joined
//! to seat tracks by co-occurrence, agreement/disagreements vs the
//! mouth-motion lane printed, lanes appended to the CSV. Also:
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

    // --- voice lanes (diarization spike, ADR 0042): embed voiced windows,
    // cosine-cluster them, join clusters to seat tracks by co-occurrence, and
    // score the voice lane against the mouth lane. Models are optional — a
    // missing file skips its lane so the visual forensics still print.
    println!("\n== voice lanes (diarization spike):");
    let windows = yc_frame::voice::plan_windows(&analysis.voiced, bin_s);
    println!(
        "  {} embed windows ({:.2}s / {:.2}s hop) over voiced spans",
        windows.len(),
        yc_frame::voice::WIN_S,
        yc_frame::voice::HOP_S
    );
    let mut lanes: Vec<VoiceLane> = Vec::new();
    for (tag, path, scale, cmn) in VOICE_MODELS {
        match build_voice_lane(
            tag,
            Path::new(path),
            scale,
            cmn,
            &samples,
            &windows,
            &analysis,
            n_bins,
            &cuts,
            dur,
            mean_faces >= 1.5,
        ) {
            Ok(Some(l)) => lanes.push(l),
            Ok(None) => {}
            Err(e) => println!("  [{tag}] FAILED: {e:#}"),
        }
    }

    // --- fused attribution (the ADR 0042 fusion rule, drafted here): voice
    // as margin tiebreak. Where a joined voice disagrees with the mouth lane
    // and the mouth cannot refute the voice's seat by its own switch margin,
    // the voice's seat takes the bin (same confirm hold); everywhere else the
    // mouth lane stands. Renders A/B with YC_VOICE_RENDER=1.
    if let Some(lane) = lanes.first() {
        let (fspeak, fconf) = fuse_attribution(&analysis, &lane.seat);
        print!("\n== fused attribution (voice tiebreak) switches:");
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
        let fused = SpeakerAnalysis {
            bin_s,
            tracks: analysis.tracks.clone(),
            voiced: analysis.voiced.clone(),
            speaking: fspeak,
            confidence: fconf,
        };
        let fplan = speaker::plan_shots(&fused, src_w, src_h, dur, &cuts);
        let changed = fplan
            .shots
            .iter()
            .filter(|s| {
                !plan.shots.iter().any(|p| {
                    (p.start_s - s.start_s).abs() < 0.02
                        && (p.end_s - s.end_s).abs() < 0.02
                        && p.track == s.track
                })
            })
            .count();
        println!("== fused plan: {} shots ({} differ from baseline):", fplan.shots.len(), changed);
        for s in &fplan.shots {
            let same = plan.shots.iter().any(|p| {
                (p.start_s - s.start_s).abs() < 0.02
                    && (p.end_s - s.end_s).abs() < 0.02
                    && p.track == s.track
            });
            let who = s.track.map(speaker::track_label).unwrap_or_else(|| "group".into());
            println!(
                "  {} {:>5.1}s..{:>5.1}s  {}",
                if same { " " } else { "*" },
                s.start_s,
                s.end_s,
                who
            );
        }
        let vfg = data_dir.join("camera_voice.fg");
        std::fs::write(&vfg, yc_render::build_camera_filtergraph(&fplan, "clip.ass"))?;
        println!("fused filtergraph: {}", vfg.display());

        // Off-screen override DEMO plan (the integration behavior, previewed
        // from the harness): a shot whose voiced time is mostly a KNOWN but
        // off-screen voice becomes the visible pair's split screen — podcast
        // grammar for "the speaker isn't in this shot". The production
        // planner is untouched; this render exists for the operator's gate.
        let off = &lane.offscreen;
        let mut dplan = fplan.clone();
        let mut flipped = 0usize;
        for s in dplan.shots.iter_mut() {
            let b0 = ((s.start_s / bin_s).round() as usize).min(n_bins);
            let b1 = ((s.end_s / bin_s).round() as usize).clamp(b0, n_bins);
            let voiced_n = (b0..b1).filter(|&b| analysis.voiced[b]).count();
            let off_n = (b0..b1).filter(|&b| off[b]).count();
            if voiced_n > 0 && off_n as f64 >= 0.5 * voiced_n as f64 && off_n as f64 * bin_s >= 1.2
            {
                s.track = None;
                s.pan_to = None;
                s.layout = speaker::group_layout_span(&analysis.tracks, b0, b1, src_w, src_h);
                flipped += 1;
                println!(
                    "== off-screen demo: shot {:.1}s..{:.1}s -> visible-pair split ({:.1}s of known off-screen voice)",
                    s.start_s,
                    s.end_s,
                    off_n as f64 * bin_s
                );
            }
        }
        if flipped > 0 {
            let ofg = data_dir.join("camera_offscreen.fg");
            std::fs::write(&ofg, yc_render::build_camera_filtergraph(&dplan, "clip.ass"))?;
            println!("off-screen demo filtergraph: {}", ofg.display());
        }

        // A/B render (the operator gate artifact), production export command
        // (NVENC + burned captions), outputs beside the stream folder's other
        // renders. B is the off-screen demo when the clip has one (the fused
        // plan rendered shot-identical to baseline on the Deddy fixture — the
        // min-shot grammar absorbs the corrected interjection), else the
        // fused plan.
        if std::env::var_os("YC_VOICE_RENDER").is_some() {
            let ffabs = std::fs::canonicalize(ffmpeg)?;
            let b_side = if flipped > 0 {
                ("camera_offscreen.fg", "../diar_offscreen_demo.mp4")
            } else {
                ("camera_voice.fg", "../diar_voice.mp4")
            };
            for (fg, out) in [("camera_diag.fg", "../diar_baseline.mp4"), b_side] {
                // ffmpeg runs with the data dir as cwd (clip.ass + fontsdir
                // resolve there), so the source is the bare segment name.
                let args =
                    yc_render::export_args_script(Path::new("segment.mp4"), seek_s, dur, fg, out);
                println!("rendering {out} ...");
                yc_render::run_export(&ffabs, &data_dir, &args, &|| false)?;
            }
            println!("A/B renders written beside the stream folder's other exports.");
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
        for l in &lanes {
            w.push_str(&format!(",{0}_cluster,{0}_seat", l.tag));
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
            for l in &lanes {
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

/// The speaker-embedding model (diarization spike): 3D-Speaker CAM++ zh_en
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

/// A voice-cluster must co-occur with a genuine mouth attribution for at
/// least this long (bins at SPEAKER_FPS) and win this share of its own
/// co-occurrence mass to join a seat; anything less stays unjoined (an
/// off-screen voice, an impure cluster, or a shared class like laughter).
/// Measured on the Deddy fixture: pure single-voice clusters co-occur with
/// their seat at 0.71-1.00 share, while the impure both-voices blob sat at
/// 0.55 — the floor lives in that gap.
#[cfg(feature = "face")]
const JOIN_MIN_BINS: usize = 24;
#[cfg(feature = "face")]
const JOIN_MIN_SHARE: f32 = 0.65;
/// Evidence floor for the per-angle-segment join (bins at SPEAKER_FPS): less
/// than the whole-clip floor because a segment is short, but still half a
/// second of co-occurrence before a voice claims a seat within one angle.
#[cfg(feature = "face")]
const JOIN_MIN_BINS_SEG: usize = 12;

/// One model's diarization result on the analysis grid: the raw voice
/// cluster per bin, and the seat it maps to through the co-occurrence join.
#[cfg(feature = "face")]
struct VoiceLane {
    tag: &'static str,
    cluster: Vec<Option<usize>>,
    seat: Vec<Option<usize>>,
    /// Bins where a KNOWN voice (joined somewhere) holds no seat in the
    /// on-screen angle — the off-screen-speaker signal (empty when the
    /// regime makes the whole-clip join valid).
    offscreen: Vec<bool>,
}

/// Build one model's voice lane and print its forensics: window count +
/// embed time, a threshold sweep scored end-to-end through the join
/// (coverage x agreement picks the cut), cluster-to-seat co-occurrence with
/// the join verdicts, agreement % vs the mouth lane, disagreement runs, and
/// the voice switch list.
#[cfg(feature = "face")]
#[allow(clippy::too_many_arguments)]
fn build_voice_lane(
    tag: &'static str,
    model: &std::path::Path,
    scale: yc_frame::voice::SampleScale,
    cmn: bool,
    samples: &[f32],
    windows: &[(f64, f64)],
    analysis: &yc_frame::speaker::SpeakerAnalysis,
    n_bins: usize,
    cuts: &[f64],
    dur: f64,
    attribution_regime: bool,
) -> anyhow::Result<Option<VoiceLane>> {
    use yc_frame::{speaker, voice};
    if !model.is_file() {
        println!("  [{tag}] missing {} — lane skipped", model.display());
        return Ok(None);
    }
    let bin_s = analysis.bin_s;
    let sr = yc_ingest::WHISPER_SR as f64;
    let t0 = std::time::Instant::now();
    let mut embedder = voice::VoiceEmbedder::load(model, scale, cmn)?;
    let mut embs: Vec<Vec<f32>> = Vec::new();
    let mut kept: Vec<(f64, f64)> = Vec::new();
    for &(s, e) in windows {
        let (i0, i1) = ((s * sr).round() as usize, ((e * sr).round() as usize).min(samples.len()));
        if i1 <= i0 || (i1 - i0) as f64 / sr < 0.25 {
            continue;
        }
        embs.push(embedder.embed(&samples[i0..i1])?);
        kept.push((s, e));
    }
    if embs.len() < 2 {
        println!("  [{tag}] only {} embeddable windows — lane skipped", embs.len());
        return Ok(None);
    }
    // Genuine mouth attribution per bin (not an off-screen hold) — the
    // join's and the scorer's reference lane.
    let genuine: Vec<Option<usize>> = (0..n_bins)
        .map(|b| {
            let id = analysis.speaking[b]?;
            let act = analysis
                .tracks
                .iter()
                .map(|t| t.activity.get(b).copied().unwrap_or(0.0))
                .fold(0.0f32, f32::max);
            (analysis.voiced[b] && act >= speaker::MIN_ACTIVITY).then_some(id)
        })
        .collect();
    // Per voiced bin: the nearest covering window's cluster.
    let bin_clusters = |assignment: &[usize]| -> Vec<Option<usize>> {
        (0..n_bins)
            .map(|b| {
                if !analysis.voiced.get(b).copied().unwrap_or(false) {
                    return None;
                }
                let t = (b as f64 + 0.5) * bin_s;
                let mut best: Option<(f64, usize)> = None;
                for (i, &(s, e)) in kept.iter().enumerate() {
                    if t >= s && t < e {
                        let d = (t - (s + e) * 0.5).abs();
                        if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                            best = Some((d, assignment[i]));
                        }
                    }
                }
                best.map(|(_, c)| c)
            })
            .collect()
    };
    // Join clusters to seats by co-occurrence on genuine-mouth bins (several
    // clusters may join one seat — an over-split voice is harmless, the join
    // reunifies it; an impure cluster joins nothing), restricted to bins the
    // `keep` filter admits so the join can be cross-validated.
    let join_on = |lane: &[Option<usize>], k: usize, keep: &dyn Fn(usize) -> bool| -> Vec<Option<usize>> {
        let mut counts = vec![std::collections::HashMap::<usize, usize>::new(); k];
        let mut totals = vec![0usize; k];
        for b in 0..n_bins {
            if !keep(b) {
                continue;
            }
            let (Some(c), Some(s)) = (lane[b], genuine[b]) else { continue };
            *counts[c].entry(s).or_default() += 1;
            totals[c] += 1;
        }
        (0..k)
            .map(|c| {
                let (&s, &n) = counts[c].iter().max_by_key(|(_, &n)| n)?;
                (totals[c] >= JOIN_MIN_BINS && n as f32 >= JOIN_MIN_SHARE * totals[c] as f32)
                    .then_some(s)
            })
            .collect()
    };
    let seat_lane = |lane: &[Option<usize>], joined: &[Option<usize>]| -> Vec<Option<usize>> {
        (0..n_bins).map(|b| lane[b].and_then(|c| joined[c])).collect()
    };
    // Agreement with the mouth lane where both claim; coverage = claimed time.
    let score = |seat: &[Option<usize>]| -> (usize, usize, usize) {
        let (mut both, mut agree, mut cov) = (0usize, 0usize, 0usize);
        for b in 0..n_bins {
            if seat[b].is_some() {
                cov += 1;
            }
            let (Some(v), Some(m)) = (seat[b], genuine[b]) else { continue };
            both += 1;
            if v == m {
                agree += 1;
            }
        }
        (cov, agree, both)
    };

    // Threshold sweep, scored OUT-OF-SAMPLE: the join is computed on
    // alternating 2 s blocks and the seat lane scored on the complementary
    // blocks (both directions). In-sample scoring is circular — with tiny
    // clusters every join copies the mouth lane on its own bins and "agrees"
    // 100% while carrying no identity (measured on the Deddy fixture: thr
    // 0.30 in-sample looked perfect and was pure overfit). A real voice
    // cluster joins the same seat from either half; an overfit singleton
    // claims nothing out-of-sample.
    let block = |b: usize| (b / 48) % 2 == 0; // 2 s blocks at 24 fps
    println!(
        "  [{tag}] {} windows embedded in {:.1}s | thr:  k joined  in-cov in-agr | cv-cov cv-agr  score",
        embs.len(),
        t0.elapsed().as_secs_f32()
    );
    let mut pick: Option<(f32, f64)> = None;
    for &t in &[0.30f32, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60] {
        let cl = voice::cluster_cosine(&embs, t);
        let lane = bin_clusters(&cl.assignment);
        let joined = join_on(&lane, cl.k, &|_| true);
        let seat = seat_lane(&lane, &joined);
        let (cov, agree, both) = score(&seat);
        // Cross-validated: even-block join claims odd blocks and vice versa.
        let join_even = join_on(&lane, cl.k, &|b| block(b));
        let join_odd = join_on(&lane, cl.k, &|b| !block(b));
        let seat_cv: Vec<Option<usize>> = (0..n_bins)
            .map(|b| {
                let j = if block(b) { &join_odd } else { &join_even };
                lane[b].and_then(|c| j[c])
            })
            .collect();
        let (cv_cov, cv_agree, cv_both) = score(&seat_cv);
        let cv_frac = cv_agree as f64 / cv_both.max(1) as f64;
        let s = cv_frac * cv_cov as f64 * bin_s;
        println!(
            "  [{tag}]     {t:.2}: {:>2} {:>6}  {:>5.1}s  {:>4.0}% | {:>5.1}s  {:>4.0}%  {s:>5.1}",
            cl.k,
            joined.iter().flatten().count(),
            cov as f64 * bin_s,
            100.0 * agree as f64 / both.max(1) as f64,
            cv_cov as f64 * bin_s,
            100.0 * cv_frac
        );
        if pick.map(|(_, ps)| s > ps).unwrap_or(true) {
            pick = Some((t, s));
        }
    }
    let thr = pick.map(|(t, _)| t).unwrap_or(0.45);
    println!("  [{tag}] picked thr {thr:.2} (best cv score)");
    let cl = voice::cluster_cosine(&embs, thr);
    let cluster = bin_clusters(&cl.assignment);
    let joined = join_on(&cluster, cl.k, &|_| true);

    // Detail rows for the picked threshold.
    let mut counts = vec![std::collections::HashMap::<usize, usize>::new(); cl.k];
    let mut totals = vec![0usize; cl.k];
    for b in 0..n_bins {
        let (Some(c), Some(s)) = (cluster[b], genuine[b]) else { continue };
        *counts[c].entry(s).or_default() += 1;
        totals[c] += 1;
    }
    println!("  [{tag}] cluster <-> seat co-occurrence (genuine-mouth bins):");
    for c in 0..cl.k {
        let n_windows = cl.assignment.iter().filter(|&&a| a == c).count();
        let voiced_s = cluster.iter().filter(|&&v| v == Some(c)).count() as f64 * bin_s;
        if voiced_s < 0.75 && joined[c].is_none() {
            continue; // singleton noise — not worth a row
        }
        let mut row: Vec<(usize, usize)> = counts[c].iter().map(|(&s, &n)| (s, n)).collect();
        row.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        let desc: Vec<String> = row
            .iter()
            .map(|(s, n)| format!("{} {:.1}s", speaker::track_label(*s), *n as f64 * bin_s))
            .collect();
        let verdict = match joined[c] {
            Some(s) => format!("-> {}", speaker::track_label(s)),
            None if totals[c] == 0 => "-> OFF-SCREEN? (never co-occurs with a moving mouth)".into(),
            None => "-> unjoined (impure or shared, e.g. laughter)".into(),
        };
        // Overlap forensic: how often BOTH mouths move during this cluster's
        // bins — a shared class (laughter, cross-talk) shows both mouths at
        // once, which no voice embedding can attribute to one person.
        let (mut vis, mut multi) = (0usize, 0usize);
        for b in 0..n_bins {
            if cluster[b] != Some(c) {
                continue;
            }
            vis += 1;
            let moving = analysis
                .tracks
                .iter()
                .filter(|t| t.activity.get(b).copied().unwrap_or(0.0) >= speaker::MIN_ACTIVITY)
                .count();
            if moving >= 2 {
                multi += 1;
            }
        }
        println!(
            "    V{c}: {n_windows} windows, {voiced_s:.1}s of voiced bins [{}] both-mouths {:.0}% {verdict}",
            desc.join(", "),
            100.0 * multi as f64 / vis.max(1) as f64
        );
    }
    // ANGLE-AWARE JOIN (measured necessity on the Deddy fixture): the same
    // screen seat holds DIFFERENT humans in different camera angles — the
    // source cuts between two-person angles of a 4+-person table, and the
    // same-seat merge welds a position's framings into one track (ADR 0038:
    // labels are seats, not identities; V2's windows sat on a green-shirted
    // man in one angle and a white-shirted man in another). A voice cluster
    // therefore joins a seat PER inter-cut segment; the whole-clip join is
    // only the fallback where a segment lacks evidence. The cluster itself
    // is the person; the per-segment map says which seat that person
    // occupies in the angle on screen (no seat = off-screen there).
    let mut seg_bounds: Vec<f64> = vec![0.0];
    for &c in cuts {
        if c > 0.03 && c < dur - 0.03 {
            seg_bounds.push(c);
        }
    }
    seg_bounds.push(dur);
    let n_segs = seg_bounds.len() - 1;
    let seg_of = |b: usize| -> usize {
        let t = (b as f64 + 0.5) * bin_s;
        seg_bounds.windows(2).position(|w| t >= w[0] && t < w[1]).unwrap_or(n_segs - 1)
    };
    // Group segments into ANGLES by seat geometry: jump cuts return to the
    // same camera over and over, and within one camera each seat's face sits
    // at the same position/size. Joining per (cluster, angle) accumulates
    // identity evidence across ALL of an angle's segments — so a claim at
    // one moment rests on other moments of the same camera, not only on the
    // mouth lane's opinion of the moment being judged (a purely per-segment
    // join just echoed the mouth lane: 99% "agreement" with no information).
    let seg_angle: Vec<usize> = {
        let mut sigs: Vec<String> = Vec::new();
        let mut ids: Vec<usize> = Vec::new();
        for g in 0..n_segs {
            let (b0, b1) = (
                (seg_bounds[g] / bin_s).round() as usize,
                ((seg_bounds[g + 1] / bin_s).round() as usize).min(n_bins),
            );
            let len = b1.saturating_sub(b0).max(1);
            let mut sig = String::new();
            for t in &analysis.tracks {
                let mut xs: Vec<f32> = Vec::new();
                let mut ys: Vec<f32> = Vec::new();
                let mut hs: Vec<f32> = Vec::new();
                for b in b0..b1 {
                    if let Some(f) = t.path.get(b).and_then(|p| p.as_ref()) {
                        xs.push(f.cx());
                        ys.push(f.cy());
                        hs.push(f.h);
                    }
                }
                if xs.len() * 5 < len * 2 {
                    continue; // seat absent from this camera (<40%)
                }
                let med = |v: &mut Vec<f32>| -> f32 {
                    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    v[v.len() / 2]
                };
                sig.push_str(&format!(
                    "{}:{},{},{};",
                    t.id,
                    (med(&mut xs) / 60.0).round() as i32,
                    (med(&mut ys) / 60.0).round() as i32,
                    (med(&mut hs) / 40.0).round() as i32
                ));
            }
            let id = sigs.iter().position(|s| *s == sig).unwrap_or_else(|| {
                sigs.push(sig.clone());
                sigs.len() - 1
            });
            ids.push(id);
        }
        ids
    };
    let n_angles = seg_angle.iter().copied().max().map(|m| m + 1).unwrap_or(1);
    let mut ang_counts = vec![std::collections::HashMap::<(usize, usize), usize>::new(); n_angles];
    let mut ang_totals = vec![std::collections::HashMap::<usize, usize>::new(); n_angles];
    for b in 0..n_bins {
        let (Some(c), Some(s)) = (cluster[b], genuine[b]) else { continue };
        let a = seg_angle[seg_of(b)];
        *ang_counts[a].entry((c, s)).or_default() += 1;
        *ang_totals[a].entry(c).or_default() += 1;
    }
    // A single-segment angle's co-occurrence is pure echo of the mouth lane
    // over that one stretch (it can never disagree with it, so it carries no
    // identity information) — only an angle seen 2+ times may override the
    // whole-clip join. On the Deddy fixture this is what lets the voice keep
    // saying "seat B" at 20.8s where the mouth lane held A for that entire
    // one-off segment (the strip shows B exclaiming).
    let ang_segments: Vec<usize> =
        (0..n_angles).map(|a| seg_angle.iter().filter(|&&x| x == a).count()).collect();
    let ang_join = |c: usize, a: usize| -> Option<usize> {
        if ang_segments[a] < 2 {
            return None;
        }
        let total = *ang_totals[a].get(&c)?;
        let (&(_, s), &n) = ang_counts[a]
            .iter()
            .filter(|((cc, _), _)| *cc == c)
            .max_by_key(|&(_, &n)| n)?;
        (total >= JOIN_MIN_BINS_SEG && n as f32 >= JOIN_MIN_SHARE * total as f32).then_some(s)
    };
    println!("  [{tag}] angles (segments grouped by seat geometry) + voice->seat per angle:");
    for a in 0..n_angles {
        let spans: Vec<String> = (0..n_segs)
            .filter(|&g| seg_angle[g] == a)
            .map(|g| format!("{:.1}-{:.1}", seg_bounds[g], seg_bounds[g + 1]))
            .collect();
        let items: Vec<String> = (0..cl.k)
            .filter_map(|c| ang_join(c, a).map(|s| format!("V{c}->{}", speaker::track_label(s))))
            .collect();
        println!(
            "    angle {a}: [{}]  {}",
            spans.join(" "),
            if items.is_empty() { "(no joined voice)".into() } else { items.join("  ") }
        );
    }
    // In the ATTRIBUTION regime with several camera angles, a seat track is a
    // SCREEN POSITION shared by different humans across angles (proven on the
    // Deddy fixture: V1's voice articulates as the left man of one angle and
    // is off-screen in another, where the left seat is a different person) —
    // so a whole-clip join must NOT leak across angles there. In the
    // follow-visible regime each track is one person's framing, so the
    // whole-clip join is the identity and stays.
    let ban_global = attribution_regime && n_angles > 1;
    let seat: Vec<Option<usize>> = (0..n_bins)
        .map(|b| {
            let c = cluster[b]?;
            let a = seg_angle[seg_of(b)];
            ang_join(c, a).or(if ban_global { None } else { joined[c] })
        })
        .collect();
    // Off-screen suspects: the voice is a KNOWN person (joined in some other
    // angle, or clip-wide) but holds no seat in the angle on screen — the
    // speaker the camera cannot show. The mouth lane can only mis-attribute
    // these (it holds a visible mouth); they are the "off-screen voice" gap
    // diarization exists to fill (ADR 0038).
    let mut offscreen = vec![false; n_bins];
    if ban_global {
        let known_elsewhere = |c: usize| -> bool {
            joined[c].is_some() || (0..n_angles).any(|a| ang_join(c, a).is_some())
        };
        for b in 0..n_bins {
            offscreen[b] =
                cluster[b].filter(|&c| seat[b].is_none() && known_elsewhere(c)).is_some();
        }
        println!("  [{tag}] off-screen suspects (known voice, no seat in the on-screen angle):");
        let mut b = 0usize;
        while b < n_bins {
            if !offscreen[b] {
                b += 1;
                continue;
            }
            let (s0, c0) = (b, cluster[b].unwrap());
            while b < n_bins && cluster[b] == Some(c0) && seat[b].is_none() {
                b += 1;
            }
            let dur_run = (b - s0) as f64 * bin_s;
            if dur_run >= 0.5 {
                println!(
                    "    {:>5.1}s..{:>5.1}s ({dur_run:.1}s): V{c0} speaks (mouth lane says {})",
                    s0 as f64 * bin_s,
                    b as f64 * bin_s,
                    analysis.speaking[s0.min(n_bins - 1)]
                        .map(speaker::track_label)
                        .unwrap_or_else(|| "nobody".into())
                );
            }
        }
    }
    let (cov, agree, both) = score(&seat);
    println!(
        "  [{tag}] agreement with mouth attribution: {:.0}% over {:.1}s co-claimed ({:.1}s claimed total)",
        100.0 * agree as f64 / both.max(1) as f64,
        both as f64 * bin_s,
        cov as f64 * bin_s
    );
    let mut b = 0usize;
    let mut printed = 0usize;
    while b < n_bins {
        let (Some(v), Some(m)) = (seat[b], genuine[b]) else {
            b += 1;
            continue;
        };
        if v == m {
            b += 1;
            continue;
        }
        let s0 = b;
        while b < n_bins && seat[b] == Some(v) && genuine[b] == Some(m) {
            b += 1;
        }
        let dur = (b - s0) as f64 * bin_s;
        if dur >= 0.4 && printed < 14 {
            println!(
                "    DISAGREE {:>5.1}s..{:>5.1}s ({dur:.1}s): mouth={} voice={}",
                s0 as f64 * bin_s,
                b as f64 * bin_s,
                speaker::track_label(m),
                speaker::track_label(v)
            );
            printed += 1;
        }
    }
    // Window dump for offline digging (audio snippets, transcript overlay).
    if std::env::var_os("YC_VOICE_WINDOWS").is_some() {
        let mut wtxt = String::from("start_s,end_s,cluster,joined_seat\n");
        for (i, &(s, e)) in kept.iter().enumerate() {
            let c = cl.assignment[i];
            wtxt.push_str(&format!(
                "{s:.3},{e:.3},{c},{}\n",
                joined[c].map(|v| v as i64).unwrap_or(-1)
            ));
        }
        let p = format!("{tag}_windows.csv");
        std::fs::write(&p, wtxt)?;
        println!("  [{tag}] window dump: {p}");
    }
    print!("  [{tag}] voice switches:");
    let mut last: Option<usize> = None;
    let mut n_sw = 0usize;
    for b in 0..n_bins {
        if let Some(v) = seat[b] {
            if last != Some(v) {
                print!(" {:.1}s->{}", b as f64 * bin_s, speaker::track_label(v));
                last = Some(v);
                n_sw += 1;
                if n_sw > 24 {
                    print!(" ...");
                    break;
                }
            }
        }
    }
    println!();
    Ok(Some(VoiceLane { tag, cluster, seat, offscreen }))
}

/// The ADR 0042 fusion rule, drafted where it can be measured: the mouth
/// lane stands wherever it can defend its bin by its own switch margin; a
/// JOINED voice seat that the mouth cannot refute by that margin takes the
/// bin instead, and a switch of the fused lane still needs the same
/// confirmation hold. The voice never replaces the visual join — it
/// tiebreaks it; bins where the voice claims nothing (shared/unjoined
/// clusters, silence) follow the mouth lane unchanged.
#[cfg(feature = "face")]
fn fuse_attribution(
    analysis: &yc_frame::speaker::SpeakerAnalysis,
    voice_seat: &[Option<usize>],
) -> (Vec<Option<usize>>, Vec<f32>) {
    use yc_frame::speaker;
    let n = analysis.speaking.len();
    // Mirrors SWITCH_MARGIN / SWITCH_CONFIRM_S in yc_frame::speaker (private
    // there; the values are pinned by its tests).
    let margin = 1.35f32;
    let confirm = (0.8 * speaker::SPEAKER_FPS).round() as usize;
    let act = |id: usize, b: usize| -> f32 {
        analysis
            .tracks
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.activity.get(b).copied())
            .unwrap_or(0.0)
    };
    // The mouth lane's own commitments pass through untouched — they already
    // went through the production margin + hold (an early draft re-held them
    // and VAD-gap resets pushed the mouth's legitimate 50.9s switch on the
    // Deddy fixture out to 69.2s). Only OVERRIDES hold: an override run
    // counts claimed bins (a breath does not reset it — voice windows
    // straddle breaths by construction) and commits RETROACTIVELY to its
    // start once it has lasted the confirm time. The hold exists to stop
    // flicker, not to shorten the interjection it rescues; the analysis is
    // offline, so back-filling is legitimate.
    let mut out: Vec<Option<usize>> = analysis.speaking.clone();
    let mut conf = analysis.confidence.clone();
    let mut challenger: Option<usize> = None;
    let mut run: Vec<usize> = Vec::new();
    for b in 0..n {
        if !analysis.voiced[b] {
            continue; // a breath neither advances nor resets an override run
        }
        let over = match (analysis.speaking[b], voice_seat[b]) {
            (Some(m), Some(v)) if m != v && act(m, b) < margin * act(v, b).max(speaker::MIN_ACTIVITY) => {
                Some(v) // the mouth cannot refute the voice by its own margin
            }
            _ => None,
        };
        match (over, challenger) {
            (Some(v), Some(c)) if v == c => {
                run.push(b);
                if run.len() >= confirm {
                    for &rb in &run {
                        out[rb] = Some(v);
                    }
                }
            }
            (Some(v), _) => {
                challenger = Some(v);
                run = vec![b];
            }
            (None, _) => {
                challenger = None;
                run.clear();
            }
        }
    }
    for b in 0..n {
        if out[b] != analysis.speaking[b] {
            if let Some(id) = out[b] {
                let total: f32 =
                    analysis.tracks.iter().map(|t| t.activity.get(b).copied().unwrap_or(0.0)).sum();
                conf[b] = if total > 0.0 { (act(id, b) / total).max(0.5) } else { 0.5 };
            }
        }
    }
    (out, conf)
}

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
