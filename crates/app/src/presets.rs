//! Caption Style presets (focus 2026-07): named starting points the operator
//! picks in the editor's Caption panel, then customizes freely. Pure data over
//! [`CaptionStyle`] — the ASS generator renders whatever the fields say, so a
//! preset is nothing but a bundle of field values (ADR 0004's "styles are
//! data" holding for the whole appearance set).

use yc_core::{CaptionGenre, CaptionStyle};

/// The built-in presets, in display order. Names are the operator-facing
/// labels; each is a complete style (picking one replaces the whole style,
/// after which every field remains editable).
pub fn caption_presets() -> Vec<CaptionStyle> {
    vec![
        // The historical app default: rolling multi-word lines, white + gold.
        CaptionStyle { name: "Classic".into(), ..CaptionStyle::for_genre(CaptionGenre::RollingPop) },
        // Word-by-word text on a dark card — the TikTok auto-caption look.
        CaptionStyle {
            name: "TikTok".into(),
            font_size: 110,
            outline: 0.0,
            shadow: 0.0,
            back_box: true,
            back_color: [10, 10, 12, 215],
            ..CaptionStyle::for_genre(CaptionGenre::HugeWord)
        },
        // Calm multi-word lines on a soft card, sized for talk content.
        CaptionStyle {
            name: "Podcast".into(),
            font_size: 84,
            accent_color: [120, 200, 255, 255],
            outline: 0.0,
            shadow: 0.0,
            back_box: true,
            back_color: [0, 0, 0, 150],
            ..CaptionStyle::for_genre(CaptionGenre::RollingPop)
        },
        // Small, thin, quiet: caption furniture, not a caption show.
        CaptionStyle {
            name: "Minimal".into(),
            font_size: 64,
            outline: 2.0,
            shadow: 0.0,
            ..CaptionStyle::for_genre(CaptionGenre::RollingPop)
        },
        // Karaoke snap with a neon accent and a heavy outline over busy footage.
        CaptionStyle {
            name: "Gaming".into(),
            accent_color: [57, 255, 20, 255],
            outline: 7.0,
            shadow: 3.0,
            bold: true,
            ..CaptionStyle::for_genre(CaptionGenre::KaraokeFill)
        },
        // One huge yellow word, thick black edge — the MrBeast-style punch.
        CaptionStyle {
            name: "MrBeast".into(),
            primary_color: [255, 222, 33, 255],
            accent_color: [255, 255, 255, 255],
            outline: 9.0,
            shadow: 4.0,
            bold: true,
            ..CaptionStyle::for_genre(CaptionGenre::HugeWord)
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_complete_and_distinct() {
        let presets = caption_presets();
        assert_eq!(presets.len(), 6);
        let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Classic", "TikTok", "Podcast", "Minimal", "Gaming", "MrBeast"]);
        // Every preset must be renderable as-is (non-degenerate fields).
        for p in &presets {
            assert!(p.font_size >= 40, "{}: unreadably small", p.name);
            assert!(!p.font_family.is_empty());
            assert!(p.outline >= 0.0 && p.shadow >= 0.0);
        }
        // Classic must be exactly the historical default (no silent drift).
        assert_eq!(
            CaptionStyle { name: "Classic".into(), ..CaptionStyle::for_genre(CaptionGenre::RollingPop) },
            presets[0]
        );
    }

    #[test]
    fn boxed_presets_use_border_style_3_paths() {
        let presets = caption_presets();
        let tiktok = presets.iter().find(|p| p.name == "TikTok").unwrap();
        assert!(tiktok.back_box && tiktok.back_color[3] > 100, "opaque-ish card");
        let minimal = presets.iter().find(|p| p.name == "Minimal").unwrap();
        assert!(!minimal.back_box);
    }
}
