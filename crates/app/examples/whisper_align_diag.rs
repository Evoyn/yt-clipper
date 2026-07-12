//! WHISPER-engine forced-alignment GATE instrument (ADR 0058, lane 1 of the
//! general caption-accuracy arc; the `caption_align_diag` counterpart for the
//! default engine): decode one clip ONCE on the production whisper path — no
//! ensemble, no vote, exactly what a whisper Creator's render transcribes —
//! then time the SAME units two ways and print the comparison the
//! pre-registered bars (ADR 0058) are judged on:
//!
//!   - DTW arm  = whisper's own spans, verbatim (today's whisper render);
//!   - ALIGN arm = `ensemble::forced_align_retime` (the EXACT production
//!     function the pipeline wires), texts asserted byte-identical (bar W3);
//!   - per ground-truth word (operator's ear): DTW vs ALIGN onset — the
//!     mis-onset bar (W1);
//!   - cross-arm onset-shift distribution — the no-regression bar on
//!     turn-taking controls (W2);
//!   - the silence-drop DELTA, word by word: a unit dropped in one arm and
//!     kept in the other is listed, never silent — the hallucination guard
//!     must not be quietly defeated (bar W4);
//!   - post-refine dwell stats (the ADR 0049 too-fast lens, word-unit basis).
//!
//! Both arms then run the production downstream: the `at_s` pin pass when the
//! store carries pins (production parity, ADR 0051) and
//! `refine_caption_timing` WITH the silence-drop armed (whisper words are one
//! decoder's unverified guess — `keep_verified` is the ensemble's rationale,
//! not this path's). Refined arms are written beside the wav as
//! `clip_wdtw.ass` / `clip_walign.ass` for the operator's eye.
//!
//!   cargo run -p yt-clipper --features align --example whisper_align_diag -- \
//!     <analysis.wav> <start_s> <end_s> [lang] [gtwords.txt] [store.json ...]
//!
//! `gtwords.txt`: optional `word<space>heard_s` lines (# comments), the
//! operator's by-ear onsets, clip-relative. Pass `-` to skip when store
//! layers follow. `store.json ...`: extra dialect store layers in production
//! order (creator store, then per-clip store).

use std::path::{Path, PathBuf};

use yc_core::{CaptionGenre, CaptionStyle, CaptionUnit, Language, TimeRange, Transcript};
use yc_ingest::{read_range_samples, WHISPER_SR};
use yc_render::UnitOutcome;

fn parse_gt(path: &Path) -> anyhow::Result<Vec<(String, f64)>> {
    let mut out = Vec::new();
    for line in std::fs::read_to_string(path)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(w), Some(t)) = (it.next(), it.next()) else { continue };
        out.push((w.to_lowercase(), t.parse::<f64>()?));
    }
    Ok(out)
}

/// Onset of the unit whose (normalized) text fuzzy-matches `word`, nearest to
/// `near` — the same nearest-occurrence rule every caption instrument uses.
fn nearest_onset(units: &[CaptionUnit], word: &str, near: f64) -> Option<f64> {
    units
        .iter()
        .filter(|u| {
            yc_transcribe::ensemble::normalize(&u.text)
                .first()
                .map(|t| yc_transcribe::ensemble::similar_word(t, word))
                .unwrap_or(false)
        })
        .map(|u| u.start_s)
        .min_by(|a, b| (a - near).abs().total_cmp(&(b - near).abs()))
}

fn dwell_stats(units: &[CaptionUnit]) -> (usize, f64, f64) {
    let mut dwells: Vec<f64> = units.iter().map(|u| u.end_s - u.start_s).collect();
    dwells.sort_by(|a, b| a.total_cmp(b));
    let n = dwells.len();
    let sub = dwells.iter().filter(|d| **d < 0.40).count();
    let median = if n == 0 { 0.0 } else { dwells[n / 2] };
    (sub, 100.0 * sub as f64 / n.max(1) as f64, median)
}

fn write_ass(t: &Transcript, out: &Path) -> anyhow::Result<()> {
    let style = CaptionStyle::for_genre(CaptionGenre::HugeWord);
    std::fs::write(out, yc_render::generate_ass(t, &style, None))?;
    Ok(())
}

