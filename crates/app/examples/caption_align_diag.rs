//! Forced-alignment timing GATE instrument (ADR 0054, the measure loop of ADR
//! 0049): decode one clip on the PRODUCTION path once, then fuse the SAME
//! voted words two ways — the shipped whisper-DTW skeleton vs the wav2vec2-CTC
//! forced alignment — and print the timing comparison the gate is judged on:
//!
//!   - per ground-truth word (operator's ear, clip-relative): DTW onset vs
//!     ALIGN onset vs heard — the ADR 0049 mis-onset bar (|delta| > 0.5 s);
//!   - the cross-method onset-shift distribution (median/p90/max) — on a
//!     turn-taking control clip this MUST stay small (no-regression bar);
//!   - post-refine dwell stats (% cues < 0.40 s) — the ADR 0049 too-fast bar
//!     must not worsen.
//!
//! Both fused variants are refined and written beside the wav as
//! `clip_dtw.ass` / `clip_align.ass` for a re-burn on the operator's eye.
//! With `YC_ALIGN_EMIT=1` it ALSO runs the real `ensemble::apply` with
//! `YC_FORCED_ALIGN=1` (5 fresh sidecar decodes) and writes
//! `clip_alignburn.ass` — the production entry point end-to-end, the artifact
//! the burn gate uses (never trust the instrument alone; the enh overclaim).
//!
//!   cargo run -p yt-clipper --features align --example caption_align_diag -- \
//!     <analysis.wav> <start_s> <end_s> [lang] [gtwords.txt] [store.json ...]
//!
//! `gtwords.txt`: optional `word<space>heard_s` lines (# comments), the
//! operator's by-ear onsets for the clip. Pass `-` to skip it when store
//! layers follow. `store.json ...`: optional extra dialect store layers in
//! production order (creator store, then per-clip store — pipeline.rs loads
//! the same two on top of the bundled base), so `at_s` pins apply here
//! exactly as on a render (the ADR 0055 pin-over-aligner check).

use std::path::{Path, PathBuf};

