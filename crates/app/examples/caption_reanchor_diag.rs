//! Mis-onset re-anchor instrument (ADR 0049 fix #3 measure-first twin). Joins
//! the three signals a laughter-mask-driven re-anchor needs, on the PRODUCTION
//! path, so the qualify rule + its false-positive bar are set from measured data
//! (never a label): whisper's raw DTW word onsets (`transcribe_range`, exactly as
//! `do_render` / `caption_diag`), the shared-reaction laughter mask over the SAME
//! analysis.wav (`yc_frame::reaction`, the ADR 0046 tagger the camera lanes use),
//! and the RMS speech onsets (`fuse_onto_timing`'s `rms_onsets`, reimplemented
//! here byte-for-byte).
//!
//!   cargo run -p yt-clipper --example caption_reanchor_diag --features face -- \
//!     <analysis.wav> <start_s> <end_s> [lang]
//!
//! Stage attribution (measured 2026-07-08): clip 3's shipped ensemble clip.ass is
//! byte-identical to the whisper-only path at every mis-onset cue, so the early
//! placement is whisper's DTW onset (NOT the ensemble fusion) — the re-anchor
//! lives in the shared refine stage and this whisper-only harness measures it.
//! Needs the `face` feature (pulls `yc-frame/voice` = the AudioSet tagger); the
//! default build prints a hint and exits.

#[cfg(not(feature = "face"))]
fn main() {
    eprintln!("caption_reanchor_diag needs the face feature (pulls the reaction tagger): cargo run -p yt-clipper --example caption_reanchor_diag --features face -- <analysis.wav> <start_s> <end_s> [lang]");
}