/// The production whisper-branch downstream for one arm: pins (only when the
/// store carries `at_s` entries — the pipeline's `any(at_s)` guard), then the
/// refine WITH the silence-drop. Returns the refined transcript plus the
/// pre-refine drop list `(index, text, onset, peak)` for the W4 delta table.
#[allow(clippy::type_complexity)]
fn arm_downstream(
    mut units: Vec<CaptionUnit>,
    lexicon: &yc_transcribe::DialectLexicon,
    range: TimeRange,
    onsets: &[f64],
    samples: &[f32],
    lang: Language,
) -> (Transcript, Vec<(usize, String, f64, f32)>) {
    if lexicon.corrections.iter().any(|c| c.at_s.is_some()) {
        yc_transcribe::ensemble::apply_store_positional(
            &mut units,
            lexicon,
            range.start_s,
            onsets,
            range.duration_s(),
        );
    }
    let pre = Transcript { language: lang, units };
    let trace = yc_render::refine_caption_timing_traced(&pre, samples, WHISPER_SR);
    let mut dropped = Vec::new();
    let mut kept = Vec::new();
    for (i, (u, o)) in pre.units.iter().zip(&trace.outcomes).enumerate() {
        match o {
            UnitOutcome::Dropped { peak } => dropped.push((i, u.text.clone(), u.start_s, *peak)),
            UnitOutcome::Kept { start_s, end_s } => kept.push(CaptionUnit {
                text: u.text.clone(),
                start_s: *start_s,
                end_s: *end_s,
            }),
        }
    }
    (Transcript { language: lang, units: kept }, dropped)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(
            // the fusion's coverage/timing lines ride tracing::info
            |_| "warn,yc_transcribe=info".into(),
        ))
        .with_writer(std::io::stderr)
        .init();

    let mut a = std::env::args().skip(1);
    let wav = PathBuf::from(a.next().expect(
        "usage: whisper_align_diag <analysis.wav> <start_s> <end_s> [lang] [gtwords.txt] [store.json ...]",
    ));
    let start_s: f64 = a.next().expect("start_s").parse()?;
    let end_s: f64 = a.next().expect("end_s").parse()?;
    let lang = match a.next().as_deref() {
        Some("en") => Language::En,
        Some("ja") => Language::Ja,
        Some("id") | None => Language::Id,
        Some(other) => anyhow::bail!("unknown lang {other:?}"),
    };
    let gt = match a.next().as_deref() {
        None | Some("-") => Vec::new(),
        Some(p) => parse_gt(Path::new(p))?,
    };
    let extra_stores: Vec<PathBuf> = a.map(PathBuf::from).collect();
    let range = TimeRange { start_s, end_s };

    let model = PathBuf::from("models/ggml-large-v3.bin");
    let align_model = PathBuf::from("models/w2v2-align-id");
    anyhow::ensure!(model.is_file(), "whisper model missing: {}", model.display());
    anyhow::ensure!(
        align_model.join("model.onnx").is_file(),
        "align model missing: {} (fetch-models.ps1)",
        align_model.display()
    );

    let samples = read_range_samples(&wav, range)?;
    println!(
        "=== whisper_align_diag: {} [{start_s:.1}-{end_s:.1}s] {lang:?} ({:.1}s) ===",
        wav.display(),
        samples.len() as f64 / WHISPER_SR as f64
    );

    let lexicon = yc_transcribe::DialectLexicon::load_layered(
        &PathBuf::from("assets/dialect"),
        &extra_stores,
        lang,
    );
    let pins = lexicon.corrections.iter().filter(|c| c.at_s.is_some()).count();
    if !extra_stores.is_empty() {
        println!("store layers: +{} (at_s pins in play: {pins})", extra_stores.len());
    }

    // --- ONE production whisper decode (the default engine's transcribe) ------
    eprintln!("[whisper_align] whisper + DTW decode (GPU)...");
    let (raw, _harvest) =
        yc_transcribe::transcribe_range_harvesting(&model, &samples, lang, &lexicon, || false)?;
    println!("whisper: {} units", raw.units.len());
    anyhow::ensure!(!raw.units.is_empty(), "whisper produced no units — nothing to compare");

    // --- the candidate arm: the EXACT production re-time -----------------------
    let t0 = std::time::Instant::now();
    let Some(ali) = yc_transcribe::ensemble::forced_align_retime(
        Some(&align_model),
        lang,
        &raw.units,
        &samples,
        WHISPER_SR,
        range.duration_s(),
    ) else {
        anyhow::bail!(
            "forced_align_retime returned None (knob off / gate / aligner failure) — \
             production would keep DTW; nothing to measure"
        );
    };
    let align_secs = t0.elapsed().as_secs_f64();
    println!("forced_align_retime: {} units in {align_secs:.1}s", ali.len());

    // --- bar W3: texts byte-identical, 1:1, pre-refine --------------------------
    anyhow::ensure!(ali.len() == raw.units.len(), "unit count changed in re-time");
    for (a_u, r_u) in ali.iter().zip(&raw.units) {
        anyhow::ensure!(
            a_u.text == r_u.text,
            "text changed in re-time: {:?} -> {:?}",
            r_u.text,
            a_u.text
        );
    }
    println!("W3 texts: byte-identical 1:1 across the re-time ✓");

    // --- cross-arm onset shift (pre-refine, 1:1 by unit index) -----------------
    let mut shifts: Vec<f64> =
        raw.units.iter().zip(&ali).map(|(d, a)| (a.start_s - d.start_s).abs()).collect();
    shifts.sort_by(|a, b| a.total_cmp(b));
    let n = shifts.len();
    println!(
        "\nonset shift DTW->ALIGN over {n} units: median {:.2}s  p90 {:.2}s  max {:.2}s  (>0.5s: {})",
        shifts[n / 2],
        shifts[(n * 9) / 10],
        shifts[n - 1],
        shifts.iter().filter(|s| **s > 0.5).count()
    );
    // The movers, named — a turn-taking control's list should be short and
    // each entry explainable (bar W2 is judged on this, not just the median).
    let mut movers: Vec<(f64, &CaptionUnit, &CaptionUnit)> = raw
        .units
        .iter()
        .zip(&ali)
        .map(|(d, a)| ((a.start_s - d.start_s).abs(), d, a))
        .filter(|(s, _, _)| *s > 0.5)
        .collect();
    movers.sort_by(|x, y| y.0.total_cmp(&x.0));
    for (s, d, a) in movers.iter().take(12) {
        println!("  mover {:>5.2}s: {:<16} DTW {:6.2} -> ALIGN {:6.2}", s, d.text, d.start_s, a.start_s);
    }
    if movers.len() > 12 {
        println!("  ... and {} more >0.5s movers", movers.len() - 12);
    }

    // --- production downstream per arm (pins + refine w/ drop) -----------------
    let onsets = yc_transcribe::ensemble::rms_onsets(&samples, WHISPER_SR);
    let (dtw_t, dtw_drops) =
        arm_downstream(raw.units.clone(), &lexicon, range, &onsets, &samples, lang);
    let (ali_t, ali_drops) = arm_downstream(ali, &lexicon, range, &onsets, &samples, lang);

    // --- bar W4: the silence-drop delta, word by word ---------------------------
    println!(
        "\nsilence-drop: DTW arm {} dropped, ALIGN arm {} dropped",
        dtw_drops.len(),
        ali_drops.len()
    );
    let dropped_in = |list: &[(usize, String, f64, f32)], idx: usize| {
        list.iter().any(|(i, ..)| *i == idx)
    };
    for (i, text, at, peak) in &dtw_drops {
        if !dropped_in(&ali_drops, *i) {
            let moved_to = ali_t
                .units
                .iter()
                .find(|u| &u.text == text && (u.start_s - at).abs() < 5.0)
                .map(|u| u.start_s);
            println!(
                "  RESCUED by align: #{i} {text:?} (DTW onset {at:.2}, peak {peak:.4}) -> kept at {:?}",
                moved_to
            );
        }
    }
    for (i, text, at, peak) in &ali_drops {
        if !dropped_in(&dtw_drops, *i) {
            println!(
                "  NEWLY DROPPED under align: #{i} {text:?} (ALIGN onset {at:.2}, peak {peak:.4})"
            );
        }
    }

    // --- bar W1: the ground-truth table (the operator's ear) --------------------
    if !gt.is_empty() {
        println!("\nword         heard    DTW-onset       ALIGN-onset     (post-refine cues)");
        let (mut dtw_bad, mut ali_bad) = (0, 0);
        for (w, heard) in &gt {
            let d = nearest_onset(&dtw_t.units, w, *heard);
            let al = nearest_onset(&ali_t.units, w, *heard);
            let fmt = |o: Option<f64>, bad: &mut i32| match o {
                Some(t) => {
                    if (t - heard).abs() > 0.5 {
                        *bad += 1;
                    }
                    format!("{t:6.2} ({:+.2})", t - heard)
                }
                None => "   MISSING".into(),
            };
            let (mut db, mut ab) = (0, 0);
            let ds = fmt(d, &mut db);
            let als = fmt(al, &mut ab);
            dtw_bad += db;
            ali_bad += ab;
            println!("{w:<12} {heard:5.1}   {ds:<15} {als:<15}");
        }
        println!(
            "mis-onset (>0.5s off the ear): DTW {dtw_bad}/{}  ALIGN {ali_bad}/{}",
            gt.len(),
            gt.len()
        );
    }

    // --- dwell + artifacts ------------------------------------------------------
    let (ds, dp, dm) = dwell_stats(&dtw_t.units);
    let (as_, ap, am) = dwell_stats(&ali_t.units);
    println!("\npost-refine dwell:  DTW {}/{} <0.40s ({dp:.0}%), median {dm:.2}s", ds, dtw_t.units.len());
    println!("                  ALIGN {}/{} <0.40s ({ap:.0}%), median {am:.2}s", as_, ali_t.units.len());

    let dir = wav.parent().unwrap_or_else(|| Path::new("."));
    write_ass(&dtw_t, &dir.join("clip_wdtw.ass"))?;
    write_ass(&ali_t, &dir.join("clip_walign.ass"))?;
    println!("wrote {}\\clip_wdtw.ass + clip_walign.ass", dir.display());
    Ok(())
}
