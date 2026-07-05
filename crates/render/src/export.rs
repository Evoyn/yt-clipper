//! ffmpeg filtergraph + NVENC export (ADR 0004): one invocation crops/scales/
//! vstacks the Panels, burns the generated ASS via libass, and encodes with
//! h264_nvenc. The same filtergraph at reduced resolution will drive the M5
//! true preview, so this builder is the single source of the composite —
//! preview and export cannot disagree.

use anyhow::{Context, Result};
use std::path::Path;
use yc_core::{CameraPlan, Crop, Layout, NoConsole, Shot, CANVAS_H, CANVAS_W};

/// `crop=w:h:x:y` in source pixels. Dimensions floored to even numbers >= 2 so
/// the yuv420p encoder never sees an odd or zero-sized Panel.
fn fmt_crop(c: &Crop) -> String {
    let even = |v: f32, min: f32| ((v.round().max(min) as i64) / 2) * 2;
    format!(
        "crop={}:{}:{}:{}",
        even(c.w, 2.0),
        even(c.h, 2.0),
        c.x.round().max(0.0) as i64,
        c.y.round().max(0.0) as i64
    )
}

/// Build the `-filter_complex` graph compositing the Layout and burning
/// `ass_name`. `ass_name` is relative to ffmpeg's working dir: we run ffmpeg in
/// the clip folder so the `subtitles` filter never has to escape a Windows
/// drive colon or backslash. `fontsdir=fonts` points libass at a **fonts-only**
/// subdirectory (the caller copies the caption font there) so it never tries to
/// open the sibling intermediates (`analysis.wav`, `clip.ass`, `project.json`)
/// as fonts — the noisy `Error opening memory font` lines a flat `fontsdir=.`
/// produced. Relative, so it stays clear of the Windows drive-colon escaping too.
pub fn build_filtergraph(layout: &Layout, ass_name: &str) -> String {
    match layout {
        Layout::Stacked { seam, gameplay, facecam } => {
            let gh = ((CANVAS_H as f32 * seam).round().clamp(2.0, (CANVAS_H - 2) as f32) as i64 / 2)
                * 2;
            let fh = CANVAS_H as i64 - gh;
            format!(
                "[0:v]{g},scale={w}:{gh},setsar=1[g];\
                 [0:v]{f},scale={w}:{fh},setsar=1[f];\
                 [g][f]vstack=inputs=2[v];\
                 [v]subtitles={ass_name}:fontsdir=fonts[out]",
                g = fmt_crop(gameplay),
                f = fmt_crop(facecam),
                w = CANVAS_W,
            )
        }
        Layout::FullFrame { crop } => format!(
            "[0:v]{g},scale={w}:{h},setsar=1[v];[v]subtitles={ass_name}:fontsdir=fonts[out]",
            g = fmt_crop(crop),
            w = CANVAS_W,
            h = CANVAS_H,
        ),
    }
}

/// The crop/scale/vstack chain for one [`Layout`], writing into `[out_label]` —
/// the shared composite core of [`build_filtergraph`] (whole-clip, `[0:v]`) and
/// the per-shot chains of [`build_camera_filtergraph`] (each shot's trimmed
/// stream). No subtitles here; the caller burns them once at the end.
fn layout_chain(layout: &Layout, in_label: &str, out_label: &str) -> String {
    match layout {
        Layout::Stacked { seam, gameplay, facecam } => {
            let gh = ((CANVAS_H as f32 * seam).round().clamp(2.0, (CANVAS_H - 2) as f32) as i64 / 2)
                * 2;
            let fh = CANVAS_H as i64 - gh;
            format!(
                "[{i}]split=2[{o}ga][{o}fa];\
                 [{o}ga]{g},scale={w}:{gh},setsar=1[{o}g];\
                 [{o}fa]{f},scale={w}:{fh},setsar=1[{o}f];\
                 [{o}g][{o}f]vstack=inputs=2[{o}]",
                i = in_label,
                o = out_label,
                g = fmt_crop(gameplay),
                f = fmt_crop(facecam),
                w = CANVAS_W,
            )
        }
        Layout::FullFrame { crop } => format!(
            "[{i}]{g},scale={w}:{h},setsar=1[{o}]",
            i = in_label,
            o = out_label,
            g = fmt_crop(crop),
            w = CANVAS_W,
            h = CANVAS_H,
        ),
    }
}

