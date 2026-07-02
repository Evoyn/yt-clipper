//! Qwen3-ASR ensemble captions (opt-in, `YC_QWEN_ENS=1`) — words from a
//! multi-decode vote, timing from whisper.
//!
//! Measured basis (2026-07-02, the "Diskusi biasa" benchmark, operator ground
//! truth in `benchmarks/diskusi-biasa.groundtruth.txt`): near whisper's limit
//! on masked fast slang, ANY single decode — whisper's or Qwen3-ASR's, any
//! knob — garbles a different region (near-tie trajectories, ADR 0033's
//! finding). But the garbles ANTICORRELATE across decode variants (denoise
//! strength, context padding, biasing text), so a token-level plurality vote
//! across variants recovers what no single decode can: on the benchmark, the
//! ensemble read real speech through whisper's 107-"eh" hallucination pile,
//! recovered a 3 s missed-speech hole, "bangke", "bajingan bajingan", and
//! "pusing cok" from the plain mix — with zero dialect-store input.
//!
//! Shape: whisper still decodes the clip (it owns TIMING — DTW word onsets,
//! the karaoke path); the Qwen ensemble owns WORDS. The merged word stream is
//! aligned back onto whisper's units: matched words adopt whisper's span,
//! whisper-only units DROP (they are the hallucination class the vote
//! outvoted), ensemble-only runs get character-proportional spans inside the
//! enclosing whisper gap. Downstream (refine, ASS, karaoke, LLM correct) is
//! unchanged.
//!
//! ADR 0033 compliance: strictly opt-in (env knob, default OFF = byte-identical
//! renders), read here so the render and any diag can't diverge; the dialect
//! store is neither read nor written by the ensemble (auto-harvest is skipped
//! by the caller when the ensemble ran — harvest indexes are whisper-keyed).
//! GPU staging stays sequential: whisper's one-shot load is dropped before the
//! sidecar spawns (llama-mtmd-cli exits between variants).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result};
use yc_core::{CaptionUnit, Language, TimeRange, Transcript};

/// The opt-in knob. Read via helper so callers and future diags agree.
pub fn enabled() -> bool {
    matches!(
        std::env::var("YC_QWEN_ENS").ok().as_deref().map(str::trim),
        Some("1") | Some("true") | Some("on")
    )
}

/// Everything the ensemble needs from the caller (paths are derived by the
/// caller from its own sidecar/model layout; nothing here reads global state).
pub struct EnsembleConfig {
    /// llama-mtmd-cli.exe (pinned llama.cpp sidecar, fetch-llama-sidecar.ps1).
    pub mtmd_cli: PathBuf,
    /// Qwen3-ASR GGUF pair (fetch-models.ps1).
    pub qwen_model: PathBuf,
    pub qwen_mmproj: PathBuf,
    pub ffmpeg: PathBuf,
    /// deep-filter.exe when present — without it the denoised variants are
    /// skipped and the vote runs on the remaining decodes (weaker, still valid).
    pub deep_filter: Option<PathBuf>,
    /// Scratch directory for the variant wavs (the session's data dir).
    pub work_dir: PathBuf,
}

/// Seconds of leading audio context prepended to the clip range for the padded
/// variants. Measured: the decode "warms up" on the preceding speech and reads
/// the clip's opening better; TAIL padding only donates bleed, so none is used.
const HEAD_PAD_S: f64 = 5.0;

/// One decode variant: which audio view + how it is biased.
struct Variant {
    /// deep-filter attenuation limit (None = raw mix).
    atten: Option<u32>,
    head_pad: bool,
}

/// The measured voter set (H1/E8/E10a/E12-shaped). Order matters only for the
/// backbone: the first variant is the merge backbone (a6 + head-pad — the
/// benchmark's best-shaped single decode).
const VARIANTS: &[Variant] = &[
    Variant { atten: Some(6), head_pad: true },
    Variant { atten: Some(12), head_pad: false },
    Variant { atten: Some(6), head_pad: false },
    Variant { atten: Some(12), head_pad: true },
    Variant { atten: None, head_pad: false },
];

