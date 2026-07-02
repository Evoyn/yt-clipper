//! ffmpeg filtergraph + NVENC export (ADR 0004): one invocation crops/scales/
//! vstacks the Panels, burns the generated ASS via libass, and encodes with
//! h264_nvenc. The same filtergraph at reduced resolution will drive the M5
//! true preview, so this builder is the single source of the composite —
//! preview and export cannot disagree.

use anyhow::{Context, Result};
use std::path::Path;
use yc_core::{CameraPlan, Crop, Layout, NoConsole, CANVAS_H, CANVAS_W};

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
/// composites its own static Layout (solo crop, or a stacked split for a group
/// shot), and the shots concat back into one 1080x1920 stream — **hard cuts**,
/// the way a human editor cuts between podcast speakers. The ASS burn runs once
/// over the concatenated stream, so caption timing is untouched (shots are
/// contiguous and start at 0, exactly the whole-clip timeline).
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
        parts.push(format!(
            "[0:v]trim=start={:.3}:end={:.3},setpts=PTS-STARTPTS[t{i}]",
            shot.start_s, shot.end_s
        ));
        parts.push(layout_chain(&shot.layout, &format!("t{i}"), &format!("s{i}")));
        labels.push(format!("[s{i}]"));
    }
    parts.push(format!(
        "{}concat=n={}:v=1:a=0[cat];[cat]subtitles={ass_name}:fontsdir=fonts[out]",
        labels.join(""),
        plan.shots.len(),
    ));
    parts.join(";")
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
                Shot { start_s: 0.0, end_s: 8.5, track: Some(0), layout: solo(100.0) },
                Shot { start_s: 8.5, end_s: 14.0, track: None, layout: split },
                Shot { start_s: 14.0, end_s: 30.0, track: Some(1), layout: solo(1200.0) },
            ],
        };
        let g = build_camera_filtergraph(&plan, "clip.ass");
        // One trim per shot, contiguous and rebased.
        assert_eq!(g.matches("trim=start=").count(), 3);
        assert!(g.contains("trim=start=0.000:end=8.500"));
        assert!(g.contains("trim=start=8.500:end=14.000"));
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
    fn empty_camera_plan_degrades_to_a_static_full_frame() {
        let g = build_camera_filtergraph(&CameraPlan::default(), "clip.ass");
        assert!(g.contains("subtitles=clip.ass"));
        assert!(!g.contains("concat"));
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
