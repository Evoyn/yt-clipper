//! Domain model for yt-clipper. Type names mirror CONTEXT.md exactly;
//! if a name here drifts from the glossary, the glossary wins.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The output canvas is always 9:16.
pub const CANVAS_W: u32 = 1080;
pub const CANVAS_H: u32 = 1920;

/// Spoken language of a VOD. Pinned per-VOD rather than auto-detected,
/// because code-switched gaming speech defeats per-segment detection (ADR 0003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    En,
    Id,
    Ja,
}

/// A streamer whose VODs the operator clips with their permission.
/// Carries defaults applied to every VOD of theirs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Creator {
    pub name: String,
    pub language: Language,
    /// Default Seam position for stacked Layouts, as a fraction of canvas height.
    pub default_seam: f32,
    /// Name of the default Caption Style preset.
    pub default_caption_style: String,
    pub default_gameplay_crop: Option<Crop>,
    pub default_facecam_crop: Option<Crop>,
}

/// Where a VOD's media comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VodSource {
    YouTube { video_id: String },
    LocalFile { path: PathBuf },
}

/// The long source recording a project is built around — a finished stream
/// recording on YouTube, or a local video file. Always belongs to a Creator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vod {
    pub creator: String,
    pub title: String,
    pub source: VodSource,
    pub language: Language,
    pub duration_s: Option<f64>,
}

/// A half-open time range within the VOD, in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start_s: f64,
    pub end_s: f64,
}

impl TimeRange {
    pub fn duration_s(&self) -> f64 {
        (self.end_s - self.start_s).max(0.0)
    }
}

/// One animatable caption unit with timing relative to the start of the
/// transcribed range (0-based seconds). For EN/ID this is a space-delimited
/// word; for JA it is a character chunk (M6). Produced by the language-aware
/// grouping layer in `yc-transcribe`, consumed by the ASS generator in
/// `yc-render` (ADR 0003/0004).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptionUnit {
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// The ordered caption units for one transcribed range, tagged with the
/// language they were grouped for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub language: Language,
    pub units: Vec<CaptionUnit>,
}

/// Per-signal scores, stored unblended so the ranking formula can be retuned
/// without re-running analysis (ADR 0002).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    pub chat_rate: Option<f32>,
    pub loudness: Option<f32>,
    pub lexicon: Option<f32>,
    pub llm: Option<f32>,
}

/// A scored candidate time range within a VOD, surfaced by analysis
/// (or marked manually), awaiting review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Moment {
    pub id: u64,
    pub range: TimeRange,
    pub signals: Signals,
    pub score: f32,
}

/// The rectangle of source-video pixels a Panel displays, in source pixels.
/// Invariant: aspect-locked to its Panel — editor operations zoom and pan,
/// they never change the aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Crop {
    /// The largest sub-rectangle of `self` with the given width/height aspect
    /// ratio, centred inside `self`. Fits a source region to a Panel without
    /// stretching or letterboxing (CONTEXT.md: Panels fill edge-to-edge). The
    /// M1 hardcoded Layout uses it; the M5 framing editor reuses it.
    pub fn fit_to_aspect(&self, target_aspect: f32) -> Crop {
        if self.w / self.h > target_aspect {
            let w = self.h * target_aspect; // too wide: keep height, trim width
            Crop { x: self.x + (self.w - w) / 2.0, y: self.y, w, h: self.h }
        } else {
            let h = self.w / target_aspect; // too tall: keep width, trim height
            Crop { x: self.x, y: self.y + (self.h - h) / 2.0, w: self.w, h }
        }
    }
}

/// The arrangement of a Clip's 1080×1920 canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Layout {
    /// Gameplay Panel above facecam Panel, divided by the Seam
    /// (a fraction of canvas height).
    Stacked { seam: f32, gameplay: Crop, facecam: Crop },
    /// A single gameplay Panel filling the canvas (no facecam).
    FullFrame { gameplay: Crop },
}

/// A Moment the operator has promoted for production: it gets framing,
/// captions, and an export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: u64,
    pub moment_id: u64,
    /// Trim-adjusted range; starts as the Moment's range.
    pub range: TimeRange,
    pub layout: Layout,
    /// Name of the Caption Style preset used.
    pub caption_style: String,
    /// Padded video segment downloaded for this Clip (ADR 0001).
    pub segment_path: Option<PathBuf>,
    pub export_path: Option<PathBuf>,
}

/// The animation genre of a Caption Style. The ASS generator branches on
/// this; everything else about a style is data (ADR 0004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionGenre {
    RollingPop,
    HugeWord,
    KaraokeFill,
}

/// A named preset describing how captions look and animate.
/// Saved per Creator, overridable per Clip. (Full parameter set lands at M6.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptionStyle {
    pub name: String,
    pub genre: CaptionGenre,
    pub font_family: String,
    pub font_size: u32,
    /// RGBA. Converted to ASS's BGR ordering inside the render crate only.
    pub primary_color: [u8; 4],
    pub accent_color: [u8; 4],
}

/// Everything the app knows about one VOD: persisted as `project.json`
/// in that VOD's workspace folder. No database, by design.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub vod: Vod,
    pub moments: Vec<Moment>,
    pub clips: Vec<Clip>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

impl Project {
    pub fn new(vod: Vod) -> Self {
        Self { vod, moments: Vec::new(), clips: Vec::new() }
    }

    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        Ok(std::fs::write(path, serde_json::to_string_pretty(self)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_roundtrips_through_json() {
        let project = Project::new(Vod {
            creator: "test-creator".into(),
            title: "test vod".into(),
            source: VodSource::YouTube { video_id: "dQw4w9WgXcQ".into() },
            language: Language::Id,
            duration_s: Some(3600.0),
        });
        let json = serde_json::to_string(&project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.vod.title, "test vod");
        assert_eq!(back.vod.language, Language::Id);
    }

    #[test]
    fn fit_to_aspect_trims_width_of_a_wide_region() {
        // Full 1920x1080 source fit to a tall gameplay Panel (aspect 1080/1190).
        let full = Crop { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 };
        let panel_aspect = 1080.0 / 1190.0;
        let c = full.fit_to_aspect(panel_aspect);
        assert!((c.h - 1080.0).abs() < 0.01); // height preserved
        assert!((c.w / c.h - panel_aspect).abs() < 1e-4); // aspect matched
        assert!((c.x - (1920.0 - c.w) / 2.0).abs() < 0.01); // centred horizontally
        assert_eq!(c.y, 0.0);
    }

    #[test]
    fn fit_to_aspect_trims_height_of_a_tall_region() {
        let tall = Crop { x: 10.0, y: 20.0, w: 100.0, h: 400.0 };
        let c = tall.fit_to_aspect(2.0); // want w/h = 2
        assert!((c.w - 100.0).abs() < 0.01); // width preserved
        assert!((c.h - 50.0).abs() < 0.01); // 100 / 2
        assert_eq!(c.x, 10.0);
        assert!((c.y - (20.0 + (400.0 - 50.0) / 2.0)).abs() < 0.01); // centred vertically
    }
}
