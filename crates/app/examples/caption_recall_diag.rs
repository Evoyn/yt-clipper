//! Caption recall instrument (ADR 0052, the measure-first twin for ADR 0049
//! fix #4 — the DROP / recall lane). Localizes WHERE each operator-dropped word
//! is lost, on the PRODUCTION decode path, so the fix lever is picked from
//! measured data (never a label):
//!
//!   - **DECODE-loss** — none of the six decoders heard the word (genuinely
//!     masked in the mix). Only the heavy lane (denoise / per-speaker / extra
//!     decode configs) can reach it.
//!   - **VOTE-loss** — a decoder heard it, but the strict-majority insert rule
//!     (`vote_merge`) dropped it. Reachable by a text-only vote-admission rule,
//!     no new audio processing.
//!
//! It decodes all six production views — whisper (`transcribe_range_harvesting`,
//! timed DTW units) + the five qwen `VARIANTS` (via `ensemble::decode_variants`,
//! untimed word lists) — then runs the REAL `vote_merge` + `apply_store_fuzzy`
//! and, for each dropped phrase's distinctive anchor, reports per-decoder
//! coverage (fuzzy `similar_word`, `penguin` ≈ `pinguin`) and whether the anchor
//! survived to `merged`. A shipped-`clip.ass` "carried" pass confirms the drop on
//! the final output (a cue fuzzy-matching the anchor within ±1.0 s = carried).
//!
//!   cargo run -p yt-clipper --example caption_recall_diag -- \
//!     <analysis.wav> <start_s> <end_s> [lang] [shipped_clip.ass]
//!
//! No `face` feature: the qwen variants are UNTIMED word lists, so "did a decoder
//! hear it" is a text-presence question — which is exactly the vote's own
//! semantics (timing enters only later, from whisper's skeleton in the fusion).

use std::path::{Path, PathBuf};
use yc_core::{Language, TimeRange};
use yc_ingest::{read_range_samples, WHISPER_SR};

/// A dropped phrase from the operator ground truth: a display label, the
/// distinctive anchor to search for (1 token, or 2 for an all-common-words
/// phrase → adjacent bigram), the clip-relative time the operator HEARD it, and
/// how many times the phrase is spoken in the clip (so a duplicated common word
/// is judged by count, not bare presence).
struct DroppedPhrase {
    label: &'static str,
    anchor: &'static [&'static str],
    heard_s: f64,
    spoken: usize,
    secondary: &'static [&'static str],
}

/// Clip 3 (VIOR fans Fadhil) operator drops — benchmarks/vior-fans-fadhil.captions.groundtruth.txt.
const DROPS: &[DroppedPhrase] = &[
    // Another speaker, ~20s, just before the real word "SDC"; whisper's
    // 20.78-23.28 SDC span swallowed it. Common-word phrase → bigram; spoken
    // twice in the clip (the mis-onset "siapa"@5 + this @20), so count decides.
    DroppedPhrase { label: "siapa tau @20 (before SDC)", anchor: &["siapa", "tau"], heard_s: 20.0, spoken: 2, secondary: &[] },
    // "yang itu isinya otot kayaknya" — only "yang" captioned.
    DroppedPhrase { label: "otot @6.5 (isinya otot kayaknya)", anchor: &["otot"], heard_s: 6.5, spoken: 1, secondary: &["isinya", "kayaknya"] },
    // "yang keluar kreatin-kreatin" — kreatin = creatine (gym supplement), distinctive.
    DroppedPhrase { label: "kreatin @8.5 (keluar kreatin)", anchor: &["kreatin"], heard_s: 8.5, spoken: 2, secondary: &["keluar"] },
    // "pinguin" spoken, never captioned.
    DroppedPhrase { label: "pinguin @52", anchor: &["pinguin"], heard_s: 52.0, spoken: 1, secondary: &[] },
];

/// Whisper time-gate: a timed unit this close to the heard moment, fuzzy-matching
/// the anchor, counts as whisper having heard it there. Generous — the point is
/// coverage, not onset accuracy (which ADR 0051 already handled).
const WHISPER_TOL_S: f64 = 1.5;

fn fuzzy_hits(hay: &[String], needle: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    if needle.is_empty() || hay.len() < needle.len() {
        return out;
    }
    for w in hay.windows(needle.len()) {
        if w.iter().zip(needle).all(|(h, n)| yc_transcribe::ensemble::similar_word(h, n)) {
            out.push(w.join(" "));
        }
    }
    out
}

fn parse_ass_time(s: &str) -> Option<f64> {
    let s = s.trim();
    let (h, rest) = s.split_once(':')?;
    let (m, sec) = rest.split_once(':')?;
    Some(h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?)
}

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

