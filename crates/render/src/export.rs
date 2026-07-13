//! ffmpeg filtergraph + NVENC export (ADR 0004): one invocation crops/scales/
//! vstacks the Panels, burns the generated ASS via libass, and encodes with
//! h264_nvenc. The same filtergraph at reduced resolution will drive the M5
//! true preview, so this builder is the single source of the composite —
//! preview and export cannot disagree.

use anyhow::{Context, Result};
use std::path::Path;
use yc_core::{CameraPlan, Crop, Layout, NoConsole, Shot, CANVAS_H, CANVAS_W};

/// `crop=w:h:x:y` in source pixels. Dimensions floored to even numbers >= 2 so
/// the yuv420p encoder never sees an odd or zero-sized Panel.
fn fmt_crop(c: &Crop) -> String {
    let even = |v: f32, min: f32| ((v.round().max(min) as i64) / 2) * 2;
    format!(
        "crop={}:{}:{}:{}",
        even(c.w, 2.0),
        even(c.h, 2.0),
        c.x.round().max(0.0) as i64,
        c.y.round().max(0.0) as i64
    )
}

/// Build the `-filter_complex` graph compositing the Layout and burning
/// `ass_name`. `ass_name` is relative to ffmpeg's working dir: we run ffmpeg in
/// the clip folder so the `subtitles` filter never has to escape a Windows
/// drive colon or backslash. `fontsdir=fonts` points libass at a **fonts-only**
/// subdirectory (the caller copies the caption font there) so it never tries to
/// open the sibling intermediates (`analysis.wav`, `clip.ass`, `project.json`)
/// as fonts — the noisy `Error opening memory font` lines a flat `fontsdir=.`
/// produced. Relative, so it stays clear of the Windows drive-colon escaping too.
pub fn build_filtergraph(layout: &Layout, ass_name: &str) -> String {
    match layout {
        Layout::Stacked { seam, gameplay, facecam } => {
            let gh = ((CANVAS_H as f32 * seam).round().clamp(2.0, (CANVAS_H - 2) as f32) as i64 / 2)
                * 2;
            let fh = CANVAS_H as i64 - gh;
            format!(
                "[0:v]{g},scale={w}:{gh},setsar=1[g];\
                 [0:v]{f},scale={w}:{fh},setsar=1[f];\
                 [g][f]vstack=inputs=2[v];\
                 [v]subtitles={ass_name}:fontsdir=fonts[out]",
                g = fmt_crop(gameplay),
                f = fmt_crop(facecam),
                w = CANVAS_W,
            )
        }
        Layout::FullFrame { crop } => format!(
            "[0:v]{g},scale={w}:{h},setsar=1[v];[v]subtitles={ass_name}:fontsdir=fonts[out]",
            g = fmt_crop(crop),
            w = CANVAS_W,
            h = CANVAS_H,
        ),
    }
}

/// The crop/scale/vstack chain for one [`Layout`], writing into `[out_label]` —
/// the shared composite core of [`build_filtergraph`] (whole-clip, `[0:v]`) and
/// the per-shot chains of [`build_camera_filtergraph`] (each shot's trimmed
/// stream). No subtitles here; the caller burns them once at the end.
fn layout_chain(layout: &Layout, in_label: &str, out_label: &str) -> String {
    match layout {
        Layout::Stacked { seam, gameplay, facecam } => {
            let gh = ((CANVAS_H as f32 * seam).round().clamp(2.0, (CANVAS_H - 2) as f32) as i64 / 2)
                * 2;
            let fh = CANVAS_H as i64 - gh;
            format!(
                "[{i}]split=2[{o}ga][{o}fa];\
                 [{o}ga]{g},scale={w}:{gh},setsar=1[{o}g];\
                 [{o}fa]{f},scale={w}:{fh},setsar=1[{o}f];\
                 [{o}g][{o}f]vstack=inputs=2[{o}]",
                i = in_label,
                o = out_label,
                g = fmt_crop(gameplay),
                f = fmt_crop(facecam),
                w = CANVAS_W,
            )
        }
        Layout::FullFrame { crop } => format!(
            "[{i}]{g},scale={w}:{h},setsar=1[{o}]",
            i = in_label,
            o = out_label,
            g = fmt_crop(crop),
            w = CANVAS_W,
            h = CANVAS_H,
        ),
    }
}