use yc_core::{CaptionGenre, CaptionStyle, Language, TimeRange, Transcript};
use yc_ingest::{read_range_samples, WHISPER_SR};

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
fn nearest_onset(units: &[yc_core::CaptionUnit], word: &str, near: f64) -> Option<f64> {
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

fn dwell_stats(units: &[yc_core::CaptionUnit]) -> (usize, f64, f64) {
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

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut a = std::env::args().skip(1);
    let wav = PathBuf::from(a.next().expect(
        "usage: caption_align_diag <analysis.wav> <start_s> <end_s> [lang] [gtwords.txt]",
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
    // Remaining args: extra dialect store layers, production order (creator
    // store, then per-clip store), most-specific last = winning (ADR 0031).
    let extra_stores: Vec<PathBuf> = a.map(PathBuf::from).collect();
    let range = TimeRange { start_s, end_s };

    let model = PathBuf::from("models/ggml-large-v3.bin");
    let models = PathBuf::from("models");
    let sidecars = PathBuf::from("sidecars");
    anyhow::ensure!(model.is_file(), "whisper model missing: {}", model.display());
    let cfg = yc_transcribe::ensemble::EnsembleConfig {
        mtmd_cli: sidecars.join("llama").join("llama-mtmd-cli.exe"),
        qwen_model: models.join("Qwen3-ASR-1.7B-Q8_0.gguf"),
        qwen_mmproj: models.join("mmproj-Qwen3-ASR-1.7B-Q8_0.gguf"),
        ffmpeg: sidecars.join("ffmpeg.exe"),
        deep_filter: {
            let df = sidecars.join("deep-filter.exe");
            df.is_file().then_some(df)
        },
        work_dir: std::env::temp_dir().join("yc_align_diag"),
        should_cancel: Box::new(|| false),
        on_stage: Box::new(|i, n| eprintln!("[align_diag] qwen variant decode {i}/{n}...")),
        align_model: Some(models.join("w2v2-align-id")),
    };

    let samples = read_range_samples(&wav, range)?;
    println!(
        "=== caption_align_diag: {} [{start_s:.1}-{end_s:.1}s] {lang:?} ({:.1}s) ===",
        wav.display(),
        samples.len() as f64 / WHISPER_SR as f64
    );

    // --- the production decode set, once --------------------------------------
    let lexicon = yc_transcribe::DialectLexicon::load_layered(
        &PathBuf::from("assets/dialect"),
        &extra_stores,
        lang,
    );
    let pins = lexicon.corrections.iter().filter(|c| c.at_s.is_some()).count();
    if !extra_stores.is_empty() {
        println!("store layers: +{} (at_s pins in play: {pins})", extra_stores.len());
    }
    eprintln!("[align_diag] whisper + DTW (GPU)...");
    let (raw, _) =
        yc_transcribe::transcribe_range_harvesting(&model, &samples, lang, &lexicon, || false)?;
    eprintln!("[align_diag] whisper suppress_nst timing skeleton (GPU)...");
    std::env::set_var("YC_SUPPRESS_NST", "1");
    let timing_extra = yc_transcribe::transcribe_range(
        &model,
        &samples,
        lang,
        &yc_transcribe::DialectLexicon::default(),
        || false,
    );
    std::env::remove_var("YC_SUPPRESS_NST");
    let timing_extra = timing_extra.ok();
    eprintln!("[align_diag] qwen ensemble variants (GPU, one-shot sidecars)...");
    let variants = yc_transcribe::ensemble::decode_variants(&cfg, &wav, range, lang)?;
    anyhow::ensure!(variants.len() >= 2, "only {} variant(s) decoded", variants.len());

    // The vote, exactly as `apply` runs it (backbone V0, voters V1.. + whisper).
    let whisper_words: Vec<String> =
        raw.units.iter().flat_map(|u| yc_transcribe::ensemble::normalize(&u.text)).collect();
    let decodes: Vec<Vec<String>> = variants.iter().map(|v| v.words.clone()).collect();
    let backbone = decodes[0].clone();
    let mut voters: Vec<Vec<String>> = decodes[1..].to_vec();
    if !whisper_words.is_empty() {
        voters.push(whisper_words);
    }
    let mut merged = yc_transcribe::ensemble::vote_merge(&backbone, &voters);
    yc_transcribe::ensemble::apply_store_fuzzy(&mut merged, &lexicon);
    println!("vote: {} merged words (whisper {} units)", merged.len(), raw.units.len());

    // --- fuse the SAME words both ways ----------------------------------------
    let onsets = yc_transcribe::ensemble::rms_onsets(&samples, WHISPER_SR);
    let mut dtw = yc_transcribe::ensemble::fuse_onto_timing(
        &merged,
        &raw,
        timing_extra.as_ref(),
        &samples,
        WHISPER_SR,
        range.duration_s(),
    );
    yc_transcribe::ensemble::apply_store_positional(
        &mut dtw,
        &lexicon,
        range.start_s,
        &onsets,
        range.duration_s(),
    );

    let mut aligner = yc_transcribe::align::Aligner::load(&models.join("w2v2-align-id"))?;
    let t0 = std::time::Instant::now();
    let spans = aligner.align_words(&merged, &samples, WHISPER_SR)?;
    let aligned_n = spans.iter().filter(|s| s.is_some()).count();
    println!(
        "forced-align: {aligned_n}/{} words aligned in {:.1}s",
        merged.len(),
        t0.elapsed().as_secs_f64()
    );
    let mut ali = yc_transcribe::ensemble::fuse_onto_alignment(
        &merged,
        &spans,
        &samples,
        WHISPER_SR,
        range.duration_s(),
    );
    yc_transcribe::ensemble::apply_store_positional(
        &mut ali,
        &lexicon,
        range.start_s,
        &onsets,
        range.duration_s(),
    );

    // --- cross-method shift distribution (pre-refine, 1:1 by word index) ------
    anyhow::ensure!(dtw.len() == ali.len(), "fusions place different unit counts");
    let mut shifts: Vec<f64> = dtw.iter().zip(&ali).map(|(d, a)| (a.start_s - d.start_s).abs()).collect();
    shifts.sort_by(|a, b| a.total_cmp(b));
    let n = shifts.len();
    println!(
        "\nonset shift DTW->ALIGN over {n} words: median {:.2}s  p90 {:.2}s  max {:.2}s  (>0.5s: {})",
        shifts[n / 2],
        shifts[(n * 9) / 10],
        shifts[n - 1],
        shifts.iter().filter(|s| **s > 0.5).count()
    );

    // --- ground-truth table (the operator's ear) -------------------------------
    if !gt.is_empty() {
        println!("\nword         heard    DTW-onset      ALIGN-onset");
        let (mut dtw_bad, mut ali_bad) = (0, 0);
        for (w, heard) in &gt {
            let d = nearest_onset(&dtw, w, *heard);
            let al = nearest_onset(&ali, w, *heard);
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
            println!("{w:<12} {heard:5.1}   {ds:<14} {als:<14}");
        }
        println!(
            "mis-onset (>0.5s off the ear): DTW {dtw_bad}/{}  ALIGN {ali_bad}/{}",
            gt.len(),
            gt.len()
        );
    }

    // --- refine + dwell stats + the burn artifacts -----------------------------
    let mk = |units: Vec<yc_core::CaptionUnit>| Transcript { language: lang, units };
    let dtw_t = yc_render::refine_caption_timing_keep_verified(mk(dtw), &samples, WHISPER_SR);
    let ali_t = yc_render::refine_caption_timing_keep_verified(mk(ali), &samples, WHISPER_SR);
    let (ds, dp, dm) = dwell_stats(&dtw_t.units);
    let (as_, ap, am) = dwell_stats(&ali_t.units);
    println!("\npost-refine dwell:  DTW {}/{} <0.40s ({dp:.0}%), median {dm:.2}s", ds, dtw_t.units.len());
    println!("                  ALIGN {}/{} <0.40s ({ap:.0}%), median {am:.2}s", as_, ali_t.units.len());

    let dir = wav.parent().unwrap_or_else(|| Path::new("."));
    write_ass(&dtw_t, &dir.join("clip_dtw.ass"))?;
    write_ass(&ali_t, &dir.join("clip_align.ass"))?;
    println!("wrote {}\\clip_dtw.ass + clip_align.ass", dir.display());

    // --- optional: the REAL production entry point, end-to-end -----------------
    if std::env::var("YC_ALIGN_EMIT").ok().as_deref() == Some("1") {
        eprintln!("[align_diag] emit: real ensemble::apply, aligned timing (5 decodes)...");
        // Pin the knob ON for the burn artifact even if the ambient env says
        // off — this arm EXISTS to burn the aligned fusion. (Default-on since
        // ADR 0055; the explicit set only guards an operator-set =0.)
        let prev = std::env::var("YC_FORCED_ALIGN").ok();
        std::env::set_var("YC_FORCED_ALIGN", "1");
        // Production (pipeline.rs) skips the suppress_nst skeleton decode when
        // alignment will run — hand apply the same None it would get.
        let emit_extra = if yc_transcribe::ensemble::forced_align_active(
            cfg.align_model.as_deref(),
        ) {
            None
        } else {
            timing_extra.as_ref()
        };
        let fused = yc_transcribe::ensemble::apply(
            &cfg,
            &wav,
            range,
            &raw,
            emit_extra,
            &samples,
            WHISPER_SR,
            &lexicon,
        );
        match prev {
            Some(v) => std::env::set_var("YC_FORCED_ALIGN", v),
            None => std::env::remove_var("YC_FORCED_ALIGN"),
        }
        let refined =
            yc_render::refine_caption_timing_keep_verified(fused?, &samples, WHISPER_SR);
        write_ass(&refined, &dir.join("clip_alignburn.ass"))?;
        println!("wrote {}\\clip_alignburn.ass ({} cues) — the burn artifact", dir.display(), refined.units.len());
    }
    Ok(())
}
