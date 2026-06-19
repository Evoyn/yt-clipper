//! ASS subtitle generation — the single home of caption animation logic
//! (ADR 0004). Caption Style presets are data; the animation `genre` selects
//! which builder runs (the full preset set is M6).
//!
//! Two genres so far:
//! - **Huge-word** (the M2 default): one word per caption — each unit is its own
//!   Dialogue event, appearing at its own spoken onset and clearing before the
//!   next word. Sync tracks the spoken word, and there is never more than one
//!   word on screen. EN/ID; JA character chunking is M6.
//! - **Rolling-pop** (M1): units grouped into on-screen lines by a character
//!   budget; each line is one Dialogue event in which every unit is laid out
//!   from the first frame but stays invisible until its spoken onset, when it
//!   fades in and scale-"pops". Units reveal in place rather than reflowing.

use yc_core::{CaptionGenre, CaptionStyle, CaptionUnit, Transcript, CANVAS_H, CANVAS_W};

/// Keep a completed line on screen this long after its last unit ends.
const LINE_HOLD_S: f64 = 0.5;
/// Max characters (including inter-unit spaces) on one on-screen line.
const MAX_LINE_CHARS: usize = 22;
/// Start a new line when the silence before a unit exceeds this, so words
/// spoken far apart never share one lingering line (which made captions appear
/// long before their later words were actually spoken).
const MAX_GAP_S: f64 = 1.0;
/// Caption anchor as a fraction of canvas height: mid gameplay Panel (which
/// ends at the Seam, 0.62) — above the facecam face below, and clear of any
/// burned-in source subtitles that sit near the bottom of the gameplay.
const CAPTION_Y_FRAC: f64 = 0.46;
/// Huge-word genre: a word shows from its onset until its `end_s` — which
/// [`refine_caption_timing`] sets to the streamer's actual vocal end from the
/// audio — clamped to a readable minimum and to the next word's onset (one word
/// at a time). Without refinement `end_s` is whisper's (often too-short) word end.
const WORD_MIN_S: f64 = 0.10;

/// Audio-driven caption timing ([`refine_caption_timing`]): RMS envelope at this
/// hop/window; the streamer's quiet/game floor is the `BASELINE_PCT` percentile of
/// energy; a word's caption ends once energy stays below
/// `baseline + VOICE_DROP_FRAC*(peak-baseline)` for `VOICE_SILENCE_S` (the
/// vocalization stopped), with the vocal peak sought within `PEAK_WINDOW_S` of onset.
const ENV_HOP_S: f64 = 0.02;
const ENV_WIN_S: f64 = 0.04;
const BASELINE_PCT: f64 = 0.25;
const VOICE_DROP_FRAC: f32 = 0.33;
const VOICE_SILENCE_S: f64 = 0.12;
const PEAK_WINDOW_S: f64 = 0.6;

/// RGBA (alpha = opacity) -> ASS `&HAABBGGRR`: bytes are ordered BGR and ASS
/// alpha is *transparency*, so 0x00 is opaque. This is the one place the
/// RGBA->BGR conversion CaptionStyle documents is allowed to live.
fn ass_color(rgba: [u8; 4]) -> String {
    let [r, g, b, a] = rgba;
    format!("&H{:02X}{:02X}{:02X}{:02X}", 255 - a, b, g, r)
}

/// Seconds -> ASS `H:MM:SS.cc` (centisecond precision).
fn ass_time(s: f64) -> String {
    let cs = (s.max(0.0) * 100.0).round() as u64;
    format!(
        "{}:{:02}:{:02}.{:02}",
        cs / 360_000,
        (cs / 6_000) % 60,
        (cs / 100) % 60,
        cs % 100
    )
}

