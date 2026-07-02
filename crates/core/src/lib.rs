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

/// The per-Creator choice of how a Clip's caption words are transcribed
/// (ADR 0035): single-decode whisper, or the Qwen ensemble's five-variant vote
/// (ADR 0034). A closed enum, deliberately NOT a model picker — the ensemble
/// recipe's constants are measured winners for Qwen3-ASR-1.7B; a new model
/// earns entry only through ADR 0034's gate. `Whisper` is the serde default so
/// every pre-picker creators.json loads unchanged (ADR 0033's opt-in contract
/// holds at the Creator level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionEngine {
    #[default]
    Whisper,
    QwenEnsemble,
}

impl CaptionEngine {
    /// Serde skip-guard: a Whisper (default) engine is not written, so a
    /// never-flipped Creator's record stays byte-identical to pre-picker files.
    pub fn is_default(&self) -> bool {
        *self == Self::Whisper
    }
}

/// A streamer whose VODs the operator clips with their permission. Carries
/// defaults remembered across that Creator's VODs (ADR 0016), persisted in the
/// global [`CreatorStore`]. Realizes the Creator scoping the per-stream output
/// folders (ADR 0015) introduced on disk. Only `language`, `default_caption_genre`
/// and `caption_engine` are applied today; the seam/crop defaults are recorded
/// `Option`s reserved for the Creator-aware framing slice (they tangle with M6
/// per-Segment auto-framing, ADR 0011), and serialize only when set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Creator {
    pub name: String,
    pub language: Language,
    /// Caption Style animation remembered for this Creator (ADR 0016): seeds the
    /// render's genre on the next import, updated to whatever the operator last
    /// rendered with. `None` until they render a clip for this Creator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_caption_genre: Option<CaptionGenre>,
    /// Caption engine for this Creator's renders (ADR 0035): whisper unless the
    /// operator deliberately flips them to the ensemble (no default flip — new
    /// Creators always start on Whisper). Seeds the import rail's picker and is
    /// saved back on each render like the genre; `YC_QWEN_ENS` remains a
    /// per-invocation override that is never written back here.
    #[serde(default, skip_serializing_if = "CaptionEngine::is_default")]
    pub caption_engine: CaptionEngine,
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
            caption_engine: CaptionEngine::default(),
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
    /// Where/how large this Clip's captions draw (ADR 0036). `None` — the
    /// default, and always in headless — keeps the built-in anchor and the
    /// style's size, so pre-editor renders are untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption_placement: Option<CaptionPlacement>,
    /// Padded video segment downloaded for this Clip (ADR 0001).
    pub segment_path: Option<PathBuf>,
    pub export_path: Option<PathBuf>,
}

/// Caption placement (ADR 0036): the per-Clip override of where the caption
/// block sits on the canvas and how large its text draws. Presentation data on
/// the Clip — never curation (a placement says nothing about words). Fractions
/// are of the canvas (`\an5` center anchor); `scale` multiplies the Caption
/// Style's font size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CaptionPlacement {
    pub x_frac: f32,
    pub y_frac: f32,
    pub scale: f32,
}

impl CaptionPlacement {
    /// The legal caption scale bounds, shared by the render's
    /// `resolve_placement` and the editor's scroll-resize so the preview and
    /// the burn-in can never disagree about how large a placement draws
    /// (ADR 0036's "size exact" promise).
    pub const SCALE_MIN: f32 = 0.3;
    pub const SCALE_MAX: f32 = 3.0;
}

impl Default for CaptionPlacement {
    /// The built-in anchor: centered, mid-gameplay height (`ass.rs`'s
    /// `CAPTION_Y_FRAC`), unscaled — `Some(default)` renders identically to
    /// `None`.
    fn default() -> Self {
        Self { x_frac: 0.5, y_frac: CAPTION_Y_FRAC, scale: 1.0 }
    }
}

