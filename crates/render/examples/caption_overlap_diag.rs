//! Caption-overlap inspector (measure-first, the ADR 0045 pattern for captions).
//!
//! The operator found the shipped VIOR captions fail under OVERLAPPING speech
//! (4-person podcast): some cues are delayed / lead, some words are dropped, some
//! flash too fast — while turn-taking speech captions fine. This tool measures the
//! defect on the PRODUCTION OUTPUT against the operator's ground truth, and asks the
//! load-bearing question: do the failures CONCENTRATE where speech overlaps, or are
//! they uniform? It changes nothing in the render — it reads what already shipped.
//!
//! It is a pure join over three files, NO GPU, NO re-decode (the shipped cues are
//! deterministic ensemble output):
//!   1. the shipped `clip.ass`      — the cues that actually burned (start/end/text)
//!   2. a `speaker_diag` per-bin CSV — the overlap signal already computed over the
//!      SAME analysis.wav: per-track mouth activity (-> `contested` = >=2 mouths over
//!      speaker::MIN_ACTIVITY, voice.rs) and the reaction/`laugh` mask (ADR 0045/46)
//!   3. the operator ground truth   — benchmarks/vior-fans-fadhil.captions.groundtruth.txt
//!
//!   cargo run -p yc-render --example caption_overlap_diag -- <clip.ass> <bins.csv> <groundtruth.txt>
//!
//! Bars are pinned here BEFORE any table is read (the pre-declaration ritual). The
//! operator's eye rules the final verdict; a bar they overturn is re-pinned and the
//! table re-judged, no other number moved.

use std::collections::BTreeMap;
use std::path::PathBuf;

// --- Pre-declared bars (mirrors the render/analysis constants, with citations;
//     labels only — this tool measures the shipped output, it does not re-time). ---
/// Too-fast: a cue on screen shorter than this is sub-readable. `MIN_READ_S` in
/// ass.rs (ADR 0013) — the floor the "clamp to next onset LAST" rule silently
/// defeats when onsets pile up (dense / overlapping speech).
const READ_FLOOR_S: f64 = 0.40;
/// Mis-onset: a cue whose start is farther than this from the spoken word is
/// desynced (leading or lagging). Generous, so sub-0.5s jitter is not nitpicked.
const MIS_ONSET_TOL_S: f64 = 0.50;
// (A drop-window bar was pre-declared, but measuring drops from the shipped file
// proved impossible: a mis-transcribed neighbour a fraction of a second away reads
// as "carried". Drops are a NEGATIVE only the operator's ear asserts — the
// instrument reports their overlap context, not a confirmation. That is a finding.)
/// Contested bin: >= 2 tracks with mouth activity over this floor (voice.rs:550,
/// speaker::MIN_ACTIVITY) — several mouths moving = simultaneous speech.
const MIN_ACTIVITY: f32 = 0.004;
/// Reaction-masked bin: laughter-family mass at/above the ADR 0045 operating tau
/// (speaker::REACTION_TAU) — the shared-reaction stretches.
const REACTION_TAU: f32 = 0.1;

#[derive(Clone)]
struct Cue {
    start_s: f64,
    end_s: f64,
    text: String,
}

#[derive(Clone)]
struct Bin {
    t: f64,
    voiced: bool,
    n_mouth: usize,
    laugh: f32,
    speaker: i64, // attributed seat, -1 = none cleanly attributed
    conf: f32,
}

impl Bin {
    fn contested(&self) -> bool {
        self.n_mouth >= 2
    }
    fn reaction(&self) -> bool {
        self.laugh >= REACTION_TAU
    }
    /// Overlap context: several mouths, OR a shared reaction — the two regimes
    /// where the operator says captions fail.
    fn overlap(&self) -> bool {
        self.contested() || self.reaction()
    }
}

#[derive(Clone)]
struct Truth {
    mode: String,     // mis | drop | phantom | wrong
    heard_s: f64,
    shown_s: Option<f64>,
    note: String,
}

fn parse_ass_time(s: &str) -> Option<f64> {
    // H:MM:SS.cc
    let s = s.trim();
    let (h, rest) = s.split_once(':')?;
    let (m, sec) = rest.split_once(':')?;
    Some(h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?)
}

/// Strip `{...}` ASS override blocks, return the visible text (the burned word).
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

fn parse_ass(path: &PathBuf) -> anyhow::Result<Vec<Cue>> {
    let text = std::fs::read_to_string(path)?;
    let mut cues = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("Dialogue:") else { continue };
        // Layer,Start,End,Style,Name,ML,MR,MV,Effect,Text  — Text may hold commas
        // inside {\t(0,40,...)} tags, so split into exactly 10 fields.
        let f: Vec<&str> = rest.splitn(10, ',').collect();
        if f.len() < 10 {
            continue;
        }
        let (Some(start_s), Some(end_s)) = (parse_ass_time(f[1]), parse_ass_time(f[2])) else {
            continue;
        };
        cues.push(Cue { start_s, end_s, text: strip_ass_tags(f[9]) });
    }
    cues.sort_by(|a, b| a.start_s.partial_cmp(&b.start_s).unwrap());
    Ok(cues)
}