/// Biasing context handed to Qwen3-ASR's system prompt. GENERIC per language —
/// a global gaming-stream lexicon, never per-clip or per-Creator content (the
/// no-curation contract; measured to fix profanity bowdlerization, e.g.
/// "bacingan" -> "bajingan"). Long lists measurably DERAIL the decode — keep it
/// one sentence.
fn bias_context(language: Language) -> &'static str {
    match language {
        Language::Id => {
            "The audio is in Indonesian (bahasa Indonesia) from a horror gaming \
             livestream. Common slang and profanity that may occur: anjing, anying, \
             bangke, bajingan, cok, goblok, kampret, biadab, ngeri, pusing, njir, buset."
        }
        Language::En => "The audio is in English, from a gaming livestream.",
        Language::Ja => "The audio is in Japanese, from a gaming livestream.",
    }
}

/// Run the full ensemble for one clip range and fuse the voted words onto the
/// whisper transcript's timing. `whisper` is the production transcript for the
/// same range (its units carry the DTW times; its words join the vote pool).
/// Fails soft at the caller (whisper captions stand when anything here errors).
pub fn apply(
    cfg: &EnsembleConfig,
    analysis_wav: &Path,
    range: TimeRange,
    whisper: &Transcript,
) -> Result<Transcript> {
    anyhow::ensure!(cfg.mtmd_cli.is_file(), "mtmd sidecar missing: {}", cfg.mtmd_cli.display());
    anyhow::ensure!(cfg.qwen_model.is_file(), "qwen model missing: {}", cfg.qwen_model.display());
    anyhow::ensure!(
        cfg.qwen_mmproj.is_file(),
        "qwen mmproj missing: {}",
        cfg.qwen_mmproj.display()
    );
    std::fs::create_dir_all(&cfg.work_dir)?;

    // --- 1. decode variants (one-shot sidecar spawns; GPU-sequential) -------
    let mut decodes: Vec<Vec<String>> = Vec::new();
    for (i, v) in VARIANTS.iter().enumerate() {
        if v.atten.is_some() && cfg.deep_filter.is_none() {
            continue;
        }
        let wav = match variant_wav(cfg, analysis_wav, range, v, i) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("qwen ensemble: variant {i} audio prep failed: {e:#}");
                continue;
            }
        };
        match decode_one(cfg, &wav, whisper.language, v.head_pad) {
            Ok(words) if !words.is_empty() => {
                tracing::info!(
                    "qwen ensemble: variant {i} (atten={:?} pad={}) -> {} words",
                    v.atten,
                    v.head_pad,
                    words.len()
                );
                decodes.push(words);
            }
            Ok(_) => tracing::warn!("qwen ensemble: variant {i} produced no words"),
            Err(e) => tracing::warn!("qwen ensemble: variant {i} decode failed: {e:#}"),
        }
    }
    anyhow::ensure!(decodes.len() >= 2, "qwen ensemble: <2 variants decoded, vote impossible");

    // --- 2. vote (whisper's words join as a voter; backbone = variant 0) ----
    let whisper_words: Vec<String> =
        whisper.units.iter().flat_map(|u| normalize(&u.text)).collect();
    let backbone = decodes[0].clone();
    let mut voters: Vec<Vec<String>> = decodes[1..].to_vec();
    if !whisper_words.is_empty() {
        voters.push(whisper_words);
    }
    let merged = vote_merge(&backbone, &voters);
    tracing::info!(
        "qwen ensemble: vote over {} voters + backbone -> {} words",
        voters.len(),
        merged.len()
    );

    // --- 3. fuse words onto whisper timing ----------------------------------
    let fused = fuse_onto_timing(&merged, whisper, range.duration_s());
    tracing::info!(
        "qwen ensemble: fused {} units (whisper had {})",
        fused.len(),
        whisper.units.len()
    );
    Ok(Transcript { language: whisper.language, units: fused })
}

