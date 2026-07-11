//! ASS subtitle generation — the single home of caption animation logic
//! (ADR 0004). Caption Style presets are data; the animation `genre` selects
//! which builder runs.
//!
//! Three genres:
//! - **Huge-word** (the M2 default): one word per caption — each unit is its own
//!   Dialogue event, appearing at its own spoken onset and clearing before the
//!   next word. Sync tracks the spoken word, and there is never more than one
//!   *cue* on screen. EN/ID; JA character chunking is later. Exception (ADR
//!   0057): a **cramped run** — onsets packed tighter than the `MIN_READ_S`
//!   floor, where the next-onset clamp would leave sub-readable flashes —
//!   falls back to one compact ≤3-word line at the line genres' scale,
//!   sharing the reading window the run physically has.
//! - **Rolling-pop** (M1): units grouped into on-screen lines by a character
//!   budget; each line is one Dialogue event in which every unit is laid out
//!   from the first frame but stays invisible until its spoken onset, when it
//!   fades in and scale-"pops". Units reveal in place rather than reflowing.
//! - **Karaoke-fill** (M7; per-word *snap* since ADR 0018): the same
//!   character-budget lines as rolling-pop, but every word is visible from the
//!   first frame in the *unsung* colour and **snaps** as a whole to the *sung*
//!   colour at its spoken onset, via ASS `\k` karaoke timing synced to the DTW
//!   word onsets (ADR 0013) — an instant per-word highlight, not a
//!   left-to-right sweep. The sung colour persists (cumulative), so a line is
//!   fully highlighted by its end. JA character-chunk karaoke rides on the
//!   deferred JA-chunking work.

use yc_core::{
    CaptionGenre, CaptionPlacement, CaptionStyle, CaptionUnit, Transcript, CANVAS_H, CANVAS_W,
};

/// Keep a completed line on screen this long after its last unit ends.
const LINE_HOLD_S: f64 = 0.5;
/// Max characters (including inter-unit spaces) on one on-screen line.
const MAX_LINE_CHARS: usize = 22;
/// Start a new line when the silence before a unit exceeds this, so words
/// spoken far apart never share one lingering line (which made captions appear
/// long before their later words were actually spoken).
const MAX_GAP_S: f64 = 1.0;
// The default caption anchor (`CAPTION_Y_FRAC`) lives in yc-core since ADR
// 0036: `CaptionPlacement::default` and this generator must agree on it.
/// Gap-fill caption timing (ADR 0013). whisper's DTW gives a precise word *onset*
/// but a zero-width *end*, so [`refine_caption_timing`] synthesises each word's
/// on-screen end by filling the gap to the next word's onset: capped at
/// `MAX_HOLD_S` (a word never lingers into a real pause — the "too slow" fix) and
/// floored at `MIN_READ_S` where there is room (nothing flashes sub-readably — the
/// "too fast" fix), one word at a time (the end never crosses the next onset).
/// `WORD_MIN_S` is the huge-word zero-duration guard.
const WORD_MIN_S: f64 = 0.10;
const MIN_READ_S: f64 = 0.40;
const MAX_HOLD_S: f64 = 1.2;

/// Too-fast grouping (ADR 0057, the ADR 0049 fix-#2 slice): when the next
/// onset arrives inside a word's read floor (`MIN_READ_S`), the next-onset
/// clamp defeats the floor by design and a huge-word cue flashes sub-readably
/// (measured 62% of cues on the 4-person overlap clip). Such a **cramped**
/// word falls back from one-word-per-cue to a compact line — grown until the
/// window to the next onset can hold `MIN_READ_S`, capped at
/// `MAX_GROUP_WORDS` words within the line genres' proven `MAX_LINE_CHARS`
/// budget — sharing one reading window. Grouped lines render at
/// `GROUP_FS_FRAC` of the resolved style size (96/150: the line genres'
/// 22-char fit at the huge-word default), carried on
/// [`PreviewLine::font_scale`] so the ASS `\fs` override and the editor
/// overlay consume the SAME number (ADR 0036: preview cannot drift).
const MAX_GROUP_WORDS: usize = 3;
const GROUP_FS_FRAC: f32 = 0.64;

/// Onset clamp (ADR 0019): whisper's DTW word onset occasionally lands slightly
/// *before* the word's audio, so the caption appears before it is spoken (an
/// intermittent early lead, across all genres — huge-word per word, rolling /
/// karaoke via the line's first-word onset). [`refine_caption_timing`] pushes
/// each word's start **forward** to its acoustic onset — the first RMS-envelope
/// frame at/after the DTW onset that rises past `ONSET_RISE_FRAC` of the word's
/// own peak — bounded by `ONSET_MAX_LEAD_S` (so a correctly-timed onset is never
/// delayed past its own peak) and never past the next word's onset. Audio already
/// present at the DTW onset ⇒ no shift. Reuses the envelope + per-word peak the
/// silence-drop already computes; per-word-peak-relative, so it scales to quiet
/// speech too.
const ONSET_RISE_FRAC: f32 = 0.30;
const ONSET_MAX_LEAD_S: f64 = 0.20;

/// The RMS envelope's one remaining job (ADR 0013): drop a word whose onset window
/// is in near-silence — whisper hallucinates tokens on silent / pure-music windows
/// (ADR 0007). Envelope at this hop/window; a word is dropped when its onset-window
/// peak (sought within `PEAK_WINDOW_S` of onset) stays below the drop threshold,
/// which is the **lower** of a relative bar `SILENCE_DROP_FRAC * loud_ref`
/// (`loud_ref` = p95 of the clip envelope) and an absolute floor `SILENCE_DROP_ABS`
/// (ADR 0021). The relative bar adapts to mic gain, but on a clip with loud
/// reactions it sits *above* quiet asides — a whispered viewer name — and wrongly
/// dropped them; the absolute floor, set between true silence and quiet speech,
/// keeps that real speech. Taking the `min` lets the (lower) relative bar still
/// govern a uniformly-quiet clip, so the floor never over-drops there.
const ENV_HOP_S: f64 = 0.02;
const ENV_WIN_S: f64 = 0.04;
const PEAK_WINDOW_S: f64 = 0.6;
const SILENCE_DROP_FRAC: f32 = 0.10;
/// Absolute RMS floor (ADR 0021): a word whose onset-window peak is at least this
/// is kept even when it is below the relative bar — quiet-but-present speech amid
/// loud moments. Set between true silence/hallucination level (clip p25 ~0.002 on
/// the repro) and quiet speech (a dropped-but-real "Terus" measured 0.0091).
/// Tune-from-use; `caption_diag` prints each dropped word's peak to recalibrate.
const SILENCE_DROP_ABS: f32 = 0.006;

/// RGBA (alpha = opacity) -> ASS `&HAABBGGRR`: bytes are ordered BGR and ASS
/// alpha is *transparency*, so 0x00 is opaque. This is the one place the
/// RGBA->BGR conversion CaptionStyle documents is allowed to live.
fn ass_color(rgba: [u8; 4]) -> String {
    let [r, g, b, a] = rgba;
    format!("&H{:02X}{:02X}{:02X}{:02X}", 255 - a, b, g, r)
}

/// RGBA -> ASS inline colour-override `&HBBGGRR&` (colour only, no alpha — alpha
/// is the separate `\Na` tag). For the `\1c`/`\2c` override tags the karaoke-fill
/// genre uses, which take six BGR hex digits, unlike the Style line's colour
/// fields (which carry alpha, see [`ass_color`]). Alpha is dropped.
fn ass_color_tag(rgba: [u8; 4]) -> String {
    let [r, g, b, _a] = rgba;
    format!("&H{:02X}{:02X}{:02X}&", b, g, r)
}

/// An ASS pixel width: integral values print bare (`6`, matching the historical
/// hardcoded Style line byte-for-byte), fractional ones with one decimal.
fn fmt_px(v: f32) -> String {
    let v = v.max(0.0);
    if (v - v.round()).abs() < 1e-3 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
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

/// One caption word as both the ASS emitters and the editor's preview overlay
/// see it: text pre-uppercased (the burn-in is all-caps), clip-relative timing.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewWord {
    pub text: String,
    pub start_s: f64,
    pub end_s: f64,
}

