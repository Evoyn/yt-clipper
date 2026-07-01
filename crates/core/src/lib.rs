//! Domain model for yt-clipper. Type names mirror CONTEXT.md exactly;
//! if a name here drifts from the glossary, the glossary wins.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// A streamer whose VODs the operator clips with their permission. Carries
/// defaults remembered across that Creator's VODs (ADR 0016), persisted in the
/// global [`CreatorStore`]. Realizes the Creator scoping the per-stream output
/// folders (ADR 0015) introduced on disk. Only `language` + `default_caption_genre`
/// are applied today; the seam/crop defaults are recorded `Option`s reserved for
/// the Creator-aware framing slice (they tangle with M6 per-Segment auto-framing,
/// ADR 0011), and serialize only when set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Creator {
    pub name: String,
    pub language: Language,
    /// Caption Style animation remembered for this Creator (ADR 0016): seeds the
    /// render's genre on the next import, updated to whatever the operator last
    /// rendered with. `None` until they render a clip for this Creator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_caption_genre: Option<CaptionGenre>,
    /// Default Seam position for stacked Layouts (fraction of canvas height).
    /// Reserved — not applied yet (Creator-aware framing slice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_seam: Option<f32>,
    /// Reserved — not applied yet (tangles with M6 per-Segment facecam detection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_gameplay_crop: Option<Crop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_facecam_crop: Option<Crop>,
}

impl Creator {
    /// A fresh record for `name`/`language` with no remembered defaults yet.
    pub fn new(name: String, language: Language) -> Self {
        Self {
            name,
            language,
            default_caption_genre: None,
            default_seam: None,
            default_gameplay_crop: None,
            default_facecam_crop: None,
        }
    }
}

/// The global store of per-Creator defaults (ADR 0016), persisted as
/// `creators.json` at the workspace root — the long-deferred `creators.json`. A
/// map keyed by the Creator's name (as it comes from VOD metadata), so importing a
/// known Creator's VOD can seed the remembered settings. No database, by design
/// (mirrors `Project`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreatorStore {
    #[serde(default)]
    pub creators: HashMap<String, Creator>,
}

impl CreatorStore {
    /// Load the store, falling back to an empty one when the file is absent or
    /// unparseable (a bad or missing store never blocks importing or rendering;
    /// use [`Self::try_load`] to surface the error instead).
    pub fn load(path: &Path) -> Self {
        Self::try_load(path).unwrap_or_default()
    }

    /// Like [`Self::load`] but surfaces a parse/read error (absent file included),
    /// so a caller that wants to warn the operator can.
    pub fn try_load(path: &Path) -> Result<Self, ProjectError> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        Ok(write_atomic(path, &serde_json::to_string_pretty(self)?)?)
    }

    /// The remembered record for `name`, if any.
    pub fn get(&self, name: &str) -> Option<&Creator> {
        self.creators.get(name)
    }

    /// Insert or replace the record for `creator.name`.
    pub fn upsert(&mut self, creator: Creator) {
        self.creators.insert(creator.name.clone(), creator);
    }
}

/// One Moment's review notes: the transcript text and the LLM judgment reason
/// (ADR 0010) shown in the review panel. Persisted as part of [`ReviewCache`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MomentNote {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub transcript: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub llm_reason: String,
}

/// Per-Moment review notes persisted as `data/review.json` beside `project.json`
/// (M8). Detection produces the transcripts + LLM reasons but the lean `Moment`
/// records don't carry them, so re-importing a VOD would lose the review text; this
/// sidecar restores it without re-transcribing. Keyed by Moment id (1..N by rank,
/// stable until the next Detect, which rewrites the whole file). A missing/bad
/// file yields an empty cache — a re-Detect refills it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewCache {
    #[serde(default)]
    pub notes: HashMap<u64, MomentNote>,
}

