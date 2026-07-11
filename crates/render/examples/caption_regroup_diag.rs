//! Too-fast regrouping instrument (ADR 0057, the measure loop of ADR 0049):
//! given one or more existing huge-word `.ass` artifacts (one word per
//! Dialogue — the pre-grouping output every clip's `clip_align.ass` /
//! `clip_alignburn.ass` is), reconstruct the caption units and run them
//! through the REAL production line model (`preview_lines` + `generate_ass`),
//! printing the before/after cue stats the ADR 0057 bars are judged on:
//!
//!   - sub-`0.40 s` cue rate before vs after (bar A);
//!   - the text-preservation walk (bar B): a grouped line's words must equal
//!     its absorbed input cues' words in order, and every SINGLETON Dialogue
//!     must be **byte-identical** to its input line (bar C's shape) — one
//!     walk proves both, plus the header byte-compare;
//!   - the grouping invariants (bar E): ≤ `3` words, ≤ `22` chars,
//!     no grouped word starting `MIN_READ_S` or later after its group start,
//!     cues monotonic and non-overlapping, `\fs` only on multi-word lines;
//!   - every grouped line, printed (the ADR 0057 eyeball evidence — on clip 3
//!     this is where GUE@28.14 / GEMES@51.48 / JALANANNYA@54.40 are shown to
//!     stay singletons, bar D, and the KAYAK+PINGUIN merge is visible, bar F).
//!
//! Reconstruction is exact up to the `.ass` centisecond quantum: input times
//! are already cs-rounded and both the singleton formula and the clamps are
//! fixed points on them (checked; violations are reported). The production
//! `YC_ALIGN_EMIT=1` emit remains the artifact of record (bar G byte-compare).
//!
//! Writes `<input>.regrouped.ass` beside each input for the A/B burn.
//!
//!   cargo run -p yc-render --example caption_regroup_diag -- <clip.ass> [more.ass ...]

use std::path::{Path, PathBuf};

use yc_core::{CaptionGenre, CaptionStyle, CaptionUnit, Language, Transcript};
use yc_render::{generate_ass, preview_lines};

/// The ADR 0049 read floor — `MIN_READ_S` in ass.rs (kept private there); a
/// cue on screen shorter than this is sub-readable.
const READ_FLOOR_S: f64 = 0.40;

struct Cue {
    start_s: f64,
    end_s: f64,
    text: String,
    raw: String,
}

fn parse_time(t: &str) -> anyhow::Result<f64> {
    // H:MM:SS.cc — centisecond precision, exactly ass_time's output.
    let mut it = t.split(':');
    let (Some(h), Some(m), Some(s)) = (it.next(), it.next(), it.next()) else {
        anyhow::bail!("bad time {t:?}");
    };
    Ok(h.parse::<f64>()? * 3600.0 + m.parse::<f64>()? * 60.0 + s.parse::<f64>()?)
}

fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn parse_ass(path: &Path) -> anyhow::Result<(String, Vec<Cue>)> {
    let src = std::fs::read_to_string(path)?;
    let mut header = String::new();
    let mut cues = Vec::new();
    for line in src.lines() {
        if let Some(rest) = line.strip_prefix("Dialogue: ") {
            // Format: Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text
            // — the Text field may contain commas inside override tags, so
            // split off exactly the first nine fields.
            let fields: Vec<&str> = rest.splitn(10, ',').collect();
            anyhow::ensure!(fields.len() == 10, "short Dialogue line: {line}");
            cues.push(Cue {
                start_s: parse_time(fields[1])?,
                end_s: parse_time(fields[2])?,
                text: strip_tags(fields[9]).trim().to_string(),
                raw: line.to_string(),
            });
        } else {
            header.push_str(line);
            header.push('\n');
        }
    }
    anyhow::ensure!(!cues.is_empty(), "no Dialogue lines in {}", path.display());
    Ok((header, cues))
}

fn sub_floor(spans: &[(f64, f64)]) -> (usize, f64, f64) {
    let mut dwells: Vec<f64> = spans.iter().map(|(s, e)| e - s).collect();
    dwells.sort_by(|a, b| a.total_cmp(b));
    let n = dwells.len();
    // The .ass carries centiseconds: a stored 0.40 must not count as sub-floor
    // through float noise.
    let sub = dwells.iter().filter(|d| **d < READ_FLOOR_S - 1e-9).count();
    (sub, 100.0 * sub as f64 / n.max(1) as f64, if n == 0 { 0.0 } else { dwells[n / 2] })
}