/// Build the dynamic-camera filtergraph for a [`CameraPlan`] (podcast
/// active-speaker mode): each [`Shot`] trims its contiguous span off the input,
/// composites its own Layout (solo crop, or a stacked split for a group shot),
/// and the shots concat back into one 1080x1920 stream — **hard cuts** between
/// speakers, the way a human editor cuts. A solo shot whose subject drifted
/// carries a `pan_to`: its crop origin glides linearly across the shot (the
/// slow follow), still a single crop filter via time expressions. The ASS burn
/// runs once over the concatenated stream, so caption timing is untouched
/// (shots are contiguous and start at 0, exactly the whole-clip timeline).
///
/// The caller writes this to a script file and passes `-filter_complex_script`
/// (a many-shot graph outgrows a comfortable command line).
pub fn build_camera_filtergraph(plan: &CameraPlan, ass_name: &str) -> String {
    // Defensive: an empty plan degrades to a centered full-frame — callers
    // shouldn't send one, but a graph that fails to parse sinks the render.
    if plan.shots.is_empty() {
        let full = Layout::FullFrame {
            crop: Crop { x: 0.0, y: 0.0, w: CANVAS_W as f32, h: CANVAS_H as f32 },
        };
        return build_filtergraph(&full, ass_name);
    }
    let mut parts: Vec<String> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for (i, shot) in plan.shots.iter().enumerate() {
        // trim + setpts rebase each shot to its own 0, so concat re-joins them
        // into one continuous timeline identical to the source clip's.
        //
        // Boundaries print at full f64 precision (`{}` is shortest-round-trip),
        // NEVER rounded: a cut boundary is a real source-frame pts (scene
        // detection returns the first frame of the incoming shot), trim's start
        // is INCLUSIVE (keeps pts >= start) and its end EXCLUSIVE, so an exact
        // boundary hands every frame to exactly one shot, with the cut frame
        // opening the INCOMING shot. The old `{:.3}` rounded ~half of all cut
        // pts UP past the cut frame, which stranded that frame at the tail of
        // the OUTGOING shot — one frame of the new scene through the old
        // shot's crop (the operator's "empty seat" flash at cuts; measured on
        // the ANTITESA export: 7 of its 14 cuts flashed, exactly the 7 whose
        // pts rounded up, e.g. 13.302833 -> 13.303).
        parts.push(format!(
            "[0:v]trim=start={}:end={},setpts=PTS-STARTPTS[t{i}]",
            shot.start_s, shot.end_s
        ));
        parts.push(shot_chain(shot, &format!("t{i}"), &format!("s{i}")));
        labels.push(format!("[s{i}]"));
    }
    parts.push(format!(
        "{}concat=n={}:v=1:a=0[cat];[cat]subtitles={ass_name}:fontsdir=fonts[out]",
        labels.join(""),
        plan.shots.len(),
    ));
    parts.join(";")
}

/// The composite chain for one [`Shot`]: its Layout statically, or — for a
/// solo shot with a follow pan — a crop whose origin glides linearly from the
/// opening to the closing position across the shot. `t` is shot-relative
/// (each shot's `setpts` rebases to 0) and the crop size never changes (the
/// zoom must not breathe). The expressions are quoted and their commas
/// escaped, so the filtergraph parser passes them to the crop filter whole.
fn shot_chain(shot: &Shot, in_label: &str, out_label: &str) -> String {
    if let (Layout::FullFrame { crop }, Some(to)) = (&shot.layout, &shot.pan_to) {
        let dur = (shot.end_s - shot.start_s).max(0.001);
        let even = |v: f32| (((v.round().max(2.0)) as i64) / 2) * 2;
        let (x0, y0) = (crop.x.max(0.0), crop.y.max(0.0));
        return format!(
            "[{i}]crop={w}:{h}:x='{x0:.1}+({dx:.1})*min(t/{dur:.3}\\,1)':y='{y0:.1}+({dy:.1})*min(t/{dur:.3}\\,1)',scale={cw}:{ch},setsar=1[{o}]",
            i = in_label,
            o = out_label,
            w = even(crop.w),
            h = even(crop.h),
            dx = to.x.max(0.0) - x0,
            dy = to.y.max(0.0) - y0,
            cw = CANVAS_W,
            ch = CANVAS_H,
        );
    }
    layout_chain(&shot.layout, in_label, out_label)
}