/// Build the dynamic-camera filtergraph for a [`CameraPlan`] (podcast
/// active-speaker mode): each [`Shot`] trims its contiguous span off the input,
/// composites its own Layout (solo crop, or a stacked split for a group shot),
/// and the shots concat back into one 1080x1920 stream — **hard cuts** between
/// speakers, the way a human editor cuts. A solo shot whose subject drifted
/// carries a `pan_to`: its crop origin glides linearly across the shot (the
/// slow follow), still a single crop filter via time expressions. The ASS burn
/// runs once over the concatenated stream, so caption timing is untouched
/// (shots are contiguous and start at 0, exactly the whole-clip timeline).
///
/// `cut_audio` is the timeline-razor mode (ADR 0065): the shots are NOT
/// contiguous (removed spans between them), so the graph also `atrim`s the
/// matching audio span per shot and concats video+audio pairs into `[out]` +
/// `[aout]` — the caller maps `[aout]` instead of the raw source audio, and
/// burns an ASS whose times were remapped onto the compressed timeline
/// (`yc_core::remap_units_through_cuts`). With `cut_audio` false the audio is
/// untouched (contiguous shots ARE the source timeline).
///
/// The caller writes this to a script file and passes `-filter_complex_script`
/// (a many-shot graph outgrows a comfortable command line).
pub fn build_camera_filtergraph(plan: &CameraPlan, ass_name: &str, cut_audio: bool) -> String {
    // Defensive: an empty plan degrades to a centered full-frame — callers
    // shouldn't send one, but a graph that fails to parse sinks the render.
    if plan.shots.is_empty() {
        let full = Layout::FullFrame {
            crop: Crop { x: 0.0, y: 0.0, w: CANVAS_W as f32, h: CANVAS_H as f32 },
        };
        return build_filtergraph(&full, ass_name);
    }
    let mut parts: Vec<String> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for (i, shot) in plan.shots.iter().enumerate() {
        // trim + setpts rebase each shot to its own 0, so concat re-joins them
        // into one continuous timeline identical to the source clip's.
        //
        // Boundaries print at full f64 precision (`{}` is shortest-round-trip),
        // NEVER rounded: a cut boundary is a real source-frame pts (scene
        // detection returns the first frame of the incoming shot), trim's start
        // is INCLUSIVE (keeps pts >= start) and its end EXCLUSIVE, so an exact
        // boundary hands every frame to exactly one shot, with the cut frame
        // opening the INCOMING shot. The old `{:.3}` rounded ~half of all cut
        // pts UP past the cut frame, which stranded that frame at the tail of
        // the OUTGOING shot — one frame of the new scene through the old
        // shot's crop (the operator's "empty seat" flash at cuts; measured on
        // the ANTITESA export: 7 of its 14 cuts flashed, exactly the 7 whose
        // pts rounded up, e.g. 13.302833 -> 13.303).
        parts.push(format!(
            "[0:v]trim=start={}:end={},setpts=PTS-STARTPTS[t{i}]",
            shot.start_s, shot.end_s
        ));
        parts.push(shot_chain(shot, &format!("t{i}"), &format!("s{i}")));
        if cut_audio {
            // The SAME span off the audio, so every video piece travels with
            // exactly its own sound — A/V can't drift no matter what was cut.
            parts.push(format!(
                "[0:a]atrim=start={}:end={},asetpts=PTS-STARTPTS[a{i}]",
                shot.start_s, shot.end_s
            ));
            labels.push(format!("[s{i}][a{i}]"));
        } else {
            labels.push(format!("[s{i}]"));
        }
    }
    if cut_audio {
        parts.push(format!(
            "{}concat=n={}:v=1:a=1[cat][aout];[cat]subtitles={ass_name}:fontsdir=fonts[out]",
            labels.join(""),
            plan.shots.len(),
        ));
    } else {
        parts.push(format!(
            "{}concat=n={}:v=1:a=0[cat];[cat]subtitles={ass_name}:fontsdir=fonts[out]",
            labels.join(""),
            plan.shots.len(),
        ));
    }
    parts.join(";")
}

/// The composite chain for one [`Shot`]: its Layout statically, or — for a
/// solo shot with a follow pan — a crop whose origin glides linearly from the
/// opening to the closing position across the shot. `t` is shot-relative
/// (each shot's `setpts` rebases to 0) and the crop size never changes (the
/// zoom must not breathe). The expressions are quoted and their commas
/// escaped, so the filtergraph parser passes them to the crop filter whole.
fn shot_chain(shot: &Shot, in_label: &str, out_label: &str) -> String {
    if let (Layout::FullFrame { crop }, Some(to)) = (&shot.layout, &shot.pan_to) {
        let dur = (shot.end_s - shot.start_s).max(0.001);
        let even = |v: f32| (((v.round().max(2.0)) as i64) / 2) * 2;
        let (x0, y0) = (crop.x.max(0.0), crop.y.max(0.0));
        return format!(
            "[{i}]crop={w}:{h}:x='{x0:.1}+({dx:.1})*min(t/{dur:.3}\\,1)':y='{y0:.1}+({dy:.1})*min(t/{dur:.3}\\,1)',scale={cw}:{ch},setsar=1[{o}]",
            i = in_label,
            o = out_label,
            w = even(crop.w),
            h = even(crop.h),
            dx = to.x.max(0.0) - x0,
            dy = to.y.max(0.0) - y0,
            cw = CANVAS_W,
            ch = CANVAS_H,
        );
    }
    layout_chain(&shot.layout, in_label, out_label)
}