fn run(path: &Path) -> anyhow::Result<()> {
    let (in_header, cues) = parse_ass(path)?;
    println!("\n================ {} ================", path.display());

    // Reconstruction sanity: every input cue must already be a fixed point of
    // the singleton formula (floor + clamp were applied by the emitter that
    // wrote the file). A violation means this .ass is NOT a huge-word
    // one-word-per-Dialogue artifact and the instrument's premise is wrong.
    for i in 0..cues.len() {
        let c = &cues[i];
        anyhow::ensure!(
            c.text.split_whitespace().count() == 1,
            "multi-word input cue #{i} ({:?}) — already regrouped? refusing",
            c.text
        );
        let mut expect = c.end_s.max(c.start_s + 0.10);
        if let Some(n) = cues.get(i + 1) {
            expect = expect.min(n.start_s);
        }
        if (expect - c.end_s).abs() > 5e-3 {
            println!("  WARN cue #{i} {:?} end {:.2} not a singleton fixed point ({expect:.2})", c.text, c.end_s);
        }
    }

    let t = Transcript {
        language: Language::Id,
        units: cues
            .iter()
            .map(|c| CaptionUnit { text: c.text.clone(), start_s: c.start_s, end_s: c.end_s })
            .collect(),
    };
    let lines = preview_lines(&t, CaptionGenre::HugeWord);
    let style = CaptionStyle::for_genre(CaptionGenre::HugeWord);
    let out = generate_ass(&t, &style, None);
    let out_dialogues: Vec<&str> =
        out.lines().filter(|l| l.starts_with("Dialogue:")).collect();
    anyhow::ensure!(out_dialogues.len() == lines.len(), "emitter/line-model drift");

    // Header byte-compare: the regrouped artifact must differ ONLY in cues.
    let out_header: String = out
        .lines()
        .filter(|l| !l.starts_with("Dialogue:"))
        .map(|l| format!("{l}\n"))
        .collect();
    let header_ok = out_header == in_header;

    // The walk (bars B + C): singleton Dialogues byte-identical to the input;
    // grouped lines absorb their input cues' words in order.
    let mut ci = 0usize;
    let mut byte_identical = 0usize;
    let mut walk_ok = true;
    for (li, l) in lines.iter().enumerate() {
        if l.words.len() == 1 {
            if out_dialogues[li] == cues[ci].raw {
                byte_identical += 1;
            } else {
                walk_ok = false;
                println!("  MISMATCH singleton #{li}:\n    in : {}\n    out: {}", cues[ci].raw, out_dialogues[li]);
            }
            ci += 1;
        } else {
            if (l.start_s - cues[ci].start_s).abs() > 1e-9 {
                walk_ok = false;
                println!("  MISMATCH group #{li} start {:.2} != first cue {:.2}", l.start_s, cues[ci].start_s);
            }
            for w in &l.words {
                if w.text != cues[ci].text {
                    walk_ok = false;
                    println!("  MISMATCH group #{li} word {:?} != cue {:?}", w.text, cues[ci].text);
                }
                ci += 1;
            }
        }
    }
    anyhow::ensure!(ci == cues.len(), "walk consumed {ci}/{} cues — words lost or invented", cues.len());

    // Invariants (bar E).
    let mut inv_ok = true;
    for (li, l) in lines.iter().enumerate() {
        let chars: usize =
            l.words.iter().map(|w| w.text.chars().count()).sum::<usize>() + l.words.len() - 1;
        if l.words.len() > 3 || chars > 22 {
            inv_ok = false;
            println!("  INVARIANT line #{li}: {} words / {chars} chars", l.words.len());
        }
        for w in &l.words {
            if w.start_s - l.start_s >= READ_FLOOR_S {
                inv_ok = false;
                println!("  INVARIANT line #{li}: word {:?} bridges a pause", w.text);
            }
        }
        if let Some(n) = lines.get(li + 1) {
            if l.end_s > n.start_s + 1e-9 {
                inv_ok = false;
                println!("  INVARIANT line #{li} overlaps next ({:.2} > {:.2})", l.end_s, n.start_s);
            }
        }
    }

    // The stats the bars are judged on.
    let before: Vec<(f64, f64)> = cues.iter().map(|c| (c.start_s, c.end_s)).collect();
    let after: Vec<(f64, f64)> = lines.iter().map(|l| (l.start_s, l.end_s)).collect();
    let (b_sub, b_pct, b_med) = sub_floor(&before);
    let (a_sub, a_pct, a_med) = sub_floor(&after);
    let grouped: Vec<&yc_render::PreviewLine> =
        lines.iter().filter(|l| l.words.len() > 1).collect();
    println!(
        "  cues {} -> {}   sub-0.40 {}/{} ({:.0}%) -> {}/{} ({:.0}%)   median dwell {:.2}s -> {:.2}s",
        cues.len(), lines.len(), b_sub, cues.len(), b_pct, a_sub, lines.len(), a_pct, b_med, a_med
    );
    println!(
        "  grouped lines: {} ({} words absorbed); singletons byte-identical: {}/{}; header {}; walk {}; invariants {}",
        grouped.len(),
        grouped.iter().map(|l| l.words.len()).sum::<usize>(),
        byte_identical,
        lines.len() - grouped.len(),
        if header_ok { "IDENTICAL" } else { "DIFFERS" },
        if walk_ok { "OK" } else { "FAILED" },
        if inv_ok { "OK" } else { "FAILED" },
    );
    let residual = grouped.iter().filter(|l| l.end_s - l.start_s < READ_FLOOR_S - 1e-9).count();
    println!("  cap-limited grouped lines still sub-floor: {residual}");
    println!("  --- grouped lines (start  dwell  text) ---");
    for l in &grouped {
        let text: Vec<&str> = l.words.iter().map(|w| w.text.as_str()).collect();
        println!("  {:8.2} {:5.2}s  {}", l.start_s, l.end_s - l.start_s, text.join(" "));
    }

    let out_path = PathBuf::from(format!("{}.regrouped.ass", path.display()));
    std::fs::write(&out_path, &out)?;
    println!("  wrote {}", out_path.display());
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    anyhow::ensure!(
        !args.is_empty(),
        "usage: caption_regroup_diag <clip.ass> [more.ass ...] (huge-word one-word-per-cue artifacts)"
    );
    for p in &args {
        run(p)?;
    }
    Ok(())
}