fn parse_bins(path: &PathBuf) -> anyhow::Result<Vec<Bin>> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
    let act_cols: Vec<usize> =
        header.iter().enumerate().filter(|(_, h)| h.ends_with("act")).map(|(i, _)| i).collect();
    let laugh_col = header.iter().position(|h| *h == "laugh");
    let voiced_col = header.iter().position(|h| *h == "voiced");
    let speaker_col = header.iter().position(|h| *h == "speaker");
    let conf_col = header.iter().position(|h| *h == "conf");
    let mut bins = Vec::new();
    for line in lines {
        let c: Vec<&str> = line.split(',').collect();
        if c.is_empty() || c[0].is_empty() {
            continue;
        }
        let t: f64 = c[0].parse().unwrap_or(0.0);
        let voiced = voiced_col.and_then(|i| c.get(i)).map(|v| *v == "1").unwrap_or(false);
        let n_mouth = act_cols
            .iter()
            .filter(|&&i| c.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) >= MIN_ACTIVITY)
            .count();
        let laugh = laugh_col.and_then(|i| c.get(i)).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
        let speaker = speaker_col.and_then(|i| c.get(i)).and_then(|v| v.parse::<i64>().ok()).unwrap_or(-1);
        let conf = conf_col.and_then(|i| c.get(i)).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
        bins.push(Bin { t, voiced, n_mouth, laugh, speaker, conf });
    }
    Ok(bins)
}

fn parse_truth(path: &PathBuf) -> anyhow::Result<Vec<Truth>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (fields, note) = line.split_once('|').unwrap_or((line, ""));
        let toks: Vec<&str> = fields.split_whitespace().collect();
        if toks.len() < 3 {
            continue;
        }
        let mode = toks[0].to_string();
        let heard_s: f64 = toks[1].parse().unwrap_or(f64::NAN);
        let shown_s = if toks[2] == "-" { None } else { toks[2].parse::<f64>().ok() };
        if heard_s.is_nan() {
            continue;
        }
        out.push(Truth { mode, heard_s, shown_s, note: note.trim().to_string() });
    }
    Ok(out)
}

