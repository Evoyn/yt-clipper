//! Produce a phantom-SUPPRESSED `clip.ass` by running the REAL
//! `yc_render::suppress_reaction_phantoms` over a shipped `clip.ass` plus a
//! `speaker_diag` per-bin CSV (the laughter mask's `laugh` column). The
//! instrument `caption_overlap_diag` then re-judges the SUPPRESSED file — the
//! ADR 0049 gate loop, exercising the PRODUCTION suppression function itself
//! (not a re-derivation of its logic).
//!
//! It is drop-only, so every surviving Dialogue line is re-emitted VERBATIM
//! (animation tags and all) and only the pile lines vanish; all non-Dialogue
//! lines (the ASS header/styles) pass through untouched. Huge-word `clip.ass`
//! emits one Dialogue per unit in spoken order, so file order IS time order —
//! exactly what the suppression run expects.
//!
//!   cargo run -p yc-render --example caption_suppress_gate -- <in.ass> <bins.csv> <out.ass>

use std::path::PathBuf;

use yc_core::{CaptionUnit, Language, Transcript};

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

/// The `laugh` column of a `speaker_diag` per-bin CSV as a mask, plus the bin
/// step (t[1]-t[0]) — exactly how `caption_overlap_diag` reads it.
fn parse_mask(path: &PathBuf) -> anyhow::Result<(Vec<f32>, f64)> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
    let laugh_col = header.iter().position(|h| *h == "laugh");
    let mut ts: Vec<f64> = Vec::new();
    let mut mask: Vec<f32> = Vec::new();
    for line in lines {
        let c: Vec<&str> = line.split(',').collect();
        if c.is_empty() || c[0].is_empty() {
            continue;
        }
        ts.push(c[0].parse().unwrap_or(0.0));
        mask.push(laugh_col.and_then(|i| c.get(i)).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0));
    }
    let bin_s = if ts.len() >= 2 { ts[1] - ts[0] } else { 0.0417 };
    Ok((mask, bin_s))
}

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let in_ass = PathBuf::from(a.next().expect("usage: caption_suppress_gate <in.ass> <bins.csv> <out.ass>"));
    let csv = PathBuf::from(a.next().expect("bins.csv"));
    let out_ass = PathBuf::from(a.next().expect("out.ass"));

    let raw = std::fs::read_to_string(&in_ass)?;
    // Dialogue lines, IN FILE ORDER, paired with the CaptionUnit the suppressor
    // reasons over. Non-Dialogue lines are remembered by their absolute index so
    // the survivors re-interleave in place.
    let mut units: Vec<CaptionUnit> = Vec::new();
    let mut dialogue_line_idx: Vec<usize> = Vec::new();
    let lines: Vec<&str> = raw.lines().collect();
    for (li, line) in lines.iter().enumerate() {
        let Some(rest) = line.strip_prefix("Dialogue:") else { continue };
        let f: Vec<&str> = rest.splitn(10, ',').collect();
        if f.len() < 10 {
            continue;
        }
        let (Some(start_s), Some(end_s)) = (parse_ass_time(f[1]), parse_ass_time(f[2])) else {
            continue;
        };
        units.push(CaptionUnit { text: strip_ass_tags(f[9]), start_s, end_s });
        dialogue_line_idx.push(li);
    }

    let (mask, bin_s) = parse_mask(&csv)?;
    let drop = yc_render::reaction_phantom_drops(&units, &mask, bin_s);
    let dropped: Vec<usize> = drop.iter().enumerate().filter(|(_, d)| **d).map(|(i, _)| i).collect();

    // Report (the gate reads this before the instrument re-judges the file).
    println!("=== caption_suppress_gate ===");
    println!("in:    {} ({} Dialogue cues)", in_ass.display(), units.len());
    println!("mask:  {} ({} bins @ {:.4}s)", csv.display(), mask.len(), bin_s);
    println!("dropped {} cue(s):", dropped.len());
    for &i in &dropped {
        let u = &units[i];
        println!("  {:>6.2}-{:<6.2} dwell {:.2}  {}", u.start_s, u.end_s, u.end_s - u.start_s, u.text);
    }
    if dropped.is_empty() {
        println!("  (none)");
    }

    // Faithful drop-only emit: same function decides, and each dropped cue's
    // Dialogue line is removed while everything else is byte-preserved. (The
    // suppress call itself is exercised too, so the survivor set can never
    // disagree with the production render's.)
    let survivors = yc_render::suppress_reaction_phantoms(
        Transcript { language: Language::Id, units: units.clone() },
        &mask,
        bin_s,
    );
    anyhow::ensure!(
        survivors.units.len() == units.len() - dropped.len(),
        "suppress() and reaction_phantom_drops() disagree ({} vs {})",
        survivors.units.len(),
        units.len() - dropped.len()
    );
    let drop_set: std::collections::HashSet<usize> = dropped.iter().copied().collect();
    let mut cue_i = 0usize;
    let mut out = String::new();
    for (li, line) in lines.iter().enumerate() {
        let is_dialogue = dialogue_line_idx.binary_search(&li).is_ok();
        if is_dialogue {
            let keep = !drop_set.contains(&cue_i);
            cue_i += 1;
            if !keep {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(&out_ass, out)?;
    println!("\nwrote {} ({} cues survive)", out_ass.display(), units.len() - dropped.len());
    Ok(())
}