/// Wrap a finished graph with the thumbnail-intro prepend (ADR 0067, plan #5):
/// the image (ffmpeg input **1**, see [`export_args`]' `intro`) scales/pads to
/// the canvas (aspect-fit, black bars), matches `fps`+SAR+format, gains
/// `anullsrc` silence, and concats AHEAD of the main stream — **after** its
/// ASS burn, so every caption/camera/razor time stays source-relative by
/// construction (the operator's "everything below should follow"). Both audio
/// branches pass `aformat` (fltp/48k/stereo) so concat's same-parameters rule
/// holds for any source. The main chain survives byte-for-byte modulo its
/// terminal labels (`[out]`→`[mainv]`, razor `[aout]`→`[maina]`); the wrapped
/// graph re-terminates in `[out]`+`[aout]`, so the arg mapping is unchanged.
///
/// `fps` is the probed source rate (≤ 0 falls back to 30 — concat still
/// timestamps correctly, the container is VFR-tolerant). `razor_audio` says
/// the graph already produces `[aout]` (the razor's per-piece audio concat);
/// otherwise the main audio joins from the raw `[0:a]`.
pub fn prepend_intro(graph: &str, intro_d: f64, fps: f64, razor_audio: bool) -> String {
    let fps = if fps.is_finite() && fps > 0.0 { fps } else { 30.0 };
    let g = graph.replace("[out]", "[mainv]");
    let (g, main_a) =
        if razor_audio { (g.replace("[aout]", "[maina]"), "[maina]") } else { (g, "[0:a]") };
    format!(
        "{g};\
         [1:v]scale={w}:{h}:force_original_aspect_ratio=decrease,\
         pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,fps={fps:.3},format=yuv420p[thumbv];\
         anullsrc=channel_layout=stereo:sample_rate=48000,atrim=duration={d:.3},\
         aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo[thumba];\
         {main_a}aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo[mainaf];\
         [thumbv][thumba][mainv][mainaf]concat=n=2:v=1:a=1[out][aout]",
        w = CANVAS_W,
        h = CANVAS_H,
        d = intro_d,
    )
}

/// Wrap a finished graph with the Music track's ONE amix (ADR 0068, plan #6)
/// — the [`prepend_intro`] move one layer further out, applied AFTER
/// everything (burn and intro included), so the mix hears the export's final
/// output clock and each clip's `at_s` positions on it via `adelay`. A graph
/// already terminating in `[aout]` (razor audio, or the intro wrap) renames
/// it to `[premix]`; otherwise the main audio joins raw from `[0:a]` (the
/// `-ss` seek applies to input 0, so its clock is already the clip's). Every
/// branch passes `aformat` (fltp / 48 kHz / stereo) so the mix negotiates
/// nothing; `duration=first` + the output `-t` bound keep the output exactly
/// the video's length — music never extends a Short — and `normalize=0`
/// keeps today's voice level byte-for-byte (amix would otherwise scale every
/// input by 1/n the moment music appears).
///
/// `input_base` is the ffmpeg input index of the FIRST music file: the files
/// ride as extra `-i` inputs after the optional intro image (see
/// [`export_args`]' `music`), so 1 without an intro, 2 with. `clips` empty
/// returns the graph untouched — byte-identical to today, test-pinned.
pub fn mix_music(graph: &str, clips: &[yc_core::MusicClip], input_base: usize) -> String {
    if clips.is_empty() {
        return graph.to_string();
    }
    const AFMT: &str = "aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo";
    let (g, main_a) = if graph.contains("[aout]") {
        (graph.replace("[aout]", "[premix]"), "[premix]")
    } else {
        (graph.to_string(), "[0:a]")
    };
    let mut parts = vec![g, format!("{main_a}{AFMT}[mbase]")];
    let mut labels = String::from("[mbase]");
    for (k, c) in clips.iter().enumerate() {
        // adelay positions the clip at `at_s` of the OUTPUT clock, in whole
        // ms (`all=1` covers every channel); trim boundaries print
        // shortest-round-trip like the camera trims — never rounded.
        let delay_ms = (c.at_s.max(0.0) * 1000.0).round() as i64;
        parts.push(format!(
            "[{i}:a]atrim=start={s}:end={e},asetpts=PTS-STARTPTS,volume={v},{AFMT},\
             adelay={delay_ms}:all=1[m{k}]",
            i = input_base + k,
            s = c.in_s.max(0.0),
            e = c.out_s.max(c.in_s),
            v = c.gain.max(0.0),
        ));
        labels.push_str(&format!("[m{k}]"));
    }
    parts.push(format!(
        "{labels}amix=inputs={n}:duration=first:normalize=0[aout]",
        n = clips.len() + 1,
    ));
    parts.join(";")
}

/// ffmpeg args for the NVENC export. `-ss` before `-i` fast-seeks `seek_s` into
/// the source; `-t` bounds the output to `duration_s` (frame-accurate under
/// re-encode). The burned ASS timeline is 0-based, matching the reset output
/// timeline produced by the seek.
///
/// `seek_s` is decoupled from the Clip's VOD range because the two ingest paths
/// seek different sources: the M1 local file is the whole VOD, so `seek_s` is
/// the range start; the M2 Segment is a padded slice, so `seek_s` is the
/// in-segment offset (`range.start - segment_start`; see `yc_ingest`).
///
/// `intro` is the thumbnail intro (ADR 0067): `Some((image, duration))` adds
/// the image as input 1 (`-loop 1 -t D`), extends the output `-t` bound to
/// `duration_s + D`, and maps the graph's `[aout]` (the wrapped graph carries
/// the intro's silence — see [`prepend_intro`]). `music` is the Music track's
/// files (ADR 0068), riding as plain `-i` inputs AFTER the optional intro —
/// the [`mix_music`] wrap addresses them from `input_base`, and with music
/// the audio always maps from the graph. `None` + empty — always in
/// headless / batch — leaves every arg byte-identical to the pre-intro export.
pub fn export_args(
    source: &Path,
    seek_s: f64,
    duration_s: f64,
    filtergraph: &str,
    out_name: &str,
    intro: Option<(&Path, f64)>,
    music: &[std::path::PathBuf],
) -> Vec<String> {
    export_args_inner(
        source,
        seek_s,
        duration_s,
        "-filter_complex",
        filtergraph,
        out_name,
        false,
        intro,
        music,
    )
}