/// ffmpeg args for the NVENC export. `-ss` before `-i` fast-seeks `seek_s` into
/// the source; `-t` bounds the output to `duration_s` (frame-accurate under
/// re-encode). The burned ASS timeline is 0-based, matching the reset output
/// timeline produced by the seek.
///
/// `seek_s` is decoupled from the Clip's VOD range because the two ingest paths
/// seek different sources: the M1 local file is the whole VOD, so `seek_s` is
/// the range start; the M2 Segment is a padded slice, so `seek_s` is the
/// in-segment offset (`range.start - segment_start`; see `yc_ingest`).
pub fn export_args(
    source: &Path,
    seek_s: f64,
    duration_s: f64,
    filtergraph: &str,
    out_name: &str,
) -> Vec<String> {
    export_args_inner(source, seek_s, duration_s, "-filter_complex", filtergraph, out_name)
}

/// [`export_args`] with the graph in a **script file** (`-filter_complex_script`,
/// relative to ffmpeg's working dir) instead of inline — the dynamic-camera
/// graph ([`build_camera_filtergraph`]) grows with its shot count and would
/// outgrow a comfortable command line.
pub fn export_args_script(
    source: &Path,
    seek_s: f64,
    duration_s: f64,
    script_name: &str,
    out_name: &str,
) -> Vec<String> {
    export_args_inner(source, seek_s, duration_s, "-filter_complex_script", script_name, out_name)
}

fn export_args_inner(
    source: &Path,
    seek_s: f64,
    duration_s: f64,
    graph_flag: &str,
    graph: &str,
    out_name: &str,
) -> Vec<String> {
    vec![
        "-ss".into(),
        format!("{seek_s:.3}"),
        "-i".into(),
        source.display().to_string(),
        "-t".into(),
        format!("{duration_s:.3}"),
        graph_flag.into(),
        graph.into(),
        "-map".into(),
        "[out]".into(),
        "-map".into(),
        "0:a:0".into(),
        "-c:v".into(),
        "h264_nvenc".into(),
        "-preset".into(),
        "p5".into(),
        "-rc".into(),
        "vbr".into(),
        "-cq".into(),
        "21".into(),
        "-b:v".into(),
        "0".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-movflags".into(),
        "+faststart".into(),
        "-y".into(),
        out_name.into(),
    ]
}

