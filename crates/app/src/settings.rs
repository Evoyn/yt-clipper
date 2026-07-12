//! App-level operator preferences, persisted across sessions in
//! `workspace/settings.json` (feature plan #7/#12). Distinct from the Creator
//! store (per-Creator render defaults, ADR 0016) and `project.json` (per-VOD):
//! these are knobs of the *app itself* — the master playback volume (which
//! shapes what the operator hears but never what renders) and their saved
//! caption-style presets (data the operator can APPLY to a render, but pure
//! preference until they do).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use yc_core::CaptionStyle;

/// One operator-saved caption look (feature plan #12): a complete
/// [`CaptionStyle`] under the operator's own name, listed beside the built-in
/// presets in the Studio's Caption panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserCaptionPreset {
    pub name: String,
    pub style: CaptionStyle,
}

/// The operator's app-level preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Master playback volume (linear rodio gain): 0.0..=2.0, default 1.0.
    /// Applies to every preview sink (Moment review and Studio playback);
    /// exports never read it.
    pub volume: f32,
    /// Saved caption-style presets, in the order they were saved.
    pub caption_presets: Vec<UserCaptionPreset>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self { volume: 1.0, caption_presets: Vec::new() }
    }
}

impl AppSettings {
    fn path(workspace: &Path) -> PathBuf {
        workspace.join("settings.json")
    }

    /// Read the settings file, defaulting every missing/invalid field — a
    /// hand-edited or older file never blocks startup.
    pub fn load(workspace: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(Self::path(workspace)) else {
            return Self::default();
        };
        match serde_json::from_str::<AppSettings>(&text) {
            Ok(mut s) => {
                s.volume = if s.volume.is_finite() { s.volume.clamp(0.0, 2.0) } else { 1.0 };
                s
            }
            Err(e) => {
                tracing::warn!("settings.json unreadable ({e}); using defaults");
                Self::default()
            }
        }
    }

    /// Write the settings file. Failure is soft (a read-only disk must not
    /// break playback) — it just won't persist.
    pub fn save(&self, workspace: &Path) {
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        let path = Self::path(workspace);
        if let Err(e) = std::fs::create_dir_all(workspace)
            .and_then(|()| std::fs::write(&path, format!("{json}\n")))
        {
            tracing::warn!("saving {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_default_on_garbage() {
        let dir = std::env::temp_dir().join(format!("yc-settings-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Missing file → defaults.
        assert_eq!(AppSettings::load(&dir).volume, 1.0);
        assert!(AppSettings::load(&dir).caption_presets.is_empty());
        // Round trip, presets included.
        let mut s = AppSettings { volume: 1.6, ..Default::default() };
        s.caption_presets.push(UserCaptionPreset {
            name: "My look".into(),
            style: CaptionStyle::for_genre(yc_core::CaptionGenre::RollingPop),
        });
        s.save(&dir);
        let back = AppSettings::load(&dir);
        assert_eq!(back, s);
        // Garbage file → defaults, no panic.
        std::fs::write(dir.join("settings.json"), "not json").unwrap();
        assert_eq!(AppSettings::load(&dir).volume, 1.0);
        // Out-of-range volume clamps; unknown fields are tolerated.
        std::fs::write(dir.join("settings.json"), r#"{"volume": 9.0, "future": 1}"#).unwrap();
        assert_eq!(AppSettings::load(&dir).volume, 2.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