/// Caption anchor as a fraction of canvas height: mid gameplay Panel (which
/// ends at the Seam, 0.62) — above the facecam face below, and clear of any
/// burned-in source subtitles near the bottom of the gameplay. The single
/// source of the default; `ass.rs` and [`CaptionPlacement::default`] both read
/// it.
pub const CAPTION_Y_FRAC: f32 = 0.46;

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
/// Saved per Creator, overridable per Clip in the editor's Caption panel.
/// The appearance fields added for the editor (outline / shadow / back box /
/// bold) default to the historical hardcoded ASS values, so a pre-editor
/// serialized style renders byte-identically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptionStyle {
    pub name: String,
    pub genre: CaptionGenre,
    pub font_family: String,
    pub font_size: u32,
    /// RGBA. Converted to ASS's BGR ordering inside the render crate only.
    pub primary_color: [u8; 4],
    pub accent_color: [u8; 4],
    /// Text outline thickness in ASS PlayRes pixels (the historical value was a
    /// hardcoded 6).
    #[serde(default = "default_outline")]
    pub outline: f32,
    /// Drop-shadow depth in ASS PlayRes pixels (historically a hardcoded 2).
    #[serde(default = "default_shadow")]
    pub shadow: f32,
    /// Outline colour, RGBA. The historical hardcoded outline was opaque black.
    #[serde(default = "default_outline_color")]
    pub outline_color: [u8; 4],
    /// Draw an opaque-ish box behind each line (ASS BorderStyle 3) — the
    /// TikTok/podcast "text on a card" look.
    #[serde(default)]
    pub back_box: bool,
    /// The box colour (RGBA; alpha is the box opacity) when `back_box` is set.
    #[serde(default = "default_back_color")]
    pub back_color: [u8; 4],
    /// Faux-bold the font (ASS `Bold`). Anton is single-weight, so this is the
    /// weight control the editor exposes.
    #[serde(default)]
    pub bold: bool,
}

fn default_outline() -> f32 {
    6.0
}
fn default_shadow() -> f32 {
    2.0
}
fn default_outline_color() -> [u8; 4] {
    [0, 0, 0, 255]
}
fn default_back_color() -> [u8; 4] {
    // Opacity 105 -> the historical hardcoded ASS BackColour `&H96000000`
    // (transparency 0x96 = 150), so a default style stays byte-identical.
    [0, 0, 0, 105]
}

impl CaptionStyle {
    /// The historical per-genre style (white text, brand-gold accent, Anton) —
    /// the pre-editor defaults every genre rendered with.
    pub fn for_genre(genre: CaptionGenre) -> Self {
        let (name, font_size) = match genre {
            CaptionGenre::HugeWord => ("Huge Word", 150),
            CaptionGenre::RollingPop => ("Rolling Pop", 96),
            CaptionGenre::KaraokeFill => ("Karaoke Fill", 96),
        };
        Self {
            name: name.into(),
            genre,
            font_family: "Anton".into(),
            font_size,
            primary_color: [255, 255, 255, 255],
            accent_color: [255, 209, 0, 255],
            outline: default_outline(),
            shadow: default_shadow(),
            outline_color: default_outline_color(),
            back_box: false,
            back_color: default_back_color(),
            bold: false,
        }
    }
}

/// How the editor frames a Clip (focus 2026-07 / podcast mode): who or what the
/// camera follows. `Manual` is the operator's own crop; the AI modes derive the
/// framing from detected faces and (for `ActiveSpeaker`) the speaker analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraMode {
    /// The operator's own static crop (drag / resize / zoom in the editor).
    #[default]
    Manual,
    /// A static centered 9:16 window.
    Center,
    /// A static crop framing the most persistent detected face.
    AutoFace,
    /// Follow whoever is talking: the speaker analysis drives a cut-based
    /// camera plan (recommended for podcasts).
    ActiveSpeaker,
    /// Frame every detected face at once (2 faces: a stacked split screen).
    Group,
}

/// One shot of a dynamic camera plan: a clip-relative time span framed by one
/// static [`Layout`] (human editors *cut* between podcast speakers; panning a
/// virtual camera across a static wide shot reads as amateur). `track` is the
/// speaker-track id the shot follows (`None` = a group shot).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shot {
    pub start_s: f64,
    pub end_s: f64,
    pub track: Option<usize>,
    pub layout: Layout,
}

/// A cut-based dynamic camera plan for one Clip: contiguous [`Shot`]s covering
/// `0..duration`. Rendered as a per-shot trim/crop concat (one ffmpeg pass).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CameraPlan {
    pub shots: Vec<Shot>,
}

impl CameraPlan {
    /// The shot covering clip-relative time `t`, if any.
    pub fn shot_at(&self, t: f64) -> Option<&Shot> {
        self.shots.iter().find(|s| s.start_s <= t && t < s.end_s)
    }