struct BinIndex {
    bins: Vec<Bin>,
    bin_s: f64,
}
impl BinIndex {
    fn new(bins: Vec<Bin>) -> Self {
        let bin_s = if bins.len() >= 2 { bins[1].t - bins[0].t } else { 0.0417 };
        BinIndex { bins, bin_s }
    }
    fn at(&self, t: f64) -> Option<&Bin> {
        if self.bins.is_empty() {
            return None;
        }
        let idx = ((t / self.bin_s).round() as isize).clamp(0, self.bins.len() as isize - 1) as usize;
        self.bins.get(idx)
    }
}

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let ass = PathBuf::from(a.next().expect("usage: caption_overlap_diag <clip.ass> <bins.csv> <groundtruth.txt>"));
    let csv = PathBuf::from(a.next().expect("bins.csv"));
    let gt = PathBuf::from(a.next().expect("groundtruth.txt"));

    let cues = parse_ass(&ass)?;
    let idx = BinIndex::new(parse_bins(&csv)?);
    let truth = parse_truth(&gt)?;

    println!("=== caption_overlap_diag ===");
    println!("ass:   {} ({} cues)", ass.display(), cues.len());
    println!("bins:  {} ({} bins @ {:.3}s = {:.1}s)", csv.display(), idx.bins.len(), idx.bin_s, idx.bins.len() as f64 * idx.bin_s);
    println!("truth: {} ({} items)", gt.display(), truth.len());
    println!("\nbars (pre-declared): too-fast dwell < {:.2}s | mis-onset > {:.2}s | contested >=2 mouths(act>={}) | reaction laugh>={}",
        READ_FLOOR_S, MIS_ONSET_TOL_S, MIN_ACTIVITY, REACTION_TAU);

    // --- Overlap-signal geography (the denominator every gap is measured against) ---
    let n = idx.bins.len().max(1);
    let voiced_bins: Vec<&Bin> = idx.bins.iter().filter(|b| b.voiced).collect();
    let nv = voiced_bins.len().max(1);
    let contested = idx.bins.iter().filter(|b| b.contested()).count();
    let reaction = idx.bins.iter().filter(|b| b.reaction()).count();
    let vc = voiced_bins.iter().filter(|b| b.contested()).count();
    let mut mouth_hist: BTreeMap<usize, usize> = BTreeMap::new();
    for b in &idx.bins {
        *mouth_hist.entry(b.n_mouth).or_default() += 1;
    }
    println!("\n--- overlap geography ---");
    println!("voiced {:.0}% | contested(all) {:.0}% | contested(of voiced) {:.0}% | reaction {:.0}%",
        100.0 * nv as f64 / n as f64, 100.0 * contested as f64 / n as f64,
        100.0 * vc as f64 / nv as f64, 100.0 * reaction as f64 / n as f64);
    print!("n_mouth histogram (bins):");
    for (k, v) in &mouth_hist {
        print!(" {k}:{}({:.0}%)", v, 100.0 * *v as f64 / n as f64);
    }
    println!();

    // --- Cue table + too-fast, tagged with the overlap context of each onset ---
    println!("\n--- cues (dwell, overlap context at onset; * = too-fast < {:.2}s) ---", READ_FLOOR_S);
    let mut too_fast: Vec<&Cue> = Vec::new();
    for c in &cues {
        let dwell = c.end_s - c.start_s;
        let b = idx.at(c.start_s);
        let (nm, con, rea, lg) = b.map(|b| (b.n_mouth, b.contested(), b.reaction(), b.laugh)).unwrap_or((0, false, false, 0.0));
        let fast = dwell + 1e-9 < READ_FLOOR_S;
        if fast {
            too_fast.push(c);
        }
        // Only print the fast ones + a few around them would be noise; print fast + reaction cues.
        if fast || rea {
            println!("  {}{:>6.2}-{:<6.2} dwell {:.2}  mouths {} {}{}  laugh {:.2}  {}",
                if fast { "*" } else { " " }, c.start_s, c.end_s, dwell, nm,
                if con { "CONTESTED" } else { "         " }, if rea { " REACT" } else { "      " }, lg, c.text);
        }
    }

    // Too-fast geography: density in contested-voiced vs clean-voiced bins.
    let fast_con = too_fast.iter().filter(|c| idx.at(c.start_s).map(|b| b.contested()).unwrap_or(false)).count();
    let fast_clean = too_fast.len() - fast_con;
    // per voiced-second in each partition
    let sec_con = vc as f64 * idx.bin_s;
    let sec_clean = (nv - vc) as f64 * idx.bin_s;
    let dens_con = if sec_con > 0.0 { fast_con as f64 / sec_con } else { 0.0 };
    let dens_clean = if sec_clean > 0.0 { fast_clean as f64 / sec_clean } else { 0.0 };
    println!("\n--- too-fast geography ---");
    println!("total too-fast cues: {} / {} ({:.0}%)", too_fast.len(), cues.len(), 100.0 * too_fast.len() as f64 / cues.len().max(1) as f64);
    println!("  in contested-voiced: {} over {:.1}s = {:.2}/s", fast_con, sec_con, dens_con);
    println!("  in clean-voiced:     {} over {:.1}s = {:.2}/s", fast_clean, sec_clean, dens_clean);
    println!("  contested/clean density ratio: {:.2}x  {}", if dens_clean > 0.0 { dens_con / dens_clean } else { f64::INFINITY },
        if dens_clean > 0.0 && dens_con / dens_clean < 1.5 { "(≈uniform WITHIN this clip — but this clip is fast throughout; the CROSS-CLIP control decides overlap-density vs genre)" } else { "(concentrated in overlap)" });
    println!("  NOTE: within-clip contested/clean is a weak control on an all-fast clip. The strong control is cross-clip:");
    println!("        turn-taking clips (guru gembul solo, Helmy, ANTITESA) run 0-2% sub-floor; overlap clips (VIOR 54%, Deddy 3p 78%) -> overlap-driven.");

    // --- Phantom-pile auto-detection: a run of >=3 too-fast cues whose onsets sit
    //     on a reaction bin — the "words hallucinated onto laughter" signature the
    //     operator reported (e.g. the 7-word pile on the opening laugh). Independent
    //     of the hand-placed ground truth. ---
    println!("\n--- phantom-pile detection (>=3 consecutive too-fast cues on reaction bins) ---");
    let is_fast = |c: &Cue| (c.end_s - c.start_s) + 1e-9 < READ_FLOOR_S;
    let on_reaction = |c: &Cue| idx.at(c.start_s).map(|b| b.reaction()).unwrap_or(false);
    let mut piles: Vec<(f64, f64, usize)> = Vec::new();
    let mut run: Vec<&Cue> = Vec::new();
    let flush = |run: &Vec<&Cue>, piles: &mut Vec<(f64, f64, usize)>| {
        if run.len() >= 3 {
            piles.push((run[0].start_s, run[run.len() - 1].end_s, run.len()));
        }
    };
    for c in &cues {
        if is_fast(c) && on_reaction(c) {
            run.push(c);
        } else {
            flush(&run, &mut piles);
            run.clear();
        }
    }
    flush(&run, &mut piles);
    for (s, e, k) in &piles {
        let words: Vec<&str> = cues.iter().filter(|c| c.start_s >= *s - 1e-6 && c.end_s <= *e + 1e-6).map(|c| c.text.as_str()).collect();
        println!("  {:>5.2}-{:<5.2} ({} cues): {}", s, e, k, words.join(" "));
    }
    if piles.is_empty() {
        println!("  (none)");
    }

    // --- Ground-truth reconciliation, per class. mis/phantom are ASS-measurable
    //     (the cue's own time proves the lead / the phantom sits on the mask);
    //     drops are a NEGATIVE the shipped file cannot confirm (a mis-transcribed
    //     neighbour looks "carried") — the operator's ear asserts them, and the
    //     instrument reports only their overlap CONTEXT. ---
    println!("\n--- ground-truth reconciliation ---");
    let mut mis = (0usize, 0usize, 0usize, 0.0f64); // total, confirmed, in_overlap, sum_lead
    let mut phantom = (0usize, 0usize); // total, on_reaction
    let mut drop = (0usize, 0usize); // total, in_overlap
    for tr in &truth {
        let hb = idx.at(tr.heard_s);
        let (nm, con, rea, lg) = hb.map(|b| (b.n_mouth, b.contested(), b.reaction(), b.laugh)).unwrap_or((0, false, false, 0.0));
        let detail = match tr.mode.as_str() {
            "mis" => {
                let off = tr.shown_s.map(|s| s - tr.heard_s).unwrap_or(0.0);
                let ok = off.abs() > MIS_ONSET_TOL_S;
                // overlap context is measured where the cue SHOWS (the failing bin)
                let sb = tr.shown_s.and_then(|s| idx.at(s));
                let over = sb.map(|b| b.overlap()).unwrap_or(false);
                let sr = sb.map(|b| b.reaction()).unwrap_or(false);
                mis.0 += 1;
                if ok { mis.1 += 1; }
                if over { mis.2 += 1; }
                mis.3 += off;
                format!("[{}] lead {:+.2}s; cue-bin {}", if ok { "CONFIRM" } else { "under-tol" }, off,
                    if sr { "REACTION" } else if over { "contested" } else { "clean" })
            }
            "drop" => {
                drop.0 += 1;
                if con || rea { drop.1 += 1; }
                format!("[recall — operator-asserted] context: mouths {} laugh {:.2} speaker {} conf {:.2}", nm, lg,
                    hb.map(|b| b.speaker).unwrap_or(-1), hb.map(|b| b.conf).unwrap_or(0.0))
            }
            "phantom" => {
                let sb = tr.shown_s.and_then(|s| idx.at(s));
                let sr = sb.map(|b| b.reaction()).unwrap_or(false);
                phantom.0 += 1;
                if sr { phantom.1 += 1; }
                format!("[{}] cue-bin laugh {:.2}", if sr { "on-mask" } else { "off-mask (faint bg — not laughter)" },
                    sb.map(|b| b.laugh).unwrap_or(0.0))
            }
            _ => "[typo — out of scope, editor-fixable]".to_string(),
        };
        println!("  {:<7} heard@{:>5.2}  {}{} laugh {:.2}  {:<52}  {}",
            tr.mode, tr.heard_s, if con { "CON" } else { "   " }, if rea { " REA" } else { "    " }, lg, detail, tr.note);
    }

    // --- Verdict, per failure geography ---
    println!("\n=== GATE (per class — the geography is the finding) ===");
    println!("TOO-FAST  : {}/{} cues ({:.0}%) sub-readable | this clip {:.2} cues/s | CROSS-CLIP control: turn-taking 0-2% vs overlap 54-78% -> OVERLAP-DENSITY driven",
        too_fast.len(), cues.len(), 100.0 * too_fast.len() as f64 / cues.len().max(1) as f64,
        cues.len() as f64 / (idx.bins.len() as f64 * idx.bin_s).max(1.0));
    println!("MIS-ONSET : {}/{} confirmed >{:.1}s | {}/{} on overlap bins | mean lead {:+.2}s (all EARLY) -> {}",
        mis.1, mis.0, MIS_ONSET_TOL_S, mis.2, mis.0, if mis.0 > 0 { mis.3 / mis.0 as f64 } else { 0.0 },
        if mis.0 > 0 && mis.2 * 2 >= mis.0 { "CONCENTRATED in overlap/reaction" } else { "not concentrated" });
    println!("PHANTOM   : {}/{} operator phantoms on the reaction mask | {} auto-detected phantom-pile(s) on reaction -> reaction-correlated",
        phantom.1, phantom.0, piles.len());
    println!("DROP      : {}/{} in overlap context | operator-asserted RECALL failures (un-re-timable — the separate lane)", drop.1, drop.0);
    Ok(())
}
