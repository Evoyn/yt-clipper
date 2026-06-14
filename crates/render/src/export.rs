//! ffmpeg filtergraph + NVENC export (ADR 0004): one invocation crops/scales/
//! vstacks the Panels, burns the generated ASS via libass, and encodes with
//! h264_nvenc. The same filtergraph at reduced resolution will drive the M5
//! true preview, so this builder is the single source of the composite —
//! preview and export cannot disagree.

use anyhow::{Context, Result};
use std::path::Path;
use yc_core::{Crop, Layout, TimeRange, CANVAS_H, CANVAS_W};

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
/// drive colon or backslash.
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
                 [v]subtitles={ass_name}:fontsdir=.[out]",
                g = fmt_crop(gameplay),
                f = fmt_crop(facecam),
                w = CANVAS_W,
            )
        }
        Layout::FullFrame { gameplay } => format!(
            "[0:v]{g},scale={w}:{h},setsar=1[v];[v]subtitles={ass_name}:fontsdir=.[out]",
            g = fmt_crop(gameplay),
            w = CANVAS_W,
            h = CANVAS_H,
        ),
    }
}

/// ffmpeg args for the NVENC export. `-ss` before `-i` fast-seeks to the clip
/// start; `-t` bounds the output to the clip duration (frame-accurate under
/// re-encode). The burned ASS timeline is 0-based, matching the reset output
/// timeline produced by the seek.
pub fn export_args(
    source: &Path,
    range: TimeRange,
    filtergraph: &str,
    out_name: &str,
) -> Vec<String> {
    vec![
        "-ss".into(),
        format!("{:.3}", range.start_s),
        "-i".into(),
        source.display().to_string(),
        "-t".into(),
        format!("{:.3}", range.duration_s()),
        "-filter_complex".into(),
        filtergraph.into(),
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
pub fn run_export(ffmpeg: &Path, workdir: &Path, args: &[String]) -> Result<()> {
    let status = std::process::Command::new(ffmpeg)
        .current_dir(workdir)
        .args(args)
        .status()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
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
        assert!(g.contains("subtitles=clip.ass:fontsdir=."));
        // gameplay panel = round(1920*0.62)=1190; facecam = 1920-1190 = 730.
        assert!(g.contains("scale=1080:1190"), "graph: {g}");
        assert!(g.contains("scale=1080:730"), "graph: {g}");
    }

    #[test]
    fn export_seeks_before_input_and_uses_nvenc() {
        let args = export_args(
            Path::new("F:/v.mp4"),
            TimeRange { start_s: 12.5, end_s: 20.0 },
            "FG",
            "export.mp4",
        );
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        let i = args.iter().position(|a| a == "-i").unwrap();
        assert!(ss < i, "-ss must precede -i for fast seek");
        assert!(args.contains(&"h264_nvenc".to_string()));
        let t = args.iter().position(|a| a == "-t").unwrap();
        assert_eq!(args[t + 1], "7.500"); // duration = end - start
    }
}