/// One on-screen caption line — the unit every genre renders (a huge-word line
/// holds exactly one word). `start_s..end_s` are the Dialogue bounds with the
/// hold and next-line clamp applied, so consecutive lines never overlap.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewLine {
    pub start_s: f64,
    pub end_s: f64,
    /// Per-line font scale relative to the resolved style size: 1.0 everywhere
    /// except huge-word grouped lines (ADR 0057), which render compact at
    /// [`GROUP_FS_FRAC`]. Consumed by BOTH the ASS emitter (inline `\fs`) and
    /// the editor overlay, so preview and burn cannot disagree (ADR 0036).
    pub font_scale: f32,
    pub words: Vec<PreviewWord>,
}

/// The caption line model (ADR 0036): grouping + line timing for a transcript
/// under a genre. The ASS emitters below and the editor's preview overlay both
/// consume this, so the preview cannot drift from the render — they are the
/// same code. Expects refine_caption_timing output (the render path's
/// transcript), like `generate_ass` always has.
pub fn preview_lines(transcript: &Transcript, genre: CaptionGenre) -> Vec<PreviewLine> {
    match genre {
        CaptionGenre::HugeWord => {
            let units = &transcript.units;
            // The verbatim pre-grouping cue: show until the gap-filled end
            // (ADR 0013), floor first as a zero-duration guard, then clamp to
            // the next onset LAST — so the floor can never push the end past
            // the next word's start. A transcript with no cramped unit takes
            // only this path and renders byte-identical to pre-ADR-0057.
            let singleton = |i: usize| {
                let u = &units[i];
                let mut end = u.end_s.max(u.start_s + WORD_MIN_S);
                if let Some(next) = units.get(i + 1) {
                    end = end.min(next.start_s);
                }
                PreviewLine {
                    start_s: u.start_s,
                    end_s: end,
                    font_scale: 1.0,
                    words: vec![PreviewWord {
                        text: u.text.to_uppercase(),
                        start_s: u.start_s,
                        end_s: end,
                    }],
                }
            };
            let mut lines = Vec::with_capacity(units.len());
            let mut i = 0;
            while i < units.len() {
                // Cramped (ADR 0057): the next onset lands inside this word's
                // read floor, so the clamp would leave a sub-readable flash.
                // Grow a group until the window to the next onset can hold
                // MIN_READ_S, or the word cap / char budget stops it. Every
                // absorbed word starts < MIN_READ_S after the group start, so
                // a group can never bridge a real pause.
                let gs = units[i].start_s;
                let mut j = i;
                let mut chars = units[i].text.chars().count();
                while let Some(next) = units.get(j + 1) {
                    if next.start_s - gs >= MIN_READ_S {
                        break; // the group now dwells readably to this onset
                    }
                    if j + 1 - i >= MAX_GROUP_WORDS {
                        break;
                    }
                    let add = next.text.chars().count() + 1;
                    if chars + add > MAX_LINE_CHARS {
                        break;
                    }
                    chars += add;
                    j += 1;
                }
                if j == i {
                    // Not cramped — or cramped but nothing could join (caps):
                    // the singleton path, byte-for-byte the old behavior.
                    lines.push(singleton(i));
                    i += 1;
                    continue;
                }
                // Grouped cue: floor at the group start (the shared reading
                // budget), clamp to the next onset LAST — the ADR 0013
                // discipline lifted to the group.
                let end = units[j]
                    .end_s
                    .max(gs + MIN_READ_S)
                    .min(units.get(j + 1).map_or(f64::INFINITY, |n| n.start_s));
                lines.push(PreviewLine {
                    start_s: gs,
                    end_s: end,
                    font_scale: GROUP_FS_FRAC,
                    words: units[i..=j]
                        .iter()
                        .map(|u| PreviewWord {
                            text: u.text.to_uppercase(),
                            start_s: u.start_s,
                            end_s: u.end_s.min(end),
                        })
                        .collect(),
                });
                i = j + 1;
            }
            lines
        }
        CaptionGenre::RollingPop | CaptionGenre::KaraokeFill => {
            let lines = group_lines(transcript, MAX_LINE_CHARS);
            (0..lines.len())
                .map(|li| {
                    let line = &lines[li];
                    let start = line.first().map_or(0.0, |u| u.start_s);
                    // Hold after the last unit, but never past the next line's
                    // start, so only one line is ever on screen.
                    let mut end = line.last().map_or(0.0, |u| u.end_s) + LINE_HOLD_S;
                    if let Some(next_start) =
                        lines.get(li + 1).and_then(|n| n.first()).map(|u| u.start_s)
                    {
                        end = end.min(next_start);
                    }
                    PreviewLine {
                        start_s: start,
                        end_s: end,
                        font_scale: 1.0,
                        words: line
                            .iter()
                            .map(|u| PreviewWord {
                                text: u.text.to_uppercase(),
                                start_s: u.start_s,
                                end_s: u.end_s,
                            })
                            .collect(),
                    }
                })
                .collect()
        }
    }
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

/// The effective anchor + font size a placement resolves to, shared by the ASS
/// generator and the editor's preview overlay so both draw the same geometry
/// (the overlay converts these PlayRes pixels to canvas points by one factor).
/// `None` (and `Some(CaptionPlacement::default())`) is the built-in anchor:
/// centered, `CAPTION_Y_FRAC`, unscaled. Defensive clamps bound a hand-edited
/// project.json: the *anchor point* stays on-canvas and the scale inside the
/// shared `CaptionPlacement` bounds — the block's extent around an extreme
/// anchor (x 0 or 1, `\an5` centering) can still overhang the edge; the
/// editor's drag clamps tighter margins for that.
pub fn resolve_placement(
    placement: Option<CaptionPlacement>,
    font_size: u32,
) -> (u32, u32, u32) {
    let p = placement.unwrap_or_default();
    let x = p.x_frac.clamp(0.0, 1.0) as f64;
    let y = p.y_frac.clamp(0.0, 1.0) as f64;
    let scale = p.scale.clamp(CaptionPlacement::SCALE_MIN, CaptionPlacement::SCALE_MAX);
    let pos_x = (CANVAS_W as f64 * x).round() as u32;
    let pos_y = (CANVAS_H as f64 * y).round() as u32;
    let size = ((font_size as f32 * scale).round() as u32).max(1);
    (pos_x, pos_y, size)
}

/// How one word of a [`PreviewLine`] presents at playhead `t` — the preview
/// overlay's per-word state, defined HERE so the genre semantics live beside
/// the ASS emitters that encode the same rules as tags (ADR 0036: the preview
/// must not re-derive render semantics UI-side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordState {
    /// Not yet revealed (rolling-pop before the word's onset): reserves its
    /// space in the line (ASS lays the line out from the first frame) but is
    /// invisible.
    Hidden,
    /// Visible in the base/primary colour.
    Base,
    /// Snapped to the accent ("sung") colour — karaoke at/after the word's
    /// onset (ADR 0018's per-word snap).
    Sung,
}

/// Per-word [`WordState`]s for `line` at playhead `t`, mirroring what the
/// genre's ASS tags do at that instant: huge-word shows its one word;
/// rolling-pop reveals each word at its onset (`\alpha` + `\t` reveal);
/// karaoke-fill shows every word, snapping it to the accent at its onset
/// (`\k` cumulative snap). One function, consumed by the editor overlay and
/// pinned by tests against the emitters' tag timings.
pub fn word_states(genre: CaptionGenre, line: &PreviewLine, t: f64) -> Vec<WordState> {
    line.words
        .iter()
        .map(|w| match genre {
            CaptionGenre::HugeWord => WordState::Base,
            CaptionGenre::RollingPop => {
                if w.start_s <= t {
                    WordState::Base
                } else {
                    WordState::Hidden
                }
            }
            CaptionGenre::KaraokeFill => {
                if w.start_s <= t {
                    WordState::Sung
                } else {
                    WordState::Base
                }
            }
        })
        .collect()
}

