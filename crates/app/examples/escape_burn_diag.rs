//! Diag: the escape_ass burn gate (review-fixes slice 1, 2026-07-14). Emits a
//! REAL `generate_ass` document — auto caption units plus operator-style
//! manual captions carrying every ASS special (`{`, `}`, `\`, and the `c:\new`
//! backslash-recombination trap) — and burns one frame with the sidecar
//! ffmpeg + the production Anton font on the production 1080x1920 canvas,
//! mirroring the export's `subtitles=clip.ass:fontsdir=fonts` invocation.
//! The operator's eye on the frame is the gate: every caption must render
//! exactly as typed (braces visible, single backslash, no phantom line break).
//!
//!   cargo run -p yt-clipper --example escape_burn_diag -- <out-dir>
//!
//! Run from the repo root (finds `sidecars\ffmpeg.exe` + `assets\fonts`).

use std::path::PathBuf;
use yc_core::{
    CaptionGenre, CaptionPlacement, CaptionStyle, CaptionUnit, Language, ManualCaption, Transcript,
};

fn main() -> anyhow::Result<()> {
    let ffmpeg = PathBuf::from("sidecars/ffmpeg.exe");
    let font = PathBuf::from("assets/fonts/Anton-Regular.ttf");
    anyhow::ensure!(ffmpeg.is_file() && font.is_file(), "run from the repo root");
    let out_dir = PathBuf::from(
        std::env::args().nth(1).unwrap_or_else(|| "target/escape_burn".into()),
    );
    std::fs::create_dir_all(out_dir.join("fonts"))?;
    std::fs::copy(&font, out_dir.join("fonts").join("Anton-Regular.ttf"))?;

    // A small auto stream (proves the genre emitters escape too) + the
    // operator-typed manual stream with the gate text.
    let units = vec![
        CaptionUnit { text: "halo {dunia}".into(), start_s: 0.0, end_s: 1.5 },
        CaptionUnit { text: "a\\b".into(), start_s: 1.5, end_s: 3.0 },
    ];
    let transcript = Transcript { units, language: Language::Id };
    let manual = vec![
        ManualCaption {
            unit: CaptionUnit { text: "{test} \\ and a brace".into(), start_s: 0.0, end_s: 3.0 },
            placement: None,
        },
        ManualCaption {
            unit: CaptionUnit { text: "c:\\new folder".into(), start_s: 0.0, end_s: 3.0 },
            placement: Some(CaptionPlacement { x_frac: 0.5, y_frac: 0.25, scale: 1.0 }),
        },
    ];
    let style = CaptionStyle::for_genre(CaptionGenre::RollingPop);
    let ass = yc_render::generate_ass(&transcript, &style, None, &manual);
    std::fs::write(out_dir.join("clip.ass"), &ass)?;

    // The export's burn shape: relative ASS + fontsdir, cwd = workdir.
    let png = "escape_burn.png";
    let status = std::process::Command::new(ffmpeg.canonicalize()?)
        .current_dir(&out_dir)
        .args([
            "-y", "-hide_banner", "-loglevel", "warning",
            "-f", "lavfi", "-i", "color=c=0x1a1a2e:s=1080x1920:d=2",
            "-vf", "subtitles=clip.ass:fontsdir=fonts",
            // Sample at 0.5 s: the rolling-pop alpha reveal has fired (at t=0
            // every word is still \alpha&HFF& invisible).
            "-ss", "0.5", "-frames:v", "1", png,
        ])
        .status()?;
    anyhow::ensure!(status.success(), "burn failed");
    println!("burned: {}", out_dir.join(png).display());
    Ok(())
}
