//! Laughter-aware caption-hold instrument (ADR 0062, the measure loop of the
//! lane-3 general arc): compute the shared-reaction mask over a clip's MIXED
//! analysis audio (the ADR 0045/0046 tagger, production conventions), join it
//! against the PRODUCTION caption cues (an emitted huge-word .ass: one cue =
//! one refined unit), and measure the two laugh classes:
//!
//!   - **pop-on-laugh**: a cue whose ONSET sits inside a mask run — the
//!     mis-onset family (ADR 0051); counted, scored against the gt's phantom
//!     rows (bar R1), and NEVER touched by the trim;
//!   - **HOL (hold-over-laugh)**: seconds of an unmasked-onset cue riding
//!     over mask runs — the CORP-hold class (2026-07-12) the trim exists for.
//!
//! It then applies the SAME pure function production would call
//! (`yc_render::trim_reaction_holds`) and re-measures: bars R3-R6 (efficacy,
//! invariants, named controls, zero words) print as PASS/FAIL; R2's defect
//! size and the per-clip judgment belong to ADR 0062's table. Karaoke/rolling
//! line-regrouping flips (the `group_lines` gap rule) are detected and each
//! flip is required to sit on a trimmed unit (bar R5's coupling clause).
//!
//!   cargo run -p yt-clipper --features face --example caption_laugh_diag -- \
//!     <analysis.wav> <start_s> <end_s> <clip.ass> [groundtruth.txt|-]
//!
//! The .ass parse is centisecond-quantized (ASS time format), so the what-if
//! trim predicts a wired production emit to <= 0.01 s (the ADR 0057 recorded
//! edge class). NO GPU, no decode: the tagger is a CPU ort session.

use std::path::{Path, PathBuf};

use yc_core::{CaptionGenre, CaptionUnit, Language, TimeRange, Transcript};
use yc_ingest::read_range_samples;

/// Readability floor, mirroring `MIN_READ_S` in yc-render's ass.rs (ADR 0013)
/// — cited like caption_overlap_diag's READ_FLOOR_S; the trim never cuts a
/// cue below it, so every post-trim mask residue must sit inside
/// `[onset, onset + this)`.
const READ_FLOOR_S: f64 = 0.40;
/// R1 tolerance: a masked step within this of a gt-named laughter moment.
const GT_LAUGH_TOL_S: f64 = 0.75;

fn parse_ass_time(s: &str) -> Option<f64> {
    let s = s.trim();
    let (h, rest) = s.split_once(':')?;
    let (m, sec) = rest.split_once(':')?;
    Some(h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?)
}

/// Strip `{...}` ASS override blocks, return the visible burned word.
fn strip_ass_tags(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Parse a huge-word emit: one Dialogue = one refined unit (start, end, word).
fn parse_ass_units(path: &Path) -> anyhow::Result<Vec<CaptionUnit>> {
    let text = std::fs::read_to_string(path)?;
    let mut units = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("Dialogue:") else { continue };
        let f: Vec<&str> = rest.splitn(10, ',').collect();
        if f.len() < 10 {
            continue;
        }
        let (Some(start_s), Some(end_s)) = (parse_ass_time(f[1]), parse_ass_time(f[2])) else {
            continue;
        };
        units.push(CaptionUnit { text: strip_ass_tags(f[9]), start_s, end_s });
    }
    units.sort_by(|a, b| a.start_s.partial_cmp(&b.start_s).unwrap());
    anyhow::ensure!(!units.is_empty(), "no Dialogue cues in {}", path.display());
    Ok(units)
}

/// gt `phantom` rows' heard_s — the operator-named laughter moments (the
/// 2026-07-08 correction: phantom = a real word ON a laugh, so its heard time
/// marks the LAUGH; bar R1 asks the mask to see what the ear named).
fn parse_gt_laugh_spots(path: &Path) -> anyhow::Result<Vec<f64>> {
    let mut out = Vec::new();
    for line in std::fs::read_to_string(path)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split('|').next().unwrap_or(line);
        let toks: Vec<&str> = fields.split_whitespace().collect();
        if toks.len() >= 2 && toks[0] == "phantom" {
            if let Ok(t) = toks[1].parse::<f64>() {
                out.push(t);
            }
        }
    }
    Ok(out)
}

/// Seconds of `[s, e)` covered by the mask runs.
fn overlap_s(s: f64, e: f64, runs: &[(f64, f64)]) -> f64 {
    runs.iter().map(|&(l, m)| (e.min(m) - s.max(l)).max(0.0)).sum()
}

fn masked_at(t: f64, runs: &[(f64, f64)]) -> bool {
    runs.iter().any(|&(l, m)| l <= t && t < m)
}