/// Generate a complete ASS document for a transcript under a Caption Style. The
/// `genre` selects the animation builder; everything else about the style is data
/// (ADR 0004), so colours/font/size flow into the shared Style line. `placement`
/// is the Clip's Caption placement (ADR 0036): `None` keeps the built-in anchor
/// and size, byte-identical to pre-placement output.
pub fn generate_ass(
    transcript: &Transcript,
    style: &CaptionStyle,
    placement: Option<CaptionPlacement>,
) -> String {
    let mut s = String::new();

    let (pos_x, pos_y, font_size) = resolve_placement(placement, style.font_size);

    s.push_str("[Script Info]\n");
    s.push_str("ScriptType: v4.00+\n");
    s.push_str(&format!("PlayResX: {CANVAS_W}\n"));
    s.push_str(&format!("PlayResY: {CANVAS_H}\n"));
    s.push_str("ScaledBorderAndShadow: yes\n");
    s.push_str("WrapStyle: 2\n\n");

    s.push_str("[V4+ Styles]\n");
    s.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    // BorderStyle 3 draws an opaque box (the style's back colour) behind each
    // line; BorderStyle 1 is the classic outline+shadow. Outline/shadow widths
    // and colours are style data since the editor's Caption panel (2026-07);
    // the defaults reproduce the historical hardcoded `1,6,2` line exactly.
    let border_style = if style.back_box { 3 } else { 1 };
    s.push_str(&format!(
        "Style: Caption,{font},{size},{primary},{accent},{outline_c},{back_c},{bold},0,0,0,100,100,0,0,{bs},{outline},{shadow},5,40,40,40,1\n\n",
        font = style.font_family,
        size = font_size,
        primary = ass_color(style.primary_color),
        accent = ass_color(style.accent_color),
        outline_c = ass_color(style.outline_color),
        back_c = ass_color(style.back_color),
        bold = if style.bold { -1 } else { 0 },
        bs = border_style,
        outline = fmt_px(style.outline),
        shadow = fmt_px(style.shadow),
    ));

    s.push_str("[Events]\n");
    s.push_str("Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");

    let lines = preview_lines(transcript, style.genre);
    let events = match style.genre {
        CaptionGenre::HugeWord => huge_word_events(&lines, pos_x, pos_y, font_size),
        CaptionGenre::RollingPop => rolling_pop_events(&lines, pos_x, pos_y),
        CaptionGenre::KaraokeFill => karaoke_fill_events(&lines, style, pos_x, pos_y),
    };
    s.push_str(&events);

    s
}

/// One cue per line (huge-word): a singleton word's Dialogue is byte-identical
/// to the pre-grouping output — appearing at its spoken onset, clearing before
/// the next word. A grouped line (cramped run, ADR 0057) joins its words and
/// renders compact via an inline `\fs` at the line's `font_scale` of the
/// resolved style size — the same number the editor overlay multiplies in, so
/// the two surfaces cannot drift (ADR 0036). Timing (floor + next-onset clamp,
/// ADR 0013; the group-level floor, ADR 0057) is already applied by
/// [`preview_lines`].
fn huge_word_events(lines: &[PreviewLine], pos_x: u32, pos_y: u32, font_size: u32) -> String {
    let mut s = String::new();
    for l in lines {
        if l.words.is_empty() {
            continue;
        }
        let fs = if (l.font_scale - 1.0).abs() > 1e-6 {
            format!("\\fs{}", ((font_size as f32 * l.font_scale).round() as u32).max(1))
        } else {
            String::new()
        };
        let words: Vec<&str> = l.words.iter().map(|w| w.text.as_str()).collect();
        let text = format!(
            "{{\\an5\\pos({pos_x},{pos_y}){fs}}}{}{}",
            rolling_pop_tags(0),
            words.join(" ")
        );
        s.push_str(&format!(
            "Dialogue: 0,{},{},Caption,,0,0,0,,{}\n",
            ass_time(l.start_s),
            ass_time(l.end_s),
            text
        ));
    }
    s
}

/// The fate of one input caption unit under [`refine_caption_timing`], in input
/// order — the *real* keep/drop + re-timing decision, surfaced so an inspector
/// (`caption_diag`) can report a trustworthy verdict without reverse-engineering
/// it from the output. Reconstructing keep-vs-drop from output timing is unsafe:
/// the onset clamp (ADR 0019) can push a kept word's start *forward all the way to
/// the next word's onset*, so an "output start < next onset" heuristic misreads
/// that kept word as dropped — and, walking a shrunken output against the full
/// input, desyncs every unit after it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnitOutcome {
    /// Dropped as near-silence: its onset-window peak stayed below `silence_drop`.
    /// This is the **only** reason `refine_caption_timing` drops a unit — whisper's
    /// spurious tokens on silent / pure-music windows (ADR 0007).
    Dropped { peak: f32 },
    /// Kept, re-timed to these onset-clamped (ADR 0019), gap-filled (ADR 0013) bounds.
    Kept { start_s: f64, end_s: f64 },
}

/// The full decision trace of [`refine_caption_timing`]: one [`UnitOutcome`] per
/// input unit (same order, same length), plus the thresholds it decided against —
/// so a caller can explain a drop without mirroring the silence math.
/// [`refine_caption_timing`] is a thin filter over [`refine_caption_timing_traced`],
/// so the surviving subsequence and this trace can never disagree.
#[derive(Debug, Clone)]
pub struct RefineTrace {
    /// Effective drop threshold: `min(SILENCE_DROP_FRAC * loud_ref, SILENCE_DROP_ABS)`
    /// (ADR 0021). A unit is dropped iff its onset-window peak is below this.
    pub silence_drop: f32,
    /// p95 of the clip's RMS envelope — the loud reference the relative bar scales
    /// from (`SILENCE_DROP_FRAC * loud_ref` is the relative half of `silence_drop`).
    pub loud_ref: f32,
    /// One outcome per input unit, in input order (1:1 with `transcript.units`).
    pub outcomes: Vec<UnitOutcome>,
}

/// The real caption-timing decision (ADR 0013 + ADR 0019 + ADR 0021) as a per-unit
/// [`RefineTrace`], the shared core of [`refine_caption_timing`]. Same inputs, same
/// order; returns the fate + re-timing of *every* input unit (kept and dropped)
/// instead of only the surviving subsequence. `caption_diag` reads this directly so
/// its verdict is the render's actual decision, never a reconstruction. `samples`
/// is the clip's 16 kHz mono audio, aligned to the transcript's 0-based times.
pub fn refine_caption_timing_traced(
    transcript: &Transcript,
    samples: &[f32],
    sr: u32,
) -> RefineTrace {
    refine_traced_inner(transcript, samples, sr, true)
}

/// [`refine_caption_timing_traced`] with the silence-drop optionally disabled.
/// The drop exists for WHISPER's failure mode — hallucinated tokens on
/// silent/music windows (ADR 0007) — where a unit's words and times are one
/// decoder's unverified guess. Ensemble captions (ADR 0034) invert that: every
/// unit's word survived a multi-decoder vote, so a near-silent onset window is
/// a *placement* artifact (fusion laid a verified word into a quiet span), and
/// dropping it silently deletes real speech (measured: a quiet "mana"). Those
/// callers keep every unit and still get the onset clamp + gap-fill re-timing.
fn refine_traced_inner(
    transcript: &Transcript,
    samples: &[f32],
    sr: u32,
    drop_silent: bool,
) -> RefineTrace {
    // Degenerate input: nothing to measure. Match refine_caption_timing's early
    // return — every unit is kept, unchanged, and no drop threshold applies.
    let keep_all_unchanged = || RefineTrace {
        silence_drop: 0.0,
        loud_ref: 0.0,
        outcomes: transcript
            .units
            .iter()
            .map(|u| UnitOutcome::Kept { start_s: u.start_s, end_s: u.end_s })
            .collect(),
    };
    let n = samples.len();
    if n == 0 || sr == 0 || transcript.units.is_empty() {
        return keep_all_unchanged();
    }
    let hop = ((sr as f64) * ENV_HOP_S).max(1.0) as usize;
    let win = ((sr as f64) * ENV_WIN_S).max(1.0) as usize;
    // RMS energy envelope over the clip - used only to drop near-silent words now.
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
        return keep_all_unchanged();
    }
    let t_of = |k: usize| (k * hop) as f64 / sr as f64;
    let k_of = |t: f64| (((t * sr as f64) / hop as f64).round() as usize).min(n_env - 1);
    // Loud reference (p95): the silence-drop is relative to this, not a baseline,
    // so quiet-but-present speech survives and a mostly-silent clip (baseline ~ 0)
    // still has a sane floor.
    let mut sorted = env.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let loud_ref = sorted[((n_env as f64 * 0.95) as usize).min(n_env - 1)];
    // The lower of the relative bar and the absolute floor (ADR 0021): on a loud
    // clip the abs floor caps the bar so quiet asides survive; on a quiet clip the
    // (lower) relative bar governs so the floor never over-drops.
    let silence_drop = (SILENCE_DROP_FRAC * loud_ref).min(SILENCE_DROP_ABS);

    let clip_end = t_of(n_env);
    let starts: Vec<f64> = transcript.units.iter().map(|u| u.start_s).collect();
    let mut outcomes: Vec<UnitOutcome> = Vec::with_capacity(starts.len());
    for idx in 0..starts.len() {
        let dtw_start = starts[idx];
        // The next word's onset bounds this one - one word on screen at a time.
        // `.max(dtw_start)` guards a non-monotonic DTW onset (rare; absent on the repro).
        let next = starts.get(idx + 1).copied().unwrap_or(clip_end).max(dtw_start);
        // Drop a word whose onset window is in near-silence: whisper hallucinates
        // tokens on silent / pure-music windows (ADR 0007). Quiet real speech sits
        // above the absolute floor and survives even when it is below the relative
        // bar (a whispered name amid loud reactions - ADR 0021).
        let k0 = k_of(dtw_start);
        let k_peak_end = k_of(dtw_start + PEAK_WINDOW_S).max(k0 + 1).min(n_env);
        let peak = env[k0..k_peak_end].iter().copied().fold(0.0_f32, f32::max);
        if drop_silent && peak < silence_drop {
            outcomes.push(UnitOutcome::Dropped { peak });
            continue;
        }
        // Onset clamp (ADR 0019): push the caption start forward to the acoustic
        // onset when the DTW onset leads the audio. Scan from the DTW onset to the
        // first envelope frame rising past ONSET_RISE_FRAC of the word's own peak,
        // bounded by ONSET_MAX_LEAD_S and the next onset; audio already present at
        // the DTW onset leaves the start unmoved. Forward only — a caption never
        // precedes its sound, and a correct onset is never delayed past its peak.
        let onset_level = ONSET_RISE_FRAC * peak;
        let k_search_end = k_of((dtw_start + ONSET_MAX_LEAD_S).min(next));
        let mut k = k0;
        while k < k_search_end && env[k] < onset_level {
            k += 1;
        }
        let start = t_of(k).max(dtw_start).min(next);
        // Gap-fill the end from the (clamped) start to the next onset, capped at
        // MAX_HOLD and floored at MIN_READ where there is room; clamp to `next` LAST
        // so neither the cap nor the floor can produce an overlap (one word - ADR 0013).
        let end = (start + MAX_HOLD_S).min(next).max(start + MIN_READ_S).min(next);
        outcomes.push(UnitOutcome::Kept { start_s: start, end_s: end });
    }
    RefineTrace { silence_drop, loud_ref, outcomes }
}

