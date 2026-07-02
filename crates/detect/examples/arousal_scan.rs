//! Whole-VOD arousal scan (ADR 0008 mixed-audio gate / discovery preview):
//!   scripts\cargo-cuda.bat run -p yc-detect --example arousal_scan --features ser -- workspace\ZSegfmsrYmE [hop_s] [target_s]
//!
//! Scores speech-emotion arousal across the WHOLE VOD (including quiet/flat
//! stretches, not just loudness candidates) to answer the gate question: does
//! arousal separate flat/quiet audio (low) from reactions (high), or is it
//! stuck near 1.0 (the mixed-audio failure where the game drowns the streamer)?
//! Dumps the arousal distribution, its correlation with loudness, the top and
//! bottom windows, and where a known reaction (target_s, default the 39:38
//! jumpscare) lands in the distribution.

#[cfg(not(feature = "ser"))]
fn main() {
    eprintln!("build with --features ser: cargo run -p yc-detect --example arousal_scan --features ser -- <vod_workdir>");
}

#[cfg(feature = "ser")]
fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;
    use yc_detect::arousal;

    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: arousal_scan <vod_workdir> [hop_s] [target_s]"));
    let hop_s: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8.0);
    let target_s: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(2378.0);
    let wav = dir.join("analysis.wav");
    let model = PathBuf::from("models/w2v2-emotion/model.onnx");
    anyhow::ensure!(model.is_file(), "SER model missing: {}", model.display());

    // Read the whole 16 kHz mono analysis wav.
    let mut reader = hound::WavReader::open(&wav)?;
    let sr = reader.spec().sample_rate as f64;
    let samples: Vec<f32> =
        reader.samples::<i16>().map(|s| s.unwrap_or(0) as f32 / 32768.0).collect();
    eprintln!("read {:.0}s of audio ({} samples)", samples.len() as f64 / sr, samples.len());

    let win = (arousal::WINDOW_S * sr) as usize;
    let hop = (hop_s * sr) as usize;
    let starts = arousal::window_starts(samples.len(), win, hop);

    let mut ser = arousal::Ser::load(&model)?;
    let t = std::time::Instant::now();
    let series = ser.arousal_series(&samples, win, hop)?;
    let secs = t.elapsed().as_secs_f64();
    eprintln!("scored {} windows in {:.0}s ({:.2}s/win)", series.len(), secs, secs / series.len().max(1) as f64);
    anyhow::ensure!(series.len() == starts.len(), "series/starts length mismatch");

    // Loudness (RMS) per window, on the same grid, to test redundancy.
    let loud: Vec<f32> = starts
        .iter()
        .map(|&s| {
            let w = &samples[s..(s + win).min(samples.len())];
            (w.iter().map(|&x| x * x).sum::<f32>() / w.len().max(1) as f32).sqrt()
        })
        .collect();

    // Distribution.
    let mut sorted = series.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| sorted[((p * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1)];
    println!(
        "\narousal over the whole VOD ({} windows, {}s win / {}s hop):",
        series.len(),
        arousal::WINDOW_S,
        hop_s
    );
    println!(
        "  min {:.3}  p10 {:.3}  p25 {:.3}  median {:.3}  p75 {:.3}  p90 {:.3}  max {:.3}  (range {:.3})",
        sorted[0],
        pct(0.10),
        pct(0.25),
        pct(0.50),
        pct(0.75),
        pct(0.90),
        sorted[sorted.len() - 1],
        sorted[sorted.len() - 1] - sorted[0],
    );
    println!(
        "  Pearson r(arousal, loudness) = {:.3}  (near 1 => redundant with loudness; lower => adds signal)",
        pearson(&series, &loud)
    );

    let mmss = |i: usize| {
        let s = (starts[i] as f64 / sr).round() as i64;
        format!("{}:{:02}", s / 60, s % 60)
    };

    // Where does the known reaction (target_s) land?
    if let Some(ti) = starts.iter().position(|&s| {
        let a = s as f64 / sr;
        target_s >= a && target_s < a + arousal::WINDOW_S + hop_s
    }) {
        let rank = series.iter().filter(|&&v| v > series[ti]).count() + 1;
        println!(
            "\n  target {} -> arousal {:.3} at {}  (rank {}/{}, p{:.0})",
            { let s = target_s.round() as i64; format!("{}:{:02}", s / 60, s % 60) },
            series[ti],
            mmss(ti),
            rank,
            series.len(),
            100.0 * (1.0 - rank as f64 / series.len() as f64),
        );
    }

    // Top / bottom windows.
    let mut idx: Vec<usize> = (0..series.len()).collect();
    idx.sort_by(|&a, &b| series[b].partial_cmp(&series[a]).unwrap_or(std::cmp::Ordering::Equal));
    println!("\n  top 15 arousal windows          bottom 10 arousal windows");
    println!("    time    arousal  loud           time    arousal  loud");
    for k in 0..15 {
        let hi = idx[k];
        let left = format!("  {:>6}  {:>6.3}  {:>5.3}", mmss(hi), series[hi], loud[hi]);
        if k < 10 {
            let lo = idx[idx.len() - 1 - k];
            println!("{}      {:>6}  {:>6.3}  {:>5.3}", left, mmss(lo), series[lo], loud[lo]);
        } else {
            println!("{left}");
        }
    }

    // --- ADD vs RE-WEIGHT (ADR 0008 discovery question) ---
    // Build the REAL loudness+chat discovery candidates, then peak-detect the
    // whole-VOD arousal series the way discovery peak-detects loudness, and count
    // how many top-N arousal candidates fall OUTSIDE every loudness/chat candidate
    // range. That off-diagonal count is the add-vs-reweight answer: ~0 => arousal
    // only re-weights candidates discovery already found (the refine weight
    // suffices); meaningful => a whole-VOD arousal discovery pass surfaces moments
    // loudness+chat never nominate.
    {
        use yc_detect::{chat, loudness, rank_moments, score, DetectParams};
        let params = DetectParams::default();
        let loud_bins = loudness::read_rms_bins(&wav, params.bin_s)?;
        let chat_path = dir.join("chat.live_chat.json");
        let chat_counts = if chat_path.exists() {
            let offs = chat::message_offsets(&chat_path)?;
            Some(score::bin_counts(&offs, params.bin_s, loud_bins.len()))
        } else {
            None
        };
        let cands = rank_moments(&loud_bins, chat_counts.as_deref(), &params);

        // Arousal candidates: robust-z the whole-VOD arousal series, peak-detect +
        // NMS on the arousal grid (hop_s spacing), then lead-window each peak
        // exactly like discovery (lead_s + the min-duration window) and take
        // top_n by arousal-z.
        let az = score::robust_z(&series);
        let peaks = score::find_peaks(&az, params.min_z);
        let min_gap = (params.min_dur_s / hop_s).round().max(1.0) as usize;
        let kept = score::nms(peaks, &az, min_gap);
        let arousal_cands: Vec<(f64, f64, f32, f32)> = kept // (start_s, end_s, arousal, loud)
            .into_iter()
            .take(params.top_n)
            .map(|i| {
                let center = starts[i] as f64 / sr + arousal::WINDOW_S / 2.0;
                let start = (center - params.lead_s).max(0.0);
                (start, start + params.min_dur_s, series[i], loud[i])
            })
            .collect();

        let ts = |t: f64| {
            let s = t.round() as i64;
            format!("{}:{:02}", s / 60, s % 60)
        };
        let overlaps =
            |a0: f64, a1: f64| cands.iter().any(|c| a0 < c.range.end_s && c.range.start_s < a1);
        let adds: Vec<_> = arousal_cands.iter().filter(|&&(s0, s1, _, _)| !overlaps(s0, s1)).collect();

        println!("\n--- ADD vs RE-WEIGHT (does whole-VOD arousal surface NEW candidates?) ---");
        println!("  loudness+chat discovery candidates : {}", cands.len());
        println!("  top-{} arousal candidates           : {}", params.top_n, arousal_cands.len());
        println!(
            "  arousal candidates OUTSIDE every loudness/chat candidate : {} (the 'adds')",
            adds.len()
        );
        if adds.is_empty() {
            println!("  => arousal RE-WEIGHTS only; no whole-VOD discovery pass needed.");
        } else {
            println!("    start     end    arousal   loud");
            for &&(s0, s1, a, l) in &adds {
                println!("  {:>6}  {:>6}  {:>7.3}  {:>5.3}", ts(s0), ts(s1), a, l);
            }
        }
    }
    Ok(())
}

/// Pearson correlation; 0 when either series is constant.
#[cfg(feature = "ser")]
fn pearson(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len() as f32;
    let (ma, mb) = (a.iter().sum::<f32>() / n, b.iter().sum::<f32>() / n);
    let (mut cov, mut va, mut vb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        cov += (x - ma) * (y - mb);
        va += (x - ma).powi(2);
        vb += (y - mb).powi(2);
    }
    if va > 0.0 && vb > 0.0 {
        cov / (va.sqrt() * vb.sqrt())
    } else {
        0.0
    }
}