    /// Mutable [`Self::shot_at`], for the editor's click-to-retarget override.
    pub fn shot_at_mut(&mut self, t: f64) -> Option<&mut Shot> {
        self.shots.iter_mut().find(|s| s.start_s <= t && t < s.end_s)
    }
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

/// Wait on a spawned child, polling `should_cancel` (~20x/s); on cancel the
/// child is killed and `None` is returned (else `Some(exit_status)`). For
/// single-process children (ffmpeg extract / NVENC export) whose whole cost is
/// wall-clock: without this, hitting Cancel merely sets a flag and the encode
/// runs to completion before anything notices. The ingest `CancelToken` keeps
/// its registered-PID *tree*-kill for yt-dlp, which spawns grandchildren.
pub fn wait_killable(
    child: &mut std::process::Child,
    should_cancel: &dyn Fn() -> bool,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    loop {
        if should_cancel() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
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
    fn clip_without_placement_deserializes_and_stays_unstamped() {
        // Pre-ADR-0036 project.json has no caption_placement key: it must load
        // (serde default -> None) and re-serialize without inventing the key,
        // so old projects round-trip byte-stable modulo unrelated fields.
        let json = r#"{
            "id": 1, "moment_id": 2,
            "range": { "start_s": 0.0, "end_s": 10.0 },
            "layout": { "kind": "full_frame", "crop": { "x": 0.0, "y": 0.0, "w": 1920.0, "h": 1080.0 } },
            "caption_style": "Huge",
            "segment_path": null, "export_path": null
        }"#;
        let clip: Clip = serde_json::from_str(json).expect("old project.json loads");
        assert_eq!(clip.caption_placement, None);
        let out = serde_json::to_string(&clip).unwrap();
        assert!(!out.contains("caption_placement"), "None is skipped, not written: {out}");
    }

    #[test]
    fn caption_placement_round_trips_and_defaults_to_the_builtin_anchor() {
        let p = CaptionPlacement { x_frac: 0.5, y_frac: 0.72, scale: 1.4 };
        let clip = Clip {
            id: 1,
            moment_id: 2,
            range: TimeRange { start_s: 0.0, end_s: 10.0 },
            layout: Layout::FullFrame { crop: Crop { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 } },
            caption_style: "Huge".into(),
            caption_placement: Some(p),
            segment_path: None,
            export_path: None,
        };
        let back: Clip = serde_json::from_str(&serde_json::to_string(&clip).unwrap()).unwrap();
        assert_eq!(back.caption_placement, Some(p));
        // Some(default) must mean exactly the built-in anchor.
        let d = CaptionPlacement::default();
        assert_eq!((d.x_frac, d.y_frac, d.scale), (0.5, CAPTION_Y_FRAC, 1.0));
    }

    #[test]
    fn wait_killable_kills_a_running_child_on_cancel() {
        // A child that would run ~10 s; an already-cancelled wait must kill it
        // and return None promptly instead of letting it run to completion.
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "10", "127.0.0.1"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("10");
            c
        };
        let started = std::time::Instant::now();
        let mut child = cmd
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn test child");
        let outcome = wait_killable(&mut child, &|| true).expect("wait");
        assert!(outcome.is_none(), "cancelled wait reports None");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "child was killed, not waited out"
        );
        // And an un-cancelled wait returns the real exit status.
        let mut quick = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "exit 0"]);
            c
        } else {
            std::process::Command::new("true")
        };
        let mut child = quick.spawn().expect("spawn quick child");
        let status = wait_killable(&mut child, &|| false).expect("wait").expect("not cancelled");
        assert!(status.success());
    }

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
    fn caption_engine_defaults_to_whisper_and_only_serializes_when_flipped() {
        // Pre-picker creators.json has no caption_engine key: it must load as
        // Whisper (ADR 0035's no-default-flip) and re-serialize without
        // inventing the key, so a never-flipped record stays byte-identical.
        let partial = r#"{"creators":{"x":{"name":"x","language":"id"}}}"#;
        let back: CreatorStore = serde_json::from_str(partial).unwrap();
        assert_eq!(back.get("x").unwrap().caption_engine, CaptionEngine::Whisper);
        let json = serde_json::to_string(&back).unwrap();
        assert!(!json.contains("caption_engine"), "whisper is skipped, not written: {json}");
        // A flipped Creator round-trips the ensemble choice.
        let mut store = CreatorStore::default();
        let mut c = Creator::new("guntur".into(), Language::Id);
        c.caption_engine = CaptionEngine::QwenEnsemble;
        store.upsert(c);
        let json = serde_json::to_string(&store).unwrap();
        assert!(json.contains("\"caption_engine\":\"qwen_ensemble\""), "json: {json}");
        let back: CreatorStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back.get("guntur").unwrap().caption_engine, CaptionEngine::QwenEnsemble);
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