/// Synthesise each caption unit's on-screen end (ADR 0013), called on the render
/// path before `generate_ass`. whisper's DTW gives a precise word *onset* but a
/// zero-width *end*, so we **gap-fill**: a word shows from its onset until the next
/// word's onset, capped at `MAX_HOLD_S` (never linger into a real pause) and
/// floored at `MIN_READ_S` where there is room (never a sub-readable flash), one
/// word at a time (the end never crosses the next onset). The earlier approach -
/// ending a word when the *mixed* RMS envelope fell toward a per-clip baseline -
/// set its threshold from the word's onset peak, which loud game SFX/music inflate,
/// so words rode background transients and cleared too fast (or held too slow): see
/// ADR 0013's diagnosis. The envelope now does one job only: **drop** a word whose
/// onset window is in near-silence (whisper's spurious tokens on silent / pure-music
/// windows, ADR 0007) - relative to the loud (p95) reference, so quiet real speech
/// survives. `samples` is the clip's 16 kHz mono audio, aligned to the transcript's
/// 0-based times.
///
/// The keep/drop + re-timing logic lives in [`refine_caption_timing_traced`]; this
/// is the filter that applies it, keeping only the surviving units. The two are
/// derived from one pass, so they cannot disagree.
pub fn refine_caption_timing(mut transcript: Transcript, samples: &[f32], sr: u32) -> Transcript {
    let trace = refine_caption_timing_traced(&transcript, samples, sr);
    let units = std::mem::take(&mut transcript.units);
    transcript.units = units
        .into_iter()
        .zip(trace.outcomes)
        .filter_map(|(mut u, outcome)| match outcome {
            UnitOutcome::Kept { start_s, end_s } => {
                u.start_s = start_s;
                u.end_s = end_s;
                Some(u)
            }
            UnitOutcome::Dropped { .. } => None,
        })
        .collect();
    transcript
}

/// [`refine_caption_timing`] for VOTE-VERIFIED transcripts (the Qwen ensemble,
/// ADR 0034): same onset clamp + gap-fill, but nothing is silence-dropped —
/// see [`refine_traced_inner`] for why the drop is wrong for this input class.
pub fn refine_caption_timing_keep_verified(
    mut transcript: Transcript,
    samples: &[f32],
    sr: u32,
) -> Transcript {
    let trace = refine_traced_inner(&transcript, samples, sr, false);
    for (u, outcome) in transcript.units.iter_mut().zip(trace.outcomes) {
        // With drop_silent=false every outcome is Kept.
        if let UnitOutcome::Kept { start_s, end_s } = outcome {
            u.start_s = start_s;
            u.end_s = end_s;
        }
    }
    transcript
}

/// Multi-word rolling-pop lines (M1): each [`PreviewLine`] is one Dialogue
/// event in which every word pops in at its onset (grouping + line bounds come
/// from [`preview_lines`]).
fn rolling_pop_events(lines: &[PreviewLine], pos_x: u32, pos_y: u32) -> String {
    let mut s = String::new();
    for l in lines {
        let mut text = format!("{{\\an5\\pos({pos_x},{pos_y})}}");
        for (i, w) in l.words.iter().enumerate() {
            let on_ms = ((w.start_s - l.start_s) * 1000.0).round() as i64;
            text.push_str(&rolling_pop_tags(on_ms));
            text.push_str(&w.text);
            if i + 1 < l.words.len() {
                text.push(' ');
            }
        }

        s.push_str(&format!(
            "Dialogue: 0,{},{},Caption,,0,0,0,,{}\n",
            ass_time(l.start_s),
            ass_time(l.end_s),
            text
        ));
    }
    s
}

