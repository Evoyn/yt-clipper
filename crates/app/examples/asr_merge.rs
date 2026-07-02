//! Ensemble merge for the caption-perfection loop (2026-07-02).
//!
//! Token-level voting across several ASR decode variants of the SAME audio:
//! pairwise-align every candidate to a backbone transcript (token Levenshtein),
//! then take a plurality vote per backbone position (backbone breaks ties) and
//! majority-of-voters for inserted runs / deletions. The empirical basis: decode
//! variants (bias text, denoise strength, context padding) each nail DIFFERENT
//! regions of a hard clip and garble others - near whisper's/Qwen's limit the
//! alternatives are near-ties, so any perturbation reshuffles the trajectory
//! (ADR 0033's finding, turned from a hazard into a lever). Deterministic,
//! dictionary-free, curation-free.
//!
//!   cargo run -p yt-clipper --example asr_merge -- <out.txt> <backbone.txt> <voter.txt>...

use std::collections::HashMap;
use std::path::PathBuf;

fn normalize(text: &str) -> Vec<String> {
    let t = match text.find("<asr_text>") {
        Some(i) => &text[i + "<asr_text>".len()..],
        None => text,
    };
    t.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Ok,
    Sub,
    Del,
    Ins,
}

fn align(a: &[String], b: &[String]) -> Vec<(Op, Option<usize>, Option<usize>)> {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0u32; m + 1]; n + 1];
    for i in 0..=n {
        d[i][0] = i as u32;
    }
    for j in 0..=m {
        d[0][j] = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[i - 1][j - 1] + if a[i - 1] == b[j - 1] { 0 } else { 1 };
            d[i][j] = sub.min(d[i - 1][j] + 1).min(d[i][j - 1] + 1);
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && d[i][j] == d[i - 1][j - 1] && a[i - 1] == b[j - 1] {
            ops.push((Op::Ok, Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if i > 0 && j > 0 && d[i][j] == d[i - 1][j - 1] + 1 {
            ops.push((Op::Sub, Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if i > 0 && d[i][j] == d[i - 1][j] + 1 {
            ops.push((Op::Del, Some(i - 1), None));
            i -= 1;
        } else {
            ops.push((Op::Ins, None, Some(j - 1)));
            j -= 1;
        }
    }
    ops.reverse();
    ops
}

/// Marker for "this voter says the backbone token isn't there".
const DEL: &str = "\u{1}del";

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out_path = PathBuf::from(args.next().expect(
        "usage: asr_merge <out.txt> <backbone.txt> <voter.txt>...",
    ));
    let backbone_path = PathBuf::from(args.next().expect("backbone file"));
    let voter_paths: Vec<PathBuf> = args.map(PathBuf::from).collect();
    anyhow::ensure!(!voter_paths.is_empty(), "need at least one voter");

    let backbone = normalize(&std::fs::read_to_string(&backbone_path)?);
    let n = backbone.len();
    // votes[i]: what each voter thinks backbone position i should be (token or DEL).
    let mut votes: Vec<Vec<String>> = vec![Vec::new(); n];
    // gap_runs[i]: token runs voters INSERT before backbone position i (i==n: at end).
    let mut gap_runs: Vec<Vec<Vec<String>>> = vec![Vec::new(); n + 1];

    for vp in &voter_paths {
        let cand = normalize(&std::fs::read_to_string(vp)?);
        let ops = align(&backbone, &cand);
        let mut pending_run: Vec<String> = Vec::new();
        for (op, bi, ci) in ops {
            match op {
                Op::Ok | Op::Sub => {
                    let i = bi.unwrap();
                    if !pending_run.is_empty() {
                        gap_runs[i].push(std::mem::take(&mut pending_run));
                    }
                    votes[i].push(cand[ci.unwrap()].clone());
                }
                Op::Del => {
                    let i = bi.unwrap();
                    if !pending_run.is_empty() {
                        gap_runs[i].push(std::mem::take(&mut pending_run));
                    }
                    votes[i].push(DEL.to_string());
                }
                Op::Ins => pending_run.push(cand[ci.unwrap()].clone()),
            }
        }
        if !pending_run.is_empty() {
            gap_runs[n].push(pending_run);
        }
    }

    let n_voters = voter_paths.len();
    // Inserts need a STRICT voter majority (the backbone implicitly votes "no"):
    // with an even pool, half the voters sharing a padding artifact (context
    // bleed past the clip edge) must not be able to insert it.
    let majority = n_voters / 2 + 1;
    // Dictionary (YC_DICT): used by the lattice pass for edit-1 expansion and
    // for the strong-win filter (a decisive vote for a real word is settled —
    // the LM rescorer only sees contested or garble-suspect slots).
    // Lossy read: the bundled wordlists aren't guaranteed clean UTF-8.
    let dict: std::collections::HashSet<String> = match std::env::var("YC_DICT") {
        Ok(d) => String::from_utf8_lossy(&std::fs::read(d)?)
            .lines()
            .map(|l| l.trim().to_lowercase())
            .filter(|l| !l.is_empty())
            .collect(),
        Err(_) => Default::default(),
    };
    let mut merged: Vec<String> = Vec::new();
    // For each merged token: the backbone position it was voted at (None for
    // inserted-run tokens) — the lattice emission keys alternatives off this —
    // and how decisively it won there.
    let mut origin: Vec<Option<usize>> = Vec::new();
    let mut win_count: Vec<usize> = Vec::new();
    let mut log = String::new();
    for i in 0..=n {
        // Inserted runs before position i: keep one if a majority of voters
        // inserted a run starting with the same token here (backbone's implicit
        // vote is "nothing here", so a strict voter majority is required).
        if !gap_runs[i].is_empty() {
            let mut by_first: HashMap<&str, Vec<&Vec<String>>> = HashMap::new();
            for run in &gap_runs[i] {
                by_first.entry(run[0].as_str()).or_default().push(run);
            }
            if let Some((_, runs)) = by_first
                .iter()
                .max_by_key(|(_, v)| v.len())
                .filter(|(_, v)| v.len() >= majority)
            {
                // The most common full run among the agreeing voters.
                let mut counts: HashMap<String, usize> = HashMap::new();
                for r in runs {
                    *counts.entry(r.join(" ")).or_default() += 1;
                }
                let run = counts.into_iter().max_by_key(|(_, c)| *c).unwrap().0;
                log.push_str(&format!("  gap@{i}: +\"{run}\" ({}/{} voters)\n", runs.len(), n_voters));
                for t in run.split_whitespace() {
                    merged.push(t.to_string());
                    origin.push(None);
                    win_count.push(0);
                }
            }
        }
        if i == n {
            break;
        }
        // Position vote: backbone token + one vote per voter; plurality wins,
        // backbone breaks ties. A DEL plurality drops the token.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        *counts.entry(backbone[i].as_str()).or_default() += 1;
        for v in &votes[i] {
            *counts.entry(v.as_str()).or_default() += 1;
        }
        let backbone_count = counts[backbone[i].as_str()];
        let (winner, wc) = counts
            .iter()
            .max_by_key(|(t, c)| (**c, **t == backbone[i].as_str()))
            .map(|(t, c)| (t.to_string(), *c))
            .unwrap();
        let winner = if wc == backbone_count { backbone[i].clone() } else { winner };
        if winner == DEL {
            log.push_str(&format!("  pos@{i}: -\"{}\" (dropped, {}x del)\n", backbone[i], wc));
        } else {
            if winner != backbone[i] {
                log.push_str(&format!(
                    "  pos@{i}: \"{}\" -> \"{winner}\" ({wc}/{} incl. backbone)\n",
                    backbone[i],
                    n_voters + 1
                ));
            }
            merged.push(winner.clone());
            origin.push(Some(i));
            win_count.push(if winner == backbone[i] { backbone_count } else { wc });
        }
    }

    let text = merged.join(" ");
    std::fs::write(&out_path, &text)?;
    println!("--- merge decisions ---\n{log}");
    println!("--- merged ({} tokens) -> {} ---\n{text}", merged.len(), out_path.display());

    // Optional lattice emission (YC_LATTICE=<path>): per merged-token alternative
    // sets for a language-model rescoring pass — the two-pass ASR shape (acoustic
    // candidates from the decodes, LM disambiguates). YC_DICT=<words.txt> expands
    // each contested set with dictionary words within edit distance 1 of an
    // acoustic alternative (len >= 4), so a word EVERY decode garbled the same
    // way (e.g. a consonant the model never heard) can still be offered — the
    // acoustic evidence bounds the candidates, the dictionary restores the
    // posterior-adjacent spellings, the LM picks by context. No curation input.
    if let Ok(lat_path) = std::env::var("YC_LATTICE") {
        // Alternatives come from the vote set at each merged token's backbone
        // position (recorded at merge time); inserted-run tokens have no vote
        // set and get none. A slot the vote won DECISIVELY with a dictionary
        // word is settled — offering it to the LM rescorer only invites
        // second-guessing (measured: it swapped a strong-vote "satu" for
        // "apa"). Only contested or garble-suspect (out-of-dictionary) slots
        // are emitted.
        let mut lat = String::new();
        for (k, tok) in merged.iter().enumerate() {
            let Some(i) = origin[k] else { continue };
            let strong_real_word =
                dict.contains(tok.as_str()) && win_count[k] * 2 > n_voters + 1;
            if strong_real_word {
                continue;
            }
            let mut set: Vec<&str> = std::iter::once(backbone[i].as_str())
                .chain(votes[i].iter().map(|s| s.as_str()))
                .filter(|s| *s != DEL)
                .collect();
            set.sort();
            set.dedup();
            if set.len() < 2 {
                continue;
            }
            let mut alts: Vec<String> = set.iter().map(|s| s.to_string()).collect();
            // dictionary expansion: edit-1 neighbors of any acoustic alternative
            // (sorted: HashSet order is nondeterministic, the cap must not be)
            let mut extra: Vec<String> = dict
                .iter()
                .filter(|w| {
                    w.len() >= 4
                        && !alts.iter().any(|a| a == *w)
                        && alts.iter().any(|a| edit1(a, w))
                })
                .cloned()
                .collect();
            extra.sort();
            extra.truncate(6);
            alts.extend(extra);
            lat.push_str(&format!("[{k}] {tok} | {}\n", alts.join(" ")));
        }
        std::fs::write(&lat_path, &lat)?;
        println!("--- lattice (contested positions) -> {lat_path} ---\n{lat}");
    }
    Ok(())
}

/// True when `a` and `b` are within one edit (sub/ins/del) of each other.
fn edit1(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (n, m) = (a.len(), b.len());
    if n.abs_diff(m) > 1 {
        return false;
    }
    if n == m {
        return a.iter().zip(&b).filter(|(x, y)| x != y).count() <= 1;
    }
    // lengths differ by 1: check one skip on the longer side
    let (long, short) = if n > m { (&a, &b) } else { (&b, &a) };
    let mut i = 0;
    while i < short.len() && long[i] == short[i] {
        i += 1;
    }
    long[i + 1..] == short[i..]
}
