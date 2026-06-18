//! Dense local arousal probe (ADR 0008 gate, second half): confirm that genuine
//! reactions actually *peak* high in arousal -- not just that silence scores low.
//!   ...--example arousal_probe --features ser -- workspace\ZSegfmsrYmE 2378,3304 [pad_s] [hop_s]
//!
//! Scans +/- pad_s around each center timestamp at a fine hop and prints the
//! per-step arousal trace, marking the window over the center and the peak.
//! Compare the peak to the whole-VOD distribution from `arousal_scan`.

#[cfg(not(feature = "ser"))]
fn main() {
    eprintln!("build with --features ser");
}

#[cfg(feature = "ser")]
fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;
    use yc_detect::arousal;

    let mut args = std::env::args().skip(1);
    let dir =
        PathBuf::from(args.next().expect("usage: arousal_probe <workdir> <center_s[,center_s..]> [pad_s] [hop_s]"));
    let centers: Vec<f64> =
        args.next().expect("need center_s").split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let pad_s: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(15.0);
    let hop_s: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let wav = dir.join("analysis.wav");
    let model = PathBuf::from("models/w2v2-emotion/model.onnx");
    anyhow::ensure!(model.is_file(), "SER model missing: {}", model.display());

    let mut reader = hound::WavReader::open(&wav)?;
    let sr = reader.spec().sample_rate as f64;
    let samples: Vec<f32> =
        reader.samples::<i16>().map(|s| s.unwrap_or(0) as f32 / 32768.0).collect();
    let win = (arousal::WINDOW_S * sr) as usize;
    let mut ser = arousal::Ser::load(&model)?;
    eprintln!("reference whole-VOD arousal: median 0.67, p90 0.91, max 1.10");

    let mmss = |t: f64| {
        let s = t.round() as i64;
        format!("{}:{:02}", s / 60, s % 60)
    };

    for c in centers {
        println!("\n=== {} +/- {}s ({}s hop, {}s win) ===", mmss(c), pad_s, hop_s, arousal::WINDOW_S);
        println!("    time    arousal  loud");
        let (mut peak_a, mut peak_t) = (f32::NEG_INFINITY, c);
        let mut t = (c - pad_s).max(0.0);
        while t <= c + pad_s {
            let start = (t * sr) as usize;
            if start >= samples.len() {
                break;
            }
            let w = &samples[start..(start + win).min(samples.len())];
            let a = ser.arousal_max(w, win, win)?; // one window (len <= win)
            let loud = (w.iter().map(|&x| x * x).sum::<f32>() / w.len().max(1) as f32).sqrt();
            let mark = if t <= c && c < t + arousal::WINDOW_S { "  <- center" } else { "" };
            println!("  {:>6}  {:>6.3}  {:>5.3}{}", mmss(t), a, loud, mark);
            if a > peak_a {
                peak_a = a;
                peak_t = t;
            }
            t += hop_s;
        }
        println!("  PEAK arousal {:.3} at {}  (whole-VOD median 0.67 / p90 0.91 / max 1.10)", peak_a, mmss(peak_t));
    }
    Ok(())
}