/// Karaoke-fill lines (M7; per-word snap since ADR 0018): the same
/// character-budget lines as rolling-pop, but each line is one Dialogue in which
/// every word is visible from the first frame in the *unsung* colour and
/// **snaps** as a whole to the *sung* colour at its spoken onset, via ASS `\k`
/// karaoke timing. `\k<cs>` switches its word's text from SecondaryColour to
/// PrimaryColour **instantly** when the karaoke cursor reaches it (not the
/// left-to-right sweep `\kf` paints), the cursor advancing by each `\k`'s `<cs>`
/// in turn; so word i snaps at its onset and `<cs>` is just the dwell until the
/// next word snaps (the last word over its own gap-filled span — ADR 0013). The
/// colours are set inline (`\1c` = sung = accent, the highlight; `\2c` = unsung =
/// primary, the base text) so the effect is independent of the shared Style
/// line's primary/secondary ordering. The sung colour persists (cumulative), so
/// the line is fully highlighted by its end and then holds briefly (`LINE_HOLD_S`,
/// clamped to the next line's start). The function keeps its `_fill` name for
/// `CaptionGenre::KaraokeFill` enum/serde stability.
fn karaoke_fill_events(
    lines: &[PreviewLine],
    style: &CaptionStyle,
    pos_x: u32,
    pos_y: u32,
) -> String {
    let mut s = String::new();
    let sung = ass_color_tag(style.accent_color); // \1c: filled / "sung" colour
    let unsung = ass_color_tag(style.primary_color); // \2c: unfilled / base colour
    for l in lines {
        let mut text = format!("{{\\an5\\pos({pos_x},{pos_y})\\1c{sung}\\2c{unsung}}}");
        for (i, w) in l.words.iter().enumerate() {
            // Karaoke dwell (centiseconds) before the next word snaps: to the next
            // word's onset, or — for the last word — its own gap-filled duration.
            // Floored at 1 cs so a zero-gap word still advances the cursor (and
            // `\k0` never stalls).
            let next_on = l.words.get(i + 1).map_or(w.end_s, |n| n.start_s);
            let dur_cs = (((next_on - w.start_s) * 100.0).round() as i64).max(1);
            text.push_str(&format!("{{\\k{dur_cs}}}"));
            text.push_str(&w.text);
            if i + 1 < l.words.len() {
                text.push(' ');
            }
        }

        s.push_str(&format!(
            "Dialogue: 0,{},{},Caption,,0,0,0,,{}\n",
            ass_time(l.start_s),
            ass_time(l.end_s),
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
            accent_color: [255, 215, 0, 255],
            ..CaptionStyle::for_genre(CaptionGenre::RollingPop)
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
        let ass = generate_ass(&t, &style(), None);
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
        let ass = generate_ass(&units(&["hello", "world"]), &style(), None);
        assert!(ass.contains("\\t(0,40,\\alpha&H00&)"));
    }

    #[test]
    fn huge_word_emits_one_nonzero_event_per_word() {
        let mut st = style();
        st.genre = CaptionGenre::HugeWord;
        let ass = generate_ass(&units(&["satu", "dua", "tiga"]), &st, None);
        let dialogues: Vec<&str> = ass.lines().filter(|l| l.starts_with("Dialogue:")).collect();
        assert_eq!(dialogues.len(), 3); // one caption per word
        // Each event carries exactly its own word (uppercased), never the next.
        assert!(dialogues[0].contains("SATU") && !dialogues[0].contains("DUA"));
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
        let ass = generate_ass(&units(&["a", "b"]), &st, None);
        let first = ass.lines().find(|l| l.starts_with("Dialogue:")).unwrap();
        let end = first.split(',').nth(2).unwrap();
        assert_eq!(end, "0:00:00.40"); // shows for the word's own end, clearing before next
    }

    #[test]
    fn huge_word_floor_never_overlaps_the_next_onset() {
        // ADR 0013 overlap bug, ADR 0057 shape: a word whose next onset (0.50)
        // arrives inside its read floor is CRAMPED — it now shares one grouped
        // Dialogue with that word (never two overlapping cues).
        let mut st = style();
        st.genre = CaptionGenre::HugeWord;
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "a".into(), start_s: 0.46, end_s: 0.50 },
                CaptionUnit { text: "b".into(), start_s: 0.50, end_s: 0.90 },
            ],
        };
        let ass = generate_ass(&t, &st, None);
        let dialogues: Vec<&str> = ass.lines().filter(|l| l.starts_with("Dialogue:")).collect();
        assert_eq!(dialogues.len(), 1, "cramped pair groups into one cue: {dialogues:?}");
        assert!(dialogues[0].contains("A B"), "{}", dialogues[0]);
        // When the char budget blocks grouping, the cramped word stays a
        // singleton and keeps the pre-grouping rule verbatim: the floor must
        // not push its end past the next word's start — the clamp is LAST.
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "a".into(), start_s: 0.46, end_s: 0.50 },
                CaptionUnit { text: "bbbbbbbbbbbbbbbbbbbbbb".into(), start_s: 0.50, end_s: 0.90 },
            ],
        };
        let ass = generate_ass(&t, &st, None);
        let first = ass.lines().find(|l| l.starts_with("Dialogue:")).unwrap();
        let end = first.split(',').nth(2).unwrap();
        assert_eq!(end, "0:00:00.50"); // clamped to "b"'s onset, not floored to 0.86
    }

    #[test]
    fn huge_word_cramped_run_groups_into_a_readable_compact_line() {
        // ADR 0057: onsets 0.2 s apart pack tighter than MIN_READ_S — the two
        // cramped words share one cue that dwells past the floor (clamped to
        // the next onset LAST), joined text at the compact scale; the sparse
        // word after them stays a full-size singleton. Text is preserved in
        // order (presentation-only), and grouped words are all Base (static
        // line) for the preview overlay.
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "ya".into(), start_s: 0.0, end_s: 0.20 },
                CaptionUnit { text: "siapa".into(), start_s: 0.20, end_s: 0.55 },
                CaptionUnit { text: "tau".into(), start_s: 0.55, end_s: 1.75 },
            ],
        };
        let lines = preview_lines(&t, CaptionGenre::HugeWord);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].words.len(), 2);
        assert!((lines[0].end_s - lines[0].start_s) >= MIN_READ_S);
        assert_eq!(lines[0].end_s, 0.55); // next group's onset, clamped LAST
        assert!((lines[0].font_scale - GROUP_FS_FRAC).abs() < 1e-6);
        assert!((lines[1].font_scale - 1.0).abs() < 1e-6);
        let texts: Vec<&str> =
            lines.iter().flat_map(|l| l.words.iter().map(|w| w.text.as_str())).collect();
        assert_eq!(texts, vec!["YA", "SIAPA", "TAU"]);
        assert_eq!(
            word_states(CaptionGenre::HugeWord, &lines[0], 0.0),
            vec![WordState::Base, WordState::Base]
        );
        // The emitted Dialogue joins the words and carries the compact \fs on
        // the resolved size (150 * 0.64 = 96, the line genres' proven fit);
        // the singleton carries none.
        let st = CaptionStyle::for_genre(CaptionGenre::HugeWord);
        let ass = generate_ass(&t, &st, None);
        let d: Vec<&str> = ass.lines().filter(|l| l.starts_with("Dialogue:")).collect();
        assert_eq!(d.len(), 2);
        assert!(d[0].contains("YA SIAPA"), "{}", d[0]);
        assert!(d[0].contains("\\fs96}"), "{}", d[0]);
        // The singleton carries no size override — its only `\fs` hits are the
        // pop animation's `\fscx`/`\fscy` scale tags.
        assert_eq!(
            d[1].matches("\\fs").count(),
            d[1].matches("\\fscx").count() + d[1].matches("\\fscy").count(),
            "{}",
            d[1]
        );
    }

    #[test]
    fn huge_word_group_caps_at_three_words_and_never_bridges_a_pause() {
        // A five-word burst at 0.1 s spacing: the word cap closes the first
        // group at 3 (its window is still cramped — the residual the
        // instrument counts), the tail regroups, and no grouped word starts
        // MIN_READ_S or later after its group's start (a group cannot bridge
        // a real pause by construction).
        let t = Transcript {
            language: Language::En,
            units: (0..5)
                .map(|k| CaptionUnit {
                    text: ((b'a' + k as u8) as char).to_string(),
                    start_s: k as f64 * 0.1,
                    end_s: k as f64 * 0.1 + 0.1,
                })
                .collect(),
        };
        let lines = preview_lines(&t, CaptionGenre::HugeWord);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].words.len(), 3);
        assert_eq!(lines[1].words.len(), 2);
        for l in &lines {
            for w in &l.words {
                assert!(w.start_s - l.start_s < MIN_READ_S, "bridged a pause: {w:?}");
            }
        }
        // Cap-limited residual: group 1 is clamped by "d"'s onset at 0.3.
        assert!((lines[0].end_s - 0.3).abs() < 1e-9);
        // Cues never overlap: each line ends by the next line's start.
        assert!(lines[0].end_s <= lines[1].start_s);
    }

    #[test]
    fn huge_word_sparse_transcript_stays_one_word_per_cue_without_fs() {
        // The 0.5 s-spaced fixture has no cramped unit: every cue is one word
        // at full size (no \fs anywhere) — the byte-identity control's shape.
        let mut st = style();
        st.genre = CaptionGenre::HugeWord;
        let t = units(&["satu", "dua", "tiga"]);
        let lines = preview_lines(&t, CaptionGenre::HugeWord);
        assert!(lines.iter().all(|l| l.words.len() == 1 && l.font_scale == 1.0));
        let ass = generate_ass(&t, &st, None);
        // No size override anywhere — every `\fs` hit is the pop animation's
        // `\fscx`/`\fscy`, exactly as before ADR 0057.
        assert_eq!(
            ass.matches("\\fs").count(),
            ass.matches("\\fscx").count() + ass.matches("\\fscy").count(),
            "sparse output must carry no \\fs override"
        );
        assert_eq!(ass.lines().filter(|l| l.starts_with("Dialogue:")).count(), 3);
    }

    #[test]
    fn refine_fills_gap_to_next_onset_not_a_flash() {
        // ADR 0013 "too fast" regression: "a" has a loud onset transient then goes
        // quiet long before "b" at 0.5 s. The old envelope timing cleared it the
        // instant energy dropped (~0.1 s flash); gap-fill shows it for the whole gap.
        let sr = 16_000u32;
        let mut samples = vec![0.1f32; sr as usize]; // 1 s, moderate floor (kept)
        for s in samples.iter_mut().take((0.1 * sr as f64) as usize) {
            *s = 0.5; // onset transient only
        }
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "a".into(), start_s: 0.0, end_s: 0.0 }, // zero-width DTW
                CaptionUnit { text: "b".into(), start_s: 0.5, end_s: 0.5 },
            ],
        };
        let r = refine_caption_timing(t, &samples, sr);
        assert_eq!(r.units.len(), 2);
        // "a" fills to "b"'s onset (gap 0.5 s, between MIN_READ and MAX_HOLD), not a flash.
        assert!((r.units[0].end_s - 0.5).abs() < 1e-6, "end = {}", r.units[0].end_s);
        assert!(r.units[0].end_s >= MIN_READ_S);
    }

    #[test]
    fn refine_caps_a_word_before_a_pause_at_max_hold() {
        // ADR 0013 "too slow" regression: "a" then a 5 s pause before "b". The word
        // must not linger the whole pause - it caps at MAX_HOLD and clears.
        let sr = 16_000u32;
        let mut samples = vec![0.0f32; 6 * sr as usize];
        for s in samples.iter_mut().take((0.2 * sr as f64) as usize) {
            *s = 0.5; // "a" onset
        }
        for s in samples.iter_mut().skip((5.0 * sr as f64) as usize).take((0.2 * sr as f64) as usize)
        {
            *s = 0.5; // "b" onset, after a long real pause
        }
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "a".into(), start_s: 0.0, end_s: 0.0 },
                CaptionUnit { text: "b".into(), start_s: 5.0, end_s: 5.0 },
            ],
        };
        let r = refine_caption_timing(t, &samples, sr);
        assert!((r.units[0].end_s - MAX_HOLD_S).abs() < 1e-6, "end = {}", r.units[0].end_s);
    }

    #[test]
    fn refine_does_not_overlap_when_onsets_are_a_frame_apart() {
        // Onsets ~0.1 s apart (faster than MIN_READ): the first word clips to the
        // next onset, never past it (no overlap), and nothing panics. Steady tone so
        // nothing drops as silence.
        let sr = 16_000u32;
        let samples = vec![0.3f32; (28.4 * sr as f64) as usize];
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "a".into(), start_s: 28.1, end_s: 28.1 },
                CaptionUnit { text: "b".into(), start_s: 28.2, end_s: 28.2 },
            ],
        };
        let r = refine_caption_timing(t, &samples, sr); // must not panic
        assert_eq!(r.units.len(), 2);
        assert!(r.units[0].end_s.is_finite() && r.units[0].end_s <= 28.2001);
    }

    #[test]
    fn refine_keeps_a_quiet_but_present_word() {
        // ADR 0013: the old drop threshold (baseline + 0.15*(loud_ref-baseline))
        // killed quiet real speech ("apa sih" on the repro). Anchored on loud_ref
        // alone, a word well above SILENCE_DROP_FRAC*loud_ref survives.
        let sr = 16_000u32;
        let mut samples = vec![0.0f32; 2 * sr as usize];
        for s in samples.iter_mut().take((0.5 * sr as f64) as usize) {
            *s = 1.0; // a loud reaction sets loud_ref ~ 1.0
        }
        for s in samples.iter_mut().skip(sr as usize).take((0.5 * sr as f64) as usize) {
            *s = 0.2; // quiet aside at 1.0 s: 0.2 > 0.10*loud_ref, must be kept
        }
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "loud".into(), start_s: 0.0, end_s: 0.0 },
                CaptionUnit { text: "quiet".into(), start_s: 1.0, end_s: 1.0 },
            ],
        };
        let r = refine_caption_timing(t, &samples, sr);
        let texts: Vec<&str> = r.units.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(texts, vec!["loud", "quiet"]);
    }

    #[test]
    fn refine_drops_a_word_that_starts_in_silence() {
        let sr = 16_000u32;
        // loud "a" 0.0-0.5, silence, loud "c" 2.0-2.5; "b" is a spurious token at
        // 1.0 in the silence (a whisper hallucination on the pause) -> dropped.
        let mut samples = vec![0.0f32; 3 * sr as usize];
        for s in samples.iter_mut().take((0.5 * sr as f64) as usize) {
            *s = 0.5;
        }
        for s in samples.iter_mut().skip((2.0 * sr as f64) as usize).take((0.5 * sr as f64) as usize)
        {
            *s = 0.5;
        }
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "a".into(), start_s: 0.0, end_s: 0.4 },
                CaptionUnit { text: "b".into(), start_s: 1.0, end_s: 1.1 },
                CaptionUnit { text: "c".into(), start_s: 2.0, end_s: 2.4 },
            ],
        };
        let r = refine_caption_timing(t, &samples, sr);
        let texts: Vec<&str> = r.units.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(texts, vec!["a", "c"]); // the silent "b" is dropped
    }

    #[test]
    fn refine_clamps_start_forward_when_dtw_onset_leads_the_audio() {
        // ADR 0019: DTW puts "a" at 0.15 s but the audio only rises at 0.30 s (a
        // 150 ms early lead). The clamp pushes the caption start forward to the
        // acoustic onset (~0.30), so the word never shows before it is spoken.
        let sr = 16_000u32;
        let mut samples = vec![0.0f32; 2 * sr as usize];
        for s in samples.iter_mut().skip((0.30 * sr as f64) as usize) {
            *s = 0.5; // speech from 0.30 s on
        }
        let t = Transcript {
            language: Language::En,
            units: vec![CaptionUnit { text: "a".into(), start_s: 0.15, end_s: 0.15 }],
        };
        let r = refine_caption_timing(t, &samples, sr);
        assert_eq!(r.units.len(), 1);
        let start = r.units[0].start_s;
        assert!((0.27..=0.34).contains(&start), "clamped to the acoustic onset, got {start}");
    }

    #[test]
    fn refine_leaves_start_when_audio_is_already_present_at_the_dtw_onset() {
        // A correctly-timed word (audio present at its DTW onset) must not be
        // delayed by the clamp.
        let sr = 16_000u32;
        let samples = vec![0.4f32; 2 * sr as usize]; // steady speech throughout
        let t = Transcript {
            language: Language::En,
            units: vec![CaptionUnit { text: "a".into(), start_s: 0.50, end_s: 0.50 }],
        };
        let r = refine_caption_timing(t, &samples, sr);
        assert!((r.units[0].start_s - 0.50).abs() < 1e-6, "unmoved, got {}", r.units[0].start_s);
    }

    #[test]
    fn refine_caps_the_onset_clamp_at_the_max_lead() {
        // A large DTW lead (onset 0.10, audio only at 0.60 = 500 ms) is corrected
        // only up to ONSET_MAX_LEAD_S (to ~0.30), never the full 0.60 — a small,
        // bounded clamp, not an aggressive re-time.
        let sr = 16_000u32;
        let mut samples = vec![0.0f32; 2 * sr as usize];
        for s in samples.iter_mut().skip((0.60 * sr as f64) as usize) {
            *s = 0.5; // speech only from 0.60 s (its peak window 0.10..0.70 still catches it -> kept)
        }
        let t = Transcript {
            language: Language::En,
            units: vec![CaptionUnit { text: "a".into(), start_s: 0.10, end_s: 0.10 }],
        };
        let r = refine_caption_timing(t, &samples, sr);
        assert_eq!(r.units.len(), 1, "kept (peak window reaches the 0.60 s speech)");
        let start = r.units[0].start_s;
        assert!((0.28..=0.32).contains(&start), "capped at +0.20, got {start}");
    }

    #[test]
    fn refine_keeps_a_quiet_aside_amid_loud_moments_via_the_abs_floor() {
        // ADR 0021: a loud reaction (loud_ref ~ 0.5, relative bar ~ 0.05) plus a
        // quiet aside whose onset peak (0.04) is below that bar but above the
        // absolute floor (0.006). The relative bar alone dropped it (the operator's
        // "quiet speech / viewer name not captioned" - the real "Terus" measured
        // 0.0091); the abs floor keeps it. A truly-silent token is still dropped.
        let sr = 16_000u32;
        let mut samples = vec![0.0f32; (35 * sr as usize) / 10]; // 3.5 s
        for s in samples.iter_mut().take(sr as usize) {
            *s = 0.5; // loud reaction 0..1 s
        }
        let (q0, q1) = ((25 * sr as usize) / 10, (29 * sr as usize) / 10); // 2.5..2.9 s
        for s in samples.iter_mut().take(q1).skip(q0) {
            *s = 0.04; // a quiet aside: RMS 0.04 < ~0.05 bar, > 0.006 floor
        }
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "loud".into(), start_s: 0.0, end_s: 0.0 },
                CaptionUnit { text: "ghost".into(), start_s: 1.5, end_s: 1.5 }, // in silence
                CaptionUnit { text: "quiet".into(), start_s: 2.5, end_s: 2.5 },
            ],
        };
        let r = refine_caption_timing(t, &samples, sr);
        let texts: Vec<&str> = r.units.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(texts, vec!["loud", "quiet"], "abs floor keeps the quiet aside; silence dropped");
    }

    #[test]
    fn trace_reports_kept_when_the_onset_clamp_reaches_the_next_onset() {
        // The bug `caption_diag` had (this is the render-side guarantee that fixes it):
        // it reconstructed keep-vs-drop by testing each kept word's *output* start
        // against the next raw onset. The onset clamp (ADR 0019) can push a kept word's
        // start FORWARD all the way to the next word's onset, so that "output start <
        // next onset" test wrongly reported the word DROPped (and desynced every word
        // after it). The trace reports the real per-unit decision, so a
        // kept-but-clamped-to-next word reads Kept. "a" (dtw 0.10) and "b" (dtw 0.20)
        // sit 0.10 s apart; no audio arrives until 0.30 s (past b's onset, beyond the
        // 0.04 s envelope window's reach), so "a"'s forward clamp - bounded by the next
        // onset - lands exactly on 0.20 == b's onset. Its (clamped) start therefore
        // equals `next`, so the old "output start < next onset" test read false and
        // reported "a" DROPped. Its peak window (0.10..0.70) still catches the 0.30 s
        // speech, so refine keeps it - the trace must say Kept.
        let sr = 16_000u32;
        let mut samples = vec![0.0f32; sr as usize]; // 1 s
        for s in samples.iter_mut().skip((0.30 * sr as f64) as usize) {
            *s = 0.5; // speech only from 0.30 s on - past b's onset
        }
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "a".into(), start_s: 0.10, end_s: 0.10 },
                CaptionUnit { text: "b".into(), start_s: 0.20, end_s: 0.20 },
            ],
        };
        let trace = refine_caption_timing_traced(&t, &samples, sr);
        assert_eq!(trace.outcomes.len(), 2, "one outcome per input unit");
        match trace.outcomes[0] {
            UnitOutcome::Kept { start_s, .. } => {
                // The clamp reached "b"'s onset (0.20) exactly - the condition (start ==
                // next) that made the old diag misreport "a" as DROP and desync the rest.
                assert!(start_s >= 0.20 - 1e-6, "a clamped up to b's onset, got {start_s}");
            }
            UnitOutcome::Dropped { peak } => panic!("a wrongly reported dropped (peak {peak})"),
        }
        assert!(matches!(trace.outcomes[1], UnitOutcome::Kept { .. }), "b must be kept");
    }

    #[test]
    fn refine_output_is_exactly_its_trace_filtered() {
        // `refine_caption_timing` is a thin filter over `refine_caption_timing_traced`:
        // the surviving subsequence must equal the Kept outcomes applied in order, so
        // the render and the inspector can never disagree. A clip with a genuine
        // silence-drop exercises both arms.
        let sr = 16_000u32;
        // loud "a" 0.0-0.5, silence, loud "c" 2.0-2.5; spurious "b" at 1.0 in silence.
        let mut samples = vec![0.0f32; 3 * sr as usize];
        for s in samples.iter_mut().take((0.5 * sr as f64) as usize) {
            *s = 0.5;
        }
        for s in samples.iter_mut().skip((2.0 * sr as f64) as usize).take((0.5 * sr as f64) as usize)
        {
            *s = 0.5;
        }
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "a".into(), start_s: 0.0, end_s: 0.4 },
                CaptionUnit { text: "b".into(), start_s: 1.0, end_s: 1.1 },
                CaptionUnit { text: "c".into(), start_s: 2.0, end_s: 2.4 },
            ],
        };
        let trace = refine_caption_timing_traced(&t, &samples, sr);
        let refined = refine_caption_timing(t.clone(), &samples, sr);
        // Derive the expected kept units from the trace, the way `refine` does.
        let expected: Vec<CaptionUnit> = t
            .units
            .iter()
            .zip(&trace.outcomes)
            .filter_map(|(u, o)| match *o {
                UnitOutcome::Kept { start_s, end_s } => {
                    Some(CaptionUnit { text: u.text.clone(), start_s, end_s })
                }
                UnitOutcome::Dropped { .. } => None,
            })
            .collect();
        assert_eq!(refined.units, expected, "refine output == trace's Kept units");
        assert!(matches!(trace.outcomes[1], UnitOutcome::Dropped { .. }), "silent b dropped");
        let texts: Vec<&str> = refined.units.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(texts, vec!["a", "c"]);
    }

    #[test]
    fn captions_are_uppercased() {
        let mut st = style();
        st.genre = CaptionGenre::HugeWord;
        let ass = generate_ass(&units(&["bocil", "gila"]), &st, None);
        assert!(ass.contains("BOCIL") && ass.contains("GILA"));
        assert!(!ass.contains("bocil"));
    }

    #[test]
    fn color_tag_is_bgr_without_alpha() {
        // The \1c/\2c override form is six BGR digits wrapped in &H..&, no alpha.
        assert_eq!(ass_color_tag([255, 0, 0, 255]), "&H0000FF&"); // red
        assert_eq!(ass_color_tag([255, 215, 0, 255]), "&H00D7FF&"); // gold
        // Alpha is dropped (unlike ass_color, which encodes it).
        assert_eq!(ass_color_tag([255, 255, 255, 0]), "&HFFFFFF&");
    }

    fn karaoke_style() -> CaptionStyle {
        let mut st = style();
        st.genre = CaptionGenre::KaraokeFill;
        st // primary white [255,255,255,255], accent gold [255,215,0,255]
    }

    #[test]
    fn karaoke_snap_emits_k_per_word_with_inline_colours() {
        // "a b c" = 5 chars -> one line, one Dialogue, three \k snap chunks.
        let ass = generate_ass(&units(&["a", "b", "c"]), &karaoke_style(), None);
        let dialogues: Vec<&str> = ass.lines().filter(|l| l.starts_with("Dialogue:")).collect();
        assert_eq!(dialogues.len(), 1);
        let d = dialogues[0];
        assert_eq!(d.matches("{\\k").count(), 3); // one karaoke snap chunk per word
        assert!(!d.contains("\\kf"), "instant \\k snap, not the \\kf sweep: {d}");
        // Inline colours: \1c = sung = accent (gold), \2c = unsung = primary (white).
        assert!(d.contains("\\1c&H00D7FF&"), "sung colour: {d}");
        assert!(d.contains("\\2c&HFFFFFF&"), "unsung colour: {d}");
        assert!(d.contains('A') && d.contains('B') && d.contains('C'));
    }

    #[test]
    fn karaoke_snap_durations_track_word_onsets() {
        // Onsets 0.0 / 0.5 / 1.0 -> each non-last word dwells the 0.5 s gap before
        // the next snaps (\k50); the last over its own gap-filled span (1.0->1.4 =
        // \k40). Same cursor maths as the old \kf sweep — only the visual changed.
        let ass = generate_ass(&units(&["a", "b", "c"]), &karaoke_style(), None);
        let d = ass.lines().find(|l| l.starts_with("Dialogue:")).unwrap();
        assert_eq!(d.matches("\\k50").count(), 2); // a and b: onset-to-onset gaps
        assert!(d.contains("\\k40")); // c: its own duration 0.4 s
    }

    #[test]
    fn karaoke_snap_handles_forced_alignment_span_shapes() {
        // The forced-alignment fusion (ADR 0054/0055) emits HONEST per-word
        // spans: non-overlapping, monotonic, with real inter-word gaps where
        // the speaker pauses — unlike the DTW fusion's abutting units (each
        // word ended where the next began). The karaoke cursor must dwell
        // across a gap to the NEXT word's onset (the snap lands on the spoken
        // moment, not the previous word's end), and a zero-width span (a CTC
        // single-frame word) must still advance the cursor (the \k1 floor).
        let t = Transcript {
            language: Language::En,
            units: vec![
                CaptionUnit { text: "a".into(), start_s: 0.0, end_s: 0.2 },
                // 0.8 s silence before b — the dwell spans it: \k from 0.0 to 1.0
                CaptionUnit { text: "b".into(), start_s: 1.0, end_s: 1.3 },
                // zero-width span right after b
                CaptionUnit { text: "c".into(), start_s: 1.3, end_s: 1.3 },
            ],
        };
        let ass = generate_ass(&t, &karaoke_style(), None);
        let d = ass.lines().find(|l| l.starts_with("Dialogue:")).unwrap();
        assert_eq!(d.matches("{\\k").count(), 3);
        assert!(d.contains("\\k100"), "a dwells across the gap to b's onset: {d}");
        assert!(d.contains("\\k30"), "b dwells to c's onset: {d}");
        assert!(d.contains("\\k1}"), "zero-width c floors at 1 cs, never \\k0: {d}");
    }

    #[test]
    fn karaoke_fill_groups_into_lines_like_rolling_pop() {
        // Same character budget as rolling-pop: a long run splits into >1 line,
        // each its own Dialogue, and the count matches group_lines.
        let t = units(&["word0", "word1", "word2", "word3", "word4", "word5"]);
        let ass = generate_ass(&t, &karaoke_style(), None);
        let dialogues = ass.lines().filter(|l| l.starts_with("Dialogue:")).count();
        assert_eq!(dialogues, group_lines(&t, MAX_LINE_CHARS).len());
        assert!(dialogues >= 2);
    }

    #[test]
    fn karaoke_fill_is_uppercased() {
        let ass = generate_ass(&units(&["bocil", "gila"]), &karaoke_style(), None);
        assert!(ass.contains("BOCIL") && ass.contains("GILA"));
        assert!(!ass.contains("bocil"));
    }

    #[test]
    fn default_style_line_matches_the_historical_hardcoded_one() {
        // The editor's appearance fields (outline/shadow/box/bold) default to
        // the values that were hardcoded before it existed: a default style's
        // Style line must stay byte-identical (`1,6,2` borders, black outline,
        // the &H96000000 back colour).
        let ass = generate_ass(&units(&["a"]), &style(), None);
        let line = ass.lines().find(|l| l.starts_with("Style:")).unwrap();
        assert!(
            line.contains(",&H00000000,&H96000000,0,0,0,0,100,100,0,0,1,6,2,5,40,40,40,1"),
            "style line drifted: {line}"
        );
    }

    #[test]
    fn customized_style_line_carries_box_outline_and_bold() {
        let mut st = style();
        st.back_box = true;
        st.back_color = [10, 20, 30, 255]; // opaque RGB(10,20,30)
        st.outline = 3.5;
        st.shadow = 0.0;
        st.outline_color = [255, 0, 0, 255]; // red
        st.bold = true;
        let ass = generate_ass(&units(&["a"]), &st, None);
        let line = ass.lines().find(|l| l.starts_with("Style:")).unwrap();
        assert!(line.contains(",3,3.5,0,5,"), "BorderStyle 3 + widths: {line}");
        assert!(line.contains("&H001E140A"), "back colour BGR: {line}");
        assert!(line.contains("&H000000FF"), "outline colour red: {line}");
        assert!(line.contains(",-1,0,0,0,"), "bold flag: {line}");
    }

    #[test]
    fn no_placement_equals_default_placement_byte_for_byte() {
        // The ADR 0036 golden guard: a Clip with no Caption placement (all
        // pre-editor renders, all headless renders) must produce exactly the
        // pre-placement document — Some(default) and None are the same anchor.
        use yc_core::CaptionPlacement;
        let t = units(&["satu", "dua", "tiga", "empat", "lima"]);
        for genre in [CaptionGenre::HugeWord, CaptionGenre::RollingPop, CaptionGenre::KaraokeFill] {
            let mut st = style();
            st.genre = genre;
            let bare = generate_ass(&t, &st, None);
            let defaulted = generate_ass(&t, &st, Some(CaptionPlacement::default()));
            assert_eq!(bare, defaulted, "genre {genre:?} drifted");
            // And the built-in anchor is what it always was: centered, 46%.
            assert!(bare.contains("\\pos(540,883)"), "anchor moved: {bare}");
        }
    }

    #[test]
    fn placement_moves_the_anchor_and_scales_the_font() {
        use yc_core::CaptionPlacement;
        let p = CaptionPlacement { x_frac: 0.5, y_frac: 0.72, scale: 1.5 };
        let ass = generate_ass(&units(&["halo"]), &style(), Some(p));
        // 1920 * 0.72 = 1382.4 -> 1382; font 96 * 1.5 = 144 in the Style line.
        assert!(ass.contains("\\pos(540,1382)"), "anchor: {ass}");
        assert!(ass.contains("Style: Caption,Anton,144,"), "font size: {ass}");
    }

    #[test]
    fn placement_is_clamped_against_hand_edited_json() {
        // Off-canvas fractions and absurd scales (a hand-edited project.json)
        // clamp instead of flinging the anchor away or the size to 0 — to the
        // SHARED CaptionPlacement bounds, the same ones the editor's resize
        // uses, so preview and burn-in can never disagree about size.
        use yc_core::CaptionPlacement;
        let (x, y, size) = resolve_placement(
            Some(CaptionPlacement { x_frac: -3.0, y_frac: 9.0, scale: 0.0 }),
            96,
        );
        assert_eq!((x, y), (0, 1920));
        assert_eq!(size, (96.0_f32 * CaptionPlacement::SCALE_MIN).round() as u32);
        let (_, _, size_hi) = resolve_placement(
            Some(CaptionPlacement { x_frac: 0.5, y_frac: 0.5, scale: 99.0 }),
            96,
        );
        assert_eq!(size_hi, (96.0_f32 * CaptionPlacement::SCALE_MAX).round() as u32);
    }

    #[test]
    fn word_states_mirror_each_genres_tag_semantics() {
        // Words at onsets 0.0 / 0.5 / 1.0. At t=0.6: rolling-pop has revealed
        // words 0-1 (the \t alpha reveal fired) and not word 2; karaoke has
        // snapped words 0-1 to the accent (\k cursor passed them) with word 2
        // still base; huge-word lines are single-word and always Base.
        let t = units(&["a", "b", "c"]);
        let rolling = preview_lines(&t, CaptionGenre::RollingPop);
        assert_eq!(
            word_states(CaptionGenre::RollingPop, &rolling[0], 0.6),
            vec![WordState::Base, WordState::Base, WordState::Hidden]
        );
        let karaoke = preview_lines(&t, CaptionGenre::KaraokeFill);
        assert_eq!(
            word_states(CaptionGenre::KaraokeFill, &karaoke[0], 0.6),
            vec![WordState::Sung, WordState::Sung, WordState::Base]
        );
        let huge = preview_lines(&t, CaptionGenre::HugeWord);
        assert_eq!(word_states(CaptionGenre::HugeWord, &huge[1], 0.6), vec![WordState::Base]);
        // Before anything is spoken, rolling shows nothing, karaoke all-base.
        assert_eq!(
            word_states(CaptionGenre::RollingPop, &rolling[0], -0.1),
            vec![WordState::Hidden; 3]
        );
        assert_eq!(
            word_states(CaptionGenre::KaraokeFill, &karaoke[0], -0.1),
            vec![WordState::Base; 3]
        );
    }

    #[test]
    fn preview_lines_match_the_emitted_dialogues() {
        // The preview overlay and the ASS output consume the same model: line
        // count equals Dialogue count and line bounds equal the Dialogue times,
        // for every genre.
        let t = units(&["word0", "word1", "word2", "word3", "word4", "word5"]);
        for genre in [CaptionGenre::HugeWord, CaptionGenre::RollingPop, CaptionGenre::KaraokeFill] {
            let mut st = style();
            st.genre = genre;
            let lines = preview_lines(&t, genre);
            let ass = generate_ass(&t, &st, None);
            let dialogues: Vec<&str> = ass.lines().filter(|l| l.starts_with("Dialogue:")).collect();
            assert_eq!(dialogues.len(), lines.len(), "genre {genre:?}");
            for (line, d) in lines.iter().zip(&dialogues) {
                let fields: Vec<&str> = d.split(',').collect();
                assert_eq!(fields[1], ass_time(line.start_s), "start, genre {genre:?}");
                assert_eq!(fields[2], ass_time(line.end_s), "end, genre {genre:?}");
            }
        }
    }

    #[test]
    fn preview_lines_never_overlap_and_are_uppercased() {
        let t = units(&["bocil", "gila", "banget", "sumpah", "keren", "abis"]);
        for genre in [CaptionGenre::HugeWord, CaptionGenre::RollingPop, CaptionGenre::KaraokeFill] {
            let lines = preview_lines(&t, genre);
            assert!(!lines.is_empty());
            for pair in lines.windows(2) {
                assert!(
                    pair[0].end_s <= pair[1].start_s + 1e-9,
                    "lines overlap under {genre:?}: {:?} then {:?}",
                    (pair[0].start_s, pair[0].end_s),
                    (pair[1].start_s, pair[1].end_s)
                );
            }
            let all_upper = lines
                .iter()
                .flat_map(|l| &l.words)
                .all(|w| w.text.chars().all(|c| !c.is_lowercase()));
            assert!(all_upper, "preview words carry the burn-in's uppercasing");
        }
    }
}