/// [`export_args`] with the graph in a **script file** (`-filter_complex_script`,
/// relative to ffmpeg's working dir) instead of inline — the dynamic-camera
/// graph ([`build_camera_filtergraph`]) grows with its shot count and would
/// outgrow a comfortable command line. `filtered_audio` maps the graph's
/// `[aout]` (the razor-cut audio concat) instead of the raw source audio.
#[allow(clippy::too_many_arguments)]
pub fn export_args_script(
    source: &Path,
    seek_s: f64,
    duration_s: f64,
    script_name: &str,
    out_name: &str,
    filtered_audio: bool,
    intro: Option<(&Path, f64)>,
    music: &[std::path::PathBuf],
) -> Vec<String> {
    export_args_inner(
        source,
        seek_s,
        duration_s,
        "-filter_complex_script",
        script_name,
        out_name,
        filtered_audio,
        intro,
        music,
    )
}

#[allow(clippy::too_many_arguments)]
fn export_args_inner(
    source: &Path,
    seek_s: f64,
    duration_s: f64,
    graph_flag: &str,
    graph: &str,
    out_name: &str,
    filtered_audio: bool,
    intro: Option<(&Path, f64)>,
    music: &[std::path::PathBuf],
) -> Vec<String> {
    let mut args = vec![
        "-ss".into(),
        format!("{seek_s:.3}"),
        "-i".into(),
        source.display().to_string(),
    ];
    let mut out_t = duration_s;
    // With an intro the audio ALWAYS comes from the graph: the prepend's
    // concat pairs the intro's silence with the main audio into `[aout]`.
    let mut audio_from_graph = filtered_audio;
    if let Some((image, intro_d)) = intro {
        args.extend([
            "-loop".into(),
            "1".into(),
            "-t".into(),
            format!("{intro_d:.3}"),
            "-i".into(),
            image.display().to_string(),
        ]);
        out_t += intro_d;
        audio_from_graph = true;
    }
    // Music files (ADR 0068): inputs after the optional intro, in clip order
    // — [`mix_music`]'s `input_base` addressing depends on this position.
    // With music the audio always comes from the graph (the amix's [aout]).
    for m in music {
        args.extend(["-i".into(), m.display().to_string()]);
        audio_from_graph = true;
    }
    args.extend([
        "-t".into(),
        format!("{out_t:.3}"),
        graph_flag.into(),
        graph.into(),
        "-map".into(),
        "[out]".into(),
        "-map".into(),
        if audio_from_graph { "[aout]".into() } else { "0:a:0".into() },
        "-c:v".into(),
        "h264_nvenc".into(),
        "-preset".into(),
        "p5".into(),
        "-rc".into(),
        "vbr".into(),
        "-cq".into(),
        "21".into(),
        "-b:v".into(),
        "0".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-movflags".into(),
        "+faststart".into(),
        "-y".into(),
        out_name.into(),
    ]);
    args
}