/// Parse a shipped clip.ass into (start_s, text) cues.
fn parse_ass(path: &Path) -> anyhow::Result<Vec<(f64, String)>> {
    let text = std::fs::read_to_string(path)?;
    let mut cues = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("Dialogue:") else { continue };
        let f: Vec<&str> = rest.splitn(10, ',').collect();
        if f.len() < 10 {
            continue;
        }
        let Some(start_s) = parse_ass_time(f[1]) else { continue };
        cues.push((start_s, strip_ass_tags(f[9])));
    }
    Ok(cues)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut a = std::env::args().skip(1);
    let wav = PathBuf::from(a.next().expect("usage: caption_recall_diag <wav> <start_s> <end_s> [lang] [clip.ass]"));
    let start_s: f64 = a.next().expect("start_s").parse()?;
    let end_s: f64 = a.next().expect("end_s").parse()?;
    let lang = match a.next().as_deref() {
        Some("en") => Language::En,
        Some("ja") => Language::Ja,
        Some("id") | None => Language::Id,
        Some(other) => anyhow::bail!("unknown lang {other:?}"),
    };
    let shipped_ass = a.next().map(PathBuf::from);
    let range = TimeRange { start_s, end_s };

    let model = PathBuf::from("models/ggml-large-v3.bin");
    anyhow::ensure!(model.is_file(), "whisper model missing: {}", model.display());
    let sidecars = PathBuf::from("sidecars");
    let models = PathBuf::from("models");
    let cfg = yc_transcribe::ensemble::EnsembleConfig {
        mtmd_cli: sidecars.join("llama").join("llama-mtmd-cli.exe"),
        qwen_model: models.join("Qwen3-ASR-1.7B-Q8_0.gguf"),
        qwen_mmproj: models.join("mmproj-Qwen3-ASR-1.7B-Q8_0.gguf"),
        ffmpeg: sidecars.join("ffmpeg.exe"),
        deep_filter: {
            let df = sidecars.join("deep-filter.exe");
            df.is_file().then_some(df)
        },
        work_dir: std::env::temp_dir().join("yc_recall_diag"),
        should_cancel: Box::new(|| false),
        on_stage: Box::new(|i, n| eprintln!("[recall_diag] qwen variant decode {i}/{n}...")),
    };
    anyhow::ensure!(cfg.mtmd_cli.is_file(), "mtmd sidecar missing: {}", cfg.mtmd_cli.display());
    anyhow::ensure!(cfg.qwen_model.is_file(), "qwen model missing: {}", cfg.qwen_model.display());
    anyhow::ensure!(cfg.deep_filter.is_some(), "deep-filter missing — denoised variants would be skipped; recall measurement needs them");

    let samples = read_range_samples(&wav, range)?;
    println!(
        "=== caption_recall_diag: {} [{:.1}-{:.1}s] {:?} ({:.1}s) ===",
        wav.display(), start_s, end_s, lang, samples.len() as f64 / WHISPER_SR as f64
    );

    // --- whisper (timed), the render path (assets/dialect base; words == production) ---
    let lexicon = yc_transcribe::DialectLexicon::load_layered(&PathBuf::from("assets/dialect"), &[], lang);
    eprintln!("[recall_diag] whisper + DTW (GPU)...");
    let (raw, _harvest) =
        yc_transcribe::transcribe_range_harvesting(&model, &samples, lang, &lexicon, || false)?;
    let whisper_words: Vec<String> = raw.units.iter().flat_map(|u| yc_transcribe::ensemble::normalize(&u.text)).collect();
    println!("whisper: {} units, {} words", raw.units.len(), whisper_words.len());

    // The suppress_nst timing skeleton (ADR 0033): a 2nd whisper decode that
    // places units exactly where the default decode is blind. Production fuses on
    // the UNION (default ∪ this), so a word the default misses can still get an
    // anchor here. Measuring only the default would read "no anchor" falsely.
    eprintln!("[recall_diag] whisper suppress_nst timing skeleton (GPU)...");
    std::env::set_var("YC_SUPPRESS_NST", "1");
    let timing_extra = yc_transcribe::transcribe_range(
        &model, &samples, lang, &yc_transcribe::DialectLexicon::default(), || false,
    );
    std::env::remove_var("YC_SUPPRESS_NST");
    let timing_extra = timing_extra
        .map_err(|e| eprintln!("[recall_diag] suppress_nst decode failed (fusing on default only): {e:#}"))
        .ok();
    if let Some(te) = &timing_extra {
        println!("suppress_nst skeleton: {} units", te.units.len());
    }

    // --- the five qwen variants (untimed word lists), the production decode path ---
    eprintln!("[recall_diag] qwen ensemble variants (GPU, one-shot sidecars)...");
    let variants = yc_transcribe::ensemble::decode_variants(&cfg, &wav, range, lang)?;
    anyhow::ensure!(variants.len() >= 2, "only {} variant(s) decoded — vote impossible", variants.len());
    for (i, v) in variants.iter().enumerate() {
        println!(
            "  V{i} atten={:<5} pad={:<5} {}-> {} words",
            format!("{:?}", v.atten), v.head_pad, if v.onset_src { "[onset] " } else { "" }, v.words.len()
        );
    }

    // --- reproduce the vote EXACTLY (apply's steps 2-3): backbone = V0, voters =
    //     V1.. + whisper; then the fuzzy store tier. `merged` is the pre-fusion
    //     word stream — the ground truth of what the vote KEPT. ---
    let decodes: Vec<Vec<String>> = variants.iter().map(|v| v.words.clone()).collect();
    let backbone = decodes[0].clone();
    let mut voters: Vec<Vec<String>> = decodes[1..].to_vec();
    if !whisper_words.is_empty() {
        voters.push(whisper_words.clone());
    }
    let mut merged = yc_transcribe::ensemble::vote_merge(&backbone, &voters);
    yc_transcribe::ensemble::apply_store_fuzzy(&mut merged, &lexicon);
    println!("\nvote: backbone V0 + {} voters -> merged {} words", voters.len(), merged.len());

    // Reproduce production PLACEMENT locally (the steps apply runs after the vote):
    // fuse the merged words onto the whisper ∪ suppress_nst skeleton + speech
    // onsets, then the positional pass (base lexicon has no at_s pins -> no-op).
    // Onset source: apply prefers the cleaned no-pad variant's audio; the mix
    // samples are close enough to LOCATE where a word lands. This is the ground
    // truth of WHERE production puts each heard word — the layer the shipped
    // clip.ass reflects (modulo decode variance between renders).
    let onsets = yc_transcribe::ensemble::rms_onsets(&samples, WHISPER_SR);
    let mut fused = yc_transcribe::ensemble::fuse_onto_timing(
        &merged, &raw, timing_extra.as_ref(), &samples, WHISPER_SR, range.duration_s(),
    );
    yc_transcribe::ensemble::apply_store_positional(
        &mut fused, &lexicon, range.start_s, &onsets, range.duration_s(),
    );
    println!("fused (local repro of production placement): {} units", fused.len());

    let shipped = shipped_ass.as_deref().map(parse_ass).transpose()?;
    if let Some(cues) = &shipped {
        println!("shipped clip.ass: {} cues", cues.len());
    }

    // --- per-drop loss localization ------------------------------------------
    println!("\n================= LOSS LOCALIZATION (per operator drop) =================");
    for d in DROPS {
        println!("\n■ {}   anchor {:?} spoken≈{}", d.label, d.anchor, d.spoken);

        // whisper: timed hits near the heard moment + flat presence (the vote uses flat)
        let w_timed: Vec<String> = raw
            .units
            .iter()
            .filter(|u| {
                (u.start_s - d.heard_s).abs() <= WHISPER_TOL_S
                    && yc_transcribe::ensemble::normalize(&u.text)
                        .first()
                        .map(|t| yc_transcribe::ensemble::similar_word(t, d.anchor[0]))
                        .unwrap_or(false)
            })
            .map(|u| format!("{}@{:.1}", u.text, u.start_s))
            .collect();
        let w_flat = fuzzy_hits(&whisper_words, d.anchor);
        let w_heard = !w_flat.is_empty();
        println!(
            "  whisper : {} (flat {}x{:?}; timed@±{}s near {:.1}: {:?})",
            if w_heard { "HEARD" } else { "-" }, w_flat.len(), w_flat, WHISPER_TOL_S, d.heard_s, w_timed
        );

        // each qwen variant
        let mut heard_by = usize::from(w_heard);
        for (i, v) in variants.iter().enumerate() {
            let hits = fuzzy_hits(&v.words, d.anchor);
            if !hits.is_empty() {
                heard_by += 1;
            }
            println!(
                "  V{i} a{:<4}: {} {}x {:?}",
                v.atten.map(|x| x.to_string()).unwrap_or_else(|| "raw".into()),
                if hits.is_empty() { "-    " } else { "HEARD" }, hits.len(), hits
            );
        }

        // secondary anchors (context; not counted in heard_by)
        for s in d.secondary {
            let n: usize = variants.iter().map(|v| fuzzy_hits(&v.words, &[*s]).len()).sum::<usize>()
                + fuzzy_hits(&whisper_words, &[*s]).len();
            println!("     · secondary {:?}: {} total fuzzy hits across all 6 decoders", s, n);
        }

        // survive the vote?
        let merged_ct = fuzzy_hits(&merged, d.anchor).len();

        // WHERE does production PLACEMENT put it (the local fuse repro — the real
        // production layer)? Cues are per-word, so match the FIRST anchor token
        // (the distinctive lead); report the fused unit nearest the heard moment.
        let lead = d.anchor[0];
        let lead_match = |text: &str| {
            yc_transcribe::ensemble::normalize(text)
                .iter()
                .any(|t| yc_transcribe::ensemble::similar_word(t, lead))
        };
        let placed_near = fused
            .iter()
            .filter(|u| yc_transcribe::ensemble::normalize(&u.text).first().map(|t| yc_transcribe::ensemble::similar_word(t, lead)).unwrap_or(false))
            .min_by(|a, b| (a.start_s - d.heard_s).abs().total_cmp(&(b.start_s - d.heard_s).abs()))
            .map(|u| (u.start_s, u.text.clone()));

        // shipped cross-check, windowed to ±3 s (far matches are edit-2 false
        // positives on short tokens — ya/mau/aku ≈ tau).
        let shipped_near: Vec<String> = shipped
            .as_ref()
            .map(|cues| {
                cues.iter()
                    .filter(|(t, text)| (t - d.heard_s).abs() <= 3.0 && lead_match(text))
                    .map(|(t, text)| format!("{text}@{t:.2}"))
                    .collect()
            })
            .unwrap_or_default();

        // classify — primary output is the loss STAGE.
        let near = placed_near.as_ref().map(|(t, _)| (t - d.heard_s).abs() <= 2.0).unwrap_or(false);
        let class = if heard_by == 0 {
            "DECODE-loss — 0/6 decoders heard it; only the heavy lane reaches it"
        } else if merged_ct == 0 {
            "VOTE-loss — heard but the vote kept none; a text-only admission rule reaches it"
        } else if merged_ct < d.spoken && placed_near.is_none() {
            "PARTIAL VOTE-loss — vote kept fewer than spoken and none placed"
        } else if near {
            "PLACED near the heard moment — production recovers it (shipped absence ⇒ decode variance / text-curation)"
        } else if placed_near.is_some() {
            "MIS-PLACED — heard + voted + fused, but far from the heard moment (timing-skeleton gap → placement lane)"
        } else {
            "IN MERGED but not placed near — investigate fusion"
        };
        println!(
            "  => heard_by {}/6 | in merged {}x (spoken≈{}) | placed(local): {} | shipped±3s: {}",
            heard_by, merged_ct, d.spoken,
            placed_near.map(|(t, tx)| format!("{tx}@{t:.2}({:+.2})", t - d.heard_s)).unwrap_or_else(|| "NONE".into()),
            if shipped_near.is_empty() { "NONE".to_string() } else { shipped_near.join(" ") },
        );
        println!("     CLASS: {class}");
    }

    // Emit a FRESH production ensemble clip.ass via the REAL `apply` (re-decodes
    // the 5 variants under the current code) for the re-burn gate — the
    // production-path validation of the recovery the table predicts. Never trust
    // the instrument alone (the enh overclaim was caught twice by NOT rendering).
    // Gated on YC_RECALL_EMIT=1 so the measure-only run stays cheap.
    if std::env::var("YC_RECALL_EMIT").ok().as_deref() == Some("1") {
        use yc_core::{CaptionGenre, CaptionStyle};
        eprintln!("[recall_diag] emit: full ensemble::apply (re-decodes 5 variants) -> clip_recall.ass ...");
        let fused_t = yc_transcribe::ensemble::apply(
            &cfg, &wav, range, &raw, timing_extra.as_ref(), &samples, WHISPER_SR, &lexicon,
        )?;
        let refined = yc_render::refine_caption_timing_keep_verified(fused_t, &samples, WHISPER_SR);
        let style = CaptionStyle::for_genre(CaptionGenre::HugeWord);
        let dir = wav.parent().unwrap_or_else(|| Path::new("."));
        let out = dir.join("clip_recall.ass");
        std::fs::write(&out, yc_render::generate_ass(&refined, &style, None))?;
        println!("\n--- emitted {} ({} cues); the four drops in the REAL apply output ---", out.display(), refined.units.len());
        for d in DROPS {
            let lead = d.anchor[0];
            let hits: Vec<String> = refined
                .units
                .iter()
                .filter(|u| yc_transcribe::ensemble::normalize(&u.text).first().map(|t| yc_transcribe::ensemble::similar_word(t, lead)).unwrap_or(false))
                .map(|u| format!("{}@{:.2}", u.text, u.start_s))
                .collect();
            println!("  {:<30} {}", d.label, if hits.is_empty() { "ABSENT".into() } else { hits.join(" ") });
        }
    }

    println!("\n(Anchors are fuzzy `similar_word` matches — matched surface tokens are printed above so any loose hit on a short anchor is visible, not silent.)");
    Ok(())
}