#[cfg(feature = "face")]
fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;
    use yc_core::Language;
    use yc_ingest::{read_range_samples, WHISPER_SR};

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut a = std::env::args().skip(1);
    let wav = PathBuf::from(a.next().expect("usage: caption_reanchor_diag <wav> <start_s> <end_s> [lang]"));
    let start_s: f64 = a.next().expect("start_s").parse()?;
    let end_s: f64 = a.next().expect("end_s").parse()?;
    let lang = match a.next().as_deref() {
        Some("en") => Language::En,
        Some("ja") => Language::Ja,
        _ => Language::Id,
    };
    let dur = end_s - start_s;

    let model = PathBuf::from("models/ggml-large-v3.bin");
    anyhow::ensure!(model.is_file(), "whisper model missing: {}", model.display());
    let tag_model = PathBuf::from("models/sherpa-onnx-zipformer-audio-tagging-2024-04-09.onnx");
    let tag_labels = PathBuf::from("models/audioset_class_labels_indices.csv");
    anyhow::ensure!(tag_model.is_file(), "tag model missing: {}", tag_model.display());
    anyhow::ensure!(tag_labels.is_file(), "tag labels missing: {}", tag_labels.display());

    let samples = read_range_samples(&wav, yc_core::TimeRange { start_s, end_s })?;
    let sr = WHISPER_SR;
    println!(
        "=== caption_reanchor_diag: {} [{:.1}-{:.1}s] {:?}  ({} samples, {:.1}s) ===",
        wav.display(), start_s, end_s, lang, samples.len(), samples.len() as f64 / sr as f64
    );

    // 1) whisper units (raw DTW), the render path (assets/dialect base; 0
    //    corrections on this clip so words == production, and timing is
    //    lexicon-independent regardless).
    let lexicon = yc_transcribe::DialectLexicon::load_layered(
        &PathBuf::from("assets/dialect"),
        &[],
        lang,
    );
    eprintln!("[reanchor_diag] whisper + DTW (GPU)...");
    let (raw, _harvest) =
        yc_transcribe::transcribe_range_harvesting(&model, &samples, lang, &lexicon, || false)?;
    println!("\nwhisper units: {}", raw.units.len());

    // 2) laughter mask over the SAME samples (ADR 0046 tagger, per 0.25 s step).
    eprintln!("[reanchor_diag] reaction tagger...");
    use yc_frame::reaction;
    let labels = reaction::parse_class_labels(&std::fs::read_to_string(&tag_labels)?);
    let family = reaction::laughter_family(&labels);
    let mut sess = reaction::TagSession::load(
        &tag_model,
        yc_frame::voice::SampleScale::Unit,
        reaction::TagOutput::Probs,
    )?;
    let steps = reaction::tag_steps(&mut sess, &samples, dur, &family)?;
    let laugh_at = |t: f64| -> f32 {
        let k = ((t / reaction::TAG_STEP_S) as usize).min(steps.len().saturating_sub(1));
        steps.get(k).copied().unwrap_or(0.0)
    };
    for &tau in &[0.10_f32, 0.20, 0.30] {
        let runs = reaction::mask_runs(&steps, tau);
        let spans: Vec<String> = runs.iter().map(|r| format!("{:.2}-{:.2}", r.0, r.1)).collect();
        println!("mask runs @tau {tau:.2} ({} runs): [{}]", runs.len(), spans.join(" "));
    }

    // 3) RMS speech onsets — the SAME grid apply_store_positional snaps to (now
    //    pub on both engines, ADR 0051), so the harness measures production exactly.
    let onsets = yc_transcribe::ensemble::rms_onsets(&samples, sr);
    println!("rms onsets: {} (first 10: {:?})", onsets.len(),
        onsets.iter().take(10).map(|t| (t * 100.0).round() / 100.0).collect::<Vec<_>>());

    // --- first-cut qualify rule (to be refined from this dump) ---------------
    // A unit's DTW onset sits ON the mask (laugh >= TAU) AND there is a CLEAN
    // (laugh < TAU) RMS speech onset later in its gap (before the next unit) →
    // that clean onset is the re-anchor target. Everything is printed so the
    // rule and its false-positive rate are read off the data, not asserted.
    const TAU: f32 = yc_frame::speaker::REACTION_TAU; // 0.1 — the production split tau (ADR 0046)
    // Full onset list with mask value — where any snap/re-anchor could land.
    println!("\n--- rms onsets ({}) as t(L@t) ---", onsets.len());
    let mut line = String::new();
    for (i, &t) in onsets.iter().enumerate() {
        line.push_str(&format!("{:>6.2}({:>4.2}) ", t, laugh_at(t)));
        if (i + 1) % 6 == 0 {
            println!("  {line}");
            line.clear();
        }
    }
    if !line.is_empty() {
        println!("  {line}");
    }

    let nearest_onset = |target: f64, clean_only: bool| -> Option<(f64, f64)> {
        onsets
            .iter()
            .copied()
            .filter(|&t| !clean_only || laugh_at(t) < TAU)
            .map(|t| (t, (t - target).abs()))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    };

    // The operator's 2026-07-08 mis-onset ground truth (unit idx, heard target_s).
    // Two feasibility questions per word: (Q-snap) is there a CLEAN speech onset
    // near the target a pin could snap to? (Q-smear) does the DTW span's late edge
    // point at the target (the long-span auto-signal)?
    let gt: &[(usize, f64)] = &[(7, 5.0), (36, 23.0), (40, 28.0), (47, 32.0), (86, 54.0)];
    println!("\n--- operator mis-onset targets: local feasibility ---");
    for &(idx, tgt) in gt {
        let u = &raw.units[idx];
        let (on, end) = (u.start_s, u.end_s);
        let span = end - on;
        // last rms onset in the DTW span's latter half (the smear-to-end target)
        let smear = onsets
            .iter()
            .copied()
            .filter(|&t| t >= on + span * 0.4 && t <= end + 0.05)
            .next_back();
        let near = nearest_onset(tgt, false);
        let near_clean = nearest_onset(tgt, true);
        println!(
            "  #{idx:<3} {:<11} dtw {:>5.2}-{:>5.2} span {:.2} L@on {:.2} | target {:.1} L@tgt {:.2}\n         nearest onset {} | nearest CLEAN onset {} | span-end onset {}",
            u.text, on, end, span, laugh_at(on), tgt, laugh_at(tgt),
            near.map(|(t, d)| format!("{t:.2} (d{d:.2}, L{:.2})", laugh_at(t))).unwrap_or("-".into()),
            near_clean.map(|(t, d)| format!("{t:.2} (d{d:.2})")).unwrap_or("-".into()),
            smear.map(|t| format!("{t:.2} (L{:.2})", laugh_at(t))).unwrap_or("-".into()),
        );
    }

    // Long-DTW-span scan: whisper couldn't localize the word (span >> a typed
    // word). The candidate auto-signal for 3 of the 5 — print every unit over the
    // threshold and whether it is one of the 5 (a catch) or not (a false positive).
    let gt_idx: std::collections::HashSet<usize> = gt.iter().map(|(i, _)| *i).collect();
    println!("\n--- long-DTW-span scan (span >= 1.0s): the smear auto-signal, catches vs false-positives ---");
    for (i, u) in raw.units.iter().enumerate() {
        let span = u.end_s - u.start_s;
        if span < 1.0 {
            continue;
        }
        let mark = if gt_idx.contains(&i) { "<< MIS (catch)" } else { "correct (FALSE POSITIVE)" };
        println!("  #{i:<3} span {:.2}  L@on {:.2}  {:<26} {}", span, laugh_at(u.start_s), mark, u.text);
    }

    // --- the gate (ADR 0051): emit BEFORE/AFTER whisper clip.ass via the real
    // production functions. The 5 operator time-pins re-anchor each mis-onset
    // word onto its speech onset. BEFORE = refine only (the shipped early times);
    // AFTER = apply_store_positional (the pass the whisper path now runs) + refine.
    // Same generate_ass + huge-word style the shipped clip used, so a re-burn
    // reproduces do_render — the operator's eye on the burn is the gate, not this
    // print (the ADR 0050 lesson).
    use yc_core::{CaptionGenre, CaptionStyle, Transcript};
    let style = CaptionStyle::for_genre(CaptionGenre::HugeWord);
    // (display text, pin at_s clip-rel, operator's heard target). A pin moves the
    // occurrence NEAREST at_s. gue's at_s is nudged below its heard 28 s because a
    // SECOND "gue" at 29.86 is nearer 28 — a duplicated common word must be pinned
    // toward its current (early) slot, else the pin grabs the wrong twin. The
    // distinctive words (name/acronym) have no twin and pin at their heard time —
    // they are the ones actually worth a durable pin (a common "gue" is a per-clip
    // Studio edit at best). Caveat documented in ADR 0051.
    let pins: &[(&str, f64, f64)] = &[
        ("Siapa", 5.0, 5.0),
        ("SDC", 23.0, 23.0),
        ("Gue", 27.6, 28.0),
        ("Fadil", 32.0, 32.0),
        ("Jalanannya", 54.0, 54.0),
    ];
    let mut pin_lex = yc_transcribe::DialectLexicon::default();
    for &(text, at, _tgt) in pins {
        pin_lex.corrections.push(yc_transcribe::Correction {
            wrong: text.into(),
            right: text.into(),
            at_s: Some(start_s + at),
            ..Default::default()
        });
    }
    let before = yc_render::refine_caption_timing(
        Transcript { language: lang, units: raw.units.clone() },
        &samples,
        sr,
    );
    let mut after_units = raw.units.clone();
    yc_transcribe::ensemble::apply_store_positional(&mut after_units, &pin_lex, start_s, &onsets, dur);
    let after =
        yc_render::refine_caption_timing(Transcript { language: lang, units: after_units }, &samples, sr);

    println!("\n--- pinned words: BEFORE -> AFTER show onset (whisper production path) ---");
    for &(text, _at, tgt) in pins {
        let find = |t: &Transcript| {
            t.units
                .iter()
                .filter(|u| u.text.eq_ignore_ascii_case(text))
                .min_by(|a, b| (a.start_s - tgt).abs().total_cmp(&(b.start_s - tgt).abs()))
                .map(|u| u.start_s)
        };
        println!(
            "  {:<11} target {:>5.1}  before {:>6}  after {:>6}",
            text,
            tgt,
            find(&before).map(|s| format!("{s:.2}")).unwrap_or_else(|| "-".into()),
            find(&after).map(|s| format!("{s:.2}")).unwrap_or_else(|| "-".into()),
        );
    }

    let dir = wav.parent().unwrap_or_else(|| std::path::Path::new("."));
    let before_p = dir.join("clip_before.ass");
    let after_p = dir.join("clip_after.ass");
    std::fs::write(&before_p, yc_render::generate_ass(&before, &style, None))?;
    std::fs::write(&after_p, yc_render::generate_ass(&after, &style, None))?;
    println!("\nwrote {}\n      {}", before_p.display(), after_p.display());
    Ok(())
}