impl ReviewCache {
    /// Load the cache, falling back to empty when the file is absent/unparseable.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        Ok(write_atomic(path, &serde_json::to_string_pretty(self)?)?)
    }
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
/// word; for JA it is a fixed-size character chunk (whisper emits JA without
/// inter-word spaces). Produced by the language-aware grouping layer in
/// `yc-transcribe`, consumed by the ASS generator in `yc-render` (ADR 0003/0004).
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
    /// Emotional activation of the streamer's voice (speech-emotion arousal),
    /// z-scored across the candidate set during refine (ADR 0008).
    pub arousal: Option<f32>,
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
    /// A catchy short-form title for this Moment, generated by the LLM judge at
    /// detect time from the transcript (ADR 0015) and used to name the rendered
    /// Short. `None` for a manually-marked Moment (no detect-time pass) — the
    /// render then falls back to a timestamp-based name. `#[serde(default)]` so a
    /// pre-0015 `project.json` (no title) still loads.
    #[serde(default)]
    pub title: Option<String>,
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
    /// A single Panel filling the canvas: the gameplay, or the Facecam alone
    /// during a talking-session Moment (ADR 0011). The field is `crop`, not
    /// `gameplay`, because full-frame is no longer gameplay-only.
    FullFrame { crop: Crop },
}

/// The operator's explicit choice of how to frame a Clip (ADR 0017), overriding
/// M6 auto-detect. `Auto` keeps the ADR 0011 three-way decision; the others
/// force that Layout (still using the detected Facecam when one is found). A
/// global session / per-invocation setting, defaulting to `Auto`; the forcing
/// itself is pure (`yc_frame::decide_layout_with_pref`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutPref {
    /// Let M6 auto-detect pick stacked / full-cam / full-frame (ADR 0011).
    #[default]
    Auto,
    /// Always stacked: gameplay Panel above facecam Panel.
    Stacked,
    /// Always full-frame on the Facecam (talking-session framing).
    FullCam,
    /// Always full-frame gameplay.
    FullGameplay,
}

/// Spawn child processes without flashing a console window on Windows
/// (`CREATE_NO_WINDOW`). The release GUI is a `windows_subsystem = "windows"`
/// binary with no console of its own, so every ffmpeg / ffprobe / yt-dlp / deno /
/// sidecar child would otherwise pop *its own* console window mid-render — a jarring
/// flicker the operator flagged. Call `.no_console()` on a `Command` before
/// spawning. A no-op off Windows (and harmless in the debug console build, where
/// the child simply runs without a console; its inherited stdio still reaches the
/// terminal).
pub trait NoConsole {
    fn no_console(&mut self) -> &mut Self;
}

impl NoConsole for std::process::Command {
    #[cfg(windows)]
    fn no_console(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW (winbase.h): the child runs with no console window.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        self.creation_flags(CREATE_NO_WINDOW)
    }
    #[cfg(not(windows))]
    fn no_console(&mut self) -> &mut Self {
        self
    }
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

impl ProjectError {
    /// True when the underlying cause is "the file does not exist" — the one
    /// load failure that is *normal* (a fresh workspace) rather than data at
    /// risk. Write paths use this to distinguish "start a new store" from "the
    /// store exists but didn't parse — do NOT overwrite it".
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
    }
}

/// Write `contents` to `path` via a same-directory temp file + rename, so a
/// crash / kill / power-loss mid-write can never leave a truncated file. Every
/// persisted store (project.json, creators.json, review.json, the dialect
/// stores) loads lossily — a bad file reads as empty so nothing blocks a
/// render — which turns a torn plain `fs::write` into silent total data loss on
/// the next save-through. The temp name appends `.tmp` (never collides with the
/// `.{lang}.json` suffix scans); rename on the same volume replaces the target
/// atomically.
pub fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

