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
    timing_extra: Option<&Transcript>,
    samples: &[f32],
    sample_rate: u32,
    lexicon: &crate::DialectLexicon,
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

    // --- 3. fuzzy store application (the curation-survives-engine-swap tier) ---
    // The dialect store's `wrong` keys are whisper's exact garbles (ADR 0033),
    // so they never literally match another engine's spelling of the SAME
    // mishear (whisper "dijekat" vs the ensemble's "dijegat"). An edit-1
    // tolerant match transfers the operator's existing confirmed corrections
    // across engines — no new curation. Guards make it conservative: single-
    // word wrongs of >=4 chars, never on a token that is a real dictionary
    // word, context-sensitive pairs skipped (they need the LLM pass by design).
    let mut merged = merged;
    apply_store_fuzzy(&mut merged, lexicon);

    // --- 4. fuse words onto the anchor skeleton + speech onsets -------------
    let fused = fuse_onto_timing(
        &merged,
        whisper,
        timing_extra,
        samples,
        sample_rate,
        range.duration_s(),
    );
    tracing::info!(
        "qwen ensemble: fused {} units (whisper had {}, extra skeleton {})",
        fused.len(),
        whisper.units.len(),
        timing_extra.map(|t| t.units.len()).unwrap_or(0)
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

/// Transfer the store's confirmed corrections onto the voted words with
/// edit-1 tolerance (see the call site for why exact keys can't match across
/// engines). Two tiers per single-word `wrong`: exact token match, or — only
/// when the token is NOT a real dictionary word and the wrong is >=4 chars —
/// an edit-1 match. `context: true` pairs are skipped (LLM-pass territory,
/// ADR 0030); multi-word wrongs are skipped in this tier (whisper's
/// multi-word garble shapes don't transfer across engines). `right` may be
/// multi-word; it splices in normalized.
pub fn apply_store_fuzzy(words: &mut Vec<String>, lexicon: &crate::DialectLexicon) {
    let pairs: Vec<(String, Vec<String>)> = lexicon
        .corrections
        .iter()
        .filter(|c| !c.right.is_empty() && !c.context)
        .filter_map(|c| {
            let wrong = normalize(&c.wrong);
            if wrong.len() != 1 {
                return None;
            }
            Some((wrong.into_iter().next().expect("len checked"), normalize(&c.right)))
        })
        .collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    for tok in words.iter() {
        let exact = pairs.iter().find(|(w, _)| w == tok);
        let hit = exact.or_else(|| {
            if lexicon.dictionary.contains(tok.as_str()) {
                return None;
            }
            pairs.iter().find(|(w, _)| w.chars().count() >= 4 && edit1(tok, w))
        });
        match hit {
            Some((w, right)) if right != &[tok.clone()] => {
                tracing::info!("qwen ensemble: store fuzzy \"{tok}\" (~\"{w}\") -> \"{}\"", right.join(" "));
                out.extend(right.iter().cloned());
            }
            _ => out.push(tok.clone()),
        }
    }
    *words = out;
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
    let (long, short) = if n > m { (&a, &b) } else { (&b, &a) };
    let mut i = 0;
    while i < short.len() && long[i] == short[i] {
        i += 1;
    }
    long[i + 1..] == short[i..]
}

/// Align the voted words onto a TIMED ANCHOR skeleton and lay the rest onto
/// speech-energy onsets.
///
/// The skeleton is the time-sorted union of the production whisper units and
/// (when provided) a second whisper decode's units — on masked clips the
/// default decode's spans are as unreliable as its words (a 4 s phantom unit
/// over three real words, holes over real speech; ADR 0033's measurements),
/// while the `suppress_nst` decode places units exactly where the default is
/// blind. Its WORDS stay out of the vote (they re-garble, ADR 0033) — only
/// its time grid is used.
///
/// Matching is similarity-gated: a DP substitution only adopts an anchor's
/// span when the two tokens look like the same word (edit distance <= 2 or a
/// prefix), otherwise the word falls through to onset placement — a text-blind
/// substitution would drag a correct word onto a DIFFERENT word's (possibly
/// phantom) anchor, which is exactly the "caption runs ahead of the streamer"
/// bug this replaces. Unmatched anchors are skipped entirely (a phantom must
/// not consume timeline). Inserted runs land on RMS onsets inside their gap
/// (rising crossings of the ADR 0021 silence bar), falling back to
/// character-proportional spread when the gap has fewer onsets than words.
/// Zero-width results are fine downstream (DTW single-token units already are).
pub fn fuse_onto_timing(
    merged: &[String],
    whisper: &Transcript,
    timing_extra: Option<&Transcript>,
    samples: &[f32],
    sample_rate: u32,
    clip_dur_s: f64,
) -> Vec<CaptionUnit> {
    // --- anchor skeleton: union of both decodes' units, time-sorted ---------
    let mut anchors: Vec<(f64, f64, String)> = whisper
        .units
        .iter()
        .chain(timing_extra.map(|t| t.units.iter()).unwrap_or_default())
        .map(|u| {
            let tok = normalize(&u.text).into_iter().next().unwrap_or_default();
            (u.start_s, u.end_s, tok)
        })
        .filter(|(_, _, t)| !t.is_empty())
        .collect();
    anchors.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let anchor_tokens: Vec<String> = anchors.iter().map(|(_, _, t)| t.clone()).collect();

    let onsets = rms_onsets(samples, sample_rate);
    let ops = align_weighted(&anchor_tokens, merged);

    // First pass: which merged word adopts which anchor (similarity-gated).
    let mut adopted: Vec<Option<usize>> = vec![None; merged.len()]; // merged idx -> anchor idx
    for (op, ai, mi) in &ops {
        match op {
            Op::Ok => adopted[mi.expect("ok keeps merged index")] = Some(ai.expect("ok keeps anchor")),
            Op::Sub => {
                let (ai, mi) = (ai.expect("sub keeps anchor"), mi.expect("sub keeps merged"));
                if similar_word(&anchor_tokens[ai], &merged[mi]) {
                    adopted[mi] = Some(ai);
                }
            }
            _ => {}
        }
    }
    // Enforce monotonic anchor starts across adoptions (two skeletons can
    // interleave): a later word may not adopt an anchor that starts before a
    // previously adopted one.
    let mut last_start = f64::NEG_INFINITY;
    for a in adopted.iter_mut() {
        if let Some(ai) = *a {
            if anchors[ai].0 < last_start {
                *a = None;
            } else {
                last_start = anchors[ai].0;
            }
        }
    }

    // Second pass: place words — adopted ones on their anchor span, runs of
    // unadopted ones onto onsets/proportional spread inside their gap.
    let mut fused: Vec<CaptionUnit> = Vec::new();
    let mut i = 0;
    let mut prev_end = 0.0_f64;
    while i < merged.len() {
        if let Some(ai) = adopted[i] {
            let (s, e, _) = anchors[ai];
            let s = s.max(prev_end.min(clip_dur_s)).min(clip_dur_s);
            fused.push(CaptionUnit { text: merged[i].clone(), start_s: s, end_s: e.max(s) });
            prev_end = e.max(s);
            i += 1;
            continue;
        }
        // run of unadopted words [i, j)
        let mut j = i;
        while j < merged.len() && adopted[j].is_none() {
            j += 1;
        }
        let gap_start = prev_end;
        let gap_end = if j < merged.len() {
            anchors[adopted[j].expect("loop bound")].0.max(gap_start)
        } else {
            clip_dur_s.max(gap_start)
        };
        place_run(&merged[i..j], gap_start, gap_end, &onsets, &mut fused);
        prev_end = fused.last().map(|u| u.end_s).unwrap_or(gap_start).max(gap_start);
        i = j;
    }
    respread_flashes(&mut fused, clip_dur_s);
    fused
}

/// Minimum readable width a fused word should get when its neighborhood has
/// the room (the refine pass extends further, but only into room that exists —
/// this pass CREATES the room by reclaiming unclaimed timeline).
const MIN_WORD_S: f64 = 0.15;

/// Redistribute flash-runs into their enclosing slack: when consecutive words
/// got squeezed against an anchor (alignment ambiguity between two similar
/// phrases can consume a later anchor and box the words in between) while
/// unclaimed timeline sits right next to them, re-place the whole run evenly
/// across the window between its timed neighbors. A run that is genuinely
/// boxed (window no bigger than the run) is left alone.
fn respread_flashes(fused: &mut [CaptionUnit], clip_dur_s: f64) {
    let n = fused.len();
    let mut i = 0;
    while i < n {
        if fused[i].end_s - fused[i].start_s >= MIN_WORD_S {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && fused[j].end_s - fused[j].start_s < MIN_WORD_S {
            j += 1;
        }
        let win_start = if i == 0 { 0.0 } else { fused[i - 1].end_s };
        let win_end = if j == n { clip_dur_s } else { fused[j].start_s };
        let need = (j - i) as f64 * MIN_WORD_S;
        if win_end - win_start > need {
            // RIGHT-aligned in the window, capped per word: the squeeze always
            // happens against the run's FOLLOWING anchor (that is where the
            // words actually belong — DP boxed them there), so the run stays
            // adjacent to it instead of drifting to the window's far start
            // (measured: a lone squeezed word handed the whole 3.4 s window
            // landed ~3 s before its speech).
            let total = ((j - i) as f64 * 0.5).min(win_end - win_start);
            let start = win_end - total;
            let total_chars: usize =
                fused[i..j].iter().map(|u| u.text.chars().count().max(1)).sum();
            let mut t = start;
            for u in fused[i..j].iter_mut() {
                let frac = u.text.chars().count().max(1) as f64 / total_chars as f64;
                let w = total * frac;
                u.start_s = t;
                u.end_s = (t + w).min(win_end);
                t += w;
            }
        }
        i = j;
    }
}

/// Rising crossings of the caption silence bar over the clip's RMS envelope —
/// where speech (or any voiced burst) begins. Same envelope constants as the
/// caption timing pass (hop 20 ms, win 40 ms) and the ADR 0021 bar
/// (min(10% of the loud reference, absolute floor 0.006) — here max'd with the
/// absolute floor so pure-noise clips don't sprout onsets everywhere).
fn rms_onsets(samples: &[f32], sample_rate: u32) -> Vec<f64> {
    if samples.is_empty() {
        return Vec::new();
    }
    let hop = (sample_rate as f64 * 0.02) as usize;
    let win = (sample_rate as f64 * 0.04) as usize;
    let mut env: Vec<f32> = Vec::with_capacity(samples.len() / hop.max(1) + 1);
    let mut i = 0;
    while i < samples.len() {
        let e = (i + win).min(samples.len());
        let w = &samples[i..e];
        env.push((w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt());
        i += hop;
    }
    let mut sorted = env.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let p95 = sorted[((sorted.len() as f64 * 0.95) as usize).min(sorted.len() - 1)];
    let bar = (0.10 * p95).max(0.006);
    let mut onsets = Vec::new();
    let mut last = f64::NEG_INFINITY;
    for (k, w) in env.windows(2).enumerate() {
        if w[0] < bar && w[1] >= bar {
            let t = (k + 1) as f64 * 0.02;
            if t - last >= 0.15 {
                onsets.push(t);
                last = t;
            }
        }
    }
    onsets
}

/// Lay a run of words into [gap_start, gap_end]: consecutive words snap to the
/// gap's speech onsets in order; words beyond the available onsets spread
/// character-proportionally through what remains. Each word ends where the
/// next begins (the refine pass owns display durations).
fn place_run(
    words: &[String],
    gap_start: f64,
    gap_end: f64,
    onsets: &[f64],
    fused: &mut Vec<CaptionUnit>,
) {
    if words.is_empty() {
        return;
    }
    let usable: Vec<f64> = onsets
        .iter()
        .copied()
        .filter(|t| *t >= gap_start && *t < gap_end - 0.02)
        .collect();
    let mut starts: Vec<f64> = Vec::with_capacity(words.len());
    let n_on = usable.len().min(words.len());
    starts.extend_from_slice(&usable[..n_on]);
    if n_on < words.len() {
        // proportional tail from the last placed start (or the gap start)
        let rem = &words[n_on..];
        let from = starts.last().copied().unwrap_or(gap_start);
        let total_chars: usize = rem.iter().map(|w| w.chars().count().max(1)).sum();
        let mut t = if n_on == 0 { gap_start } else { from + 0.15 };
        let span = (gap_end - t).max(0.0);
        for w in rem {
            starts.push(t.min(gap_end));
            t += span * (w.chars().count().max(1) as f64 / total_chars as f64);
        }
    }
    // monotonic guard (onsets are sorted; the proportional tail could start
    // before an earlier onset only if the gap math degenerated)
    for k in 1..starts.len() {
        if starts[k] < starts[k - 1] {
            starts[k] = starts[k - 1];
        }
    }
    for (k, w) in words.iter().enumerate() {
        let s = starts[k];
        let e = starts.get(k + 1).copied().unwrap_or(gap_end).max(s);
        fused.push(CaptionUnit { text: w.clone(), start_s: s, end_s: e.min(gap_end).max(s) });
    }
}

/// Similarity-weighted alignment for TIMING fusion: equal 0.0 / similar-sub
/// 0.5 / ins-or-del 1.0 / dissimilar-sub 2.2. A dissimilar substitution costs
/// more than skip+insert, so the DP can never pair a word with a different
/// word's anchor; a similar garble pairing beats skipping (0.5 < 2.0). This is
/// what routes a REPEATED phrase to its own anchors — with uniform costs the
/// clip's two "pusing ..." phrases tied, the backtrace picked the wrong slots,
/// and the second phrase compressed against the clip edge (operator-heard).
fn align_weighted(a: &[String], b: &[String]) -> Vec<(Op, Option<usize>, Option<usize>)> {
    const SIM: f64 = 0.5;
    const GAP: f64 = 1.0;
    const DIS: f64 = 2.2;
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0.0_f64; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i as f64 * GAP;
    }
    for j in 0..=m {
        d[0][j] = j as f64 * GAP;
    }
    let sub_cost = |x: &str, y: &str| {
        if x == y {
            0.0
        } else if similar_word(x, y) {
            SIM
        } else {
            DIS
        }
    };
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[i - 1][j - 1] + sub_cost(&a[i - 1], &b[j - 1]);
            d[i][j] = sub.min(d[i - 1][j] + GAP).min(d[i][j - 1] + GAP);
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    let eq = |x: f64, y: f64| (x - y).abs() < 1e-9;
    // Tie-break order: diagonal, then INS, then DEL. Preferring Ins over Del
    // at equal cost biases words toward LATER anchors — measured on the
    // phantom-anchor case: [phantom-pusing, bangke] anchors vs
    // [bangke, pusing] words costs 2.0 both ways, and the Del-first path
    // adopts the phantom (the exact bug); the Ins-first path matches bangke
    // to its real anchor and lets pusing fall to its later onset.
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && eq(d[i][j], d[i - 1][j - 1] + sub_cost(&a[i - 1], &b[j - 1])) {
            let op = if a[i - 1] == b[j - 1] || similar_word(&a[i - 1], &b[j - 1]) {
                Op::Ok
            } else {
                Op::Sub
            };
            ops.push((op, Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if j > 0 && eq(d[i][j], d[i][j - 1] + GAP) {
            ops.push((Op::Ins, None, Some(j - 1)));
            j -= 1;
        } else {
            ops.push((Op::Del, Some(i - 1), None));
            i -= 1;
        }
    }
    ops.reverse();
    ops
}

/// "Same word, different garble": edit distance <= 2, or one is a prefix of
/// the other (whisper's multi-word store collapses keep only the first token).
fn similar_word(a: &str, b: &str) -> bool {
    if a == b || a.starts_with(b) || b.starts_with(a) {
        return true;
    }
    let (av, bv): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (n, m) = (av.len(), bv.len());
    if n.abs_diff(m) > 2 {
        return false;
    }
    // bounded Levenshtein (cap 2)
    let mut prev: Vec<u32> = (0..=m as u32).collect();
    for i in 1..=n {
        let mut row = vec![i as u32; m + 1];
        for j in 1..=m {
            let sub = prev[j - 1] + u32::from(av[i - 1] != bv[j - 1]);
            row[j] = sub.min(prev[j] + 1).min(row[j - 1] + 1);
        }
        if row.iter().min().copied().unwrap_or(u32::MAX) > 2 {
            return false;
        }
        prev = row;
    }
    prev[m] <= 2
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

    /// n seconds of silence at 16 kHz (no onsets -> proportional placement).
    fn silence(secs: f64) -> Vec<f32> {
        vec![0.0; (16000.0 * secs) as usize]
    }

    /// Silence with 0.1-amplitude bursts at the given [start, end) second spans.
    fn bursts(secs: f64, spans: &[(f64, f64)]) -> Vec<f32> {
        let mut s = silence(secs);
        for (a, b) in spans {
            let (a, b) = ((a * 16000.0) as usize, ((b * 16000.0) as usize).min(s.len()));
            for x in s[a..b].iter_mut() {
                *x = 0.1;
            }
        }
        s
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
        let fused = fuse_onto_timing(&merged, &whisper, None, &silence(5.0), 16000, 5.0);
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
        let fused = fuse_onto_timing(&merged, &whisper, None, &silence(8.0), 16000, 8.0);
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
        let fused = fuse_onto_timing(&merged, &whisper, None, &silence(4.0), 16000, 4.0);
        assert_eq!(fused.len(), 3);
        assert!(fused[2].end_s <= 4.0 + 1e-9);
        assert!(fused[1].start_s >= 1.0 - 1e-9);
    }

    #[test]
    fn fuse_never_drags_a_word_onto_a_different_words_phantom_anchor() {
        // The bug the operator heard: whisper's phantom 4 s "pusing" unit sits
        // where "bangke" is actually said; a text-blind fusion adopted it and
        // the caption ran ~5 s ahead of the streamer. With similarity gating
        // neither word adopts a dissimilar anchor — both fall through to the
        // speech onsets (bursts at 19.7 and 24.0).
        let whisper = Transcript {
            language: Language::Id,
            units: vec![CaptionUnit { text: "pusing".into(), start_s: 19.5, end_s: 23.5 }],
        };
        let extra = Transcript {
            language: Language::Id,
            units: vec![CaptionUnit { text: "bangke".into(), start_s: 19.7, end_s: 19.9 }],
        };
        let samples = bursts(30.0, &[(19.7, 20.0), (24.0, 24.4)]);
        let merged = words("bangke pusing");
        let fused = fuse_onto_timing(&merged, &whisper, Some(&extra), &samples, 16000, 30.0);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].text, "bangke");
        assert!(
            (fused[0].start_s - 19.7).abs() < 0.15,
            "bangke lands on its real onset, got {}",
            fused[0].start_s
        );
        assert_eq!(fused[1].text, "pusing");
        assert!(
            (fused[1].start_s - 24.0).abs() < 0.15,
            "pusing lands on the LATER onset, not the phantom anchor, got {}",
            fused[1].start_s
        );
    }

    #[test]
    fn fuse_sub_adopts_anchor_only_for_the_same_garbled_word() {
        // dijegat (voted) vs dijekat (whisper's spelling): same word, edit-1 -
        // adopts the anchor span. A dissimilar pair must not.
        assert!(similar_word("dijekat", "dijegat"));
        assert!(similar_word("apaan", "apa")); // prefix
        assert!(!similar_word("pusing", "bangke"));
        let whisper = Transcript {
            language: Language::Id,
            units: vec![CaptionUnit { text: "dijekat".into(), start_s: 2.1, end_s: 2.32 }],
        };
        let merged = words("dicegat");
        let fused = fuse_onto_timing(&merged, &whisper, None, &silence(4.0), 16000, 4.0);
        assert_eq!(fused.len(), 1);
        assert_eq!((fused[0].start_s, fused[0].end_s), (2.1, 2.32));
    }

    #[test]
    fn fuse_routes_a_repeated_phrase_to_its_own_anchors() {
        // The clip has "pusing cok main" THEN "pusing kan dibilang"; with
        // uniform alignment costs the two phrases tied and the second one
        // compressed against the clip edge (operator-heard). Weighted costs
        // route kan/dibilang onto their own anchors.
        let whisper = Transcript {
            language: Language::Id,
            units: vec![
                CaptionUnit { text: "pusing".into(), start_s: 24.5, end_s: 24.9 },
                CaptionUnit { text: "main".into(), start_s: 25.1, end_s: 25.3 },
                CaptionUnit { text: "kan".into(), start_s: 26.34, end_s: 26.5 },
                CaptionUnit { text: "bilang".into(), start_s: 26.78, end_s: 27.0 },
                CaptionUnit { text: "cimri".into(), start_s: 29.26, end_s: 29.46 },
            ],
        };
        let merged = words("pusing cok main pusing kan dibilang");
        let fused = fuse_onto_timing(&merged, &whisper, None, &silence(30.0), 16000, 30.0);
        assert_eq!(fused.len(), 6);
        assert_eq!((fused[0].text.as_str(), fused[0].start_s), ("pusing", 24.5));
        assert_eq!((fused[2].text.as_str(), fused[2].start_s), ("main", 25.1));
        assert_eq!((fused[4].text.as_str(), fused[4].start_s), ("kan", 26.34));
        assert_eq!((fused[5].text.as_str(), fused[5].start_s), ("dibilang", 26.78));
        // the second pusing lands between its neighbors, not at the clip edge
        assert!(
            fused[3].start_s >= 25.3 && fused[3].start_s < 26.34,
            "second pusing placed in its gap, got {}",
            fused[3].start_s
        );
    }

    #[test]
    fn respread_reclaims_unclaimed_timeline_for_flash_runs() {
        // b and c got squeezed to (near-)zero width against d's anchor while
        // 0.9 s of unclaimed timeline sits between a and d — the run respreads.
        let mut fused = vec![
            CaptionUnit { text: "a".into(), start_s: 1.0, end_s: 2.0 },
            CaptionUnit { text: "bb".into(), start_s: 2.0, end_s: 2.0 },
            CaptionUnit { text: "cc".into(), start_s: 2.0, end_s: 2.05 },
            CaptionUnit { text: "d".into(), start_s: 2.9, end_s: 3.5 },
        ];
        respread_flashes(&mut fused, 4.0);
        assert_eq!(fused[1].start_s, 2.0);
        assert!((fused[1].end_s - 2.45).abs() < 1e-6);
        assert!((fused[2].start_s - 2.45).abs() < 1e-6);
        assert!((fused[2].end_s - 2.9).abs() < 1e-6);
        // neighbors untouched
        assert_eq!(fused[0].end_s, 2.0);
        assert_eq!(fused[3].start_s, 2.9);
    }

    #[test]
    fn respread_right_aligns_a_lone_squeezed_word_near_its_anchor() {
        // A single flash word before an anchor gets up to 0.5 s ending AT the
        // anchor — not the whole slack window (that dragged it seconds early).
        let mut fused = vec![
            CaptionUnit { text: "bangke".into(), start_s: 1.0, end_s: 2.0 },
            CaptionUnit { text: "mana".into(), start_s: 5.36, end_s: 5.40 },
            CaptionUnit { text: "tadi".into(), start_s: 5.40, end_s: 6.0 },
        ];
        respread_flashes(&mut fused, 8.0);
        assert!((fused[1].start_s - 4.9).abs() < 1e-6, "got {}", fused[1].start_s);
        assert!((fused[1].end_s - 5.4).abs() < 1e-6);
    }

    #[test]
    fn respread_leaves_genuinely_boxed_runs_alone() {
        let mut fused = vec![
            CaptionUnit { text: "a".into(), start_s: 1.0, end_s: 2.0 },
            CaptionUnit { text: "b".into(), start_s: 2.0, end_s: 2.05 },
            CaptionUnit { text: "c".into(), start_s: 2.1, end_s: 2.2 },
        ];
        let before: Vec<(f64, f64)> = fused.iter().map(|u| (u.start_s, u.end_s)).collect();
        // window after "a" to clip end is 2.0..2.2 via next... c is also <MIN so
        // the run is b,c with window 2.0..2.2 (clip end) = 0.2 < 2*0.15 -> no-op
        respread_flashes(&mut fused, 2.2);
        let after: Vec<(f64, f64)> = fused.iter().map(|u| (u.start_s, u.end_s)).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn rms_onsets_find_burst_starts() {
        let samples = bursts(4.0, &[(1.0, 1.2), (2.5, 2.7)]);
        let onsets = rms_onsets(&samples, 16000);
        assert_eq!(onsets.len(), 2, "got {onsets:?}");
        assert!((onsets[0] - 1.0).abs() < 0.1);
        assert!((onsets[1] - 2.5).abs() < 0.1);
    }

    #[test]
    fn store_fuzzy_transfers_correction_across_engine_spellings() {
        // whisper's garble was curated as dijekat->dicegat; the ensemble
        // spells the same mishear dijegat — edit-1 from the stored key.
        let mut lex = crate::DialectLexicon::default();
        lex.corrections.push(crate::Correction {
            wrong: "dijekat".into(),
            right: "dicegat".into(),
            ..Default::default()
        });
        lex.dictionary.insert("dicegat".into());
        let mut w = words("kau dijegat mana");
        apply_store_fuzzy(&mut w, &lex);
        assert_eq!(w, words("kau dicegat mana"));
    }

    #[test]
    fn store_fuzzy_never_touches_real_dictionary_words_or_context_pairs() {
        let mut lex = crate::DialectLexicon::default();
        // context pair (LLM-only, ADR 0030) must not fire even on exact match
        lex.corrections.push(crate::Correction {
            wrong: "cowok".into(),
            right: "cok".into(),
            context: true,
            ..Default::default()
        });
        // fuzzy would match maen~main, but maen is a real (dictionary) word
        lex.corrections.push(crate::Correction {
            wrong: "main".into(),
            right: "kelamin".into(),
            ..Default::default()
        });
        lex.dictionary.insert("maen".into());
        let mut w = words("cowok maen");
        apply_store_fuzzy(&mut w, &lex);
        assert_eq!(w, words("cowok maen"));
    }

    #[test]
    fn normalize_strips_asr_header_and_punctuation() {
        assert_eq!(
            normalize("language Indonesian<asr_text>Mana? Ini satu."),
            words("mana ini satu")
        );
    }
}