/// Cut (and optionally denoise) one variant's audio view of the range.
fn variant_wav(
    cfg: &EnsembleConfig,
    analysis_wav: &Path,
    range: TimeRange,
    v: &Variant,
    idx: usize,
) -> Result<PathBuf> {
    let start = if v.head_pad { (range.start_s - HEAD_PAD_S).max(0.0) } else { range.start_s };
    let dur = range.end_s - start;
    let cut = cfg.work_dir.join(format!("_ens_{idx}.wav"));
    let status = Command::new(&cfg.ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-ss"])
        .arg(format!("{start}"))
        .arg("-t")
        .arg(format!("{dur}"))
        .arg("-i")
        .arg(analysis_wav)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(&cut)
        .status()
        .context("spawning ffmpeg for ensemble cut")?;
    anyhow::ensure!(status.success(), "ffmpeg ensemble cut failed ({status})");
    let Some(atten) = v.atten else { return Ok(cut) };
    let df = cfg.deep_filter.as_ref().expect("checked by caller");
    // deep-filter keeps the input basename in the output dir.
    let out_dir = cfg.work_dir.join(format!("_ens_df{idx}"));
    std::fs::create_dir_all(&out_dir)?;
    let status = Command::new(df)
        .arg("-a")
        .arg(atten.to_string())
        .arg("-o")
        .arg(&out_dir)
        .arg(&cut)
        .status()
        .context("spawning deep-filter for ensemble variant")?;
    anyhow::ensure!(status.success(), "deep-filter failed ({status})");
    Ok(out_dir.join(cut.file_name().expect("cut has a name")))
}

/// One one-shot llama-mtmd-cli decode -> normalized words. Head-padded
/// variants keep their extra leading words: the vote's strict-majority insert
/// rule outvotes pad bleed, and the timing fusion drops anything whisper's
/// range has no anchor or gap for.
fn decode_one(
    cfg: &EnsembleConfig,
    wav: &Path,
    language: Language,
    _head_pad: bool,
) -> Result<Vec<String>> {
    let out = Command::new(&cfg.mtmd_cli)
        .arg("-m")
        .arg(&cfg.qwen_model)
        .arg("--mmproj")
        .arg(&cfg.qwen_mmproj)
        .arg("--audio")
        .arg(wav)
        .args(["--temp", "0", "-ngl", "99", "-p", "Transcribe the audio."])
        .args(["-sys", bias_context(language)])
        .output()
        .context("spawning llama-mtmd-cli")?;
    anyhow::ensure!(
        out.status.success(),
        "llama-mtmd-cli failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).chars().take(400).collect::<String>()
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(normalize(&stdout))
}

/// Lowercased word tokens; strips Qwen's `language X<asr_text>` header and all
/// punctuation. The same normalization the benchmark scorer uses.
pub fn normalize(text: &str) -> Vec<String> {
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

#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Ok,
    Sub,
    Del,
    Ins,
}

/// Token-level Levenshtein alignment (ops keyed to `a`=left, `b`=right).
fn align(a: &[String], b: &[String]) -> Vec<(Op, Option<usize>, Option<usize>)> {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0u32; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i as u32;
    }
    for j in 0..=m {
        d[0][j] = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[i - 1][j - 1] + u32::from(a[i - 1] != b[j - 1]);
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

/// Marker: a voter says "the backbone token isn't there".
const DEL_VOTE: &str = "\u{1}del";

/// Plurality vote per backbone position (backbone breaks ties); inserted runs
/// need a STRICT voter majority (the backbone implicitly votes "no", and half
/// an even pool sharing a padding artifact must not carry it). Ported verbatim
/// from the benchmark-validated `asr_merge` inspector.
pub fn vote_merge(backbone: &[String], voters: &[Vec<String>]) -> Vec<String> {
    let n = backbone.len();
    let mut votes: Vec<Vec<String>> = vec![Vec::new(); n];
    let mut gap_runs: Vec<Vec<Vec<String>>> = vec![Vec::new(); n + 1];
    for cand in voters {
        let ops = align(backbone, cand);
        let mut pending: Vec<String> = Vec::new();
        for (op, bi, ci) in ops {
            match op {
                Op::Ok | Op::Sub => {
                    let i = bi.expect("ok/sub keeps a backbone index");
                    if !pending.is_empty() {
                        gap_runs[i].push(std::mem::take(&mut pending));
                    }
                    votes[i].push(cand[ci.expect("ok/sub keeps a voter index")].clone());
                }
                Op::Del => {
                    let i = bi.expect("del keeps a backbone index");
                    if !pending.is_empty() {
                        gap_runs[i].push(std::mem::take(&mut pending));
                    }
                    votes[i].push(DEL_VOTE.to_string());
                }
                Op::Ins => pending.push(cand[ci.expect("ins keeps a voter index")].clone()),
            }
        }
        if !pending.is_empty() {
            gap_runs[n].push(pending);
        }
    }
    let majority = voters.len() / 2 + 1;
    let mut merged: Vec<String> = Vec::new();
    for i in 0..=n {
        if !gap_runs[i].is_empty() {
            let mut by_first: std::collections::HashMap<&str, Vec<&Vec<String>>> =
                std::collections::HashMap::new();
            for run in &gap_runs[i] {
                by_first.entry(run[0].as_str()).or_default().push(run);
            }
            if let Some((_, runs)) = by_first
                .iter()
                .max_by_key(|(_, v)| v.len())
                .filter(|(_, v)| v.len() >= majority)
            {
                let mut counts: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for r in runs {
                    *counts.entry(r.join(" ")).or_default() += 1;
                }
                let run = counts.into_iter().max_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0))).unwrap().0;
                merged.extend(run.split_whitespace().map(String::from));
            }
        }
        if i == n {
            break;
        }
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        *counts.entry(backbone[i].as_str()).or_default() += 1;
        for v in &votes[i] {
            *counts.entry(v.as_str()).or_default() += 1;
        }
        let backbone_count = counts[backbone[i].as_str()];
        let (winner, wc) = counts
            .iter()
            // deterministic: count first, then backbone-preference, then lexical
            .max_by_key(|(t, c)| (**c, **t == backbone[i].as_str(), std::cmp::Reverse(**t)))
            .map(|(t, c)| (t.to_string(), *c))
            .expect("counts never empty");
        let winner = if wc == backbone_count { backbone[i].clone() } else { winner };
        if winner != DEL_VOTE {
            merged.push(winner);
        }
    }
    merged
}