impl Project {
    pub fn new(vod: Vod) -> Self {
        Self { vod, moments: Vec::new(), clips: Vec::new() }
    }

    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        Ok(write_atomic(path, &serde_json::to_string_pretty(self)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_existing_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join("yc_write_atomic_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.json");
        write_atomic(&path, "{\"v\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"v\":1}");
        // Overwrite an existing file (the every-render save-through path).
        write_atomic(&path, "{\"v\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"v\":2}");
        // The temp sibling is renamed away, not left to confuse suffix scans.
        assert!(!dir.join("store.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_console_is_chainable_and_builds() {
        // The shim must stay chainable on a Command without spawning (cross-platform:
        // a no-op off Windows, the CREATE_NO_WINDOW flag on it).
        let mut cmd = std::process::Command::new("yc-does-not-run");
        let _ = cmd.no_console().arg("x");
    }

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
    fn creator_store_roundtrips_and_upserts() {
        let mut store = CreatorStore::default();
        let mut c = Creator::new("Joddy Barat".into(), Language::Id);
        c.default_caption_genre = Some(CaptionGenre::KaraokeFill);
        store.upsert(c);
        // Round-trip through JSON.
        let json = serde_json::to_string(&store).unwrap();
        let back: CreatorStore = serde_json::from_str(&json).unwrap();
        let got = back.get("Joddy Barat").expect("creator present");
        assert_eq!(got.language, Language::Id);
        assert_eq!(got.default_caption_genre, Some(CaptionGenre::KaraokeFill));
        // Unknown name -> None.
        assert!(back.get("someone else").is_none());
        // Upsert replaces in place (one entry, updated genre).
        let mut store = back;
        let mut c2 = Creator::new("Joddy Barat".into(), Language::En);
        c2.default_caption_genre = Some(CaptionGenre::HugeWord);
        store.upsert(c2);
        assert_eq!(store.creators.len(), 1);
        assert_eq!(store.get("Joddy Barat").unwrap().language, Language::En);
        assert_eq!(
            store.get("Joddy Barat").unwrap().default_caption_genre,
            Some(CaptionGenre::HugeWord)
        );
    }

    #[test]
    fn creator_store_skips_unset_optional_defaults_in_json() {
        // A minimal record serializes without the reserved seam/crop fields (they
        // skip_serializing_if None), so creators.json stays clean until they are used.
        let mut store = CreatorStore::default();
        store.upsert(Creator::new("solo".into(), Language::Id));
        let json = serde_json::to_string(&store).unwrap();
        assert!(!json.contains("default_seam"), "json: {json}");
        assert!(!json.contains("default_gameplay_crop"), "json: {json}");
        // And a partial/older creators.json still loads (serde defaults fill in).
        let partial = r#"{"creators":{"x":{"name":"x","language":"id"}}}"#;
        let back: CreatorStore = serde_json::from_str(partial).unwrap();
        let c = back.get("x").unwrap();
        assert!(c.default_caption_genre.is_none() && c.default_seam.is_none());
    }

    #[test]
    fn review_cache_roundtrips_per_moment_notes() {
        let mut cache = ReviewCache::default();
        cache.notes.insert(
            1,
            MomentNote { transcript: "kaget banget gua".into(), llm_reason: "shock reaction".into() },
        );
        cache.notes.insert(2, MomentNote { transcript: "menu reading".into(), llm_reason: String::new() });
        let json = serde_json::to_string(&cache).unwrap();
        let back: ReviewCache = serde_json::from_str(&json).unwrap();
        assert_eq!(back.notes.len(), 2);
        assert_eq!(back.notes[&1].transcript, "kaget banget gua");
        assert_eq!(back.notes[&1].llm_reason, "shock reaction");
        // An empty llm_reason is skipped in JSON but loads back as empty.
        assert!(!json.contains("\"llm_reason\":\"\""));
        assert_eq!(back.notes[&2].llm_reason, "");
        // u64 keys round-trip (serde_json encodes integer map keys as strings).
        assert!(back.notes.contains_key(&2));
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