/// (pop-on-laugh count, HOL seconds, per-cue HOL detail rows).
fn measure(units: &[CaptionUnit], runs: &[(f64, f64)]) -> (usize, f64, Vec<String>) {
    let mut pops = 0usize;
    let mut hol = 0.0f64;
    let mut rows = Vec::new();
    for u in units {
        if masked_at(u.start_s, runs) {
            pops += 1;
            continue;
        }
        let o = overlap_s(u.start_s, u.end_s, runs);
        if o > 1e-9 {
            hol += o;
            rows.push(format!(
                "    {:<14} {:>6.2}-{:<6.2} rides {:.2}s over the mask",
                u.text, u.start_s, u.end_s, o
            ));
        }
    }
    (pops, hol, rows)
}

/// Line boundaries as unit indices (index of each line's first unit) for the
/// grouping genres — the regroup-flip detector (R5's coupling clause).
fn line_boundaries(units: &[CaptionUnit], genre: CaptionGenre) -> Vec<usize> {
    let t = Transcript { language: Language::Id, units: units.to_vec() };
    let lines = yc_render::preview_lines(&t, genre);
    let mut idx = 0usize;
    let mut bounds = Vec::new();
    for l in &lines {
        bounds.push(idx);
        idx += l.words.len();
    }
    bounds
}

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let wav = PathBuf::from(a.next().expect(
        "usage: caption_laugh_diag <analysis.wav> <start_s> <end_s> <clip.ass> [gt|-]",
    ));
    let start_s: f64 = a.next().expect("start_s").parse()?;
    let end_s: f64 = a.next().expect("end_s").parse()?;
    let ass = PathBuf::from(a.next().expect("clip.ass"));
    let gt_spots = match a.next().as_deref() {
        None | Some("-") => Vec::new(),
        Some(p) => parse_gt_laugh_spots(Path::new(p))?,
    };
    let range = TimeRange { start_s, end_s };

    let models = PathBuf::from("models");
    let tag_model = models.join("sherpa-onnx-zipformer-audio-tagging-2024-04-09.onnx");
    let tag_labels = models.join("audioset_class_labels_indices.csv");
    anyhow::ensure!(
        tag_model.is_file() && tag_labels.is_file(),
        "tagger files missing under {} (Diagnostics downloads)",
        models.display()
    );

    // The MIXED analysis audio for the clip range — the tagger's production
    // input (ADR 0050's wiring precedent; never the sep/enh caption variants).
    let samples = read_range_samples(&wav, range)?;
    let dur = range.duration_s();
    let steps = yc_frame::reaction::laugh_steps(&tag_model, &tag_labels, &samples, dur)?;
    let tau = yc_frame::speaker::REACTION_TAU;
    let runs = yc_frame::reaction::mask_runs(&steps, tau);
    let masked_total: f64 = runs.iter().map(|&(l, m)| m - l).sum();

    let pre = parse_ass_units(&ass)?;

    println!("=== caption_laugh_diag (ADR 0062) ===");
    println!("wav:  {} [{start_s:.1}-{end_s:.1}] ({dur:.1}s)", wav.display());
    println!("ass:  {} ({} cues)", ass.display(), pre.len());
    println!(
        "mask: tau {tau} | {} run(s), {masked_total:.2}s masked ({:.0}% of clip)",
        runs.len(),
        100.0 * masked_total / dur
    );
    for &(l, m) in &runs {
        println!("    run {l:>6.2}-{m:<6.2} ({:.2}s)", m - l);
    }

    // --- R1: the mask sees what the operator's ear named (gt phantom rows) ---
    if !gt_spots.is_empty() {
        let mut hits = 0usize;
        println!("\n--- R1: gt-named laughter moments vs the mask (tol +/-{GT_LAUGH_TOL_S}s) ---");
        for &spot in &gt_spots {
            let hit = steps.iter().enumerate().any(|(k, &s)| {
                s >= tau && ((k as f64 + 0.5) * yc_frame::reaction::TAG_STEP_S - spot).abs()
                    <= GT_LAUGH_TOL_S
            });
            if hit {
                hits += 1;
            }
            println!("    laugh@{spot:<5.2} {}", if hit { "MASKED" } else { "off-mask" });
        }
        println!(
            "R1: {hits}/{} gt laughter moments masked -> {}",
            gt_spots.len(),
            if hits >= 2 { "PASS (>=2)" } else { "FAIL" }
        );
    }

    // --- PRE: the defect, sized (R2's value) ---
    let (pre_pops, pre_hol, pre_rows) = measure(&pre, &runs);
    println!("\n--- PRE (production cues as emitted) ---");
    println!("pop-on-laugh {pre_pops} cue(s) | HOL {pre_hol:.2}s over {} cue(s)", pre_rows.len());
    for r in &pre_rows {
        println!("{r}");
    }

    // --- the trim, through the production function ---
    let mut post = pre.clone();
    let trims = yc_render::trim_reaction_holds(&mut post, &runs);
    println!("\n--- TRIM (production fn): {} cue(s) trimmed ---", trims.len());
    for t in &trims {
        println!(
            "    {:<14} {:>6.2}: end {:.2} -> {:.2} (run @{:.2}, -{:.2}s)",
            post[t.index].text,
            post[t.index].start_s,
            t.old_end_s,
            t.new_end_s,
            t.run_start_s,
            t.old_end_s - t.new_end_s
        );
    }

    // --- POST + bars R3/R4/R5/R6 ---
    let (post_pops, post_hol, post_rows) = measure(&post, &runs);
    println!("\n--- POST ---");
    println!("pop-on-laugh {post_pops} cue(s) | HOL {post_hol:.2}s over {} cue(s)", post_rows.len());
    for r in &post_rows {
        println!("{r}");
    }

    // R3 clause 2: every residual masked second is floor-protected.
    let mut residue_ok = true;
    for u in &post {
        if masked_at(u.start_s, &runs) {
            continue;
        }
        let all = overlap_s(u.start_s, u.end_s, &runs);
        let floored = overlap_s(u.start_s, u.end_s.min(u.start_s + READ_FLOOR_S), &runs);
        if (all - floored).abs() > 1e-6 {
            residue_ok = false;
            println!("    R3 VIOLATION: {:?} residue past the floor window", u.text);
        }
    }

    // R4 invariants.
    let mut r4 = true;
    if pre.len() != post.len() {
        r4 = false;
        println!("    R4 VIOLATION: unit count changed");
    }
    for (b, a) in pre.iter().zip(&post) {
        if b.text != a.text {
            r4 = false;
            println!("    R4 VIOLATION: text {:?} -> {:?}", b.text, a.text);
        }
        if (b.start_s - a.start_s).abs() > 1e-9 {
            r4 = false;
            println!("    R4 VIOLATION: onset moved on {:?}", b.text);
        }
        if a.end_s > b.end_s + 1e-9 {
            r4 = false;
            println!("    R4 VIOLATION: end grew on {:?}", b.text);
        }
    }
    for t in &trims {
        let d = post[t.index].end_s - post[t.index].start_s;
        if d + 1e-9 < READ_FLOOR_S {
            r4 = false;
            println!("    R4 VIOLATION: trimmed {:?} below the floor ({d:.2}s)", post[t.index].text);
        }
    }
    if pre_pops != post_pops {
        r4 = false;
        println!("    R4 VIOLATION: pop-on-laugh count changed {pre_pops} -> {post_pops}");
    }

    // R5 coupling clause: karaoke/rolling regroup flips only at trimmed units.
    let mut r5_flips_ok = true;
    let pre_b = line_boundaries(&pre, CaptionGenre::KaraokeFill);
    let post_b = line_boundaries(&post, CaptionGenre::KaraokeFill);
    let flips: Vec<usize> = post_b
        .iter()
        .filter(|k| !pre_b.contains(k))
        .chain(pre_b.iter().filter(|k| !post_b.contains(k)))
        .copied()
        .collect();
    if flips.is_empty() {
        println!("\nkaraoke regroup: line boundaries identical ({} lines)", pre_b.len());
    } else {
        println!(
            "\nkaraoke regroup: {} -> {} lines, {} boundary flip(s):",
            pre_b.len(),
            post_b.len(),
            flips.len()
        );
        for k in &flips {
            let on_trim = *k > 0 && trims.iter().any(|t| t.index == k - 1);
            if !on_trim {
                r5_flips_ok = false;
            }
            println!(
                "    boundary before {:?}@{:.2} — preceding unit {} trimmed: {}",
                post.get(*k).map(|u| u.text.as_str()).unwrap_or("?"),
                post.get(*k).map(|u| u.start_s).unwrap_or(0.0),
                k.saturating_sub(1),
                if on_trim { "YES (a laugh IS the after-silence pause)" } else { "NO — VIOLATION" }
            );
        }
    }

    // R6: zero words added/removed (structural for a trim; measured anyway).
    let r6 = pre.len() == post.len()
        && pre.iter().zip(&post).all(|(b, a)| b.text == a.text);

    println!("\n=== BARS (generic; R2/R3 thresholds judged per clip in ADR 0062) ===");
    println!("HOL pre {pre_hol:.2}s -> post {post_hol:.2}s ({}% residual){}",
        if pre_hol > 0.0 { format!("{:.0}", 100.0 * post_hol / pre_hol) } else { "n/a ".into() },
        if pre_hol > 0.0 && post_hol <= 0.30 * pre_hol { "  [<=30%: R3 clause 1 PASS]" } else if pre_hol > 0.0 { "  [R3 clause 1 FAIL]" } else { "" });
    println!("R3 residue floor-protected: {}", if residue_ok { "PASS" } else { "FAIL" });
    println!("R4 invariants:              {}", if r4 { "PASS" } else { "FAIL" });
    println!("R5 trims named+on-mask:     {} trim(s) printed above (run onset structural); regroup flips on trims: {}",
        trims.len(), if r5_flips_ok { "PASS" } else { "FAIL" });
    println!("R6 zero words +/-:          {}", if r6 { "PASS" } else { "FAIL" });
    Ok(())
}