/// Run the export. ffmpeg runs with `workdir` as cwd so the relative ASS and
/// `fontsdir=.` resolve (and Windows filtergraph path-escaping is avoided).
/// `should_cancel` is polled while the encode runs: the NVENC export is the
/// longest single child the app spawns, and before this a Cancel merely set a
/// flag the worker read *after* the full encode finished.
pub fn run_export(
    ffmpeg: &Path,
    workdir: &Path,
    args: &[String],
    should_cancel: &dyn Fn() -> bool,
) -> Result<()> {
    let mut child = std::process::Command::new(ffmpeg)
        .no_console()
        .current_dir(workdir)
        .args(args)
        .spawn()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    let status = yc_core::wait_killable(&mut child, should_cancel)
        .context("waiting on ffmpeg export")?;
    let Some(status) = status else {
        anyhow::bail!("cancelled");
    };
    anyhow::ensure!(status.success(), "ffmpeg export failed ({status})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacked_graph_crops_scales_vstacks_and_burns() {
        let layout = Layout::Stacked {
            seam: 0.62,
            gameplay: Crop { x: 454.0, y: 0.0, w: 1012.0, h: 1080.0 },
            facecam: Crop { x: 1440.0, y: 810.0, w: 480.0, h: 270.0 },
        };
        let g = build_filtergraph(&layout, "clip.ass");
        assert_eq!(g.matches("crop=").count(), 2);
        assert!(g.contains("vstack=inputs=2"));
        assert!(g.contains("subtitles=clip.ass:fontsdir=fonts"));
        // gameplay panel = round(1920*0.62)=1190; facecam = 1920-1190 = 730.
        assert!(g.contains("scale=1080:1190"), "graph: {g}");
        assert!(g.contains("scale=1080:730"), "graph: {g}");
    }

    #[test]
    fn camera_graph_trims_composites_and_concats_shots() {
        use yc_core::Shot;
        let solo = |x: f32| Layout::FullFrame { crop: Crop { x, y: 0.0, w: 608.0, h: 1080.0 } };
        let split = Layout::Stacked {
            seam: 0.5,
            gameplay: Crop { x: 100.0, y: 200.0, w: 640.0, h: 568.0 },
            facecam: Crop { x: 1100.0, y: 200.0, w: 640.0, h: 568.0 },
        };
        let plan = CameraPlan {
            shots: vec![
                Shot { start_s: 0.0, end_s: 8.5, track: Some(0), layout: solo(100.0), pan_to: None },
                Shot { start_s: 8.5, end_s: 14.0, track: None, layout: split, pan_to: None },
                Shot { start_s: 14.0, end_s: 30.0, track: Some(1), layout: solo(1200.0), pan_to: None },
            ],
        };
        let g = build_camera_filtergraph(&plan, "clip.ass");
        // One trim per shot, contiguous and rebased, boundaries printed
        // shortest-round-trip (never rounded — rounding across a source frame's
        // pts strands that frame in the wrong shot: a 1-frame flash).
        assert_eq!(g.matches("trim=start=").count(), 3);
        assert!(g.contains("trim=start=0:end=8.5,"), "graph: {g}");
        assert!(g.contains("trim=start=8.5:end=14,"), "graph: {g}");
        assert!(g.contains("trim=start=14:end=30,"), "graph: {g}");
        assert!(g.contains("setpts=PTS-STARTPTS"));
        // The group shot splits its trimmed stream for the two panels.
        assert!(g.contains("split=2"), "group shot needs an explicit split: {g}");
        assert!(g.contains("vstack=inputs=2"));
        // Concat re-joins all three, then the ASS burns once.
        assert!(g.contains("concat=n=3:v=1:a=0"));
        assert_eq!(g.matches("subtitles=").count(), 1);
        assert!(g.contains("subtitles=clip.ass:fontsdir=fonts[out]"));
        // Every shot scales to the canvas.
        assert!(g.matches("scale=1080:1920").count() == 2, "solo shots: {g}");
        assert!(g.contains("scale=1080:960"), "split panels: {g}");
    }

    #[test]
    fn a_cut_frame_is_not_stranded_in_the_outgoing_shot() {
        // The operator's flash-at-a-cut: a real source-frame pts like 13.302833
        // rounds UP to 13.303 under the old `:.3`. `trim` start is inclusive
        // (keeps pts >= start), so a boundary of 13.303 fails `13.302833 >= start`
        // and drops that first new-scene frame into the OUTGOING shot — one
        // frame of the new scene through the old shot's crop. The boundary must
        // land AT or BELOW the frame's pts so it joins the INCOMING shot. Both
        // constants are measured cut pts from the ANTITESA production clip:
        // CUT_7DP also rounds up under a fixed `{:.6}` (0.0812889 -> 0.081289),
        // which only shortest-round-trip printing survives.
        use yc_core::Shot;
        let solo = |x: f32| Layout::FullFrame { crop: Crop { x, y: 0.0, w: 608.0, h: 1080.0 } };
        const CUT_7DP: f64 = 0.081_288_9;
        const CUT: f64 = 13.302_833;
        let plan = CameraPlan {
            shots: vec![
                Shot { start_s: 0.0, end_s: CUT_7DP, track: Some(1), layout: solo(1200.0), pan_to: None },
                Shot { start_s: CUT_7DP, end_s: CUT, track: Some(0), layout: solo(100.0), pan_to: None },
                Shot { start_s: CUT, end_s: 25.0, track: Some(1), layout: solo(1200.0), pan_to: None },
            ],
        };
        let g = build_camera_filtergraph(&plan, "clip.ass");
        // Each incoming shot's trim start must be <= its cut frame's pts (so
        // `pts >= start` keeps the frame) yet not reach back to the previous
        // frame (~41.7 ms earlier at 23.976 fps).
        let start_at = |nth: usize| {
            g.split("trim=start=")
                .nth(nth)
                .and_then(|s| s.split(':').next())
                .and_then(|s| s.parse::<f64>().ok())
                .expect("shot has a trim start")
        };
        for (nth, cut) in [(2, CUT_7DP), (3, CUT)] {
            let start = start_at(nth);
            assert!(
                start <= cut,
                "incoming trim start {start} must not exceed the cut frame pts {cut} (graph: {g})"
            );
            assert!(start > cut - 0.041, "and must not reach the previous frame (graph: {g})");
        }
    }

    #[test]
    fn empty_camera_plan_degrades_to_a_static_full_frame() {
        let g = build_camera_filtergraph(&CameraPlan::default(), "clip.ass");
        assert!(g.contains("subtitles=clip.ass"));
        assert!(!g.contains("concat"));
    }

    #[test]
    fn follow_shot_pans_the_crop_origin_across_the_shot() {
        let plan = CameraPlan {
            shots: vec![Shot {
                start_s: 2.0,
                end_s: 10.0,
                track: Some(0),
                layout: Layout::FullFrame {
                    crop: Crop { x: 100.0, y: 40.0, w: 452.0, h: 802.0 },
                },
                pan_to: Some(Crop { x: 220.0, y: 40.0, w: 452.0, h: 802.0 }),
            }],
        };
        let g = build_camera_filtergraph(&plan, "clip.ass");
        // Same-size crop, origin gliding over the 8 s shot; commas escaped so
        // the expression survives the filtergraph parser.
        assert!(g.contains("crop=452:802:x='100.0+(120.0)*min(t/8.000\\,1)'"), "graph: {g}");
        assert!(g.contains(":y='40.0+(0.0)*min(t/8.000\\,1)'"), "graph: {g}");
        // A pan shot still scales to the canvas and burns once after concat.
        assert!(g.contains("scale=1080:1920"));
        assert_eq!(g.matches("subtitles=").count(), 1);
    }

    #[test]
    fn static_shots_keep_the_plain_crop() {
        let plan = CameraPlan {
            shots: vec![Shot {
                start_s: 0.0,
                end_s: 5.0,
                track: Some(0),
                layout: Layout::FullFrame {
                    crop: Crop { x: 380.0, y: 0.0, w: 452.0, h: 802.0 },
                },
                pan_to: None,
            }],
        };
        let g = build_camera_filtergraph(&plan, "clip.ass");
        assert!(g.contains("crop=452:802:380:0"), "graph: {g}");
        assert!(!g.contains("min(t/"), "no expression on a static shot: {g}");
    }

    #[test]
    fn export_args_script_uses_the_script_flag() {
        let args = export_args_script(Path::new("F:/seg.mp4"), 1.0, 30.0, "camera.fg", "o.mp4");
        let f = args.iter().position(|a| a == "-filter_complex_script").unwrap();
        assert_eq!(args[f + 1], "camera.fg");
        assert!(!args.contains(&"-filter_complex".to_string()));
        assert!(args.contains(&"h264_nvenc".to_string()));
    }

    #[test]
    fn export_seeks_before_input_and_uses_nvenc() {
        // M2 promote: seek the in-segment offset (2.0s), not the VOD range start.
        let args = export_args(Path::new("F:/segment.mp4"), 2.0, 7.5, "FG", "export.mp4");
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        let i = args.iter().position(|a| a == "-i").unwrap();
        assert!(ss < i, "-ss must precede -i for fast seek");
        assert_eq!(args[ss + 1], "2.000"); // seek = in-segment offset
        assert!(args.contains(&"h264_nvenc".to_string()));
        let t = args.iter().position(|a| a == "-t").unwrap();
        assert_eq!(args[t + 1], "7.500"); // -t bounds the output to the clip duration
    }
}