/// Group units into on-screen lines, each at most `max_chars` wide (counting a
/// single space between adjacent units).
fn group_lines<'a>(t: &'a Transcript, max_chars: usize) -> Vec<Vec<&'a CaptionUnit>> {
    let mut lines: Vec<Vec<&CaptionUnit>> = Vec::new();
    let mut cur: Vec<&CaptionUnit> = Vec::new();
    let mut len = 0usize;
    for u in &t.units {
        let w = u.text.chars().count();
        let add = if cur.is_empty() { w } else { w + 1 };
        let gap = cur.last().map_or(0.0, |p| u.start_s - p.end_s);
        let over_budget = !cur.is_empty() && len + add > max_chars;
        let after_silence = !cur.is_empty() && gap > MAX_GAP_S;
        if over_budget || after_silence {
            lines.push(std::mem::take(&mut cur));
            len = 0;
        }
        len += if cur.is_empty() { w } else { w + 1 };
        cur.push(u);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Reveal+pop override block for one unit, timed at relative onset `on_ms`:
/// fade from transparent to opaque (40 ms), scale 100->115% (100 ms), settle
/// 115->100% (80 ms). All `\t` times are relative to the Dialogue's Start.
fn rolling_pop_tags(on_ms: i64) -> String {
    let on = on_ms.max(0);
    format!(
        "{{\\alpha&HFF&\\t({on},{a},\\alpha&H00&)\\t({on},{b},\\fscx115\\fscy115)\\t({b},{c},\\fscx100\\fscy100)}}",
        a = on + 40,
        b = on + 100,
        c = on + 180,
    )
}

/// Generate a complete ASS document for a transcript under a Caption Style.
/// M1: assumes `style.genre == RollingPop`; the genre match arrives at M6.
pub fn generate_ass(transcript: &Transcript, style: &CaptionStyle) -> String {
    let mut s = String::new();

    s.push_str("[Script Info]\n");
    s.push_str("ScriptType: v4.00+\n");
    s.push_str(&format!("PlayResX: {CANVAS_W}\n"));
    s.push_str(&format!("PlayResY: {CANVAS_H}\n"));
    s.push_str("ScaledBorderAndShadow: yes\n");
    s.push_str("WrapStyle: 2\n\n");

    s.push_str("[V4+ Styles]\n");
    s.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    s.push_str(&format!(
        "Style: Caption,{font},{size},{primary},{accent},&H00000000,&H96000000,0,0,0,0,100,100,0,0,1,6,2,5,40,40,40,1\n\n",
        font = style.font_family,
        size = style.font_size,
        primary = ass_color(style.primary_color),
        accent = ass_color(style.accent_color),
    ));

    s.push_str("[Events]\n");
    s.push_str("Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");

    let pos_x = CANVAS_W / 2;
    let pos_y = (CANVAS_H as f64 * CAPTION_Y_FRAC).round() as u32;

    let events = match style.genre {
        CaptionGenre::HugeWord => huge_word_events(transcript, pos_x, pos_y),
        CaptionGenre::RollingPop => rolling_pop_events(transcript, pos_x, pos_y),
        // KaraokeFill lands at M6; fall back to rolling-pop until then.
        CaptionGenre::KaraokeFill => rolling_pop_events(transcript, pos_x, pos_y),
    };
    s.push_str(&events);

    s
}

/// One word per caption (huge-word): each unit is its own Dialogue event,
/// appearing at its spoken onset and clearing before the next word (or after a
/// short hold), so exactly one word is on screen and timing tracks speech.
fn huge_word_events(transcript: &Transcript, pos_x: u32, pos_y: u32) -> String {
    let mut s = String::new();
    let units = &transcript.units;
    for (i, u) in units.iter().enumerate() {
        // `end_s` is the word's vocal end (set by refine_caption_timing); show
        // until then, one word at a time (clear by the next onset), readable min.
        let mut end = u.end_s;
        if let Some(next) = units.get(i + 1) {
            end = end.min(next.start_s);
        }
        let end = end.max(u.start_s + WORD_MIN_S); // never zero-duration / unreadable
        let text = format!("{{\\an5\\pos({pos_x},{pos_y})}}{}{}", rolling_pop_tags(0), u.text);
        s.push_str(&format!(
            "Dialogue: 0,{},{},Caption,,0,0,0,,{}\n",
            ass_time(u.start_s),
            ass_time(end),
            text
        ));
    }
    s
}

/// Refine each caption unit's end to the streamer's actual vocal end, read from
/// the clip's RMS energy envelope (called on the promote path before
/// `generate_ass`). whisper marks a word's end where the phoneme is recognized -
/// far short of a drawn-out scream - so a fixed hold either cuts screams or
/// lingers in silence. Here a word's caption ends once energy falls back toward
/// the clip's quiet/game baseline (the vocalization stopped), bounded by the next
/// word's onset: a screamed word holds for its whole sound, a normal word clears
/// as it drops, and nothing hangs in silence. `samples` is the clip's 16 kHz mono
/// audio, aligned to the transcript's 0-based times.
pub fn refine_caption_timing(mut transcript: Transcript, samples: &[f32], sr: u32) -> Transcript {
    let n = samples.len();
    if n == 0 || sr == 0 || transcript.units.is_empty() {
        return transcript;
    }
    let hop = ((sr as f64) * ENV_HOP_S).max(1.0) as usize;
    let win = ((sr as f64) * ENV_WIN_S).max(1.0) as usize;
    // RMS energy envelope over the clip.
    let mut env: Vec<f32> = Vec::with_capacity(n / hop + 1);
    let mut i = 0;
    while i < n {
        let e = (i + win).min(n);
        let w = &samples[i..e];
        env.push((w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt());
        i += hop;
    }
    let n_env = env.len();
    if n_env == 0 {
        return transcript;
    }
    let t_of = |k: usize| (k * hop) as f64 / sr as f64;
    let k_of = |t: f64| (((t * sr as f64) / hop as f64).round() as usize).min(n_env - 1);
    // Quiet/game floor = a low percentile of the envelope.
    let mut sorted = env.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let baseline = sorted[((n_env as f64 * BASELINE_PCT) as usize).min(n_env - 1)];
    let silence_hops = ((VOICE_SILENCE_S / ENV_HOP_S).round() as usize).max(1);

    let starts: Vec<f64> = transcript.units.iter().map(|u| u.start_s).collect();
    for idx in 0..starts.len() {
        let start = starts[idx];
        let next = starts.get(idx + 1).copied().unwrap_or_else(|| t_of(n_env));
        let k0 = k_of(start);
        let k_next = k_of(next).max(k0 + 1).min(n_env);
        // The word's vocal peak near its onset.
        let k_peak_end = k_of(start + PEAK_WINDOW_S).max(k0 + 1).min(k_next);
        let peak = env[k0..k_peak_end].iter().copied().fold(0.0_f32, f32::max);
        let thresh = baseline + VOICE_DROP_FRAC * (peak - baseline);
        // Walk forward tracking the last loud frame; stop once silence is sustained.
        let mut last_loud = k0;
        let mut below = 0usize;
        let mut k = k0;
        while k < k_next {
            if env[k] >= thresh {
                last_loud = k;
                below = 0;
            } else {
                below += 1;
                if below >= silence_hops {
                    break;
                }
            }
            k += 1;
        }
        let end = (t_of(last_loud) + ENV_WIN_S).clamp(start + WORD_MIN_S, next);
        transcript.units[idx].end_s = end;
    }
    transcript
}

/// Multi-word rolling-pop lines (M1): units grouped into <=MAX_LINE_CHARS lines,
/// each line one Dialogue event in which every unit pops in at its onset.
fn rolling_pop_events(transcript: &Transcript, pos_x: u32, pos_y: u32) -> String {
    let mut s = String::new();
    let lines = group_lines(transcript, MAX_LINE_CHARS);
    for (li, line) in lines.iter().enumerate() {
        let line_start = line.first().map_or(0.0, |u| u.start_s);
        // Hold after the last unit, but never past the next line's start, so
        // only one line is ever on screen (consecutive lines were overlapping).
        let mut line_end = line.last().map_or(0.0, |u| u.end_s) + LINE_HOLD_S;
        if let Some(next_start) = lines.get(li + 1).and_then(|n| n.first()).map(|u| u.start_s) {
            line_end = line_end.min(next_start);
        }

        let mut text = format!("{{\\an5\\pos({pos_x},{pos_y})}}");
        for (i, u) in line.iter().enumerate() {
            let on_ms = ((u.start_s - line_start) * 1000.0).round() as i64;
            text.push_str(&rolling_pop_tags(on_ms));
            text.push_str(&u.text);
            if i + 1 < line.len() {
                text.push(' ');
            }
        }

        s.push_str(&format!(
            "Dialogue: 0,{},{},Caption,,0,0,0,,{}\n",
            ass_time(line_start),
            ass_time(line_end),
            text
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use yc_core::{CaptionGenre, Language};

    fn style() -> CaptionStyle {
        CaptionStyle {
            name: "Rolling".into(),
            genre: CaptionGenre::RollingPop,
            font_family: "Anton".into(),
            font_size: 96,
            primary_color: [255, 255, 255, 255],
            accent_color: [255, 215, 0, 255],
        }
    }

    fn units(words: &[&str]) -> Transcript {
        let units = words
            .iter()
            .enumerate()
            .map(|(i, w)| CaptionUnit {
                text: (*w).into(),
                start_s: i as f64 * 0.5,
                end_s: i as f64 * 0.5 + 0.4,
            })
            .collect();
        Transcript { language: Language::En, units }
    }

    #[test]
    fn color_is_bgr_with_inverted_alpha() {
        assert_eq!(ass_color([255, 0, 0, 255]), "&H000000FF"); // opaque red
        assert_eq!(ass_color([0, 0, 255, 255]), "&H00FF0000"); // opaque blue
        assert_eq!(ass_color([255, 255, 255, 128]), "&H7FFFFFFF"); // half-transparent white
    }

    #[test]
    fn time_is_centiseconds() {
        assert_eq!(ass_time(0.0), "0:00:00.00");
        assert_eq!(ass_time(1.23), "0:00:01.23");
        assert_eq!(ass_time(3661.5), "1:01:01.50");
    }

    #[test]
    fn lines_respect_char_budget() {
        // "aaaa bbbb" = 9 chars fits; adding "cccc" would be 14 > 9.
        let t = units(&["aaaa", "bbbb", "cccc", "dddd"]);
        let lines = group_lines(&t, 9);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 2);
        assert_eq!(lines[1].len(), 2);
    }

    #[test]
    fn lines_break_on_long_silence() {
        // Two short words well within the 22-char budget, but a 3s gap between
        // them must still split the line (regression: far-apart words lingered).
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "hey".into(), start_s: 0.0, end_s: 0.4 },
                CaptionUnit { text: "there".into(), start_s: 3.4, end_s: 3.8 },
            ],
        };
        let lines = group_lines(&t, MAX_LINE_CHARS);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn one_dialogue_per_line_with_header() {
        let t = units(&["word0", "word1", "word2", "word3", "word4", "word5"]);
        let ass = generate_ass(&t, &style());
        assert!(ass.contains("PlayResX: 1080"));
        assert!(ass.contains("PlayResY: 1920"));
        assert!(ass.contains("Anton"));
        let dialogues = ass.lines().filter(|l| l.starts_with("Dialogue:")).count();
        assert_eq!(dialogues, group_lines(&t, MAX_LINE_CHARS).len());
        assert!(dialogues >= 1);
    }

    #[test]
    fn first_unit_onset_is_zero() {
        // The first unit of each line pops at relative t=0.
        let ass = generate_ass(&units(&["hello", "world"]), &style());
        assert!(ass.contains("\\t(0,40,\\alpha&H00&)"));
    }

    #[test]
    fn huge_word_emits_one_nonzero_event_per_word() {
        let mut st = style();
        st.genre = CaptionGenre::HugeWord;
        let ass = generate_ass(&units(&["satu", "dua", "tiga"]), &st);
        let dialogues: Vec<&str> = ass.lines().filter(|l| l.starts_with("Dialogue:")).collect();
        assert_eq!(dialogues.len(), 3); // one caption per word
        // Each event carries exactly its own word, never the next.
        assert!(dialogues[0].contains("satu") && !dialogues[0].contains("dua"));
        // No zero-duration captions (the Start,End fields must differ).
        for d in &dialogues {
            let fields: Vec<&str> = d.split(',').collect();
            assert_ne!(fields[1], fields[2], "zero-duration caption: {d}");
        }
    }

    #[test]
    fn huge_word_clears_each_word_before_the_next_starts() {
        let mut st = style();
        st.genre = CaptionGenre::HugeWord;
        // Words at 0.0 and 0.5; the first must end no later than 0.5.
        let ass = generate_ass(&units(&["a", "b"]), &st);
        let first = ass.lines().find(|l| l.starts_with("Dialogue:")).unwrap();
        let end = first.split(',').nth(2).unwrap();
        assert_eq!(end, "0:00:00.40"); // shows for the word's own end, clearing before next
    }

    #[test]
    fn refine_extends_a_held_word_and_clears_at_silence() {
        let sr = 16_000u32;
        // 2 s: a held "hold" loud 0.0-1.2, then silence; a quiet "next" blip at 1.6.
        let mut samples = vec![0.0f32; 2 * sr as usize];
        for s in samples.iter_mut().take((1.2 * sr as f64) as usize) {
            *s = 0.5;
        }
        for s in samples.iter_mut().skip((1.6 * sr as f64) as usize).take((0.1 * sr as f64) as usize)
        {
            *s = 0.4;
        }
        let t = Transcript {
            language: Language::En,
            units: vec![
                // whisper marked "hold" ending at 0.2 - far short of the scream.
                CaptionUnit { text: "hold".into(), start_s: 0.0, end_s: 0.2 },
                CaptionUnit { text: "next".into(), start_s: 1.6, end_s: 1.7 },
            ],
        };
        let r = refine_caption_timing(t, &samples, sr);
        // "hold" extends to ~1.2 (the scream's end) - not 0.2, and not 1.6 (the next).
        assert!(r.units[0].end_s > 1.0 && r.units[0].end_s < 1.45, "end = {}", r.units[0].end_s);
    }
}