/// Run the export. ffmpeg runs with `workdir` as cwd so the relative ASS and
/// `fontsdir=.` resolve (and Windows filtergraph path-escaping is avoided).
/// `should_cancel` is polled while the encode runs: the NVENC export is the
/// longest single child the app spawns, and before this a Cancel merely set a
/// flag the worker read *after* the full encode finished.
pub fn run_export(
    ffmpeg: &Path,
    workdir: &Path,
    args: &[String],
    should_cancel: &dyn Fn() -> bool,
) -> Result<()> {
    let mut child = std::process::Command::new(ffmpeg)
        .no_console()
        .current_dir(workdir)
        .args(args)
        .spawn()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    let status = yc_core::wait_killable(&mut child, should_cancel)
        .context("waiting on ffmpeg export")?;
    let Some(status) = status else {
        anyhow::bail!("cancelled");
    };
    anyhow::ensure!(status.success(), "ffmpeg export failed ({status})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacked_graph_crops_scales_vstacks_and_burns() {
        let layout = Layout::Stacked {
            seam: 0.62,
            gameplay: Crop { x: 454.0, y: 0.0, w: 1012.0, h: 1080.0 },
            facecam: Crop { x: 1440.0, y: 810.0, w: 480.0, h: 270.0 },
        };
        let g = build_filtergraph(&layout, "clip.ass");
        assert_eq!(g.matches("crop=").count(), 2);
        assert!(g.contains("vstack=inputs=2"));
        assert!(g.contains("subtitles=clip.ass:fontsdir=fonts"));
        // gameplay panel = round(1920*0.62)=1190; facecam = 1920-1190 = 730.
        assert!(g.contains("scale=1080:1190"), "graph: {g}");
        assert!(g.contains("scale=1080:730"), "graph: {g}");
    }

    #[test]
    fn camera_graph_trims_composites_and_concats_shots() {
        use yc_core::Shot;
        let solo = |x: f32| Layout::FullFrame { crop: Crop { x, y: 0.0, w: 608.0, h: 1080.0 } };
        let split = Layout::Stacked {
            seam: 0.5,
            gameplay: Crop { x: 100.0, y: 200.0, w: 640.0, h: 568.0 },
            facecam: Crop { x: 1100.0, y: 200.0, w: 640.0, h: 568.0 },
        };
        let plan = CameraPlan {
            shots: vec![
                Shot { start_s: 0.0, end_s: 8.5, track: Some(0), layout: solo(100.0), pan_to: None },
                Shot { start_s: 8.5, end_s: 14.0, track: None, layout: split, pan_to: None },
                Shot { start_s: 14.0, end_s: 30.0, track: Some(1), layout: solo(1200.0), pan_to: None },
            ],
        };
        let g = build_camera_filtergraph(&plan, "clip.ass", false);
        // One trim per shot, contiguous and rebased, boundaries printed
        // shortest-round-trip (never rounded — rounding across a source frame's
        // pts strands that frame in the wrong shot: a 1-frame flash).
        assert_eq!(g.matches("trim=start=").count(), 3);
        assert!(g.contains("trim=start=0:end=8.5,"), "graph: {g}");
        assert!(g.contains("trim=start=8.5:end=14,"), "graph: {g}");
        assert!(g.contains("trim=start=14:end=30,"), "graph: {g}");
        assert!(g.contains("setpts=PTS-STARTPTS"));
        // The group shot splits its trimmed stream for the two panels.
        assert!(g.contains("split=2"), "group shot needs an explicit split: {g}");
        assert!(g.contains("vstack=inputs=2"));
        // Concat re-joins all three, then the ASS burns once.
        assert!(g.contains("concat=n=3:v=1:a=0"));
        assert_eq!(g.matches("subtitles=").count(), 1);
        assert!(g.contains("subtitles=clip.ass:fontsdir=fonts[out]"));
        // Every shot scales to the canvas.
        assert!(g.matches("scale=1080:1920").count() == 2, "solo shots: {g}");
        assert!(g.contains("scale=1080:960"), "split panels: {g}");
    }

    #[test]
    fn a_cut_frame_is_not_stranded_in_the_outgoing_shot() {
        // The operator's flash-at-a-cut: a real source-frame pts like 13.302833
        // rounds UP to 13.303 under the old `:.3`. `trim` start is inclusive
        // (keeps pts >= start), so a boundary of 13.303 fails `13.302833 >= start`
        // and drops that first new-scene frame into the OUTGOING shot — one
        // frame of the new scene through the old shot's crop. The boundary must
        // land AT or BELOW the frame's pts so it joins the INCOMING shot. Both
        // constants are measured cut pts from the ANTITESA production clip:
        // CUT_7DP also rounds up under a fixed `{:.6}` (0.0812889 -> 0.081289),
        // which only shortest-round-trip printing survives.
        use yc_core::Shot;
        let solo = |x: f32| Layout::FullFrame { crop: Crop { x, y: 0.0, w: 608.0, h: 1080.0 } };
        const CUT_7DP: f64 = 0.081_288_9;
        const CUT: f64 = 13.302_833;
        let plan = CameraPlan {
            shots: vec![
                Shot { start_s: 0.0, end_s: CUT_7DP, track: Some(1), layout: solo(1200.0), pan_to: None },
                Shot { start_s: CUT_7DP, end_s: CUT, track: Some(0), layout: solo(100.0), pan_to: None },
                Shot { start_s: CUT, end_s: 25.0, track: Some(1), layout: solo(1200.0), pan_to: None },
            ],
        };
        let g = build_camera_filtergraph(&plan, "clip.ass", false);
        // Each incoming shot's trim start must be <= its cut frame's pts (so
        // `pts >= start` keeps the frame) yet not reach back to the previous
        // frame (~41.7 ms earlier at 23.976 fps).
        let start_at = |nth: usize| {
            g.split("trim=start=")
                .nth(nth)
                .and_then(|s| s.split(':').next())
                .and_then(|s| s.parse::<f64>().ok())
                .expect("shot has a trim start")
        };
        for (nth, cut) in [(2, CUT_7DP), (3, CUT)] {
            let start = start_at(nth);
            assert!(
                start <= cut,
                "incoming trim start {start} must not exceed the cut frame pts {cut} (graph: {g})"
            );
            assert!(start > cut - 0.041, "and must not reach the previous frame (graph: {g})");
        }
    }

    #[test]
    fn empty_camera_plan_degrades_to_a_static_full_frame() {
        let g = build_camera_filtergraph(&CameraPlan::default(), "clip.ass", false);
        assert!(g.contains("subtitles=clip.ass"));
        assert!(!g.contains("concat"));
    }

    #[test]
    fn follow_shot_pans_the_crop_origin_across_the_shot() {
        let plan = CameraPlan {
            shots: vec![Shot {
                start_s: 2.0,
                end_s: 10.0,
                track: Some(0),
                layout: Layout::FullFrame {
                    crop: Crop { x: 100.0, y: 40.0, w: 452.0, h: 802.0 },
                },
                pan_to: Some(Crop { x: 220.0, y: 40.0, w: 452.0, h: 802.0 }),
            }],
        };
        let g = build_camera_filtergraph(&plan, "clip.ass", false);
        // Same-size crop, origin gliding over the 8 s shot; commas escaped so
        // the expression survives the filtergraph parser.
        assert!(g.contains("crop=452:802:x='100.0+(120.0)*min(t/8.000\\,1)'"), "graph: {g}");
        assert!(g.contains(":y='40.0+(0.0)*min(t/8.000\\,1)'"), "graph: {g}");
        // A pan shot still scales to the canvas and burns once after concat.
        assert!(g.contains("scale=1080:1920"));
        assert_eq!(g.matches("subtitles=").count(), 1);
    }

    #[test]
    fn static_shots_keep_the_plain_crop() {
        let plan = CameraPlan {
            shots: vec![Shot {
                start_s: 0.0,
                end_s: 5.0,
                track: Some(0),
                layout: Layout::FullFrame {
                    crop: Crop { x: 380.0, y: 0.0, w: 452.0, h: 802.0 },
                },
                pan_to: None,
            }],
        };
        let g = build_camera_filtergraph(&plan, "clip.ass", false);
        assert!(g.contains("crop=452:802:380:0"), "graph: {g}");
        assert!(!g.contains("min(t/"), "no expression on a static shot: {g}");
    }

    #[test]
    fn razor_graph_cuts_audio_with_video_and_maps_aout() {
        // Timeline razor (ADR 0065): non-contiguous shots — every video piece
        // must travel with exactly its own audio span, pairs interleaved into
        // one v+a concat, and the args must map the graph's [aout].
        use yc_core::Shot;
        let solo = |x: f32| Layout::FullFrame { crop: Crop { x, y: 0.0, w: 608.0, h: 1080.0 } };
        let plan = CameraPlan {
            shots: vec![
                Shot { start_s: 0.0, end_s: 3.0, track: None, layout: solo(100.0), pan_to: None },
                Shot { start_s: 6.0, end_s: 10.0, track: None, layout: solo(100.0), pan_to: None },
            ],
        };
        let g = build_camera_filtergraph(&plan, "clip.ass", true);
        assert_eq!(g.matches("atrim=start=").count(), 2, "one audio trim per piece: {g}");
        assert!(g.contains("atrim=start=6:end=10"), "audio spans match video: {g}");
        assert!(g.contains("asetpts=PTS-STARTPTS"));
        assert!(g.contains("[s0][a0][s1][a1]concat=n=2:v=1:a=1[cat][aout]"), "graph: {g}");
        assert_eq!(g.matches("subtitles=").count(), 1, "ASS burns once, post-concat");
        let args = export_args_script(
            Path::new("F:/seg.mp4"), 1.0, 30.0, "camera.fg", "o.mp4", true, None, &[],
        );
        assert!(args.contains(&"[aout]".to_string()), "maps the cut audio");
        assert!(!args.contains(&"0:a:0".to_string()), "raw source audio must not be mapped");
        // The contiguous camera path keeps the raw audio map.
        let plain = export_args_script(
            Path::new("F:/seg.mp4"), 1.0, 30.0, "camera.fg", "o.mp4", false, None, &[],
        );
        assert!(plain.contains(&"0:a:0".to_string()));
    }

    #[test]
    fn export_args_script_uses_the_script_flag() {
        let args = export_args_script(
            Path::new("F:/seg.mp4"), 1.0, 30.0, "camera.fg", "o.mp4", false, None, &[],
        );
        let f = args.iter().position(|a| a == "-filter_complex_script").unwrap();
        assert_eq!(args[f + 1], "camera.fg");
        assert!(!args.contains(&"-filter_complex".to_string()));
        assert!(args.contains(&"h264_nvenc".to_string()));
    }

    #[test]
    fn export_seeks_before_input_and_uses_nvenc() {
        // M2 promote: seek the in-segment offset (2.0s), not the VOD range start.
        let args =
            export_args(Path::new("F:/segment.mp4"), 2.0, 7.5, "FG", "export.mp4", None, &[]);
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        let i = args.iter().position(|a| a == "-i").unwrap();
        assert!(ss < i, "-ss must precede -i for fast seek");
        assert_eq!(args[ss + 1], "2.000"); // seek = in-segment offset
        assert!(args.contains(&"h264_nvenc".to_string()));
        let t = args.iter().position(|a| a == "-t").unwrap();
        assert_eq!(args[t + 1], "7.500"); // -t bounds the output to the clip duration
    }

    // ---- thumbnail intro (ADR 0067) ----------------------------------------

    #[test]
    fn intro_prepends_after_the_single_burn_and_concats_av() {
        // The pre-registered bar: the prepend sits AFTER the one subtitles=
        // (count stays 1), the main chain survives byte-for-byte modulo its
        // terminal label, and the wrapped graph re-terminates in [out]+[aout].
        let layout = Layout::FullFrame {
            crop: Crop { x: 0.0, y: 0.0, w: 608.0, h: 1080.0 },
        };
        let base = build_filtergraph(&layout, "clip.ass");
        let g = prepend_intro(&base, 1.0, 23.976, false);
        assert_eq!(g.matches("subtitles=").count(), 1, "ASS burns once: {g}");
        // The main chain is intact — only its terminal label renamed.
        assert!(g.starts_with(&base.replace("[out]", "[mainv]")), "main chain rewritten: {g}");
        // The image (input 1) aspect-fits the canvas and matches the grid.
        assert!(g.contains("[1:v]scale=1080:1920:force_original_aspect_ratio=decrease"), "{g}");
        assert!(g.contains("pad=1080:1920:(ow-iw)/2:(oh-ih)/2:color=black"), "{g}");
        assert!(g.contains("fps=23.976"), "intro runs on the source grid: {g}");
        // Silence + aformat on BOTH branches (concat's same-parameters rule).
        assert!(g.contains("anullsrc="), "{g}");
        assert!(g.contains("atrim=duration=1.000"), "{g}");
        assert_eq!(g.matches("aformat=sample_fmts=fltp").count(), 2, "{g}");
        // Intro FIRST, then the burned main — one v+a concat into [out]/[aout].
        assert!(
            g.contains("[thumbv][thumba][mainv][mainaf]concat=n=2:v=1:a=1[out][aout]"),
            "graph: {g}"
        );
        // Unknown fps degrades to the 30 fallback, never a broken filter.
        assert!(prepend_intro(&base, 1.0, 0.0, false).contains("fps=30.000"));
    }

    #[test]
    fn intro_composes_with_the_razor_camera_graph() {
        // Razor + camera + intro in ONE graph: the per-piece a/v pairing
        // stays, the burn stays post-concat and single, and the razor's
        // [aout] feeds the intro concat instead of the raw source audio.
        use yc_core::Shot;
        let solo = |x: f32| Layout::FullFrame { crop: Crop { x, y: 0.0, w: 608.0, h: 1080.0 } };
        let plan = CameraPlan {
            shots: vec![
                Shot { start_s: 0.0, end_s: 3.0, track: None, layout: solo(100.0), pan_to: None },
                Shot { start_s: 6.0, end_s: 10.0, track: None, layout: solo(100.0), pan_to: None },
            ],
        };
        let base = build_camera_filtergraph(&plan, "clip.ass", true);
        let g = prepend_intro(&base, 1.5, 30.0, true);
        assert_eq!(g.matches("subtitles=").count(), 1, "ASS burns once: {g}");
        assert_eq!(g.matches("atrim=start=").count(), 2, "razor audio pairing intact: {g}");
        assert!(g.contains("[s0][a0][s1][a1]concat=n=2:v=1:a=1[cat][maina]"), "graph: {g}");
        assert!(g.contains("[maina]aformat="), "razor audio feeds the intro concat: {g}");
        assert!(!g.contains("[0:a]aformat="), "raw audio must not bypass the razor: {g}");
        assert!(
            g.contains("[thumbv][thumba][mainv][mainaf]concat=n=2:v=1:a=1[out][aout]"),
            "graph: {g}"
        );
    }

    #[test]
    fn intro_args_add_the_image_input_extend_t_and_map_aout() {
        let img = Path::new("F:/covers/thumb.png");
        let args =
            export_args(Path::new("F:/seg.mp4"), 2.0, 7.5, "FG", "o.mp4", Some((img, 1.0)), &[]);
        // The image is input 1: -loop 1 -t D ahead of ITS -i, after the source.
        let loops: Vec<usize> =
            args.iter().enumerate().filter(|(_, a)| *a == "-loop").map(|(i, _)| i).collect();
        assert_eq!(loops.len(), 1);
        let l = loops[0];
        assert!(args[..l].contains(&"F:/seg.mp4".to_string()), "source input first");
        assert_eq!(args[l + 1], "1");
        assert_eq!(args[l + 2], "-t");
        assert_eq!(args[l + 3], "1.000");
        assert_eq!(args[l + 4], "-i");
        assert_eq!(args[l + 5], img.display().to_string());
        // The OUTPUT -t (after both inputs) covers intro + clip.
        let last_i = args.iter().rposition(|a| a == "-i").unwrap();
        let t = last_i + args[last_i..].iter().position(|a| a == "-t").unwrap();
        assert_eq!(args[t + 1], "8.500", "output bound = intro + clip: {args:?}");
        // Audio comes from the wrapped graph, never the raw source.
        assert!(args.contains(&"[aout]".to_string()));
        assert!(!args.contains(&"0:a:0".to_string()));
        // And WITHOUT an intro the args stay byte-identical to today (the
        // headless/batch pin: None means untouched).
        let plain = export_args(Path::new("F:/seg.mp4"), 2.0, 7.5, "FG", "o.mp4", None, &[]);
        assert!(!plain.contains(&"-loop".to_string()));
        assert!(plain.contains(&"0:a:0".to_string()));
        let t = plain.iter().position(|a| a == "-t").unwrap();
        assert_eq!(plain[t + 1], "7.500");
    }

    // ---- music track (ADR 0068) ---------------------------------------------

    fn music(at_s: f64, in_s: f64, out_s: f64, gain: f32) -> yc_core::MusicClip {
        yc_core::MusicClip {
            path: std::path::PathBuf::from("F:/music/bed.mp3"),
            at_s,
            in_s,
            out_s,
            gain,
        }
    }

    #[test]
    fn empty_music_leaves_the_graph_byte_identical() {
        let layout = Layout::FullFrame { crop: Crop { x: 0.0, y: 0.0, w: 608.0, h: 1080.0 } };
        let base = build_filtergraph(&layout, "clip.ass");
        assert_eq!(mix_music(&base, &[], 1), base, "music: [] must change nothing");
    }

    #[test]
    fn music_wraps_the_raw_audio_graph_with_one_amix() {
        // The plain path (no razor, no intro): the graph has no [aout], so the
        // main audio joins raw from the seeked source. One amix, after the one
        // burn; adelay in whole ms; trim + gain per clip; normalize=0.
        let layout = Layout::FullFrame { crop: Crop { x: 0.0, y: 0.0, w: 608.0, h: 1080.0 } };
        let base = build_filtergraph(&layout, "clip.ass");
        let g = mix_music(&base, &[music(12.5, 3.0, 48.0, 0.8)], 1);
        assert!(g.starts_with(&base), "the finished graph survives byte-for-byte: {g}");
        assert_eq!(g.matches("subtitles=").count(), 1, "ASS burns once: {g}");
        assert!(g.contains("[0:a]aformat="), "raw main audio joins the mix: {g}");
        assert!(
            g.contains("[1:a]atrim=start=3:end=48,asetpts=PTS-STARTPTS,volume=0.8,"),
            "graph: {g}"
        );
        assert!(g.contains("adelay=12500:all=1[m0]"), "at_s positions in ms: {g}");
        assert!(
            g.contains("[mbase][m0]amix=inputs=2:duration=first:normalize=0[aout]"),
            "graph: {g}"
        );
        // Every branch is format-pinned (main + one clip).
        assert_eq!(g.matches("aformat=sample_fmts=fltp").count(), 2, "{g}");
    }

    #[test]
    fn music_mixes_after_the_intro_wrap_with_shifted_inputs() {
        // Intro + music in ONE graph: the amix wraps AFTER the intro concat
        // (renaming ITS terminal [aout]), and the music inputs shift to base 2
        // (the image is input 1).
        let layout = Layout::FullFrame { crop: Crop { x: 0.0, y: 0.0, w: 608.0, h: 1080.0 } };
        let base = prepend_intro(&build_filtergraph(&layout, "clip.ass"), 1.0, 30.0, false);
        let clips = [music(0.5, 0.0, 30.0, 1.0), music(45.0, 10.0, 20.0, 1.5)];
        let g = mix_music(&base, &clips, 2);
        assert_eq!(g.matches("subtitles=").count(), 1, "ASS burns once: {g}");
        // The intro concat now feeds the mix, not the output.
        assert!(
            g.contains("[thumbv][thumba][mainv][mainaf]concat=n=2:v=1:a=1[out][premix]"),
            "graph: {g}"
        );
        assert!(g.contains("[premix]aformat="), "the wrapped audio joins the mix: {g}");
        assert!(g.contains("[2:a]atrim=start=0:end=30,"), "input base shifts past the intro: {g}");
        assert!(g.contains("[3:a]atrim=start=10:end=20,"), "clips ride in order: {g}");
        assert!(g.contains("adelay=500:all=1[m0]"), "{g}");
        assert!(g.contains("adelay=45000:all=1[m1]"), "{g}");
        assert!(g.contains("volume=1.5,"), "{g}");
        assert!(
            g.contains("[mbase][m0][m1]amix=inputs=3:duration=first:normalize=0[aout]"),
            "one amix over all clips: {g}"
        );
        assert_eq!(g.matches("amix").count(), 1, "ONE amix: {g}");
    }

    #[test]
    fn music_composes_with_the_razor_camera_graph() {
        // Razor + camera + music: the per-piece a/v pairing stays, the razor's
        // [aout] feeds the mix, and the raw source audio never bypasses it.
        use yc_core::Shot;
        let solo = |x: f32| Layout::FullFrame { crop: Crop { x, y: 0.0, w: 608.0, h: 1080.0 } };
        let plan = CameraPlan {
            shots: vec![
                Shot { start_s: 0.0, end_s: 3.0, track: None, layout: solo(100.0), pan_to: None },
                Shot { start_s: 6.0, end_s: 10.0, track: None, layout: solo(100.0), pan_to: None },
            ],
        };
        let base = build_camera_filtergraph(&plan, "clip.ass", true);
        let g = mix_music(&base, &[music(0.0, 0.0, 7.0, 1.0)], 1);
        assert_eq!(g.matches("atrim=start=").count(), 3, "2 razor pieces + 1 music trim: {g}");
        assert!(g.contains("[s0][a0][s1][a1]concat=n=2:v=1:a=1[cat][premix]"), "graph: {g}");
        assert!(g.contains("[premix]aformat="), "the razor audio joins the mix: {g}");
        assert!(!g.contains("[0:a]aformat="), "raw audio must not bypass the razor: {g}");
        assert!(g.contains("amix=inputs=2:duration=first:normalize=0[aout]"), "graph: {g}");
    }

    #[test]
    fn music_args_add_inputs_after_the_intro_and_map_aout() {
        let beds = [
            std::path::PathBuf::from("F:/music/a.mp3"),
            std::path::PathBuf::from("F:/music/b.flac"),
        ];
        // Without an intro the music files are inputs 1.. and the audio maps
        // from the graph (the amix's [aout]), never the raw source.
        let args = export_args(Path::new("F:/seg.mp4"), 2.0, 7.5, "FG", "o.mp4", None, &beds);
        let inputs: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(i, _)| i.checked_sub(1).is_some_and(|p| args[p] == "-i"))
            .map(|(_, a)| a)
            .collect();
        assert_eq!(inputs, ["F:/seg.mp4", "F:/music/a.mp3", "F:/music/b.flac"]);
        assert!(args.contains(&"[aout]".to_string()));
        assert!(!args.contains(&"0:a:0".to_string()), "music always maps the graph audio");
        // The output -t stays the clip bound — music never extends a Short.
        let t = args.iter().position(|a| a == "-t").unwrap();
        assert_eq!(args[t + 1], "7.500");
        // With an intro the image stays input 1 and music follows it — the
        // mix_music input_base=2 addressing.
        let img = Path::new("F:/covers/thumb.png");
        let args = export_args(
            Path::new("F:/seg.mp4"), 2.0, 7.5, "FG", "o.mp4", Some((img, 1.0)), &beds,
        );
        let inputs: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(i, _)| i.checked_sub(1).is_some_and(|p| args[p] == "-i"))
            .map(|(_, a)| a)
            .collect();
        assert_eq!(
            inputs,
            ["F:/seg.mp4", "F:/covers/thumb.png", "F:/music/a.mp3", "F:/music/b.flac"]
        );
    }
}