/// Align the voted words onto whisper's timed units. Matched positions adopt
/// whisper's span with the voted word; whisper-only units DROP (the
/// hallucination class); voted-only runs are laid out character-proportionally
/// inside the enclosing whisper gap (clip edges bound the outermost gaps).
/// Zero-width results are fine downstream (DTW single-token units already are).
pub fn fuse_onto_timing(merged: &[String], whisper: &Transcript, clip_dur_s: f64) -> Vec<CaptionUnit> {
    let whisper_words: Vec<String> =
        whisper.units.iter().map(|u| u.text.trim().to_lowercase()).collect();
    // Whisper units can be multi-word after store corrections; align on the
    // units' primary token for anchoring (fusion cares about time, not text).
    let anchor_tokens: Vec<String> = whisper_words
        .iter()
        .map(|w| normalize(w).into_iter().next().unwrap_or_default())
        .collect();
    let ops = align(&anchor_tokens, merged.to_vec().as_slice());
    let mut fused: Vec<CaptionUnit> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut last_end = 0.0_f64;
    let flush_pending = |fused: &mut Vec<CaptionUnit>,
                         pending: &mut Vec<String>,
                         gap_start: f64,
                         gap_end: f64| {
        if pending.is_empty() {
            return;
        }
        let total_chars: usize = pending.iter().map(|w| w.chars().count().max(1)).sum();
        let gap = (gap_end - gap_start).max(0.0);
        let mut t = gap_start;
        for w in pending.drain(..) {
            let frac = w.chars().count().max(1) as f64 / total_chars as f64;
            let d = gap * frac;
            fused.push(CaptionUnit { text: w, start_s: t, end_s: (t + d).min(gap_end) });
            t += d;
        }
    };
    for (op, wi, mi) in ops {
        match op {
            Op::Ok | Op::Sub => {
                let u = &whisper.units[wi.expect("anchored")];
                flush_pending(&mut fused, &mut pending, last_end, u.start_s);
                fused.push(CaptionUnit {
                    text: merged[mi.expect("anchored")].clone(),
                    start_s: u.start_s,
                    end_s: u.end_s,
                });
                last_end = u.end_s;
            }
            Op::Del => {
                // whisper-only unit: dropped (outvoted fabrication) — but its
                // span still advances the gap cursor so inserted runs before
                // and after it don't overlap.
                let u = &whisper.units[wi.expect("del keeps whisper index")];
                flush_pending(&mut fused, &mut pending, last_end, u.start_s);
                last_end = last_end.max(u.end_s);
            }
            Op::Ins => pending.push(merged[mi.expect("ins keeps merged index")].clone()),
        }
    }
    flush_pending(&mut fused, &mut pending, last_end, clip_dur_s.max(last_end));
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn vote_restores_majority_word_over_backbone_garble() {
        // The benchmark's bajingan/pacingan case: backbone garbles, plurality
        // holds (a TIE keeps the backbone, so the win needs a strict count).
        let backbone = words("pacingan pacingan bangke");
        let voters = vec![
            words("bajingan bajingan bangke"),
            words("bajingan bajingan bangke"),
            words("racengan racengan bangke"),
        ];
        assert_eq!(vote_merge(&backbone, &voters), words("bajingan bajingan bangke"));
        // Tie (2 bajingan vs backbone+1 pacingan) -> backbone stands.
        let voters =
            vec![words("bajingan bajingan bangke"), words("bajingan bajingan bangke"), words("pacingan pacingan bangke")];
        assert_eq!(vote_merge(&backbone, &voters), words("pacingan pacingan bangke"));
    }

    #[test]
    fn vote_insert_needs_strict_majority() {
        // Two of four voters carrying tail bleed must NOT insert it.
        let backbone = words("pusing kan dibilang");
        let voters = vec![
            words("pusing kan dibilang suka dah"),
            words("pusing kan dibilang suka dah"),
            words("pusing kan dibilang"),
            words("pusing kan dibilang"),
        ];
        assert_eq!(vote_merge(&backbone, &voters), words("pusing kan dibilang"));
        // Three of four DO carry it.
        let voters = vec![
            words("pusing kan dibilang suka dah"),
            words("pusing kan dibilang suka dah"),
            words("pusing kan dibilang suka dah"),
            words("pusing kan dibilang"),
        ];
        assert_eq!(vote_merge(&backbone, &voters), words("pusing kan dibilang suka dah"));
    }

    #[test]
    fn vote_drops_backbone_token_on_del_plurality() {
        // Pad bleed: only the backbone (and no voter) has the leading words.
        let backbone = words("ada nggak itu kebuka");
        let voters = vec![words("kebuka"), words("kebuka"), words("kebuka")];
        assert_eq!(vote_merge(&backbone, &voters), words("kebuka"));
    }

    #[test]
    fn fuse_adopts_whisper_timing_for_matches_and_drops_whisper_only() {
        let whisper = Transcript {
            language: Language::Id,
            units: vec![
                CaptionUnit { text: "mana".into(), start_s: 1.0, end_s: 1.4 },
                CaptionUnit { text: "eh".into(), start_s: 2.0, end_s: 2.0 },
                CaptionUnit { text: "bangke".into(), start_s: 3.0, end_s: 3.5 },
            ],
        };
        let merged = words("mana bangke");
        let fused = fuse_onto_timing(&merged, &whisper, 5.0);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].text, "mana");
        assert_eq!((fused[0].start_s, fused[0].end_s), (1.0, 1.4));
        assert_eq!(fused[1].text, "bangke");
        assert_eq!((fused[1].start_s, fused[1].end_s), (3.0, 3.5));
    }

    #[test]
    fn fuse_lays_inserted_run_into_the_whisper_gap() {
        let whisper = Transcript {
            language: Language::Id,
            units: vec![
                CaptionUnit { text: "keren".into(), start_s: 1.0, end_s: 2.0 },
                CaptionUnit { text: "banget".into(), start_s: 6.0, end_s: 6.5 },
            ],
        };
        // qwen recovered two words whisper missed inside the 2.0-6.0 hole
        let merged = words("keren mana dah banget");
        let fused = fuse_onto_timing(&merged, &whisper, 8.0);
        assert_eq!(fused.len(), 4);
        assert_eq!(fused[1].text, "mana");
        assert_eq!(fused[2].text, "dah");
        assert!(fused[1].start_s >= 2.0 - 1e-9 && fused[2].end_s <= 6.0 + 1e-9);
        assert!(fused[1].end_s <= fused[2].start_s + 1e-9);
        // ordering stays monotonic
        for w in fused.windows(2) {
            assert!(w[0].start_s <= w[1].start_s + 1e-9);
        }
    }

    #[test]
    fn fuse_tail_run_bounded_by_clip_duration() {
        let whisper = Transcript {
            language: Language::Id,
            units: vec![CaptionUnit { text: "mulai".into(), start_s: 0.5, end_s: 1.0 }],
        };
        let merged = words("mulai satu dua");
        let fused = fuse_onto_timing(&merged, &whisper, 4.0);
        assert_eq!(fused.len(), 3);
        assert!(fused[2].end_s <= 4.0 + 1e-9);
        assert!(fused[1].start_s >= 1.0 - 1e-9);
    }

    #[test]
    fn normalize_strips_asr_header_and_punctuation() {
        assert_eq!(
            normalize("language Indonesian<asr_text>Mana? Ini satu."),
            words("mana ini satu")
        );
    }
}
