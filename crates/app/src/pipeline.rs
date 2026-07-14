//! The M2 pipeline worker: one background thread (ADR 0005) running the
//! two-phase ingest off the UI thread, reporting progress over a channel.
//!
//! - **Import** resolves a VOD (YouTube URL or local file) to its whole-VOD
//!   analysis audio + raw chat + metadata, and persists `project.json` in the
//!   per-VOD workspace folder. It leaves a [`Session`] the worker remembers.
//! - **Prepare** (ADR 0012) fetches the padded Segment, probes it for the
//!   in-segment offset and layout dimensions, auto-detects the seed Layout
//!   (Ultraface locates the Facecam; `build_layout` picks stacked / full-cam /
//!   full-frame, M6/ADR 0011), and extracts a handful of preview frames. It
//!   leaves a [`PreparedClip`] the worker holds for the matching Render.
//! - **Render** composites the operator's (possibly nudged in the editor) Layout
//!   over the prepared Segment: transcribe the range (once, then cached on the
//!   PreparedClip so re-renders are NVENC-only), generate captions, NVENC export.
//!
//! Splitting promote in two is what lets the nudge editor interject between the
//! auto-detected Layout and the render (ADR 0012); whisper is deferred to Render
//! so Prepare stays CPU/network and the editor opens fast.
//!
//! A [`CancelToken`] (shared with the UI) kills the yt-dlp/ffmpeg child tree
//! mid-download - the M1 "hang" scar.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result};
use yc_core::{
    CameraPlan, CaptionEngine, CaptionGenre, CaptionPlacement, CaptionStyle, Clip, Creator,
    CreatorStore, Language, Layout, LayoutPref, Moment, NoConsole, Project, ReviewCache, Signals,
    ThumbnailIntro, TimeRange, Transcript, Vod, VodSource,
};
use yc_detect::DetectParams;
use yc_frame::speaker::SpeakerAnalysis;
use yc_ingest::{CancelToken, Sidecars};

/// Resolved inputs the worker needs, captured once at spawn.
#[derive(Clone)]
pub struct PipelinePaths {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub ytdlp: PathBuf,
    pub deno_dir: Option<PathBuf>,
    pub model: PathBuf,
    /// CPU speech-emotion model for the arousal Signal (ADR 0008). May be
    /// absent: detection runs without it (combined_score renormalizes). Only
    /// read by the `ser`-gated refine pass.
    #[cfg_attr(not(feature = "ser"), allow(dead_code))]
    pub ser_model: PathBuf,
    /// LLM judgment GGUF for the `llm` Signal (ADR 0010). With `llm_judge` present
    /// the app runs the out-of-process judge over it; if either is missing the
    /// signal is omitted (combined_score renormalizes), like a missing SER model.
    pub llm_model: PathBuf,
    /// The `yc-llm-judge` sidecar binary (it links llama; the app does not).
    /// Absent where it was not built/shipped - detection then omits the llm signal.
    pub llm_judge: PathBuf,
    /// Ultraface face model for M6 auto-framing (ADR 0011). May be absent:
    /// framing falls back to full-frame gameplay (combined with a non-`face`
    /// build). Only read by the `face`-gated auto-frame pass.
    #[cfg_attr(not(feature = "face"), allow(dead_code))]
    pub face_model: PathBuf,
    /// CAM++ speaker-embedding model for the voice lane (ADR 0042). May be
    /// absent: the speaker analysis is then mouth-only (exactly the
    /// pre-integration behavior) and the Camera panel says so. Only read by
    /// the `face`-gated speaker analysis.
    #[cfg_attr(not(feature = "face"), allow(dead_code))]
    pub voice_model: PathBuf,
    /// YuNet landmarks + SFace embedding models for the occupant map (ADR
    /// 0044) — the person evidence the voice join is scoped by in the
    /// attribution regime. Either absent: the join runs seat-scoped (the ADR
    /// 0042 shipped behavior, off-screen flag off) and the Camera panel says
    /// so. Only read by the `face`-gated speaker analysis.
    #[cfg_attr(not(feature = "face"), allow(dead_code))]
    pub yunet_model: PathBuf,
    #[cfg_attr(not(feature = "face"), allow(dead_code))]
    pub sface_model: PathBuf,
    /// AudioSet event tagger + its class-label CSV for the shared-reaction
    /// mask (ADR 0045/0046). Either absent: no reaction mask (the split
    /// grammar simply never sees laughter fuel) and the Camera panel says so.
    /// Only read by the `face`-gated speaker analysis, attribution regime.
    #[cfg_attr(not(feature = "face"), allow(dead_code))]
    pub tag_model: PathBuf,
    #[cfg_attr(not(feature = "face"), allow(dead_code))]
    pub tag_labels: PathBuf,
    /// htdemucs vocals model for the Vocal-stem captions (`sep`). May be absent:
    /// the export then captions the mixed analysis audio. Only read by the
    /// `sep`-gated caption pass.
    #[cfg_attr(not(feature = "sep"), allow(dead_code))]
    pub sep_model: PathBuf,
    /// The bundled `deep-filter` DeepFilterNet sidecar for Cleaned-voice captions
    /// (`enh`, ADR 0029): a gentle denoise of the caption audio before whisper, so
    /// the streamer's voice reads above game SFX. May be absent: the export then
    /// captions the mixed analysis audio. Only read by the `enh`-gated caption pass.
    #[cfg_attr(not(feature = "enh"), allow(dead_code))]
    pub deep_filter: PathBuf,
    /// llama.cpp's multimodal CLI + the Qwen3-ASR GGUF pair for the ensemble
    /// Caption engine (ADR 0034). May be absent: an ensemble-engine render then
    /// falls back to the whisper transcript it already holds.
    pub mtmd_cli: PathBuf,
    pub qwen_model: PathBuf,
    pub qwen_mmproj: PathBuf,
    /// wav2vec2-CTC forced-alignment model dir (`models/w2v2-align-id`, ADR
    /// 0054) — the ensemble's DEFAULT timing skeleton since ADR 0055
    /// (`YC_FORCED_ALIGN=0` is the off-switch). May be absent: the ensemble
    /// then fuses on the whisper DTW skeleton as before (the aligner is
    /// double-gated on the `align` feature + the model being fetched).
    pub align_model: PathBuf,
    /// Directory of per-language dialect/slang correction stores (`<lang>.json`,
    /// see `yc_transcribe::DialectLexicon`). Primes whisper + patches known
    /// mishears; a missing file just disables the fix-ups for that language.
    pub dialect_dir: PathBuf,
    pub font: PathBuf,
    pub workspace: PathBuf,
}

impl PipelinePaths {
    fn sidecars(&self) -> Sidecars {
        Sidecars {
            ytdlp: self.ytdlp.clone(),
            ffmpeg: self.ffmpeg.clone(),
            ffprobe: self.ffprobe.clone(),
            deno_dir: self.deno_dir.clone(),
        }
    }
}

/// Where a VOD is imported from.
pub enum ImportSource {
    YouTube(String),
    Local(PathBuf),
}

/// A unit of work requested by the UI. Cancel is out-of-band (the worker is
/// busy inside a job), so it travels via the [`CancelToken`], not this channel.
pub enum Job {
    /// `language: None` = **Auto** (the GUI default / an omitted CLI token): the
    /// import applies this Creator's saved language from `creators.json` (ADR
    /// 0016), falling back to Bahasa Indonesia for an unknown Creator. An explicit
    /// `Some(lang)` (operator picked a language / CLI `en|id|ja`) always wins.
    Import { source: ImportSource, language: Option<Language> },
    /// Run detection over the imported VOD (discover + refine), surfacing
    /// ranked candidate Moments (ADR 0007). Operates on the current session.
    /// `max_dur_s` caps the adaptive Moment window (clamped to the 180 s
    /// YouTube-Shorts ceiling) — the operator's "Max clip length" setting.
    Detect { max_dur_s: f64 },
    /// Phase-2a (ADR 0012): fetch the padded Segment, probe it, choose the seed
    /// Layout, and extract preview frames for the nudge editor. Leaves a
    /// [`PreparedClip`] the worker holds for the matching [`Job::Render`]. `title`
    /// is the promoted Moment's LLM-generated title (ADR 0015), carried to the
    /// render to name the Short; `None` for a manually-marked / headless clip.
    /// `layout_pref` is the operator's explicit framing choice (ADR 0017): `Auto`
    /// runs M6 auto-detect, the others force a Layout. `preview` is whether an
    /// editor will open on the result: headless/batch pass `false` and Prepare
    /// skips the filmstrip extraction (ADR 0036) — 120 decoded frames would
    /// otherwise be shipped and dropped per clip.
    Prepare { range: TimeRange, title: Option<String>, layout_pref: LayoutPref, preview: bool },
    /// Transcribe the held [`PreparedClip`]'s range ahead of any render (the
    /// editor's caption pre-pass): runs exactly the render path's transcription
    /// (engine resolution, ensemble, correction, harvest, timing refine) and
    /// caches the result on the PreparedClip, emitting [`Progress::Captions`]
    /// so the editor's transcript panel fills. The following Render reuses the
    /// cache and is NVENC-only — the whisper cost just moves earlier.
    Transcribe { correct: bool, caption_engine: Option<CaptionEngine> },
    /// Analyze the held [`PreparedClip`] for podcast speakers (focus 2026-07):
    /// track every visible face, measure per-face mouth activity against the
    /// clip audio, attribute a speaker per time bin, and derive the cut-based
    /// active-speaker [`CameraPlan`]. Emits [`Progress::Speakers`]. Needs the
    /// `face` build + model; CPU-only (safe alongside nothing — the worker is
    /// serial anyway).
    AnalyzeSpeakers,
    /// Phase-2b (ADR 0012): render the operator's (possibly nudged) `layout`
    /// over the held [`PreparedClip`] - transcribe (once, then cached), caption,
    /// NVENC export. `style` is the full Caption Style (genre + appearance —
    /// the editor's preset/custom pick; the genre is what persists per
    /// Creator). `correct` requests the LLM caption-correction pass (ADR
    /// 0030/0031) for this render — the operator's per-render toggle (only
    /// effective in a `correct` build with the sidecar; `YC_CORRECT=0` is a
    /// global override). `placement` is the Clip's Caption placement (ADR
    /// 0036): where/how large the captions draw, from the editor's
    /// drag/resize; `None` (always in headless) keeps the built-in anchor,
    /// byte-identical to pre-placement output. `caption_engine` is the
    /// operator's Caption engine pick for this render (ADR 0035): `Some` = the
    /// GUI's explicit selection (persisted per Creator on success), `None`
    /// (headless/CLI) = the Creator's saved engine from `creators.json`,
    /// Whisper for an unknown Creator. `YC_QWEN_ENS` overrides the resolved
    /// engine either way (tri-state; never saved back). `camera` is a dynamic
    /// active-speaker plan: `Some` renders the per-shot cut concat instead of
    /// the static `layout`. `transcript_override` is the operator's edited
    /// transcript from the editor's caption panel: it becomes the render's
    /// truth verbatim (no whisper, no harvest, no re-timing — the operator's
    /// words are not guesses to second-guess). `keep` is the timeline razor's
    /// kept spans (ADR 0065, clip-relative, sorted): `Some` drops everything
    /// between them from the export — video, audio, and captions together —
    /// `None` renders the whole clip. `manual_captions` is the operator's own
    /// caption stream (ADR 0065): burned as separate simultaneous events
    /// above the auto captions, razor-remapped like them. `intro` is the
    /// thumbnail intro (ADR 0067): `Some` concat-prepends the image AHEAD of
    /// the finished stream, after its ASS burn — captions/camera/razor stay
    /// source-relative by construction; `None` (always in headless/batch)
    /// renders byte-identically to pre-intro output. `music` is the Music
    /// track (ADR 0068): the finished graph — intro included — is wrapped
    /// with one amix positioning each clip on the export's output clock
    /// (`at_s` arrives already razor-compressed from the editor); empty
    /// (always in headless/batch) renders byte-identically to pre-music
    /// output. `fade` is the edge fades (ADR 0070): the OUTERMOST wrap —
    /// after the amix, so one envelope dims the video and kills the final
    /// mixed audio (voice + music) together at the Short's edges; `None`
    /// (always in headless/batch) renders byte-identically to pre-fade
    /// output.
    Render {
        layout: Layout,
        style: CaptionStyle,
        correct: bool,
        placement: Option<CaptionPlacement>,
        caption_engine: Option<CaptionEngine>,
        camera: Option<CameraPlan>,
        transcript_override: Option<Transcript>,
        keep: Option<Vec<TimeRange>>,
        manual_captions: Vec<yc_core::ManualCaption>,
        intro: Option<ThumbnailIntro>,
        music: Vec<yc_core::MusicClip>,
        fade: Option<yc_core::FadeSpec>,
    },
    /// Fetch missing dependencies from their pinned official sources (ADR
    /// 0041): stream to a `.part` beside the destination, verify the pinned
    /// SHA-256, then atomically rename (or unzip) into place. Runs on this
    /// same serial worker — a download and a GPU job never race, and the
    /// existing [`CancelToken`] aborts mid-stream. Needs no session.
    Download { specs: Vec<DownloadSpec> },
}

/// One fetchable dependency artifact (ADR 0041): a pinned, version-stable URL,
/// the SHA-256 the download must hash to, and how it installs. A single spec
/// can satisfy several Diagnostics rows (the ffmpeg zip carries ffprobe too);
/// the registry in `main.rs` maps rows to spec ids.
#[derive(Debug, Clone)]
pub struct DownloadSpec {
    /// Stable key the registry rows reference.
    pub id: &'static str,
    /// Human label for progress ("whisper large-v3 model").
    pub label: &'static str,
    pub url: &'static str,
    /// Lowercase-hex SHA-256 of the artifact the URL serves. Verified before
    /// anything is installed; a mismatch (tampering, or an upstream that moved
    /// under a stale pin) fails loudly and leaves nothing behind.
    pub sha256: &'static str,
    /// Pinned size, bytes — drives the progress fraction (a redirect chain
    /// does not always carry Content-Length).
    pub total_bytes: u64,
    pub install: Install,
}

/// How a verified download lands on disk.
#[derive(Debug, Clone)]
pub enum Install {
    /// The artifact *is* the file: rename the verified `.part` to this path.
    File(PathBuf),
    /// The artifact is a zip: extract entries whose (slash-normalized) name
    /// ends with each pick's suffix to the paired path, plus optionally every
    /// `*.dll` flat into a directory (the llama.cpp runtime layout).
    Unzip { picks: Vec<(&'static str, PathBuf)>, dll_sweep_to: Option<PathBuf> },
}

/// Whole-VOD signal series for the review waveform, one value per `bin_s` bin
/// (raw RMS loudness and, when present, viewer-message counts). The UI
/// normalizes and downsamples to pixel width when painting.
pub struct Timeline {
    pub bin_s: f64,
    pub loudness: Vec<f32>,
    pub chat: Option<Vec<f32>>,
}

/// Progress reported back to the UI thread.
pub enum Progress {
    Stage(&'static str),
    /// A dependency download's progress (ADR 0041): `frac` is 0..=1 of the
    /// spec's pinned size. Drives the status bar's progress bar.
    Download { label: &'static str, frac: f32 },
    /// Import finished; the VOD is ready to detect / promote ranges from.
    /// `analysis_wav` lets the review UI play a Moment's audio range.
    /// `caption_genre` is this Creator's remembered Caption Style (ADR 0016), if
    /// known, so the UI seeds its caption-style picker to the operator's usual
    /// choice for this streamer; `None` for an unknown/new Creator. `moments` are
    /// the Moments persisted from a prior session (M8), so a re-import restores the
    /// review list without re-detecting (empty for a never-detected VOD), and
    /// `transcripts`/`llm_reasons` restore the review panel text from `review.json`.
    Imported {
        title: String,
        duration_s: Option<f64>,
        /// The language this import resolved to — the operator's explicit pick, or
        /// (on Auto) the Creator's saved default (ADR 0016) — so the UI can show
        /// what the transcription will actually use.
        language: Language,
        analysis_wav: PathBuf,
        caption_genre: Option<CaptionGenre>,
        /// This Creator's saved Caption engine (ADR 0035), so the UI seeds its
        /// engine picker and can warn when the operator flips it. `None` for an
        /// unknown/new Creator — the picker then resets to Whisper (no default
        /// flip; a previous session's ensemble pick must not leak onto a new
        /// Creator).
        caption_engine: Option<CaptionEngine>,
        moments: Vec<Moment>,
        transcripts: HashMap<u64, String>,
        llm_reasons: HashMap<u64, String>,
        /// The per-Creator dialect store path (`workspace/<creator>/<lang>.json`, ADR
        /// 0031) and the VOD's `video_id` (`None` for a local file), so the
        /// review-queue panel (ADR 0032) can load + curate this Creator's caption
        /// to-dos and deep-link each garble to its moment in the source VOD.
        creator_store: PathBuf,
        video_id: Option<String>,
        /// The per-clip dialect stores in the stream folder (`<ClipStem>.<lang>.json`,
        /// ADR 0031), so the review queue can also surface a fresh render's harvested
        /// to-dos, not just the per-Creator backlog (ADR 0032).
        clip_stores: Vec<PathBuf>,
    },
    /// Detection finished; ranked candidate Moments, each one's transcript text
    /// (keyed by Moment id), the LLM judgment Signal's one-line reason per Moment
    /// id (`llm_reasons`, empty unless built `--features llm` with the GGUF
    /// present; ADR 0010), and the whole-VOD signal timeline for the waveform.
    Detected {
        moments: Vec<Moment>,
        transcripts: HashMap<u64, String>,
        llm_reasons: HashMap<u64, String>,
        timeline: Timeline,
    },
    /// Prepare finished (ADR 0012): the auto-detected seed Layout, the Segment's
    /// source dimensions, and the preview filmstrip (raw rgb24 frames, each
    /// `frame_w` x `frame_h`, sampled at `frame_fps` across the clip range — ADR
    /// 0036's playback strip; frame i sits at `i / frame_fps` seconds). The UI
    /// uploads the frames to textures and opens the nudge editor seeded with
    /// `layout`; headless echoes `layout` straight back as a `Render` (no nudging).
    Prepared {
        layout: Layout,
        src_w: f32,
        src_h: f32,
        frames: Vec<Vec<u8>>,
        frame_w: u32,
        frame_h: u32,
        frame_fps: f64,
        range: TimeRange,
        /// The persistent face clusters the auto-framing found (M6) — the
        /// editor uses the count to decide whether to auto-run the podcast
        /// speaker analysis (2+ visible people ≈ a podcast frame).
        faces: Vec<yc_frame::FaceCluster>,
        /// The resolved render source + its in-source seek offset, so the
        /// editor's live playback (streaming ffmpeg decode) reads the same
        /// media the render will.
        render_src: PathBuf,
        seek_s: f64,
        /// Probed source frame rate (0 = unknown): the live preview decodes on
        /// the source's own grid so the camera crop can bind to the exact
        /// frame on screen — a hardcoded rate re-quantizes cuts (the preview's
        /// "blank at a cut").
        src_fps: f64,
    },
    /// The podcast speaker analysis + the derived active-speaker camera plan
    /// (focus 2026-07): tracks, per-bin attribution (fused with the voice
    /// lane when its model is present — ADR 0042), and cut-based shots for
    /// the editor's overlays, speaker timeline, and Active Speaker mode.
    /// `voice_note` says why the voice lane is off when `analysis.voice` is
    /// `None` (missing model, too little speech, a broken session) — the
    /// Camera panel's status line.
    Speakers { analysis: SpeakerAnalysis, plan: CameraPlan, voice_note: Option<String> },
    /// The refined transcript a Render is about to burn (post-correction,
    /// post-refine — exactly what `generate_ass` consumes), sent as soon as it is
    /// known so the editor's caption preview shows the render's truth while NVENC
    /// still runs (ADR 0036). Emitted on every Render, cached or not.
    Captions { transcript: Transcript },
    /// A Clip rendered to this path.
    Done(PathBuf),
    /// A non-render job (Transcribe / AnalyzeSpeakers) finished: the UI
    /// returns to Idle without a Done path.
    JobDone,
    Cancelled,
    Failed(String),
}

/// What an import leaves ready for promotion. The worker holds one between jobs.
///
/// Output organization (ADR 0015): each VOD owns a **stream folder**
/// `workspace/<creator>/<stream-title>/` (sanitized from VOD metadata, realizing
/// Creator scoping without a `creators.json` defaults store yet). Rendered Shorts
/// land at its root; every intermediate (analysis.wav, audio.m4a, segment*,
/// chat, project.json, clip.ass, font) lives under its `data/` subfolder.
struct Session {
    vod: Vod,
    promote: PromoteSource,
    /// `workspace/<creator>/<stream-title>/` — the rendered Shorts go here.
    stream_dir: PathBuf,
    /// `<stream_dir>/data/` — analysis audio, segments, project.json, captions.
    data_dir: PathBuf,
    analysis_wav: PathBuf,
    /// Moments persisted from a prior session (M8), surfaced to the review UI on
    /// import so a re-import doesn't lose detection work. Empty for a never-detected
    /// VOD. Read once (for `Progress::Imported`); a later Detect supersedes them.
    moments: Vec<Moment>,
}

/// How `Prepare` obtains the video to render from, and where it seeks.
enum PromoteSource {
    /// Fetch a padded Segment per-promote (section-seekable format, ADR 0059);
    /// seek the offset.
    YouTube(String),
    /// Use the local file directly as the "Segment"; seek the range start.
    Local(PathBuf),
}

/// What `Prepare` leaves ready for the nudge editor and the matching `Render`
/// (ADR 0012): the resolved render source + in-segment seek + source
/// dimensions, the picked range, the auto-detected seed Layout, and - once the
/// first Render has run - the cached transcript, so re-renders skip whisper and
/// are NVENC-only. The worker holds one until the next Prepare or Import.
struct PreparedClip {
    render_src: PathBuf,
    seek_s: f64,
    src_w: f32,
    src_h: f32,
    /// Probed source frame rate (`r_frame_rate`; 0 = unknown) — the editor's
    /// live preview decodes on this grid (see `Progress::Prepared::src_fps`).
    src_fps: f64,
    range: TimeRange,
    auto_layout: Layout,
    transcript: Option<Transcript>,
    /// Whether the cached `transcript` was REQUESTED from the ensemble path
    /// (ADR 0035). The transcript is engine-derived, so flipping the Caption
    /// engine between re-renders of the same Prepare must invalidate the cache
    /// and re-transcribe — unlike genre/placement, which only re-emit ASS.
    transcript_ens: bool,
    /// Whether the cached `transcript` is the OPERATOR's edited truth (the
    /// editor's caption panel). Operator words are never invalidated by an
    /// engine flip and never re-timed/harvested — they are not whisper guesses.
    transcript_operator: bool,
    /// The promoted Moment's LLM-generated title (ADR 0015), used to name the
    /// rendered Short. `None` for a manual / headless clip → render falls back to
    /// a timestamp name. Held here so a re-render after a nudge keeps the name.
    title: Option<String>,
}

/// Spawn the worker thread. Returns the job sender, the progress receiver, a
/// [`CancelToken`] the UI flips to kill an in-flight download, and the worker's
/// [`JoinHandle`] — the app's exit hook polls it so a window close can wait
/// (bounded) for the worker's kill paths to reap any child before the process
/// dies (an unwaited close used to orphan a mid-render ffmpeg/NVENC child).
pub fn spawn(
    paths: PipelinePaths,
) -> (Sender<Job>, Receiver<Progress>, CancelToken, thread::JoinHandle<()>) {
    let (tx_job, rx_job) = mpsc::channel::<Job>();
    let (tx_prog, rx_prog) = mpsc::channel::<Progress>();
    let cancel = CancelToken::new();
    let worker_cancel = cancel.clone();
    let worker = thread::spawn(move || {
        // Mutable only for `deno_dir`: a Download can materialize the deno
        // sidecar after startup resolved it absent (ADR 0041).
        let mut paths = paths;
        let mut session: Option<Session> = None;
        // The clip prepared for the nudge editor, held between Prepare and its
        // Render(s) (ADR 0012). Invalidated by a new Import or Prepare.
        let mut prepared: Option<PreparedClip> = None;
        while let Ok(job) = rx_job.recv() {
            // A cancel of the previous job must not bleed into this one - except
            // into the pre-pass jobs queued behind it (Prepare chains Transcribe,
            // then AnalyzeSpeakers): those belong to the editor session the cancel
            // aimed at, so a still-set token flushes them instead of resetting
            // into them. Deliberate jobs (Import/Detect/Prepare/Render) reset.
            if worker_cancel.is_cancelled()
                && matches!(job, Job::Transcribe { .. } | Job::AnalyzeSpeakers)
            {
                let _ = tx_prog.send(Progress::Cancelled);
                continue;
            }
            worker_cancel.reset();
            match job {
                Job::Import { source, language } => {
                    match do_import(&paths, source, language, &worker_cancel, &tx_prog) {
                        Ok(s) => {
                            let (transcripts, llm_reasons) = load_review(&s.data_dir);
                            let _ = tx_prog.send(Progress::Imported {
                                title: s.vod.title.clone(),
                                duration_s: s.vod.duration_s,
                                language: s.vod.language,
                                analysis_wav: s.analysis_wav.clone(),
                                caption_genre: remembered_caption_genre(&paths.workspace, &s.vod),
                                caption_engine: remembered_caption_engine(
                                    &paths.workspace,
                                    &s.vod,
                                ),
                                moments: s.moments.clone(),
                                transcripts,
                                llm_reasons,
                                // Per-Creator store lives beside the stream folder:
                                // workspace/<creator>/<lang>.json (ADR 0031), same path
                                // do_render loads/promotes to.
                                creator_store: s
                                    .stream_dir
                                    .parent()
                                    .unwrap_or(s.stream_dir.as_path())
                                    .join(format!("{}.json", dialect_lang_code(s.vod.language))),
                                video_id: match &s.vod.source {
                                    VodSource::YouTube { video_id } => Some(video_id.clone()),
                                    VodSource::LocalFile { .. } => None,
                                },
                                clip_stores: clip_store_paths(&s.stream_dir, s.vod.language),
                            });
                            session = Some(s);
                            prepared = None; // a new VOD invalidates any prepared clip
                        }
                        Err(e) => {
                            let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                        }
                    }
                }
                Job::Detect { max_dur_s } => match &session {
                    None => {
                        let _ = tx_prog
                            .send(Progress::Failed("import a VOD before detecting".into()));
                    }
                    Some(s) => match do_detect(&paths, s, max_dur_s, &worker_cancel, &tx_prog) {
                        Ok((moments, transcripts, llm_reasons, timeline)) => {
                            let _ = tx_prog.send(Progress::Detected {
                                moments,
                                transcripts,
                                llm_reasons,
                                timeline,
                            });
                        }
                        Err(e) => {
                            let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                        }
                    },
                },
                Job::Prepare { range, title, layout_pref, preview } => match &session {
                    None => {
                        let _ = tx_prog
                            .send(Progress::Failed("import a VOD before making a clip".into()));
                    }
                    Some(s) => match do_prepare(&paths, s, range, title, layout_pref, preview, &worker_cancel, &tx_prog) {
                        Ok((pc, frames, frame_w, frame_h, frame_fps, faces)) => {
                            let _ = tx_prog.send(Progress::Prepared {
                                layout: pc.auto_layout.clone(),
                                src_w: pc.src_w,
                                src_h: pc.src_h,
                                frames,
                                frame_w,
                                frame_h,
                                frame_fps,
                                range: pc.range,
                                faces,
                                render_src: pc.render_src.clone(),
                                seek_s: pc.seek_s,
                                src_fps: pc.src_fps,
                            });
                            prepared = Some(pc);
                        }
                        Err(e) => {
                            let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                        }
                    },
                },
                Job::Transcribe { correct, caption_engine } => match (&session, &mut prepared) {
                    (Some(s), Some(pc)) => {
                        match ensure_transcript(&paths, s, pc, correct, caption_engine, &worker_cancel, &tx_prog) {
                            Ok(_) => {
                                let t = pc.transcript.clone().expect("set by ensure_transcript");
                                let _ = tx_prog.send(Progress::Captions { transcript: t });
                                // The pre-pass leaves the app Idle, not Done —
                                // nothing was exported.
                                let _ = tx_prog.send(Progress::Stage("Captions ready"));
                                let _ = tx_prog.send(Progress::JobDone);
                            }
                            Err(e) => {
                                let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                            }
                        }
                    }
                    _ => {
                        let _ = tx_prog
                            .send(Progress::Failed("prepare a clip before transcribing".into()));
                    }
                },
                Job::AnalyzeSpeakers => match (&session, &prepared) {
                    (Some(s), Some(pc)) => {
                        match do_analyze_speakers(&paths, s, pc, &worker_cancel, &tx_prog) {
                            Ok((analysis, plan, voice_note)) => {
                                let _ =
                                    tx_prog.send(Progress::Speakers { analysis, plan, voice_note });
                                let _ = tx_prog.send(Progress::JobDone);
                            }
                            Err(e) => {
                                let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                            }
                        }
                    }
                    _ => {
                        let _ = tx_prog.send(Progress::Failed(
                            "prepare a clip before analyzing speakers".into(),
                        ));
                    }
                },
                Job::Render { layout, style, correct, placement, caption_engine, camera, transcript_override, keep, manual_captions, intro, music, fade } => match (&session, &mut prepared) {
                    (Some(s), Some(pc)) => {
                        match do_render(&paths, s, pc, layout, style, correct, placement, caption_engine, camera, transcript_override, keep, manual_captions, intro, music, fade, &worker_cancel, &tx_prog) {
                            Ok(out) => {
                                let _ = tx_prog.send(Progress::Done(out));
                            }
                            Err(e) => {
                                let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                            }
                        }
                    }
                    _ => {
                        let _ = tx_prog
                            .send(Progress::Failed("prepare a clip before rendering".into()));
                    }
                },
                Job::Download { specs } => {
                    match do_download(&specs, &worker_cancel, &tx_prog) {
                        Ok(()) => {
                            // A fresh deno sidecar changes the resolved dir the
                            // ingest children get on their PATH — re-resolve now
                            // rather than requiring a restart.
                            if paths.deno_dir.is_none() {
                                if let Some(sidecars) = paths.ytdlp.parent() {
                                    paths.deno_dir = yc_ingest::resolve_deno_dir(sidecars);
                                }
                            }
                            let _ = tx_prog.send(Progress::Stage("Downloads finished"));
                            let _ = tx_prog.send(Progress::JobDone);
                        }
                        Err(e) => {
                            let _ = tx_prog.send(fail_or_cancel(e, &worker_cancel));
                        }
                    }
                }
            }
            // A cancel aims at everything the operator had in flight — the job
            // it interrupted AND whatever was already queued behind it. The
            // measured case (2026-07-03): a Render clicked during a crawling
            // ensemble pre-pass sat in this channel, and the moment the
            // cancelled job returned it would RESET the token above and start
            // a fresh full transcribe+render — right after the operator hit
            // Cancel. Drain what is queued NOW; a job sent after this instant
            // is new intent and proceeds normally.
            if worker_cancel.is_cancelled() {
                while rx_job.try_recv().is_ok() {
                    let _ = tx_prog.send(Progress::Cancelled);
                }
            }
        }
    });
    (tx_job, rx_prog, cancel, worker)
}

/// A job that ended in error reports as `Cancelled` if the token was flipped
/// (the child was tree-killed), else as a genuine `Failed`.
fn fail_or_cancel(e: anyhow::Error, cancel: &CancelToken) -> Progress {
    if cancel.is_cancelled() {
        Progress::Cancelled
    } else {
        Progress::Failed(format!("{e:#}"))
    }
}

// --- import (phase 1) -------------------------------------------------------

fn do_import(
    paths: &PipelinePaths,
    source: ImportSource,
    language: Option<Language>,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<Session> {
    anyhow::ensure!(paths.ffmpeg.is_file(), "ffmpeg sidecar missing - run fetch-sidecars.ps1");
    match source {
        ImportSource::YouTube(url) => import_youtube(paths, url, language, cancel, tx),
        ImportSource::Local(path) => import_local(paths, path, language, cancel, tx),
    }
}

/// The transcription language an import (`Auto`) falls back to when the Creator
/// has no saved default — the operator's primary content language.
const AUTO_LANGUAGE_FALLBACK: Language = Language::Id;

/// Resolve an import's language (ADR 0016): an explicit operator pick wins;
/// `None` (**Auto**) takes the Creator's saved language from `creators.json`
/// (recorded on every render), else [`AUTO_LANGUAGE_FALLBACK`]. Applying the
/// saved language is what makes the Creator store's recorded language *do*
/// something — before this it was written but never read (CONTEXT.md).
fn resolve_language(workspace: &Path, creator: &str, explicit: Option<Language>) -> Language {
    if let Some(l) = explicit {
        return l;
    }
    match CreatorStore::load(&creators_path(workspace)).get(creator).map(|c| c.language) {
        Some(l) => {
            tracing::info!("language: Auto -> {creator}'s saved default {l:?} (ADR 0016)");
            l
        }
        None => AUTO_LANGUAGE_FALLBACK,
    }
}

fn import_youtube(
    paths: &PipelinePaths,
    url: String,
    language: Option<Language>,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<Session> {
    anyhow::ensure!(paths.ytdlp.is_file(), "yt-dlp sidecar missing - run fetch-sidecars.ps1");
    let sc = paths.sidecars();

    let _ = tx.send(Progress::Stage("Fetching metadata"));
    // The Creator's name is only known once the metadata arrives, so fetch with a
    // provisional language, then resolve Auto against the Creator store.
    let mut vod =
        yc_ingest::youtube_metadata(&sc, &url, language.unwrap_or(AUTO_LANGUAGE_FALLBACK), cancel)?;
    vod.language = resolve_language(&paths.workspace, &vod.creator, language);
    let vod = vod;
    anyhow::ensure!(
        matches!(vod.source, VodSource::YouTube { .. }),
        "expected a YouTube VOD"
    );

    // Output organization (ADR 0015): `workspace/<creator>/<stream-title>/`, with
    // intermediates under `data/`. The folder names come from VOD metadata
    // (creator from yt-dlp uploader/channel, title from `title`), sanitized to
    // safe path segments. Keyed by metadata (not the opaque video_id), so a
    // re-import of the same VOD lands in the same folder and reuses cached audio.
    let (stream_dir, data_dir) = stream_dirs(&paths.workspace, &vod);
    fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;

    let _ = tx.send(Progress::Stage("Downloading audio"));
    let analysis_wav = yc_ingest::youtube_fetch_audio(&sc, &url, &data_dir, cancel)?;

    let _ = tx.send(Progress::Stage("Fetching chat"));
    let _chat = yc_ingest::youtube_fetch_chat(&sc, &url, &data_dir, cancel)?;

    let project = save_project(&vod, &data_dir)?;
    Ok(Session {
        vod,
        promote: PromoteSource::YouTube(url),
        stream_dir,
        data_dir,
        analysis_wav,
        moments: project.moments,
    })
}

fn import_local(
    paths: &PipelinePaths,
    path: PathBuf,
    language: Option<Language>,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<Session> {
    // A local file's Creator is the "local" placeholder (ADR 0015), so Auto
    // resolves to the language last rendered for local files, else the fallback.
    let language = resolve_language(&paths.workspace, "local", language);
    // Absolutize: the export runs ffmpeg in the clip folder (so libass resolves
    // clip.ass + the font by relative name), so a relative source path would
    // resolve against the wrong cwd there. YouTube Segments are already absolute.
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().context("resolving current dir")?.join(path)
    };
    anyhow::ensure!(path.is_file(), "video not found: {}", path.display());
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".into());
    // A local file has no Creator: scope it under `workspace/local/<file-stem>/`
    // (ADR 0015), matching the YouTube hierarchy. The fallback creator is "local".
    let vod = Vod {
        creator: "local".into(),
        title: stem,
        source: VodSource::LocalFile { path: path.clone() },
        language,
        duration_s: None,
    };
    let (stream_dir, data_dir) = stream_dirs(&paths.workspace, &vod);
    fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;

    let _ = tx.send(Progress::Stage("Extracting audio"));
    let analysis_wav = data_dir.join("analysis.wav");
    yc_ingest::extract_audio(&paths.ffmpeg, &path, &analysis_wav, cancel)?;

    let project = save_project(&vod, &data_dir)?;
    Ok(Session {
        vod,
        promote: PromoteSource::Local(path),
        stream_dir,
        data_dir,
        analysis_wav,
        moments: project.moments,
    })
}

/// Max chars for a sanitized folder segment (`<creator>` / `<stream-title>`).
/// Bounded well under Windows MAX_PATH so `workspace/<creator>/<title>/data/
/// segment.mp4` and the Short at the stream root both stay short enough.
const DIR_SEGMENT_MAX: usize = 64;
/// Max chars for a generated Short's filename stem — the Shorts-title ceiling
/// (the LLM is asked for <=60; this enforces it after sanitizing).
const TITLE_STEM_MAX: usize = 60;

/// The `(stream_dir, data_dir)` for a VOD under `workspace` (ADR 0015):
/// `workspace/<creator>/<stream-title>/` and its `data/` subfolder, both names
/// sanitized from the VOD's metadata to safe path segments. Pure (no I/O), so the
/// layout is unit-testable; the caller creates the directories.
fn stream_dirs(workspace: &Path, vod: &Vod) -> (PathBuf, PathBuf) {
    let creator = yc_ingest::sanitize_segment(&vod.creator, "unknown", DIR_SEGMENT_MAX);
    let title = yc_ingest::sanitize_segment(&vod.title, "untitled", DIR_SEGMENT_MAX);
    let stream_dir = workspace.join(creator).join(title);
    let data_dir = stream_dir.join("data");
    (stream_dir, data_dir)
}

/// Persist `project.json` at import, **preserving** any Moments/Clips a prior
/// session detected/promoted (M8): a re-import must not wipe detection work. Only
/// the Vod metadata is refreshed. Returns the (possibly pre-existing) Project so
/// the worker can surface its Moments back to the review UI.
fn save_project(vod: &Vod, data_dir: &Path) -> Result<Project> {
    let mut project = load_or_new_project(vod, data_dir);
    project.vod = vod.clone();
    project
        .save(&data_dir.join("project.json"))
        .with_context(|| format!("writing project.json in {}", data_dir.display()))?;
    Ok(project)
}

/// `workspace/creators.json` — the global per-Creator defaults store (ADR 0016).
fn creators_path(workspace: &Path) -> PathBuf {
    workspace.join("creators.json")
}

/// This VOD's Creator's remembered Caption Style genre (ADR 0016), if the store
/// knows this Creator — used to seed the render's caption style on import. `None`
/// for an unknown/new Creator (the operator's current selection then stands, and
/// is saved to the store on the first render).
fn remembered_caption_genre(workspace: &Path, vod: &Vod) -> Option<CaptionGenre> {
    CreatorStore::load(&creators_path(workspace))
        .get(&vod.creator)
        .and_then(|c| c.default_caption_genre)
}

/// This VOD's Creator's saved Caption engine (ADR 0035), if the store knows
/// this Creator. `None` for an unknown/new Creator — callers treat that as
/// Whisper (no default flip): a new Creator's first render must never inherit
/// another Creator's ensemble choice.
fn remembered_caption_engine(workspace: &Path, vod: &Vod) -> Option<CaptionEngine> {
    CreatorStore::load(&creators_path(workspace))
        .get(&vod.creator)
        .map(|c| c.caption_engine)
}

/// Remember what the operator just rendered this Creator with (ADR 0016 /
/// ADR 0035): upsert the Creator record (creating it if new), keeping its
/// language current and recording `genre` + `engine` as the new defaults. The
/// engine recorded is the render's SELECTION (rail pick or the store's own
/// value) — never the `YC_QWEN_ENS` override, which is per-invocation by
/// contract. Best-effort — a store read/write failure logs and never fails the
/// render.
fn remember_creator_render(workspace: &Path, vod: &Vod, genre: CaptionGenre, engine: CaptionEngine) {
    let path = creators_path(workspace);
    // Load-mutate-save on the GLOBAL store: a lossy load here would rewrite the
    // whole file with just this one Creator, silently wiping every other
    // Creator's defaults. Absent file = a fresh workspace (fine); any other
    // read/parse failure = data we must not overwrite, so skip the remember.
    let mut store = match CreatorStore::try_load(&path) {
        Ok(s) => s,
        Err(e) if e.is_not_found() => CreatorStore::default(),
        Err(e) => {
            tracing::warn!(
                "creators.json unreadable ({e}); NOT overwriting it - render defaults not remembered"
            );
            return;
        }
    };
    let mut creator = store
        .get(&vod.creator)
        .cloned()
        .unwrap_or_else(|| Creator::new(vod.creator.clone(), vod.language));
    creator.language = vod.language; // keep the recorded language current
    creator.default_caption_genre = Some(genre);
    creator.caption_engine = engine;
    store.upsert(creator);
    if let Err(e) = store.save(&path) {
        tracing::warn!("creators.json save failed ({e})");
    }
}

/// Load the VOD's persisted project, or start a fresh one if none exists / it is
/// unreadable. Lets detect and promote update `project.json` without clobbering
/// each other's records.
fn load_or_new_project(vod: &Vod, data_dir: &Path) -> Project {
    Project::load(&data_dir.join("project.json")).unwrap_or_else(|_| Project::new(vod.clone()))
}

/// Persist the per-Moment review notes (transcript + LLM reason) as
/// `data/review.json` (M8), so a re-import restores the review panel without
/// re-transcribing. Best-effort — a write failure logs and never fails Detect.
fn save_review(
    transcripts: &HashMap<u64, String>,
    llm_reasons: &HashMap<u64, String>,
    data_dir: &Path,
) {
    let mut cache = ReviewCache::default();
    for (&id, t) in transcripts {
        cache.notes.entry(id).or_default().transcript = t.clone();
    }
    for (&id, r) in llm_reasons {
        cache.notes.entry(id).or_default().llm_reason = r.clone();
    }
    if let Err(e) = cache.save(&data_dir.join("review.json")) {
        tracing::warn!("review.json save failed ({e})");
    }
}

/// Load the persisted review notes as the two `Moment id -> text` maps the review
/// UI consumes (empty when no `review.json` — a re-Detect refills it). Skips empty
/// strings so a Moment with no LLM reason doesn't get a blank entry.
fn load_review(data_dir: &Path) -> (HashMap<u64, String>, HashMap<u64, String>) {
    let cache = ReviewCache::load(&data_dir.join("review.json"));
    let mut transcripts = HashMap::new();
    let mut llm_reasons = HashMap::new();
    for (id, note) in cache.notes {
        if !note.transcript.is_empty() {
            transcripts.insert(id, note.transcript);
        }
        if !note.llm_reason.is_empty() {
            llm_reasons.insert(id, note.llm_reason);
        }
    }
    (transcripts, llm_reasons)
}

// --- detect (M3) ------------------------------------------------------------

/// Detect candidate Moments over the imported VOD (ADR 0007). Discover with the
/// cheap whole-VOD signals (chat-rate + loudness, no whisper), then refine:
/// transcribe each candidate once with a *resident* whisper model, snap its
/// bounds to sentence start/end (ADR 0040), and score the excitement lexicon
/// over the words the final clip actually contains. Returns Moments ranked by
/// final score; persists them to `project.json`.
/// The hard Moment-length ceiling: YouTube Shorts allow up to 3 minutes.
pub const MAX_CLIP_CEILING_S: f64 = 180.0;
/// How far past each side of a candidate's signal window refine transcribes
/// (ADR 0040), so sentence snapping outward and adjacent-sentence context fill
/// have material to work with.
const SNAP_PAD_S: f64 = 15.0;

fn do_detect(
    paths: &PipelinePaths,
    session: &Session,
    max_dur_s: f64,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<(Vec<Moment>, HashMap<u64, String>, HashMap<u64, String>, Timeline)> {
    anyhow::ensure!(paths.model.is_file(), "whisper model missing - run fetch-models.ps1");
    let mut params = DetectParams::default();
    params.max_dur_s = max_dur_s.clamp(params.min_dur_s, MAX_CLIP_CEILING_S);

    // Discover: cheap, whole-VOD, no GPU. Read the bins here (rather than via
    // yc_detect::discover) so the same series feeds both the ranking and the
    // review timeline - no second wav read.
    let _ = tx.send(Progress::Stage("Detecting moments (chat + loudness)"));
    let chat = session.data_dir.join("chat.live_chat.json");
    let loud_bins = yc_detect::loudness::read_rms_bins(&session.analysis_wav, params.bin_s)?;
    let chat_bins = if chat.exists() {
        let offsets = yc_detect::chat::message_offsets(&chat)?;
        Some(yc_detect::score::bin_counts(&offsets, params.bin_s, loud_bins.len()))
    } else {
        None
    };
    let mut moments = yc_detect::rank_moments(&loud_bins, chat_bins.as_deref(), &params);
    let timeline = Timeline { bin_s: params.bin_s, loudness: loud_bins, chat: chat_bins };
    if moments.is_empty() {
        tracing::info!("no moments discovered");
        return Ok((moments, HashMap::new(), HashMap::new(), timeline));
    }

    // Refine: one model load for the whole candidate batch (the resident
    // Transcriber). Each candidate transcribes a padded window, snaps its
    // bounds to sentence start/end (ADR 0040 — no clip opens or closes on a
    // half-spoken thought; word timestamps are heuristic here, which is noise
    // at clip scale), then lexicon-scores the words INSIDE the final bounds.
    // Manual Moments never reach this loop (they are not detect candidates),
    // so their verbatim ranges stay untouched by construction.
    let _ = tx.send(Progress::Stage("Refining moments (whisper, GPU)"));
    // Text-only (no DTW): the lexicon needs words, not word timing, and DTW
    // aborts on sparse music/SFX windows (see Transcriber::load_text_only).
    let lexicon = yc_transcribe::DialectLexicon::load(&paths.dialect_dir, session.vod.language);
    let transcriber = yc_transcribe::Transcriber::load_text_only(&paths.model)?;
    let mut densities = Vec::with_capacity(moments.len());
    let mut texts = Vec::with_capacity(moments.len());
    for m in &mut moments {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let pad = TimeRange {
            start_s: (m.range.start_s - SNAP_PAD_S).max(0.0),
            end_s: m.range.end_s + SNAP_PAD_S, // read_range_samples clamps to the wav
        };
        let samples = yc_ingest::read_range_samples(&session.analysis_wav, pad)?;
        let transcript = transcriber.transcribe(&samples, session.vod.language, &lexicon, {
            let c = cancel.clone();
            move || c.is_cancelled()
        })?;
        // Unit times are window-relative; sentence snapping works in VOD time.
        let abs_units: Vec<yc_core::CaptionUnit> = transcript
            .units
            .iter()
            .map(|u| yc_core::CaptionUnit {
                text: u.text.clone(),
                start_s: u.start_s + pad.start_s,
                end_s: u.end_s + pad.start_s,
            })
            .collect();
        m.range = yc_detect::sentence::sentence_bounds(
            &abs_units,
            session.vod.language,
            m.range,
            params.min_dur_s,
            params.max_dur_s,
        );
        // The lexicon signal, the LLM judge, and the review pane must all read
        // the words the FINAL clip contains — not the padded window's.
        let kept = Transcript {
            language: transcript.language,
            units: abs_units
                .into_iter()
                .filter(|u| u.start_s >= m.range.start_s - 0.05 && u.end_s <= m.range.end_s + 0.05)
                .collect(),
        };
        densities.push(yc_detect::lexicon::density(&kept, session.vod.language));
        texts.push(kept.units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>().join(" "));
    }
    drop(transcriber); // free VRAM before any later GPU stage (M5 LLM)
    // Snapped bounds can, rarely, share an edge sentence between two adjacent
    // top candidates (both extended toward each other). Accepted residual —
    // ADR 0040 — but worth a trace when it happens on a real VOD.
    for i in 0..moments.len() {
        for j in (i + 1)..moments.len() {
            let (a, b) = (&moments[i].range, &moments[j].range);
            if a.start_s < b.end_s && b.start_s < a.end_s {
                tracing::debug!(
                    "snapped Moments {} and {} overlap after sentence growth",
                    moments[i].id,
                    moments[j].id
                );
            }
        }
    }
    yc_detect::lexicon::apply(&mut moments, &densities, &params.weights);

    // Arousal (ADR 0008): a CPU speech-emotion model scores how emotionally
    // *activated* the streamer's voice is, demoting loud-but-flat moments
    // (game explosions, music, cutscenes). Runs after the whisper drop (CPU, no
    // VRAM). Only when built `--features ser` and the model is present;
    // combined_score renormalizes when arousal is absent, so detection still
    // ranks fine without it. A present-but-unloadable model must not fail the
    // detect either (ADR 0063): the whisper refine above is already paid for,
    // so it degrades to the missing-model ranking, exactly like the llm arm
    // below. Only a cancel stops the job.
    #[cfg(feature = "ser")]
    if paths.ser_model.is_file() {
        let _ = tx.send(Progress::Stage("Refining moments (arousal, CPU)"));
        match arousal_refine(&paths.ser_model, &session.analysis_wav, &moments, cancel) {
            Ok(arousals) => yc_detect::arousal::apply(&mut moments, &arousals, &params.weights),
            Err(e) if cancel.is_cancelled() => return Err(e),
            Err(e) => tracing::warn!("arousal refine failed: {e:#}; omitting arousal signal"),
        }
    }

    // LLM judgment (ADR 0010): a local GGUF model reads each candidate's
    // transcript and scores clip-worthiness, instructed to discount scripted
    // game narration (the load-bearing mitigation - ADR 0009's #11 cutscene at
    // arousal 1.107, streamer silent). Runs on the GPU after the whisper `drop`
    // (VRAM free) and after the CPU arousal pass, so the prompt can corroborate
    // against the z-scored chat/loudness/arousal now on each Moment. Only when
    // built `--features llm` with the GGUF present; combined_score renormalizes
    // when the llm signal is absent, so detection still ranks fine without it.
    // LLM judgment (ADR 0010): runs in the separate `yc-llm-judge` process
    // (whisper.cpp and llama.cpp can't co-link - duplicate ggml). We hand the
    // whole candidate batch to it as JSON over stdin and read back one verdict per
    // candidate. The child loads the GGUF after our whisper `drop` freed VRAM,
    // scores, and frees that VRAM on exit - sequential GPU staging across the
    // process boundary. Skipped (signal omitted, combined_score renormalizes) when
    // the judge binary or the GGUF is absent, exactly like a missing SER model.
    let mut llm_reasons_vec: Vec<String> = Vec::new();
    // LLM-generated Shorts titles, aligned to `moments` (ADR 0015). Used to name
    // each rendered Short when the Moment is promoted; carried through the rank
    // reorder below onto `Moment.title`.
    let mut llm_titles_vec: Vec<String> = Vec::new();
    if paths.llm_judge.is_file() && paths.llm_model.is_file() {
        let _ = tx.send(Progress::Stage("Refining moments (LLM judgment, GPU)"));
        let request = yc_detect::llm::JudgeRequest {
            model_path: paths.llm_model.to_string_lossy().into_owned(),
            language: session.vod.language,
            candidates: moments
                .iter()
                .enumerate()
                .map(|(i, m)| yc_detect::llm::JudgeCandidate {
                    transcript: texts[i].clone(),
                    chat_z: m.signals.chat_rate,
                    loudness_z: m.signals.loudness,
                    arousal_z: m.signals.arousal,
                })
                .collect(),
        };
        match run_llm_judge(&paths.llm_judge, &request, cancel) {
            Ok(verdicts) if verdicts.len() == moments.len() => {
                let scores: Vec<f32> = verdicts.iter().map(|v| v.score).collect();
                llm_titles_vec = verdicts.iter().map(|v| v.title.clone()).collect();
                llm_reasons_vec = verdicts.into_iter().map(|v| v.reason).collect();
                yc_detect::llm::apply(&mut moments, &scores, &params.weights);
            }
            Ok(v) => tracing::warn!(
                "llm-judge returned {} verdicts for {} moments; omitting llm signal",
                v.len(),
                moments.len()
            ),
            Err(e) if cancel.is_cancelled() => return Err(e),
            Err(e) => tracing::warn!("llm-judge failed: {e:#}; omitting llm signal"),
        }
    }

    // Rank by final score; carry each Moment's transcript + LLM reason through
    // the reorder, then renumber so ids read as the review rank.
    let mut order: Vec<usize> = (0..moments.len()).collect();
    order.sort_by(|&a, &b| {
        moments[b].score.partial_cmp(&moments[a].score).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut ranked = Vec::with_capacity(moments.len());
    let mut transcripts = HashMap::with_capacity(moments.len());
    let mut llm_reasons = HashMap::new();
    for (rank, &i) in order.iter().enumerate() {
        let mut m = moments[i].clone();
        m.id = (rank + 1) as u64;
        if let Some(t) = llm_titles_vec.get(i).map(|t| t.trim()).filter(|t| !t.is_empty()) {
            m.title = Some(t.to_string());
        }
        transcripts.insert(m.id, texts[i].clone());
        if let Some(r) = llm_reasons_vec.get(i).filter(|r| !r.trim().is_empty()) {
            llm_reasons.insert(m.id, r.clone());
        }
        ranked.push(m);
    }

    let mut project = load_or_new_project(&session.vod, &session.data_dir);
    project.moments = ranked.clone();
    project
        .save(&session.data_dir.join("project.json"))
        .with_context(|| format!("writing project.json in {}", session.data_dir.display()))?;
    // Persist the review notes alongside (M8) so a re-import restores the panel.
    save_review(&transcripts, &llm_reasons, &session.data_dir);
    Ok((ranked, transcripts, llm_reasons, timeline))
}

/// Max-pooled arousal per candidate (ADR 0008): slide the SER window across each
/// Moment's `analysis.wav` range and keep the peak. Fallible as one unit so the
/// caller can treat ANY failure (unloadable model, unreadable wav) as "Signal
/// absent for the whole candidate set" rather than a failed detect (ADR 0063) —
/// a partial set must never reach `arousal::apply`, whose z-score is across all
/// candidates. Cancellation also surfaces as an error; the caller tells the two
/// apart with `cancel.is_cancelled()`, like the llm arm.
#[cfg(feature = "ser")]
fn arousal_refine(
    model: &Path,
    analysis_wav: &Path,
    moments: &[Moment],
    cancel: &CancelToken,
) -> Result<Vec<f32>> {
    let sr = yc_ingest::WHISPER_SR as f64;
    let win = (yc_detect::arousal::WINDOW_S * sr) as usize;
    let hop = (yc_detect::arousal::HOP_S * sr) as usize;
    let mut ser = yc_detect::arousal::Ser::load(model)?;
    let mut arousals = Vec::with_capacity(moments.len());
    for m in moments {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let samples = yc_ingest::read_range_samples(analysis_wav, m.range)?;
        arousals.push(ser.arousal_max(&samples, win, hop)?);
    }
    Ok(arousals)
}

/// Run the out-of-process LLM judge over the whole candidate batch (ADR 0010):
/// write the [`yc_detect::llm::JudgeRequest`] to its stdin, read the verdicts from
/// its stdout. stderr is inherited so llama.cpp's load logs reach the operator's
/// terminal. The child is killed if the detect is cancelled mid-run (releasing its
/// VRAM). The payload is a few KB (well under the pipe buffer) and the response is
/// small, so writing the request fully before reading the reply can't deadlock.
fn run_llm_judge(
    bin: &Path,
    request: &yc_detect::llm::JudgeRequest,
    cancel: &CancelToken,
) -> Result<Vec<yc_detect::llm::JudgeVerdict>> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    let payload = serde_json::to_vec(request).context("serializing llm-judge request")?;
    let mut child = Command::new(bin)
        .no_console()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning {}", bin.display()))?;

    child
        .stdin
        .take()
        .context("llm-judge stdin unavailable")?
        .write_all(&payload)
        .context("writing llm-judge request")?;

    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("cancelled");
        }
        match child.try_wait().context("waiting on llm-judge")? {
            Some(status) => {
                anyhow::ensure!(status.success(), "llm-judge exited with {status}");
                break;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }

    let mut out = String::new();
    child
        .stdout
        .take()
        .context("llm-judge stdout unavailable")?
        .read_to_string(&mut out)
        .context("reading llm-judge output")?;
    serde_json::from_str(&out).context("parsing llm-judge verdicts")
}

/// LLM caption-correction pass (ADR 0030): hand whisper's caption units + the
/// dialect store's curated **context overrides** + the clip's topic to the
/// out-of-process `yc-llm-judge --correct`, then apply its reply — accepting **only**
/// a curated override the model placed in context (`cowok -> cok`), refusing any
/// other change. Garbles/names are left for the harvest -> curate -> dict loop, not
/// guessed: the operator's real-render A/B showed the model guessing at un-curated
/// garbles (the guest "Guntur" became "buntut") and collapsing real repeated
/// reactions made captions *worse*, so this pass now does only the one thing the
/// global dict can't — apply a real-word slang/name override in context. Runs
/// **before** caption timing. Best-effort: a missing sidecar/GGUF, no curated
/// overrides, or a sidecar failure leaves captions uncorrected — never sinks a
/// render. Off by default (`--features correct` + the sidecar; `YC_CORRECT=0`
/// disables at runtime for an A/B), pending operator sign-off.
#[cfg(feature = "correct")]
fn correct_captions(
    paths: &PipelinePaths,
    session: &Session,
    prepared: &PreparedClip,
    transcript: &mut Transcript,
    lexicon: &yc_transcribe::DialectLexicon,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) {
    // Gating is the caller's `correct` bool (GUI checkbox / headless YC_CORRECT);
    // here we only need the sidecar + GGUF present.
    if !paths.llm_judge.is_file() || !paths.llm_model.is_file() {
        tracing::info!("correct: sidecar or GGUF absent; captions left uncorrected");
        return;
    }
    // Topic for the corrector: REAL domain context — the clip's generated title,
    // the Creator's name, and the VOD's title (which names the game / guests /
    // session type). NOT the layered store's `note`: after layering that is the
    // bundled base's meta-description ("Generic Indonesian base store..."), and
    // that context-free topic is what tipped the pancingan 1-of-2 under-apply
    // (ADR 0030 "Scope limits"). Safe to feed a chat model (unlike whisper's
    // initial_prompt, which it never reaches).
    let topic = yc_transcribe::correction_topic(
        prepared.title.as_deref(),
        &session.vod.creator,
        &session.vod.title,
    );
    let ctx = yc_transcribe::CorrectionContext { language: session.vod.language, topic };
    let Some(req) = yc_transcribe::build_correction_request(&transcript.units, lexicon, &ctx)
    else {
        return; // no curated context overrides — nothing for the LLM to apply
    };
    let _ = tx.send(Progress::Stage("Correcting captions (LLM)"));
    match run_llm_correct(&paths.llm_judge, &paths.llm_model, &req.system, &req.user, cancel) {
        Ok(raw) => {
            let stats = yc_transcribe::apply_correction(&mut transcript.units, lexicon, &raw);
            tracing::info!("correct: {}", stats.summary());
        }
        Err(e) if cancel.is_cancelled() => tracing::info!("correct: cancelled ({e:#})"),
        Err(e) => tracing::warn!("correct: failed ({e:#}); captions left uncorrected"),
    }
}

/// Run `yc-llm-judge --correct` over one caption (ADR 0030): write the
/// `{model_path, system, user}` request to its stdin and read back the raw
/// free-form completion (the corrected `N: word` list) from its stdout. Mirrors
/// [`run_llm_judge`] — stderr inherited for the load logs, killed on cancel to free
/// VRAM, request small enough that writing it before reading can't deadlock.
#[cfg(feature = "correct")]
fn run_llm_correct(
    bin: &Path,
    model: &Path,
    system: &str,
    user: &str,
    cancel: &CancelToken,
) -> Result<String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    let payload = serde_json::to_vec(&serde_json::json!({
        "model_path": model.to_string_lossy(),
        "system": system,
        "user": user,
    }))
    .context("serializing correct request")?;
    let mut child = Command::new(bin)
        .arg("--correct")
        .no_console()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning {}", bin.display()))?;

    child
        .stdin
        .take()
        .context("correct stdin unavailable")?
        .write_all(&payload)
        .context("writing correct request")?;

    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("cancelled");
        }
        match child.try_wait().context("waiting on correct")? {
            Some(status) => {
                anyhow::ensure!(status.success(), "yc-llm-judge --correct exited with {status}");
                break;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }

    let mut out = String::new();
    child
        .stdout
        .take()
        .context("correct stdout unavailable")?
        .read_to_string(&mut out)
        .context("reading correct output")?;
    Ok(out)
}

// --- prepare + render (phase 2, split for the nudge editor - ADR 0012) ------

/// The preview filmstrip (ADR 0036, replacing the 7-frame scrub of ADR 0012):
/// dense enough that scrubbing/playback reads as motion, capped so the texture
/// budget stays bounded on the 8 GB card (`STRIP_MAX_FRAMES` frames at ~480p
/// RGBA ≈ 200 MB). A clip longer than `STRIP_MAX_FRAMES / STRIP_FPS` seconds
/// lowers its fps rather than growing the strip.
const STRIP_MAX_FRAMES: usize = 120;
/// Target strip density. Playback interpolates nothing — the playhead shows the
/// nearest frame — so this is the visual "frame rate" of the preview.
const STRIP_FPS: f64 = 4.0;
/// Longest strip-frame edge, in pixels, by clip length: the editor canvas
/// draws at modest size, so ~480p covers a short clip crisply; a long clip
/// (the 180 s Shorts ceiling) drops to ~360p so the resident texture budget on
/// the shared 8 GB card stays roughly constant (the strip is texture-resident
/// through the whole edit + render). Geometry — the thing being edited — is
/// stored in source pixels and unaffected; the render reads the full-res
/// Segment.
fn preview_long_edge(dur_s: f64) -> f32 {
    if dur_s <= 60.0 {
        854.0
    } else {
        640.0
    }
}

/// Aspect-preserving preview-frame dimensions (even, >= 2) with the longest edge
/// at most `long_edge`. Unlike the 320x240 detection pass (ADR 0011, which
/// tolerates aspect distortion), the editor preview must not distort.
fn preview_dims(src_w: f32, src_h: f32, long_edge: f32) -> (u32, u32) {
    let even = |v: f32| (((v.round().max(2.0)) as u32) / 2) * 2;
    if src_w >= src_h {
        let w = src_w.min(long_edge);
        (even(w), even(w * src_h / src_w))
    } else {
        let h = src_h.min(long_edge);
        (even(h * src_w / src_h), even(h))
    }
}

/// Phase-2a (ADR 0012): resolve the render source (fetch the padded Segment for
/// YouTube, or use the local file), probe it for the in-segment seek offset and
/// source resolution, auto-detect the seed Layout (M6/ADR 0011), and sample
/// preview frames across the clip range. CPU/network only - whisper is deferred
/// to `do_render` so the editor opens fast. Returns the [`PreparedClip`] plus
/// the preview frames and their dimensions for the UI to texture.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn do_prepare(
    paths: &PipelinePaths,
    session: &Session,
    range: TimeRange,
    title: Option<String>,
    layout_pref: LayoutPref,
    preview: bool,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<(PreparedClip, Vec<Vec<u8>>, u32, u32, f64, Vec<yc_frame::FaceCluster>)> {
    anyhow::ensure!(range.duration_s() > 0.0, "pick a range with end > start");
    let sc = paths.sidecars();

    // Obtain the render source, the in-segment seek offset, and the source
    // resolution (for the Layout).
    let (render_src, seek_s, src_w, src_h, src_fps) = match &session.promote {
        PromoteSource::Local(path) => {
            let p = yc_ingest::probe_segment(&paths.ffprobe, path, cancel)?;
            (path.clone(), range.start_s, p.width as f32, p.height as f32, p.fps)
        }
        PromoteSource::YouTube(url) => {
            let (segment, offset, p) = resolve_segment(paths, &sc, session, url, range, cancel, tx)?;
            (segment, offset, p.width as f32, p.height as f32, p.fps)
        }
    };

    // Choose the seed Layout: Ultraface locates the Facecam (CPU; no GPU
    // contention), then build_layout applies the operator's Layout preference
    // (ADR 0017) - Auto runs M6's stacked / full-cam / full-frame decision, a
    // forced kind overrides it (still using the detected Facecam when found).
    // The editor opens seeded with this and the operator nudges from there.
    let _ = tx.send(Progress::Stage("Framing (face detect)"));
    let faces = detect_facecam(paths, &render_src, seek_s, src_w, src_h);
    let auto_layout =
        yc_frame::decide_layout_with_pref(layout_pref, &faces, src_w, src_h, yc_frame::SEAM_DEFAULT);

    // Sample the preview filmstrip across the clip range (ADR 0036): STRIP_FPS,
    // dropping to fit STRIP_MAX_FRAMES on a long clip, floored so a degenerate
    // range still yields frames. Headless/batch open no editor (`preview` is
    // false): skip the extraction instead of decoding 120 frames to drop them.
    let (frame_w, frame_h) =
        preview_dims(src_w, src_h, preview_long_edge(range.duration_s()));
    let fps = STRIP_FPS.min(STRIP_MAX_FRAMES as f64 / range.duration_s().max(0.1)).max(0.1);
    let frames = if preview {
        let _ = tx.send(Progress::Stage("Extracting preview frames"));
        yc_ingest::extract_frames_rgb(
            &paths.ffmpeg,
            &render_src,
            seek_s,
            frame_w,
            frame_h,
            fps,
            STRIP_MAX_FRAMES,
        )?
    } else {
        Vec::new()
    };

    let prepared = PreparedClip {
        render_src,
        seek_s,
        src_w,
        src_h,
        src_fps,
        range,
        auto_layout,
        transcript: None,
        transcript_ens: false,
        transcript_operator: false,
        title,
    };
    Ok((prepared, frames, frame_w, frame_h, fps, faces))
}

/// Extra request padding past a measured coverage shortfall on a refetch: the
/// section download snaps to whole stream fragments, so ask for one typical
/// fragment more than the exact miss.
const REFETCH_EXTRA_S: f64 = 8.0;

/// Fetch the padded Segment, then **measure** where it actually sits on the
/// VOD timeline (`yc_ingest::align`) instead of assuming the download honored
/// the requested section start. yt-dlp's HLS section download snaps to stream
/// fragment boundaries on some VODs (measured 5.84 s early on a podcast VOD,
/// exact on others); under the old assumption every seek consumer — the export
/// cut, face detect, speaker-analysis frames, preview, the `enh` caption
/// window — read media shifted by the snap while captions stayed on the true
/// VOD timeline from analysis.wav: a constant caption-vs-audio offset in the
/// export, and mouth-vs-voice misattribution in the speaker pass. The snapped
/// section also *ends* short of the request, so when the measured window
/// leaves the clip's tail uncovered, refetch once with the end widened by the
/// shortfall (+ a fragment allowance). No confident lock (or a measurement
/// error) falls back to the requested-start assumption — today's behavior.
fn resolve_segment(
    paths: &PipelinePaths,
    sc: &yc_ingest::Sidecars,
    session: &Session,
    url: &str,
    range: TimeRange,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<(PathBuf, f64, yc_ingest::SegmentProbe)> {
    let mut padded = yc_ingest::pad_range(range, session.vod.duration_s);
    let mut refetched = false;
    loop {
        let _ = tx.send(Progress::Stage("Fetching segment"));
        let segment = yc_ingest::fetch_segment(sc, url, padded, &session.data_dir, cancel)?;
        let probe = yc_ingest::probe_segment(&paths.ffprobe, &segment, cancel)?;
        let assumed = yc_ingest::in_segment_offset(range.start_s, padded.start_s, &probe);
        let _ = tx.send(Progress::Stage("Verifying segment timing"));
        let anchor = match yc_ingest::measure_segment_anchor(
            &paths.ffmpeg,
            &paths.ffprobe,
            &segment,
            &session.analysis_wav,
            padded.start_s,
            cancel,
        ) {
            Ok(a) => a,
            Err(e) if cancel.is_cancelled() => return Err(e),
            Err(e) => {
                tracing::warn!("segment anchor measurement failed: {e:#}");
                None
            }
        };
        let Some(anchor) = anchor else {
            tracing::warn!("segment anchor: no confident lock; assuming the requested start");
            return Ok((segment, assumed, probe));
        };
        let seek = (range.start_s - anchor.vod_t0_s).max(0.0);
        if (seek - assumed).abs() > 0.25 {
            tracing::warn!(
                assumed,
                measured = seek,
                corr = anchor.corr,
                "segment anchored {:+.2}s off the requested section; seeking the measured offset",
                seek - assumed
            );
        }
        let shortfall = yc_ingest::tail_shortfall_s(anchor.vod_t0_s, probe.duration_s, range.end_s);
        if shortfall > 0.01 && !refetched {
            refetched = true;
            let end = padded.end_s + shortfall + REFETCH_EXTRA_S;
            padded.end_s = match session.vod.duration_s {
                Some(d) => end.min(d),
                None => end,
            };
            tracing::warn!(
                shortfall,
                "segment misses the clip tail; refetching with the request widened to {:.1}s",
                padded.end_s
            );
            continue;
        }
        if shortfall > 0.01 {
            tracing::warn!(shortfall, "segment still misses the clip tail; the export may truncate");
        }
        return Ok((segment, seek, probe));
    }
}

/// The 16 kHz-mono samples whisper captions from. With the `sep` feature and the
/// htdemucs model present, this is the **Vocal stem** of the Segment's range —
/// the streamer's voice split from music/SFX (CONTEXT.md), so whisper reads the
/// voice rather than the loudest sound in the mix. Otherwise it is the mixed
/// analysis-wav range (the historical path). Either way the rendered clip's
/// audible audio stays the mix — only what whisper *hears* changes.
#[cfg_attr(not(any(feature = "sep", feature = "enh")), allow(unused_variables))]
fn caption_samples(
    paths: &PipelinePaths,
    session: &Session,
    prepared: &PreparedClip,
    range: TimeRange,
    tx: &Sender<Progress>,
) -> Result<Vec<f32>> {
    // Cleaned-voice captions (ADR 0029): a gentle DeepFilterNet denoise of the
    // caption audio before whisper, so the streamer's voice reads above game SFX.
    // Measured to recover masked speech while leaving clean clips intact; full
    // denoise, by contrast, over-suppressed quiet speech to silence and whisper
    // hallucinated. Takes precedence over `sep`; the clip's audible audio is the mix.
    #[cfg(feature = "enh")]
    {
        if paths.deep_filter.is_file() {
            let _ = tx.send(Progress::Stage("Cleaning voice"));
            let wd = &session.data_dir;
            let in48 = wd.join("_enh_in48k.wav");
            let out_dir = wd.join("_enh_out");
            let clean48 = out_dir.join("_enh_in48k.wav"); // deep-filter keeps the basename
            let clean16 = wd.join("_enh_clean16k.wav");
            // 48 kHz mono for the clip range, from the render source at its
            // in-segment offset — the same window the mix would caption.
            ffmpeg_extract_mono_48k(
                &paths.ffmpeg,
                &prepared.render_src,
                prepared.seek_s,
                range.duration_s(),
                &in48,
            )?;
            run_deep_filter(&paths.deep_filter, &in48, &out_dir)?;
            // Back to whisper's 16 kHz mono; the temp file spans exactly the range.
            ffmpeg_resample_16k_mono(&paths.ffmpeg, &clean48, &clean16)?;
            return yc_ingest::read_range_samples(
                &clean16,
                TimeRange { start_s: 0.0, end_s: range.duration_s() + 1.0 },
            );
        }
    }
    #[cfg(feature = "sep")]
    {
        if paths.sep_model.is_file() {
            let _ = tx.send(Progress::Stage("Separating vocal stem"));
            let wd = &session.data_dir;
            let in44 = wd.join("_sep_in44k.wav");
            let voc44 = wd.join("_sep_voc44k.wav");
            let voc16 = wd.join("_sep_voc16k.wav");
            // 44.1 kHz stereo for the clip range, from the render source at its
            // in-segment offset — the same window the mix would caption.
            ffmpeg_extract_stereo_44k(
                &paths.ffmpeg,
                &prepared.render_src,
                prepared.seek_s,
                range.duration_s(),
                &in44,
            )?;
            yc_detect::sep::separate_vocals_wav(&paths.sep_model, &in44, &voc44)?;
            // Back to whisper's 16 kHz mono; the temp file spans exactly the range.
            ffmpeg_resample_16k_mono(&paths.ffmpeg, &voc44, &voc16)?;
            return yc_ingest::read_range_samples(
                &voc16,
                TimeRange { start_s: 0.0, end_s: range.duration_s() + 1.0 },
            );
        }
    }
    yc_ingest::read_range_samples(&session.analysis_wav, range)
}

/// Extract `dur_s` of `src` from `seek_s` as a 44.1 kHz **stereo** PCM wav — the
/// htdemucs vocal-sep input. `-ss` before `-i` fast-seeks (the render source is
/// the padded Segment, or the whole local file).
#[cfg(feature = "sep")]
fn ffmpeg_extract_stereo_44k(
    ffmpeg: &Path,
    src: &Path,
    seek_s: f64,
    dur_s: f64,
    out: &Path,
) -> Result<()> {
    let args: Vec<String> = vec![
        "-ss".into(),
        format!("{seek_s:.3}"),
        "-t".into(),
        format!("{dur_s:.3}"),
        "-i".into(),
        src.display().to_string(),
        "-map".into(),
        "0:a:0".into(),
        "-ac".into(),
        "2".into(),
        "-ar".into(),
        "44100".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        "-y".into(),
        out.display().to_string(),
    ];
    let status = std::process::Command::new(ffmpeg)
        .no_console()
        .args(&args)
        .status()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    anyhow::ensure!(status.success(), "ffmpeg vocal-sep extract failed ({status})");
    Ok(())
}

/// Resample a wav to whisper's 16 kHz mono PCM (the cleaned/stem -> caption input).
#[cfg(any(feature = "sep", feature = "enh"))]
fn ffmpeg_resample_16k_mono(ffmpeg: &Path, src: &Path, out: &Path) -> Result<()> {
    let args: Vec<String> = vec![
        "-i".into(),
        src.display().to_string(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        yc_ingest::WHISPER_SR.to_string(),
        "-c:a".into(),
        "pcm_s16le".into(),
        "-y".into(),
        out.display().to_string(),
    ];
    let status = std::process::Command::new(ffmpeg)
        .no_console()
        .args(&args)
        .status()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    anyhow::ensure!(status.success(), "ffmpeg vocal-sep resample failed ({status})");
    Ok(())
}

/// Gentle DeepFilterNet attenuation limit (dB) for Cleaned-voice captions (ADR
/// 0029): mixes ~12 dB of the original back, so quiet speech buried in game noise
/// is denoised but never gated to the dead silence that makes whisper hallucinate
/// repetitions. The full default (100 dB) was *measured* to destroy masked speech
/// (40 real units -> "eh" x211). Tune-from-use, like the caption-timing consts.
#[cfg(feature = "enh")]
const ENH_ATTEN_LIM_DB_DEFAULT: &str = "12";

/// The DeepFilterNet attenuation limit (dB), overridable at runtime via
/// `YC_ENH_ATTEN` for tuning without a rebuild. **Lower = gentler** (less noise
/// suppression, more of the original mixed back). Measured on clip-7: the default
/// `12` is too strong — it pushes whisper into a long "eh" repetition loop on a
/// masked stretch — while `6` recovers the masked speech (e.g. "diam") and the
/// name "Guntur" with no loop. Re-tune against a real render before changing the
/// default (the ADR 0029 lesson: validate on the production Segment audio).
#[cfg(feature = "enh")]
fn enh_atten_lim_db() -> String {
    std::env::var("YC_ENH_ATTEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ENH_ATTEN_LIM_DB_DEFAULT.to_string())
}

/// Extract `dur_s` of `src` from `seek_s` as a 48 kHz **mono** PCM wav — the
/// DeepFilterNet input (the model is full-band 48 kHz). `-ss` before `-i`
/// fast-seeks (the render source is the padded Segment, or the whole local file).
#[cfg(feature = "enh")]
fn ffmpeg_extract_mono_48k(
    ffmpeg: &Path,
    src: &Path,
    seek_s: f64,
    dur_s: f64,
    out: &Path,
) -> Result<()> {
    let args: Vec<String> = vec![
        "-ss".into(),
        format!("{seek_s:.3}"),
        "-t".into(),
        format!("{dur_s:.3}"),
        "-i".into(),
        src.display().to_string(),
        "-map".into(),
        "0:a:0".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        "48000".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        "-y".into(),
        out.display().to_string(),
    ];
    let status = std::process::Command::new(ffmpeg)
        .no_console()
        .args(&args)
        .status()
        .with_context(|| format!("spawning ffmpeg at {}", ffmpeg.display()))?;
    anyhow::ensure!(status.success(), "ffmpeg cleaned-voice extract failed ({status})");
    Ok(())
}

/// Run the bundled `deep-filter` DeepFilterNet sidecar over `in_wav`, writing the
/// cleaned wav into `out_dir` under the same file name. Gentle attenuation limit
/// (`-a`) plus delay compensation (`-D`) so the cleaned audio stays time-aligned
/// with the input for caption timing. The model is baked into the binary (no `-m`).
#[cfg(feature = "enh")]
fn run_deep_filter(deep_filter: &Path, in_wav: &Path, out_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let args: Vec<String> = vec![
        "-a".into(),
        enh_atten_lim_db(),
        "-D".into(),
        "-o".into(),
        out_dir.display().to_string(),
        in_wav.display().to_string(),
    ];
    let status = std::process::Command::new(deep_filter)
        .no_console()
        .args(&args)
        .status()
        .with_context(|| format!("spawning deep-filter at {}", deep_filter.display()))?;
    anyhow::ensure!(status.success(), "deep-filter (cleaned voice) failed ({status})");
    Ok(())
}

/// Peak sample magnitude (|s|, 0..1) below which a promoted clip is treated as
/// having no speech, so transcription is skipped (M8): a near-silent window trips
/// whisper.cpp's DTW assertion and aborts the process. Real speech peaks far above
/// this; only a genuinely silent range (an accidental promote of a quiet gap)
/// falls below. Tune-from-use.
const SILENT_CLIP_PEAK: f32 = 0.01;

/// Whether a clip's caption samples carry effectively no audio (peak below
/// [`SILENT_CLIP_PEAK`]) — the no-speech guard for the whisper DTW abort (M8).
/// Pure, so the threshold is unit-tested without a model.
fn is_silent_clip(samples: &[f32]) -> bool {
    samples.iter().fold(0.0f32, |m, &s| m.max(s.abs())) < SILENT_CLIP_PEAK
}

/// Per-decode ensemble stage labels for the status bar. [`Progress::Stage`]
/// deliberately carries `&'static str` (no allocation on the progress
/// channel), so the labels are a fixed table — the variant set is a
/// compile-time constant of five. Falls back to a countless label if the set
/// ever grows without this table following.
fn ens_stage_label(i: usize, n: usize) -> &'static str {
    const LABELS: [&str; 5] = [
        "Qwen ensemble — decode 1/5",
        "Qwen ensemble — decode 2/5",
        "Qwen ensemble — decode 3/5",
        "Qwen ensemble — decode 4/5",
        "Qwen ensemble — decode 5/5",
    ];
    if n == LABELS.len() {
        if let Some(l) = LABELS.get(i.wrapping_sub(1)) {
            return l;
        }
    }
    "Qwen ensemble — decoding"
}

/// The render path's transcription, shared by [`Job::Render`] and the editor's
/// [`Job::Transcribe`] pre-pass: resolve the Caption engine (ADR 0035),
/// transcribe once (whisper GPU, optional Qwen ensemble, optional LLM
/// correction, dialect harvest, timing refine) and cache the result on the
/// `PreparedClip` — a later call reuses the cache, so a render after the
/// pre-pass is NVENC-only. Returns the resolved engine SELECTION (what
/// `remember_creator_render` persists). An operator-edited transcript
/// (`transcript_operator`) is never invalidated or recomputed here.
fn ensure_transcript(
    paths: &PipelinePaths,
    session: &Session,
    prepared: &mut PreparedClip,
    correct: bool,
    caption_engine: Option<CaptionEngine>,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<CaptionEngine> {
    anyhow::ensure!(paths.model.is_file(), "whisper model missing - run fetch-models.ps1");
    // `correct` gates the LLM caption-correction pass below; it is only read inside
    // the `#[cfg(feature = "correct")]` block, so silence the unused warning otherwise.
    #[cfg(not(feature = "correct"))]
    let _ = correct;
    let range = prepared.range;

    // The Caption engine for this render (ADR 0035): the GUI rail's explicit
    // pick when one was sent, else the Creator's saved engine (headless/CLI),
    // the ensemble for an unknown Creator (the operator's new-Creator ruling,
    // ADR 0061 — existing key-less records still read Whisper; only the seed
    // changed). `YC_QWEN_ENS` then overrides the resolved selection in either
    // direction (tri-state, per-invocation — the gate fixtures pin an engine
    // with it regardless of how a Creator is flipped); only the SELECTION is
    // saved back below, never the override.
    let engine = caption_engine
        .or_else(|| remembered_caption_engine(&paths.workspace, &session.vod))
        .unwrap_or(CaptionEngine::FOR_NEW_CREATORS);
    let use_ensemble = yc_transcribe::ensemble::engine_override()
        .unwrap_or(engine == CaptionEngine::QwenEnsemble);

    // Transcribe once, then reuse: re-rendering a nudged Layout skips whisper.
    // The cache is engine-derived, though — an engine flip between re-renders
    // of the same Prepare must re-transcribe, or the flip would silently no-op.
    // An operator-edited transcript is exempt: their words outrank any engine.
    if prepared.transcript.is_some()
        && !prepared.transcript_operator
        && prepared.transcript_ens != use_ensemble
    {
        tracing::info!(
            "caption engine changed since the cached transcript (ensemble {} -> {}); re-transcribing",
            prepared.transcript_ens,
            use_ensemble
        );
        prepared.transcript = None;
    }
    if prepared.transcript.is_none() {
        // Captions read the Vocal stem when `sep` is built (music/SFX split off
        // the streamer's voice); otherwise the mixed analysis audio, as before.
        let samples = caption_samples(paths, session, prepared, range, tx)?;
        // Guard whisper's DTW process-abort on a token-starved window (M8): a
        // (near-)silent promote feeds whisper almost no speech and trips the
        // whisper.cpp DTW assertion (filter_width < ne[2]), which *aborts the
        // whole process* (uncatchable from Rust; detection dodges it with the
        // no-DTW `load_text_only`, ADR 0007). If the clip has effectively no
        // audio, skip transcription -> empty captions and still render the video.
        // Real speech peaks far above this floor. (The harder music-but-no-speech
        // case wants whisper out-of-process, like the judge; noted, deferred.)
        let transcript = if is_silent_clip(&samples) {
            tracing::warn!(
                "caption: clip below silence floor {SILENT_CLIP_PEAK}; \
                 skipping transcription (no speech)"
            );
            Transcript { language: session.vod.language, units: Vec::new() }
        } else {
            let _ = tx.send(Progress::Stage("Transcribing (whisper, GPU)"));
            let language = session.vod.language;
            // Layered dialect stores (ADR 0031): bundled base < per-Creator < per-clip.
            // The per-Creator store (workspace/<creator>/<lang>.json) carries a
            // streamer's confirmed slang/names across all their VODs; the per-clip
            // store (<stream>/<stem>.<lang>.json, beside the exported Short) gets THIS
            // clip's harvest, so each export has a small, easy-to-curate review queue.
            let lc = dialect_lang_code(language);
            let clip_stem = clip_title_stem(prepared.title.as_deref(), range);
            let creator_store = session
                .stream_dir
                .parent()
                .unwrap_or(session.stream_dir.as_path())
                .join(format!("{lc}.json"));
            let clip_store = session.stream_dir.join(format!("{clip_stem}.{lc}.json"));
            let lexicon = yc_transcribe::DialectLexicon::load_layered(
                &paths.dialect_dir,
                &[creator_store.clone(), clip_store.clone()],
                language,
            );
            let (mut transcript, harvest) = yc_transcribe::transcribe_range_harvesting(
                &paths.model,
                &samples,
                language,
                &lexicon,
                {
                    let c = cancel.clone();
                    move || c.is_cancelled()
                },
            )?;
            // The in-process whisper poll only aborts at its coarse
            // checkpoints — make the stage boundary explicit so a cancel never
            // rolls on into the (long) ensemble below.
            anyhow::ensure!(!cancel.is_cancelled(), "cancelled");
            // Qwen3-ASR ensemble captions (ADR 0034/0035): words from a
            // multi-decode vote; timing from wav2vec2-CTC forced alignment on
            // `align` builds (the ADR 0055 default), falling back to the
            // whisper transcript above (whose one-shot model is already
            // dropped — GPU staging stays sequential). Runs when the resolved
            // Caption engine is the ensemble (Creator's saved engine / rail
            // pick / YC_QWEN_ENS override). Fails soft: whisper captions stand
            // if the sidecar/models are absent or any stage errors. Whisper
            // Creators stay byte-identical (this block never runs; ADR 0033's
            // opt-in contract, now at the Creator level).
            let mut ens_used = false;
            if use_ensemble {
                let _ = tx.send(Progress::Stage("Ensemble captions (Qwen3-ASR)"));
                // Second whisper decode for the DTW TIMING skeleton only: on
                // masked clips the default decode's spans are as wrong as its
                // words (phantom multi-second units, holes over real speech),
                // while suppress_nst places units exactly where the default is
                // blind (ADR 0033's measurement). Its words never enter the
                // vote — they re-garble, which is why the knob stays off for
                // caption TEXT — only its time grid feeds the fusion. Since the
                // forced-alignment default flip (ADR 0055) this skeleton only
                // feeds the FALLBACK fusion, so the decode — a whole GPU pass —
                // runs only when that fallback will actually be the timing
                // source (off-switch set, `align` feature absent, or model not
                // fetched). A RUNTIME aligner failure after we skipped it fuses
                // on the default whisper skeleton alone: rare, degraded,
                // captions never missing. Scoped env write: the knobs are
                // env-based by design (ADR 0033) and renders are serialized,
                // so save/restore keeps an operator-set value.
                let timing_extra = if yc_transcribe::ensemble::forced_align_active(Some(
                    &paths.align_model,
                )) {
                    None
                } else {
                    let prev = std::env::var("YC_SUPPRESS_NST").ok();
                    std::env::set_var("YC_SUPPRESS_NST", "1");
                    let r = yc_transcribe::transcribe_range(
                        &paths.model,
                        &samples,
                        language,
                        &yc_transcribe::DialectLexicon::default(),
                        {
                            let c = cancel.clone();
                            move || c.is_cancelled()
                        },
                    );
                    match prev {
                        Some(v) => std::env::set_var("YC_SUPPRESS_NST", v),
                        None => std::env::remove_var("YC_SUPPRESS_NST"),
                    }
                    match r {
                        Ok(t) => Some(t),
                        Err(e) => {
                            tracing::warn!(
                                "qwen ensemble: timing-skeleton decode failed \
                                 (fusing on the default skeleton only): {e:#}"
                            );
                            None
                        }
                    }
                };
                let cfg = yc_transcribe::ensemble::EnsembleConfig {
                    // Resolved once in AppPaths (the Diagnostics registry shows
                    // the same paths) and carried here - never re-derived.
                    mtmd_cli: paths.mtmd_cli.clone(),
                    qwen_model: paths.qwen_model.clone(),
                    qwen_mmproj: paths.qwen_mmproj.clone(),
                    ffmpeg: paths.ffmpeg.clone(),
                    deep_filter: paths.deep_filter.is_file().then(|| paths.deep_filter.clone()),
                    work_dir: session.data_dir.clone(),
                    // Cancel + progress plumbing (the 2026-07-03 hang report:
                    // the ensemble's children ignored Cancel entirely, and one
                    // static stage label sat unchanged over the app's longest
                    // stage — indistinguishable from a freeze).
                    should_cancel: Box::new({
                        let c = cancel.clone();
                        move || c.is_cancelled()
                    }),
                    on_stage: Box::new({
                        let tx = tx.clone();
                        move |i, n| {
                            let _ = tx.send(Progress::Stage(ens_stage_label(i, n)));
                        }
                    }),
                    align_model: Some(paths.align_model.clone()),
                };
                match yc_transcribe::ensemble::apply(
                    &cfg,
                    &session.analysis_wav,
                    range,
                    &transcript,
                    timing_extra.as_ref(),
                    &samples,
                    yc_ingest::WHISPER_SR,
                    &lexicon,
                ) {
                    Ok(fused) => {
                        transcript = fused;
                        ens_used = true;
                    }
                    // A cancel mid-ensemble must abort the whole job
                    // (fail_or_cancel reports Cancelled) — falling back to
                    // whisper captions here would go on to render a clip the
                    // operator just told us to stop.
                    Err(e) if cancel.is_cancelled() => return Err(e),
                    Err(e) => {
                        // The watchdog class gets a visible notice: the
                        // operator sees WHY the export continues with whisper
                        // words (GPU-contention crawl, 2026-07-03), not just a
                        // silent quality drop.
                        if e.downcast_ref::<yc_transcribe::ensemble::DecodeTimeout>().is_some() {
                            let _ = tx.send(Progress::Stage(
                                "Ensemble timed out (GPU busy?) — continuing with whisper captions",
                            ));
                        }
                        tracing::warn!("qwen ensemble failed; keeping whisper captions: {e:#}");
                    }
                }
            }
            // Auto-promote (the operator's choice, ADR 0031): any correction they have
            // confirmed in this clip's store rises to the per-Creator store, so it
            // applies to every future clip of theirs. Best-effort.
            let promoted = yc_transcribe::DialectLexicon::promote_confirmed(
                &clip_store,
                &creator_store,
                language,
            );
            if promoted > 0 {
                tracing::info!(
                    "dialect: promoted {promoted} confirmed correction(s) -> {}",
                    creator_store.display()
                );
            }
            // LLM caption-correction pass (ADR 0030): apply the operator's curated
            // context overrides (slang/names in context) the dict can't do safely.
            // Gated on the per-render `correct` toggle (GUI checkbox / CLI), the
            // `correct` feature, and the sidecar — off by default until sign-off.
            #[cfg(feature = "correct")]
            if correct {
                correct_captions(paths, session, prepared, &mut transcript, &lexicon, cancel, tx);
            }
            // NOT wired: a whisper-path forced-align re-time
            // (`ensemble::forced_align_retime`) was built and measured here
            // (ADR 0058, 2026-07-12) and its defect-clip bar FAILED — with no
            // vote to clean whisper's token list, the aligner amplifies
            // whisper's double-transcriptions across multi-second stretches
            // (clip 3's SUSU DEDDY CORP dragged ~2.7 s early). Wire it only
            // behind a whisper-side word-cleaning mechanism (the recall/vote
            // lane) or an engine-default policy change; the instrument is
            // `whisper_align_diag`. Whisper renders keep whisper's own DTW
            // spans, exactly as before.
            // Auto-harvest this clip's unsure/unknown words to the PER-CLIP store
            // (ADR 0031), so the operator curates a small per-export list (with each
            // word's title + VOD timestamp, ADR 0022) — but only words the timing
            // pass KEEPS. A unit the refine drops as near-silence is whisper
            // hallucinating into a silent/music window; recording it would send the
            // operator chasing a word that never appears in the caption (the trace
            // is 1:1 with pre-refine units, so `unit_index` addresses it directly).
            // Best-effort, never sinks the render. Skipped when the ensemble
            // replaced the words: harvest candidates are keyed to WHISPER's
            // units (unit_index) and record whisper's garbles — indexing them
            // into the fused transcript would misattribute, and harvesting
            // garbles that no longer render would send the operator curating
            // words the caption doesn't show (the ensemble path is store-free
            // by design).
            if lexicon.harvest && !ens_used {
                let trace = yc_render::refine_caption_timing_traced(
                    &transcript,
                    &samples,
                    yc_ingest::WHISPER_SR,
                );
                let n_raw = harvest.len();
                let kept: Vec<yc_transcribe::HarvestCandidate> = harvest
                    .into_iter()
                    .filter(|c| {
                        matches!(
                            trace.outcomes.get(c.unit_index),
                            Some(yc_render::UnitOutcome::Kept { .. })
                        )
                    })
                    .collect();
                if kept.len() < n_raw {
                    tracing::info!(
                        "dialect: {} harvest candidate(s) skipped (unit silence-dropped)",
                        n_raw - kept.len()
                    );
                }
                let n = yc_transcribe::DialectLexicon::harvest_to_file(
                    &clip_store,
                    language,
                    &kept,
                    range.start_s,
                    prepared.title.as_deref(),
                );
                if n > 0 {
                    tracing::info!("dialect: harvested {n} word(s) -> {}", clip_store.display());
                }
            }
            // Positional (`at_s`) store pins on the WHISPER path (engine parity,
            // ADR 0051): the ensemble applies these inside `ensemble::apply`, but a
            // whisper render never did — an operator's time-pin silently no-op'd on
            // the default engine. Snap each pinned word onto its real speech onset
            // here too, over the SAME `rms_onsets` grid the fusion uses, so a
            // "cue shows early, the word is at ~28 s" correction works on BOTH
            // engines. After harvest (whose `unit_index` addresses the pre-pin
            // units) and before refine (which then gap-fills the moved word). A
            // store with no `at_s` entries — the common case — is a no-op, so the
            // default render stays byte-identical.
            // The `any(at_s)` guard skips even the onset envelope in that case.
            if !ens_used && lexicon.corrections.iter().any(|c| c.at_s.is_some()) {
                yc_transcribe::ensemble::apply_store_positional(
                    &mut transcript.units,
                    &lexicon,
                    range.start_s,
                    &yc_transcribe::ensemble::rms_onsets(&samples, yc_ingest::WHISPER_SR),
                    range.duration_s(),
                );
            }
            // NOT wired — and REFUSED at the operator's eye: the
            // laughter-aware hold trim (`yc_render::trim_reaction_holds`,
            // ADR 0062, 2026-07-12) would run HERE, after the refine below.
            // It measured floor-exact on the 5-clip corpus, failed its
            // pre-registered R3 letter, and the A/B burn then refused it:
            // the flagged holds cover DRAWN-OUT words ("gemesss…") whose
            // sound stretches INTO the laugh — a laugh-mask onset is not an
            // end-of-word marker, so trimming there cuts captions on live
            // speech. Do not re-attempt without an end-of-vocalization
            // signal (e.g. the aligner's word ENDS), behind its own gate.
            // Instrument: `caption_laugh_diag`; what-if emit:
            // `caption_align_diag` with `YC_LAUGH_TRIM=1`.
            // Refine caption end-times to the streamer's actual vocalization: a
            // screamed / drawn-out word holds for its full sound and a normal word
            // clears when the sound drops, instead of huge-word's fixed hold.
            // (Recomputes the same pure trace as the harvest filter above — the two
            // can never disagree, and the envelope pass is trivial next to whisper.)
            // Ensemble transcripts keep every unit: their words are vote-verified
            // across decoders, so a near-silent onset means fusion placed a real
            // word into a quiet span — re-time it, don't delete it (ADR 0034).
            if ens_used {
                yc_render::refine_caption_timing_keep_verified(
                    transcript,
                    &samples,
                    yc_ingest::WHISPER_SR,
                )
            } else {
                yc_render::refine_caption_timing(transcript, &samples, yc_ingest::WHISPER_SR)
            }
        };
        prepared.transcript = Some(transcript);
        prepared.transcript_ens = use_ensemble;
        prepared.transcript_operator = false;
    }
    Ok(engine)
}

/// Phase-2b (ADR 0012): render the operator's `layout` over the prepared
/// Segment. Transcription happens once via [`ensure_transcript`] (cached on the
/// `PreparedClip`), so a re-render after another nudge — or after the editor's
/// Transcribe pre-pass — is NVENC-only. `camera` switches the composite to the
/// dynamic active-speaker cut plan; `transcript_override` burns the operator's
/// edited captions verbatim.
#[allow(clippy::too_many_arguments)]
fn do_render(
    paths: &PipelinePaths,
    session: &Session,
    prepared: &mut PreparedClip,
    layout: Layout,
    style: CaptionStyle,
    correct: bool,
    placement: Option<CaptionPlacement>,
    caption_engine: Option<CaptionEngine>,
    camera: Option<CameraPlan>,
    transcript_override: Option<Transcript>,
    keep: Option<Vec<TimeRange>>,
    manual_captions: Vec<yc_core::ManualCaption>,
    intro: Option<ThumbnailIntro>,
    music: Vec<yc_core::MusicClip>,
    fade: Option<yc_core::FadeSpec>,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<PathBuf> {
    let range = prepared.range;
    // Edge fades (ADR 0070): an inactive spec is no fade at all — normalize
    // it away so the untouched paths stay untouched (the razor's own rule).
    let fade = fade.filter(|f| f.active());
    // Thumbnail intro (ADR 0067): the operator asked for it, so a vanished
    // image FAILS loudly with its path — silently shipping without it would
    // lie (the drop-with-a-note rule is for loading stale project.json).
    if let Some(i) = &intro {
        anyhow::ensure!(
            i.path.is_file(),
            "thumbnail image missing: {} — re-pick it in the editor",
            i.path.display()
        );
    }
    // Music (ADR 0068): the same rule — the operator placed it, so a
    // vanished file fails loudly with its path.
    for m in &music {
        anyhow::ensure!(
            m.path.is_file(),
            "music file missing: {} — re-pick it in the editor",
            m.path.display()
        );
    }
    // Timeline razor (ADR 0065): a keep list that covers the whole clip is no
    // cut at all — normalize it away so the untouched paths stay untouched.
    let keep = keep.filter(|k| {
        !k.is_empty()
            && (k.len() > 1
                || k[0].start_s > 1e-6
                || k[0].end_s < range.duration_s() - 1e-6)
    });
    // The operator's edited transcript is the render's truth: cache it as
    // operator-owned (no whisper, no harvest, no re-timing — these are not
    // guesses to second-guess) and skip transcription entirely.
    if let Some(t) = transcript_override {
        prepared.transcript = Some(t);
        prepared.transcript_operator = true;
    }
    let engine = ensure_transcript(paths, session, prepared, correct, caption_engine, cancel, tx)?;
    let transcript = prepared.transcript.as_ref().expect("transcript set above");
    // The editor's caption preview draws exactly what this render burns (ADR
    // 0036): ship the refined transcript now, so captions are on the operator's
    // canvas while NVENC still runs. Every Render emits it (cached re-renders
    // included) — the editor may have opened after the first one.
    let _ = tx.send(Progress::Captions { transcript: transcript.clone() });

    // Timeline razor (ADR 0065): the export drops the removed spans. The
    // camera plan (or the static layout, promoted to one shot per kept span)
    // is intersected with the kept spans IN SOURCE TIME — the filtergraph
    // trims by source time and its concat compresses the output — and the
    // captions remap onto that compressed timeline through the spans the plan
    // ACTUALLY kept (a sub-frame sliver piece is dropped), so burned text can
    // never drift from the cut audio. The editor keeps the full-clip
    // transcript (sent above); only the burn compresses.
    let export_camera: Option<CameraPlan> = match (&camera, &keep) {
        (Some(plan), Some(k)) => Some(plan.cut_to(k)),
        (Some(plan), None) => Some(plan.clone()),
        (None, Some(k)) => Some(CameraPlan {
            shots: k
                .iter()
                .map(|seg| yc_core::Shot {
                    start_s: seg.start_s,
                    end_s: seg.end_s,
                    track: None,
                    layout: layout.clone(),
                    pan_to: None,
                })
                .collect(),
        }),
        (None, None) => None,
    };
    let (ass_transcript, ass_manual) = match (&keep, &export_camera) {
        (Some(_), Some(plan)) => {
            let spans = plan.kept_spans();
            (
                Transcript {
                    language: transcript.language,
                    units: yc_core::remap_units_through_cuts(&transcript.units, &spans),
                },
                // Each manual caption remaps like the transcript (a caption
                // fully inside removed time drops); its placement rides along.
                manual_captions
                    .iter()
                    .filter_map(|c| {
                        yc_core::remap_units_through_cuts(
                            std::slice::from_ref(&c.unit),
                            &spans,
                        )
                        .into_iter()
                        .next()
                        .map(|unit| yc_core::ManualCaption { unit, placement: c.placement })
                    })
                    .collect(),
            )
        }
        _ => (transcript.clone(), manual_captions.clone()),
    };

    // Captions: write the ASS into the data folder and the font into a fonts-only
    // `data/fonts/` subdir. ffmpeg runs in the data folder, so the relative
    // `subtitles=clip.ass:fontsdir=fonts` resolves both (dodging Windows
    // filtergraph path escaping) while libass scans only fonts — not the sibling
    // analysis.wav / project.json a flat fontsdir tried (and failed) to open.
    let _ = tx.send(Progress::Stage("Generating captions"));
    let ass = yc_render::generate_ass(&ass_transcript, &style, placement, &ass_manual);
    fs::write(session.data_dir.join("clip.ass"), ass).context("writing clip.ass")?;
    let fonts_dir = session.data_dir.join("fonts");
    fs::create_dir_all(&fonts_dir)
        .with_context(|| format!("creating {}", fonts_dir.display()))?;
    fs::copy(&paths.font, fonts_dir.join("Anton-Regular.ttf"))
        .with_context(|| format!("copying font from {}", paths.font.display()))?;

    // Name the Short from the LLM-generated title (ADR 0015), de-collided so a
    // re-promote never clobbers a previous Short. It lands at the stream-folder
    // root; intermediates (clip.ass, font, segment) stay under data/.
    let stem = clip_title_stem(prepared.title.as_deref(), range);
    let out_path = unique_path(&session.stream_dir, &stem, "mp4");

    // Record the promoted Clip with the operator's Layout + its export path, then
    // render it.
    let clip = build_clip(
        range,
        layout,
        &style.name,
        placement,
        intro.clone(),
        music.clone(),
        fade,
        &out_path,
    );
    persist_clip(&session.vod, &clip, &session.data_dir)?;

    let _ = tx.send(Progress::Stage("Rendering (NVENC)"));
    // The output is an absolute path (only the `subtitles=clip.ass` filter must
    // stay relative for libass); ffmpeg runs in data/ so the relative ASS + font
    // resolve, and writes the Short up at the stream-folder root.
    // Thumbnail intro (ADR 0067): the image rides as ffmpeg input 1 and the
    // finished graph is wrapped AFTER its ASS burn (`prepend_intro`) — the
    // ASS bytes above are identical with and without it. Music (ADR 0068)
    // wraps one amix around everything, the intro included, with its files
    // as the inputs after the image (`input_base`). Edge fades (ADR 0070)
    // wrap LAST — outside even the amix, so one envelope dims the video and
    // kills the final MIXED audio together at the Short's edges.
    let out_name = out_path.to_string_lossy();
    let intro_arg = intro.as_ref().map(|i| (i.path.as_path(), i.duration_s));
    let music_paths: Vec<PathBuf> = music.iter().map(|m| m.path.clone()).collect();
    let music_base = 1 + usize::from(intro.is_some());
    let args = match &export_camera {
        // Active-speaker camera / timeline razor: the per-shot cut concat. The
        // graph grows with the shot count, so it travels as a script file.
        // With razor cuts the graph also cuts the audio per kept piece.
        Some(plan) if !plan.shots.is_empty() => {
            let cut_audio = keep.is_some();
            let mut graph = yc_render::build_camera_filtergraph(plan, "clip.ass", cut_audio);
            if let Some(i) = &intro {
                graph = yc_render::prepend_intro(&graph, i.duration_s, prepared.src_fps, cut_audio);
            }
            graph = yc_render::mix_music(&graph, &music, music_base);
            // Edge fades (ADR 0070) wrap LAST, and the fade-out anchors on
            // the REAL output end — the concat's kept total plus the intro —
            // never `-t`, which only bounds (a razor-cut output ENDS earlier).
            let kept_total: f64 = plan.shots.iter().map(|s| s.end_s - s.start_s).sum();
            let out_total = intro.as_ref().map(|i| i.duration_s).unwrap_or(0.0) + kept_total;
            let has_aout = graph.contains("[aout]");
            let (graph, filtered_audio) =
                yc_render::fade_edges(&graph, fade.as_ref(), out_total, has_aout);
            fs::write(session.data_dir.join("camera.fg"), graph)
                .context("writing camera filtergraph")?;
            yc_render::export_args_script(
                &prepared.render_src,
                prepared.seek_s,
                range.duration_s(),
                "camera.fg",
                &out_name,
                filtered_audio,
                intro_arg,
                &music_paths,
            )
        }
        _ => {
            let mut filtergraph = yc_render::build_filtergraph(&clip.layout, "clip.ass");
            if let Some(i) = &intro {
                filtergraph =
                    yc_render::prepend_intro(&filtergraph, i.duration_s, prepared.src_fps, false);
            }
            filtergraph = yc_render::mix_music(&filtergraph, &music, music_base);
            // No camera and no razor on this arm by construction (a razor
            // promotes to the camera path), so the real output end is simply
            // intro + clip; the fade still wraps last (ADR 0070).
            let out_total =
                intro.as_ref().map(|i| i.duration_s).unwrap_or(0.0) + range.duration_s();
            let has_aout = filtergraph.contains("[aout]");
            let (filtergraph, filtered_audio) =
                yc_render::fade_edges(&filtergraph, fade.as_ref(), out_total, has_aout);
            yc_render::export_args(
                &prepared.render_src,
                prepared.seek_s,
                range.duration_s(),
                &filtergraph,
                &out_name,
                filtered_audio,
                intro_arg,
                &music_paths,
            )
        }
    };
    yc_render::run_export(&paths.ffmpeg, &session.data_dir, &args, &|| cancel.is_cancelled())?;

    // Remember this Creator's Caption Style + engine for the next import
    // (ADR 0016 / ADR 0035). `engine` is the resolved selection, pre-override.
    remember_creator_render(&paths.workspace, &session.vod, style.genre, engine);

    Ok(out_path)
}

/// Language code for the dialect-store filenames (`<lang>.json`), matching
/// `yc_transcribe`'s internal mapping — used for the per-Creator and per-clip
/// stores (ADR 0031).
fn dialect_lang_code(l: Language) -> &'static str {
    match l {
        Language::En => "en",
        Language::Id => "id",
        Language::Ja => "ja",
    }
}

/// The per-clip dialect stores in a stream folder (`<ClipStem>.<lang>.json`, ADR
/// 0031) — the review queue surfaces their harvested to-dos too (ADR 0032). The
/// per-Creator store lives in the *parent* dir, so scanning the stream folder for
/// the `.<lang>.json` suffix picks up only per-clip stores. Best effort: an
/// unreadable/absent dir yields none. Sorted for a stable panel order.
fn clip_store_paths(stream_dir: &Path, language: Language) -> Vec<PathBuf> {
    let suffix = format!(".{}.json", dialect_lang_code(language));
    let mut out: Vec<PathBuf> = std::fs::read_dir(stream_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).map(|n| n.ends_with(&suffix)).unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

/// The filename stem for a rendered Short (no extension): the promoted Moment's
/// LLM-generated title sanitized to a safe segment, or — when there is no title
/// (manual / headless clip) or it sanitizes to nothing — a deterministic
/// timestamp name `clip-<m-ss>` from the clip's start, so every Short still gets
/// a meaningful, collision-resistant name (ADR 0015).
fn clip_title_stem(title: Option<&str>, range: TimeRange) -> String {
    title
        .map(|t| yc_ingest::sanitize_segment(t, "", TITLE_STEM_MAX))
        // Require a real word: an all-symbol / emoji title (sanitizes to e.g.
        // "___" or stays punctuation) is not a meaningful filename — fall back.
        .filter(|s| s.chars().any(|c| c.is_alphanumeric()))
        .unwrap_or_else(|| format!("clip-{}", clock_stem(range.start_s)))
}

/// `m-ss` (or `h-mm-ss` past an hour) for a timestamp — a filesystem-safe clock
/// (no `:`), used in the fallback Short name.
fn clock_stem(t_s: f64) -> String {
    let s = t_s.round().max(0.0) as u64;
    if s >= 3600 {
        format!("{}-{:02}-{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}-{:02}", s / 60, s % 60)
    }
}

/// A non-colliding `dir/<stem>.<ext>`: returned as-is when free, else
/// `dir/<stem> (2).<ext>`, `(3)`, … so a re-promote of the same Moment never
/// overwrites a previous Short (ADR 0015). Bounded so a pathological directory
/// can't loop forever (falls back to the base name after the cap).
fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    for n in 2..10_000 {
        let candidate = dir.join(format!("{stem} ({n}).{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

/// A stable id for a promoted range: its start in whole milliseconds. Distinct
/// Moments (their ranges are seconds apart) get distinct ids, so a batch of N records
/// N Clips; a re-render of the same range reuses the id. (Was a hardcoded `1`, which
/// made `persist_clip`'s dedup collapse every batch render onto a single record.)
fn clip_id_for(range: TimeRange) -> u64 {
    (range.start_s * 1000.0).round().max(0.0) as u64
}

/// A promoted range -> a Clip record (CONTEXT.md). The id is provisional; `persist_clip`
/// relinks it to the detected Moment covering this range.
#[allow(clippy::too_many_arguments)]
fn build_clip(
    range: TimeRange,
    layout: Layout,
    caption_style: &str,
    caption_placement: Option<CaptionPlacement>,
    thumbnail: Option<ThumbnailIntro>,
    music: Vec<yc_core::MusicClip>,
    fade: Option<yc_core::FadeSpec>,
    export_path: &Path,
) -> Clip {
    let id = clip_id_for(range);
    Clip {
        id,
        moment_id: id,
        range,
        layout,
        caption_style: caption_style.to_string(),
        caption_placement,
        thumbnail,
        music,
        fade,
        segment_path: None,
        export_path: Some(export_path.to_path_buf()),
    }
}

/// Re-save `project.json` with the promoted Clip recorded, preserving any
/// detected Moments. Records the source Moment only if it isn't already known
/// (e.g. a directly promoted range that never went through detection).
fn persist_clip(vod: &Vod, clip: &Clip, data_dir: &Path) -> Result<()> {
    let mut project = load_or_new_project(vod, data_dir);
    let mut clip = clip.clone();
    // Link the Clip to the detected Moment covering its range (matched by start within
    // the Moment's window, so a nudge-trim still links to its source Moment), so the
    // record references the real Moment and a re-render replaces the same entry. A
    // directly-promoted range with no detected Moment gets a Moment recorded for it.
    let linked = project
        .moments
        .iter()
        .find(|m| clip.range.start_s >= m.range.start_s - 1.0 && clip.range.start_s <= m.range.end_s + 1.0)
        .map(|m| m.id);
    match linked {
        Some(id) => clip.moment_id = id,
        None => project.moments.push(Moment {
            id: clip.moment_id,
            range: clip.range,
            signals: Signals::default(),
            score: 0.0,
            title: None,
        }),
    }
    clip.id = clip.moment_id;
    // One entry per promoted Moment (ADR 0012): a re-render replaces its own record;
    // distinct Moments coexist — so a batch of N records all N, not just the last (the
    // bug was a constant id that overwrote every render onto one entry).
    project.clips.retain(|c| c.moment_id != clip.moment_id);
    project.clips.push(clip);
    project
        .save(&data_dir.join("project.json"))
        .with_context(|| format!("writing project.json in {}", data_dir.display()))
}

// --- auto-detect framing (M6, ADR 0011) -------------------------------------

/// Detect the static Facecam(s) in the Segment (ADR 0011, multi-face ext) — one
/// per cam, two for a co-stream cam — or an empty Vec when the `face` feature /
/// model is absent, detection errors, or no face persists.
#[cfg(feature = "face")]
fn detect_facecam(
    paths: &PipelinePaths,
    render_src: &Path,
    seek_s: f64,
    src_w: f32,
    src_h: f32,
) -> Vec<yc_frame::FaceCluster> {
    if !paths.face_model.is_file() {
        tracing::info!("face model absent; no Facecam (Layout uses pref seeds / fallback)");
        return Vec::new();
    }
    match detect_facecam_inner(paths, render_src, seek_s, src_w, src_h) {
        Ok(faces) => faces,
        Err(e) => {
            tracing::warn!("auto-frame failed: {e:#}; no Facecam (pref seeds / fallback)");
            Vec::new()
        }
    }
}

/// Without the `face` feature there is no detector: no Facecam, and the Layout
/// comes entirely from the operator's preference seeds (ADR 0017) or Auto's
/// full-frame fallback.
#[cfg(not(feature = "face"))]
fn detect_facecam(
    _paths: &PipelinePaths,
    _render_src: &Path,
    _seek_s: f64,
    _src_w: f32,
    _src_h: f32,
) -> Vec<yc_frame::FaceCluster> {
    Vec::new()
}

/// Sample frames from the Segment, run Ultraface per frame, and cluster the
/// static Facecam(s) (ADR 0011). Frames are scaled to the model's fixed input; its
/// normalized detections map straight to source pixels.
#[cfg(feature = "face")]
fn detect_facecam_inner(
    paths: &PipelinePaths,
    render_src: &Path,
    seek_s: f64,
    src_w: f32,
    src_h: f32,
) -> Result<Vec<yc_frame::FaceCluster>> {
    let frames = yc_ingest::extract_frames_rgb(
        &paths.ffmpeg,
        render_src,
        seek_s,
        yc_frame::infer::DET_W as u32,
        yc_frame::infer::DET_H as u32,
        yc_frame::SAMPLE_FPS,
        yc_frame::MAX_FRAMES,
    )?;
    let mut detector = yc_frame::Detector::load(&paths.face_model)?;
    let mut per_frame = Vec::with_capacity(frames.len());
    for f in &frames {
        per_frame.push(detector.detect(f, src_w, src_h)?);
    }
    let faces: usize = per_frame.iter().map(|f| f.len()).sum();
    let clusters = yc_frame::cluster_static_faces(&per_frame, src_w, src_h);
    tracing::info!(
        frames = frames.len(),
        faces,
        cams = clusters.len(),
        persistence = clusters.first().map(|c| c.persistence),
        "auto-frame detection"
    );
    Ok(clusters)
}

// --- podcast speaker analysis (focus 2026-07) ---------------------------------

/// Tracking-frame dimensions for the speaker pass: aspect-preserving, long edge
/// ~640 (mouth motion needs more pixels than Ultraface's 320x240, far fewer
/// than the source), even so the ffmpeg scaler never complains.
#[cfg_attr(not(feature = "face"), allow(dead_code))]
fn tracking_dims(src_w: f32, src_h: f32) -> (u32, u32) {
    const LONG_EDGE: f32 = 640.0;
    let even = |v: f32| (((v.round().max(2.0)) as u32) / 2) * 2;
    if src_w >= src_h {
        let w = src_w.min(LONG_EDGE);
        (even(w), even(w * src_h / src_w))
    } else {
        let h = src_h.min(LONG_EDGE);
        (even(h * src_w / src_h), even(h))
    }
}

/// Podcast speaker analysis over the prepared Segment (focus 2026-07): stream
/// tracking frames at [`yc_frame::speaker::SPEAKER_FPS`], detect faces per
/// frame (Ultraface, CPU), build persistent tracks with per-bin mouth
/// activity, gate by the clip audio, attribute the speaker per bin, join the
/// voice lane (ADR 0042 — CAM++ embeddings fused as a margin tiebreak, when
/// the model is present), and derive the cut-based active-speaker
/// [`CameraPlan`]. Memory stays one frame deep (streamed); a 60 s clip runs
/// in roughly the time of its face inference (the voice lane adds ~2 s).
/// Returns the analysis, the plan, and a note for the Camera panel when the
/// voice lane is off.
#[cfg(feature = "face")]
fn do_analyze_speakers(
    paths: &PipelinePaths,
    session: &Session,
    prepared: &PreparedClip,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<(SpeakerAnalysis, CameraPlan, Option<String>)> {
    use yc_frame::speaker;
    anyhow::ensure!(
        paths.face_model.is_file(),
        "face model missing - download version-RFB-320.onnx (fetch-models.ps1)"
    );
    let _ = tx.send(Progress::Stage("Analyzing speakers (faces + voice)"));
    let dur = prepared.range.duration_s();
    let fps = speaker::SPEAKER_FPS;
    let max_frames = (dur * fps).ceil() as usize + 4;
    let (tw, th) = tracking_dims(prepared.src_w, prepared.src_h);
    let mut detector = yc_frame::Detector::load(&paths.face_model)?;
    let mut builder =
        speaker::TrackBuilder::new(prepared.src_w, prepared.src_h, tw as usize, th as usize);
    let mut det_buf = vec![0u8; yc_frame::infer::DET_W * yc_frame::infer::DET_H * 3];
    let mut detect_err: Option<anyhow::Error> = None;
    let (src_w, src_h) = (prepared.src_w, prepared.src_h);
    yc_ingest::stream_frames_rgb(
        &paths.ffmpeg,
        &prepared.render_src,
        prepared.seek_s,
        dur,
        tw,
        th,
        fps,
        max_frames,
        &mut |rgb| {
            if cancel.is_cancelled() {
                return false;
            }
            speaker::downscale_rgb(
                rgb,
                tw as usize,
                th as usize,
                &mut det_buf,
                yc_frame::infer::DET_W,
                yc_frame::infer::DET_H,
            );
            match detector.detect(&det_buf, src_w, src_h) {
                Ok(faces) => {
                    builder.observe(&faces, rgb);
                    true
                }
                Err(e) => {
                    detect_err = Some(e);
                    false
                }
            }
        },
    )?;
    if let Some(e) = detect_err {
        return Err(e.context("face detection during speaker analysis"));
    }
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let tracks = builder.finish();
    tracing::info!(tracks = tracks.len(), "speaker analysis: tracks built");

    let samples = yc_ingest::read_range_samples(&session.analysis_wav, prepared.range)?;
    let bin_s = 1.0 / fps;
    let n_bins = (dur * fps).ceil().max(1.0) as usize;
    let voiced = speaker::voiced_bins(&samples, yc_ingest::WHISPER_SR, bin_s, n_bins);
    let (speaking, confidence) = speaker::attribute_speakers(&tracks, &voiced);
    let mut analysis =
        SpeakerAnalysis { bin_s, tracks, voiced, speaking, confidence, voice: None, reaction: None };
    // The source's own cut frames (pixel-level scene detection), so a multicam
    // plan cuts exactly where the source does — no sampling grid to lag it.
    // Detected BEFORE the voice lane: its per-angle joins group the segments
    // between these cuts by seat geometry.
    let cuts = detect_scene_cuts(&paths.ffmpeg, &prepared.render_src, prepared.seek_s, dur);
    tracing::info!(cuts = cuts.len(), "speaker analysis: source cuts");

    // --- occupant map (ADR 0044): who occupies each seat, per camera — the
    // person evidence the voice join is scoped by. Computed only where it
    // means something (the attribution regime; follow-visible ignores it by
    // construction) and only when both face-id models are present — missing
    // models degrade to the seat-scoped ADR 0042 join, never a failed job.
    let attribution = speaker::attribution_regime(&analysis);
    let mut face_note: Option<String> = None;
    let occupant_map: Option<yc_frame::occupant::OccupantMap> = if !attribution {
        None
    } else if !(paths.yunet_model.is_file() && paths.sface_model.is_file()) {
        face_note = Some(
            "Person id off — face-id models missing (Diagnostics ▸ Downloads); seat-scoped voice join"
                .into(),
        );
        tracing::info!("face-id models absent; voice join is seat-scoped");
        None
    } else {
        let _ = tx.send(Progress::Stage("Mapping seat occupants (face id)"));
        match build_occupant_map_via_seeks(paths, prepared, &analysis, &cuts, dur, cancel) {
            Ok(Some(map)) => {
                tracing::info!(
                    persons = map.n_persons,
                    cameras = map.camera_visits.len(),
                    "speaker analysis: occupant map built"
                );
                Some(map)
            }
            Ok(None) => {
                tracing::info!("occupant map: too few face entries; voice join is seat-scoped");
                None
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                face_note =
                    Some(format!("Person id failed — seat-scoped voice join ({e:#})"));
                tracing::warn!(error = %format!("{e:#}"), "occupant map failed; seat-scoped join");
                None
            }
        }
    };

    // --- voice lane (ADR 0042): the diarization tiebreak beside the mouth
    // lane. Additive by design — a missing or broken model yields exactly the
    // mouth-only analysis (plus a note for the Camera panel), never a failed
    // job. The fused speaking/confidence become THE lane every consumer reads.
    let mut voice_note: Option<String> = None;
    if !paths.voice_model.is_file() {
        voice_note = Some("Voice lane off — model missing (Diagnostics ▸ Downloads)".into());
        tracing::info!("voice model absent; speaker analysis is mouth-only");
    } else {
        match yc_frame::voice::embed_windows(
            &paths.voice_model,
            yc_frame::voice::SampleScale::Unit,
            true,
            &samples,
            &analysis.voiced,
            bin_s,
        ) {
            Ok((embs, kept)) => {
                match yc_frame::voice::build_lane(
                    &embs,
                    &kept,
                    &analysis,
                    &cuts,
                    dur,
                    attribution,
                    occupant_map.as_ref(),
                ) {
                    Some((mut lane, diag)) => {
                        for l in &diag.lines {
                            tracing::debug!(line = %l, "voice lane");
                        }
                        let (fspeak, fconf, overridden) =
                            yc_frame::voice::fuse_attribution(&analysis, &lane.seat);
                        tracing::info!(
                            thr = diag.picked_thr,
                            claimed_s = diag.claimed_s,
                            agreement = diag.agreement,
                            offscreen_s = diag.offscreen_s,
                            overridden_bins = overridden.iter().filter(|o| **o).count(),
                            "speaker analysis: voice lane joined"
                        );
                        lane.overridden = overridden;
                        analysis.speaking = fspeak;
                        analysis.confidence = fconf;
                        analysis.voice = Some(lane);
                    }
                    None => {
                        voice_note =
                            Some("Voice lane off — too little speech to identify voices".into());
                        tracing::info!("voice lane skipped: too few embeddable windows");
                    }
                }
            }
            Err(e) => {
                voice_note = Some(format!("Voice lane failed — mouth-only analysis ({e:#})"));
                tracing::warn!(error = %format!("{e:#}"), "voice lane failed; mouth-only analysis");
            }
        }
    }
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }

    // --- shared-reaction lane (ADR 0045/0046): the laughter-class mask the
    // split grammar reads as "the group reacts together". Attribution regime
    // only — a follow-visible plan never consults it (the source already
    // chose its subject), so those clips never pay the tagging cost and the
    // follow-visible byte-pin holds structurally. Additive like the voice
    // lane: a missing or broken tagger yields exactly the mask-less analysis
    // plus a Camera-panel note, never a failed job.
    let mut reaction_note: Option<String> = None;
    if attribution {
        if !(paths.tag_model.is_file() && paths.tag_labels.is_file()) {
            reaction_note =
                Some("Shared reaction off — tagger missing (Diagnostics ▸ Downloads)".into());
            tracing::info!("reaction tagger absent; split grammar sees no laughter fuel");
        } else {
            let _ = tx.send(Progress::Stage("Scoring shared reactions (audio tags)"));
            let tag = || -> Result<Vec<f32>> {
                use yc_frame::reaction;
                let labels =
                    reaction::parse_class_labels(&std::fs::read_to_string(&paths.tag_labels)?);
                let family = reaction::laughter_family(&labels);
                anyhow::ensure!(
                    !family.is_empty(),
                    "no laughter-family classes in {}",
                    paths.tag_labels.display()
                );
                // Conventions pinned by the tagselftest (ADR 0045), not
                // assumed: fbank on [-1, 1] samples (Int16 audibly breaks the
                // model) and a sigmoid-terminated export (re-sigmoiding was
                // the first cut's caught bug).
                let mut sess = reaction::TagSession::load(
                    &paths.tag_model,
                    yc_frame::voice::SampleScale::Unit,
                    reaction::TagOutput::Probs,
                )?;
                let steps = reaction::tag_steps(&mut sess, &samples, dur, &family)?;
                Ok(reaction::project_to_bins(&steps, n_bins, bin_s))
            };
            match tag() {
                Ok(bins) => {
                    let masked_s = bins
                        .iter()
                        .filter(|&&s| s >= speaker::REACTION_TAU)
                        .count() as f64
                        * bin_s;
                    tracing::info!(masked_s, "speaker analysis: shared-reaction mask");
                    analysis.reaction = Some(bins);
                }
                Err(e) => {
                    reaction_note =
                        Some(format!("Shared reaction failed — no reaction mask ({e:#})"));
                    tracing::warn!(error = %format!("{e:#}"), "reaction lane failed; no mask");
                }
            }
        }
    }
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }

    let mut plan = speaker::plan_shots(&analysis, prepared.src_w, prepared.src_h, dur, &cuts);
    tracing::info!(shots = plan.shots.len(), "speaker analysis: camera plan");

    // --- solo presence (ADR 0048): verify each solo shot frames a real,
    // visible subject face, then rewrite the failures into an honest
    // split/hold/wide. Attribution regime + both face-id models only — a
    // follow-visible plan has no identity anchor (its A-class waits, ADR 0048)
    // and a models-absent clip keeps the draft plan (the pre-0048 behaviour,
    // covered by the person-id note the occupant map already set). The seek
    // pass reuses the occupant map's targeted full-res decode; a shot measured
    // for more than half its bins is never seeked (it can never flag).
    let mut presence: Vec<speaker::SoloPresence> = Vec::new();
    if attribution && paths.yunet_model.is_file() && paths.sface_model.is_file() {
        let _ = tx.send(Progress::Stage("Verifying solo framing (face id)"));
        match presence_seeks(paths, prepared, &analysis, &plan, cancel) {
            Ok(heights) => {
                let mut v = speaker::evaluate_solo_presence(&analysis, &plan, &heights);
                let flagged = v.iter().filter(|x| x.flagged).count();
                if flagged > 0 {
                    plan = speaker::rewrite_for_presence(
                        &plan,
                        &analysis,
                        &mut v,
                        &cuts,
                        prepared.src_w,
                        prepared.src_h,
                    );
                    tracing::info!(flagged, "solo presence: healed flagged solo shots");
                }
                presence = v;
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                tracing::warn!(error = %format!("{e:#}"), "solo presence pass failed; draft plan stands");
            }
        }
    }

    // Jitter-class defects (a crop moving without subject cause) and healed
    // solo-presence windows (ADR 0048) are flagged here and in the Studio's
    // Camera panel BEFORE any export renders them.
    for f in speaker::audit_camera_plan(&analysis, &plan, &presence) {
        tracing::warn!(finding = %f, "camera plan audit");
    }
    // One note string, one line per lane that is off/degraded — the Camera
    // panel prints each line.
    let lines: Vec<String> =
        [voice_note, face_note, reaction_note].into_iter().flatten().collect();
    let note = (!lines.is_empty()).then(|| lines.join("\n"));
    Ok((analysis, plan, note))
}

/// Build the occupant map from targeted full-res seeks (ADR 0044): the
/// production twin of the harness face lane — the same sampling plan, region
/// crops, landmark alignment, and aggregation, through the same pure
/// functions. ~13 s per 70 s clip harness-measured (52 seeks + 50 embeddings,
/// one core), sequential; only runs in the attribution regime with both
/// models present, so follow-visible clips never pay it.
#[cfg(feature = "face")]
fn build_occupant_map_via_seeks(
    paths: &PipelinePaths,
    prepared: &PreparedClip,
    analysis: &SpeakerAnalysis,
    cuts: &[f64],
    dur: f64,
    cancel: &CancelToken,
) -> Result<Option<yc_frame::occupant::OccupantMap>> {
    use yc_frame::{face_id, occupant};
    let bin_s = analysis.bin_s;
    let n_bins = analysis.speaking.len();
    let (src_w, src_h) = (prepared.src_w as usize, prepared.src_h as usize);
    let bounds = yc_frame::voice::segment_bounds(cuts, dur);
    let mut ident = face_id::FaceIdentifier::load(&paths.yunet_model, &paths.sface_model)?;
    let samples = occupant::plan_samples(analysis, &bounds, occupant::SAMPLES_PER_SEG);
    let mut acc: std::collections::HashMap<(usize, usize), Vec<Vec<f32>>> = Default::default();
    for &(g, t) in &samples {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        // One full-res frame at the sampled time (a short window, first frame).
        let mut frame: Option<Vec<u8>> = None;
        yc_ingest::stream_frames_rgb(
            &paths.ffmpeg,
            &prepared.render_src,
            prepared.seek_s + t,
            (2.5 / prepared.src_fps.max(1.0)).max(0.05),
            prepared.src_w as u32,
            prepared.src_h as u32,
            prepared.src_fps,
            1,
            &mut |rgb| {
                frame = Some(rgb.to_vec());
                false
            },
        )?;
        let Some(frame) = frame else { continue };
        let b = ((t / bin_s) as usize).min(n_bins.saturating_sub(1));
        for tr in &analysis.tracks {
            let Some(fb) = tr.path.get(b).and_then(|p| p.as_ref()) else { continue };
            let side = ((fb.w.max(fb.h) * occupant::REGION_EXPAND) as usize).max(64);
            let (region, rw, rh, rx, ry) = occupant::crop_rgb(
                &frame,
                src_w,
                src_h,
                (fb.cx() - side as f32 * 0.5) as i32,
                (fb.cy() - side as f32 * 0.5) as i32,
                side,
                side,
            );
            let dets = ident.detect(&region, rw, rh)?;
            let (ecx, ecy) = (fb.cx() - rx as f32, fb.cy() - ry as f32);
            let Some(det) = occupant::pick_track_face(&dets, ecx, ecy, fb.h) else { continue };
            let emb = ident.embed(&region, rw, rh, &det.kps)?;
            acc.entry((g, tr.id)).or_default().push(emb);
        }
    }
    let mut entries: Vec<occupant::FaceEntry> = acc
        .into_iter()
        .filter_map(|((seg, track), embs)| {
            face_id::aggregate_unit(&embs).map(|emb| occupant::FaceEntry { seg, track, emb })
        })
        .collect();
    entries.sort_by(|a, b| (a.seg, a.track).cmp(&(b.seg, b.track)));
    Ok(occupant::build_occupant_map(&entries, bounds.len().saturating_sub(1)))
}

/// Verify **solo presence** (ADR 0048): for each solo shot whose subject is
/// unmeasured for at least [`speaker::PRESENCE_UNMEASURED_FRAC`] of its bins
/// (a mostly-measured shot is already anchored and can never flag, so it is
/// never seeked), scan the PLANNED CROP for a real face at the shot's span
/// quantiles and its largest unmeasured gap. Returns `(shot_idx, tallest
/// crop-face px)` per SEEKED shot — the impure half of the presence pass (the
/// pure Bar P and the fallback rewrite live in `yc_frame::speaker`). A shot
/// with no decoded frame is omitted, so ignorance never flags. Reuses the
/// occupant map's machinery: the same full-res targeted decode and `crop_rgb`,
/// YuNet over the crop only (no SFace — the class-A trigger needs no identity).
#[cfg(feature = "face")]
fn presence_seeks(
    paths: &PipelinePaths,
    prepared: &PreparedClip,
    analysis: &SpeakerAnalysis,
    plan: &CameraPlan,
    cancel: &CancelToken,
) -> Result<Vec<(usize, f32)>> {
    use yc_frame::{face_id, occupant, speaker};
    let bin_s = analysis.bin_s;
    let n_bins = analysis.speaking.len();
    let (src_w, src_h) = (prepared.src_w as usize, prepared.src_h as usize);
    let mut ident = face_id::FaceIdentifier::load(&paths.yunet_model, &paths.sface_model)?;
    let mut out: Vec<(usize, f32)> = Vec::new();
    let (mut n_shots, mut n_seeks) = (0usize, 0usize);
    let t0 = std::time::Instant::now();
    for (idx, s) in plan.shots.iter().enumerate() {
        let (Some(id), Layout::FullFrame { crop }) = (s.track, &s.layout) else { continue };
        let Some(tr) = analysis.tracks.iter().find(|t| t.id == id) else { continue };
        let b0 = ((s.start_s / bin_s).round() as usize).min(n_bins);
        let b1 = ((s.end_s / bin_s).round() as usize).clamp(b0, n_bins);
        let nb = (b1 - b0).max(1);
        let at = |b: usize| tr.path.get(b).and_then(|p| p.as_ref());
        let meas_n = (b0..b1).filter(|&b| at(b).is_some()).count();
        // Arm 1 pre-filter: measured for more than half its bins -> anchored ->
        // can never flag. Skip its seeks (this is what keeps the pass cheap).
        if ((nb - meas_n) as f32 / nb as f32) < speaker::PRESENCE_UNMEASURED_FRAC {
            continue;
        }
        n_shots += 1;
        // The largest unmeasured run — its midpoint is an extra probe time.
        let (mut gs, mut gl, mut cl) = (0usize, 0usize, 0usize);
        for k in b0..b1 {
            if at(k).is_none() {
                cl += 1;
                if cl > gl {
                    gl = cl;
                    gs = k + 1 - cl;
                }
            } else {
                cl = 0;
            }
        }
        let (gap_t0, gap_t1) = (gs as f64 * bin_s, (gs + gl) as f64 * bin_s);
        let sdur = (s.end_s - s.start_s).max(1e-6);
        let crop_at = |t: f64| -> yc_core::Crop {
            match &s.pan_to {
                Some(p) => crop.lerp(p, (((t - s.start_s) / sdur) as f32).clamp(0.0, 1.0)),
                None => *crop,
            }
        };
        // Probe times: span quantiles + the gap midpoint, inset from the cut
        // edges (a cut-straddling decode would crop the wrong angle's pixels).
        let inset = bin_s.max(0.12);
        let lo = s.start_s + inset;
        let hi = (s.end_s - inset).max(lo);
        let mut times: Vec<f64> = if sdur >= 1.2 {
            [0.2f64, 0.5, 0.8].iter().map(|q| s.start_s + sdur * q).collect()
        } else {
            vec![s.start_s + sdur * 0.5]
        };
        if gl as f64 * bin_s >= 1.0 {
            let gmid = (gap_t0 + gap_t1) * 0.5;
            if times.iter().all(|t| (t - gmid).abs() > 0.25) {
                times.push(gmid);
            }
        }
        for t in times.iter_mut() {
            *t = t.clamp(lo, hi);
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        times.dedup_by(|a, b| (*a - *b).abs() < 0.15);
        let (mut max_h, mut got) = (0.0f32, 0usize);
        for t in times {
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }
            let mut frame: Option<Vec<u8>> = None;
            yc_ingest::stream_frames_rgb(
                &paths.ffmpeg,
                &prepared.render_src,
                prepared.seek_s + t,
                (2.5 / prepared.src_fps.max(1.0)).max(0.05),
                prepared.src_w as u32,
                prepared.src_h as u32,
                prepared.src_fps,
                1,
                &mut |rgb| {
                    frame = Some(rgb.to_vec());
                    false
                },
            )?;
            let Some(frame) = frame else { continue };
            got += 1;
            n_seeks += 1;
            let c = crop_at(t);
            let (region, rw, rh, _rx, _ry) = occupant::crop_rgb(
                &frame,
                src_w,
                src_h,
                c.x as i32,
                c.y as i32,
                c.w as usize,
                c.h as usize,
            );
            if let Ok(dets) = ident.detect(&region, rw, rh) {
                for d in &dets {
                    max_h = max_h.max(d.bbox.h);
                }
            }
        }
        // Omit a shot with no decoded frame — measured absence flags, ignorance
        // does not (`evaluate_solo_presence` never flags an absent shot).
        if got > 0 {
            out.push((idx, max_h));
        }
    }
    tracing::info!(
        shots = n_shots,
        seeks = n_seeks,
        secs = t0.elapsed().as_secs_f32(),
        "solo presence: crop-face seeks"
    );
    Ok(out)
}

/// The `scene` value above which an inter-frame change is a source **cut**, not
/// motion. Measured on a real multicam VOD: hard cuts score ~0.3-0.5, the
/// busiest in-shot motion stays under ~0.1 — 0.2 separates them with margin. A
/// stray trigger costs nothing (it merges into its neighbour when the subject
/// is unchanged); a miss would leave two shots fused, so err low.
#[cfg(feature = "face")]
const SCENE_CUT_THRESHOLD: f64 = 0.2;

/// Detect the source's cut frames over the clip (`[seek_s, seek_s+dur]`) with
/// ffmpeg's scene-change filter, returning clip-relative cut times (seconds).
/// Best-effort: any failure yields an empty list, and the plan falls back to
/// approximating cuts from the per-bin subject. One extra full-rate decode of
/// the clip (cheap — pixel diff, no model), so the cuts are frame-exact even
/// when the source frame rate differs from the analysis grid.
#[cfg(feature = "face")]
fn detect_scene_cuts(ffmpeg: &Path, src: &Path, seek_s: f64, dur_s: f64) -> Vec<f64> {
    let args: Vec<String> = vec![
        "-v".into(),
        "info".into(),
        "-ss".into(),
        format!("{seek_s:.3}"),
        "-t".into(),
        format!("{dur_s:.3}"),
        "-i".into(),
        src.display().to_string(),
        "-vf".into(),
        format!("select='gt(scene,{SCENE_CUT_THRESHOLD})',metadata=print"),
        "-an".into(),
        "-f".into(),
        "null".into(),
        "-".into(),
    ];
    let output = std::process::Command::new(ffmpeg)
        .no_console()
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();
    let Ok(output) = output else {
        tracing::warn!("scene-cut detection failed to spawn; plan will approximate cuts");
        return Vec::new();
    };
    // metadata=print logs `... pts_time:<clip-relative seconds> ...` to stderr.
    let text = String::from_utf8_lossy(&output.stderr);
    let mut cuts: Vec<f64> = text
        .lines()
        .filter_map(|l| l.split("pts_time:").nth(1))
        .filter_map(|s| s.split_whitespace().next())
        .filter_map(|s| s.parse::<f64>().ok())
        .collect();
    cuts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    cuts.dedup_by(|a, b| (*a - *b).abs() < 0.02);
    cuts
}

/// Without the `face` feature there is no detector: speaker analysis cannot run.
#[cfg(not(feature = "face"))]
fn do_analyze_speakers(
    _paths: &PipelinePaths,
    _session: &Session,
    _prepared: &PreparedClip,
    _cancel: &CancelToken,
    _tx: &Sender<Progress>,
) -> Result<(SpeakerAnalysis, CameraPlan, Option<String>)> {
    anyhow::bail!(
        "speaker detection needs a `face` build (cargo --features face) and the face model"
    )
}

// --- dependency downloads (ADR 0041) -----------------------------------------

/// Fetch each spec from its pinned official source: stream to a `.part` next
/// to the destination (same volume, so the final rename is atomic), hash while
/// streaming, verify the pinned SHA-256, then install. Any failure removes the
/// partial and stops the batch — nothing unverified ever lands on a real path.
fn do_download(specs: &[DownloadSpec], cancel: &CancelToken, tx: &Sender<Progress>) -> Result<()> {
    anyhow::ensure!(!specs.is_empty(), "nothing to download");
    let agent = download_agent();
    for spec in specs {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let _ = tx.send(Progress::Download { label: spec.label, frac: 0.0 });
        tracing::info!("downloading {} from {}", spec.label, spec.url);
        let staging = staging_path(spec);
        if let Some(dir) = staging.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        if let Err(e) = fetch_verified(&agent, spec, &staging, cancel, tx) {
            let _ = fs::remove_file(&staging);
            return Err(e.context(format!("downloading {} from {}", spec.label, spec.url)));
        }
        install_download(&staging, &spec.install)
            .with_context(|| format!("installing {}", spec.label))?;
        let _ = tx.send(Progress::Download { label: spec.label, frac: 1.0 });
    }
    Ok(())
}

/// The downloads' HTTP agent: rustls, redirects followed (GitHub/HF assets
/// live behind them), a bounded connect. No whole-body timeout — a model is
/// gigabytes on an unknown line; the cancel token is the abort lever.
fn download_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .build()
        .into()
}

/// Where a spec's in-flight download stages: beside its (first) destination,
/// so the verified rename never crosses a volume.
fn staging_path(spec: &DownloadSpec) -> PathBuf {
    let dest_dir = match &spec.install {
        Install::File(dest) => dest.parent().map(Path::to_path_buf),
        Install::Unzip { picks, dll_sweep_to } => picks
            .first()
            .and_then(|(_, to)| to.parent().map(Path::to_path_buf))
            .or_else(|| dll_sweep_to.clone()),
    };
    dest_dir.unwrap_or_else(std::env::temp_dir).join(format!("{}.download.part", spec.id))
}

/// Stream the URL to `staging`, hashing as it goes; error (and leave the
/// caller to clean up) unless the hash equals the spec's pin. Progress is
/// throttled to ~1% / 4 MB steps so the channel is not flooded.
fn fetch_verified(
    agent: &ureq::Agent,
    spec: &DownloadSpec,
    staging: &Path,
    cancel: &CancelToken,
    tx: &Sender<Progress>,
) -> Result<()> {
    use sha2::Digest;
    use std::io::{Read, Write};

    let mut resp = agent.get(spec.url).call().context("request failed")?;
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(spec.total_bytes)
        .max(1);
    let mut reader = resp.body_mut().as_reader();
    let mut out = fs::File::create(staging)
        .with_context(|| format!("creating {}", staging.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut done: u64 = 0;
    let mut last_sent: u64 = 0;
    loop {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let n = reader.read(&mut buf).context("reading response body")?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).context("writing download")?;
        hasher.update(&buf[..n]);
        done += n as u64;
        if done - last_sent >= (total / 100).max(4 * 1024 * 1024) {
            last_sent = done;
            let frac = (done as f64 / total as f64).min(1.0) as f32;
            let _ = tx.send(Progress::Download { label: spec.label, frac });
        }
    }
    out.flush().ok();
    drop(out);
    let got = hex_digest(hasher.finalize().as_slice());
    anyhow::ensure!(
        got == spec.sha256,
        "SHA-256 mismatch: expected {}, got {got}. The pinned source may have \
         changed since this build - nothing was installed.",
        spec.sha256
    );
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Land a verified staging file: a plain file renames into place; a zip
/// extracts its picked entries (each via its own `.part` + rename) and
/// optionally sweeps every DLL flat into a dir, then the archive is removed.
fn install_download(staging: &Path, install: &Install) -> Result<()> {
    match install {
        Install::File(dest) => replace_file(staging, dest),
        Install::Unzip { picks, dll_sweep_to } => {
            let res = extract_zip(staging, picks, dll_sweep_to.as_deref());
            let _ = fs::remove_file(staging);
            res
        }
    }
}

/// Move `from` over `dest` (Windows `rename` refuses an existing target, so
/// the stale file goes first — `from` is already verified at this point).
fn replace_file(from: &Path, dest: &Path) -> Result<()> {
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let _ = fs::remove_file(dest);
    fs::rename(from, dest)
        .with_context(|| format!("renaming {} to {}", from.display(), dest.display()))
}

/// Extract a pinned archive: entries are matched by slash-normalized,
/// case-insensitive name *suffix* (release zips nest under a versioned root
/// dir). Every pick must match — a miss means the upstream layout changed
/// under the pin, which must fail loudly rather than half-install.
fn extract_zip(
    zip_path: &Path,
    picks: &[(&'static str, PathBuf)],
    dll_sweep_to: Option<&Path>,
) -> Result<()> {
    let file =
        fs::File::open(zip_path).with_context(|| format!("opening {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("reading zip")?;
    let mut matched = vec![false; picks.len()];
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("reading zip entry")?;
        if !entry.is_file() {
            continue;
        }
        let raw = entry.name().replace('\\', "/");
        let name = raw.to_ascii_lowercase();
        let mut dest: Option<PathBuf> = None;
        for (k, (suffix, to)) in picks.iter().enumerate() {
            if !matched[k] && name.ends_with(&suffix.to_ascii_lowercase()) {
                matched[k] = true;
                dest = Some(to.clone());
                break;
            }
        }
        if dest.is_none() && name.ends_with(".dll") {
            if let Some(dir) = dll_sweep_to {
                let base = raw.rsplit('/').next().unwrap_or(&raw);
                dest = Some(dir.join(base));
            }
        }
        let Some(dest) = dest else { continue };
        let part = dest.with_file_name(format!(
            "{}.part",
            dest.file_name().and_then(|s| s.to_str()).unwrap_or("dep")
        ));
        if let Some(dir) = part.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let mut out = fs::File::create(&part)
            .with_context(|| format!("creating {}", part.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("extracting {raw}"))?;
        drop(out);
        replace_file(&part, &dest)?;
    }
    if let Some(k) = matched.iter().position(|m| !m) {
        anyhow::bail!(
            "the archive holds no '{}' - the pinned zip's layout changed; not installing",
            picks[k].0
        );
    }
    Ok(())
}

// --- output organization (ADR 0015) -----------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use yc_core::VodSource;

    #[test]
    fn ens_stage_labels_count_the_variants() {
        assert_eq!(ens_stage_label(1, 5), "Qwen ensemble — decode 1/5");
        assert_eq!(ens_stage_label(5, 5), "Qwen ensemble — decode 5/5");
        // Out-of-table shapes (variant set grew, or a zero index) fall back to
        // the countless label instead of panicking mid-render.
        assert_eq!(ens_stage_label(6, 5), "Qwen ensemble — decoding");
        assert_eq!(ens_stage_label(0, 5), "Qwen ensemble — decoding");
        assert_eq!(ens_stage_label(3, 7), "Qwen ensemble — decoding");
    }

    fn vod(creator: &str, title: &str) -> Vod {
        Vod {
            creator: creator.into(),
            title: title.into(),
            source: VodSource::YouTube { video_id: "abc".into() },
            language: Language::Id,
            duration_s: None,
        }
    }

    #[test]
    fn stream_dirs_lay_out_creator_title_and_data_subfolder() {
        let ws = Path::new("F:/ws");
        let (stream, data) = stream_dirs(ws, &vod("Joddy Barat", "Main Game / Live!"));
        // `<creator>/<title>` with the path-illegal '/' sanitized to '_'.
        assert_eq!(stream, ws.join("Joddy Barat").join("Main Game _ Live!"));
        // Intermediates live under a `data/` child of the stream folder.
        assert_eq!(data, stream.join("data"));
    }

    #[test]
    fn stream_dirs_fall_back_on_empty_metadata() {
        let ws = Path::new("F:/ws");
        let (stream, _) = stream_dirs(ws, &vod("", "..."));
        assert_eq!(stream, ws.join("unknown").join("untitled"));
    }

    #[test]
    fn clip_title_stem_prefers_the_title_else_a_timestamp_name() {
        let r = TimeRange { start_s: 754.0, end_s: 784.0 }; // 12:34
        assert_eq!(clip_title_stem(Some("He LOST it on the boss"), r), "He LOST it on the boss");
        // No title (manual / headless clip) -> deterministic timestamp name.
        assert_eq!(clip_title_stem(None, r), "clip-12-34");
        // A title that sanitizes to nothing also falls back to the timestamp.
        assert_eq!(clip_title_stem(Some("///"), r), "clip-12-34");
    }

    #[test]
    fn resolve_language_applies_the_creators_saved_default_only_on_auto() {
        let ws = std::env::temp_dir().join("yc_resolve_language_test");
        let _ = fs::remove_dir_all(&ws);
        fs::create_dir_all(&ws).unwrap();
        // Auto + unknown Creator -> the fallback.
        assert_eq!(resolve_language(&ws, "Somebody", None), AUTO_LANGUAGE_FALLBACK);
        // Auto + a saved Creator language -> the saved language (ADR 0016)...
        let mut store = CreatorStore::default();
        store.upsert(Creator::new("Somebody".into(), Language::Ja));
        store.save(&creators_path(&ws)).unwrap();
        assert_eq!(resolve_language(&ws, "Somebody", None), Language::Ja);
        // ...but an explicit operator pick always wins.
        assert_eq!(resolve_language(&ws, "Somebody", Some(Language::En)), Language::En);
        let _ = fs::remove_dir_all(&ws);
    }

    /// ADR 0063: a PRESENT-but-corrupt SER model must degrade to "Signal
    /// absent" (the call site warns and omits), never fail the detect that
    /// already paid for the whisper refine. The helper is the fallible unit:
    /// garbage bytes -> a contexted Err for the warn+omit arm, not a panic.
    #[cfg(feature = "ser")]
    #[test]
    fn arousal_refine_errs_on_a_corrupt_model_instead_of_failing_detect() {
        let dir = std::env::temp_dir().join("yc_arousal_corrupt_model_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let model = dir.join("model.onnx");
        fs::write(&model, b"not an onnx protobuf").unwrap();
        let err = arousal_refine(&model, Path::new("missing.wav"), &[], &CancelToken::new())
            .expect_err("garbage model bytes must fail the load");
        // The load's context names the model, so the warn+omit log says what broke.
        assert!(format!("{err:#}").contains("SER model"), "unexpected error: {err:#}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clock_stem_is_filesafe_and_handles_hours() {
        assert_eq!(clock_stem(0.0), "0-00");
        assert_eq!(clock_stem(754.0), "12-34");
        assert_eq!(clock_stem(3661.0), "1-01-01"); // past an hour
        assert!(!clock_stem(754.0).contains(':')); // no colon -> filesystem-safe
    }

    #[test]
    fn unique_path_dedupes_collisions_with_a_counter() {
        let dir = std::env::temp_dir().join("yc_unique_path_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // First is the bare name; each existing file bumps the counter.
        let p1 = unique_path(&dir, "My Clip", "mp4");
        assert_eq!(p1, dir.join("My Clip.mp4"));
        fs::write(&p1, b"x").unwrap();
        let p2 = unique_path(&dir, "My Clip", "mp4");
        assert_eq!(p2, dir.join("My Clip (2).mp4"));
        fs::write(&p2, b"x").unwrap();
        assert_eq!(unique_path(&dir, "My Clip", "mp4"), dir.join("My Clip (3).mp4"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_silent_clip_flags_only_near_silence() {
        // Pure silence and a tiny-noise floor read as silent (would crash whisper's
        // DTW); a clip with any real speech-level energy does not.
        assert!(is_silent_clip(&[0.0; 16_000]));
        assert!(is_silent_clip(&[0.001, -0.002, 0.003])); // dither / noise floor
        assert!(!is_silent_clip(&[0.0, 0.0, 0.2, 0.0])); // one speech-level peak is enough
        assert!(!is_silent_clip(&[0.5; 100])); // loud
        assert!(is_silent_clip(&[])); // empty -> nothing to transcribe, treat as silent
    }

    #[test]
    fn review_notes_save_and_load_roundtrip() {
        // M8: Detect's transcript + llm-reason maps persist to review.json and a
        // re-import loads them back, so the review panel restores without re-detect.
        let dir = std::env::temp_dir().join("yc_review_roundtrip");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let transcripts = HashMap::from([
            (1u64, "kaget banget gua".to_string()),
            (2u64, "baca menu doang".to_string()),
        ]);
        // Only Moment 1 got an LLM reason (Moment 2's judge ran without one).
        let llm_reasons = HashMap::from([(1u64, "shock reaction".to_string())]);

        save_review(&transcripts, &llm_reasons, &dir);
        assert!(dir.join("review.json").is_file(), "wrote the sidecar");
        let (t, r) = load_review(&dir);
        assert_eq!(t.len(), 2);
        assert_eq!(t[&1], "kaget banget gua");
        assert_eq!(t[&2], "baca menu doang");
        assert_eq!(r.len(), 1); // empty reasons are skipped, not stored blank
        assert_eq!(r[&1], "shock reaction");
        assert!(!r.contains_key(&2));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_project_preserves_prior_moments_on_reimport() {
        // M8: a re-import must not wipe a prior session's detected Moments.
        let dir = std::env::temp_dir().join("yc_save_project_preserve");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let v = vod("local", "stream");

        // Seed project.json as if a prior Detect had run.
        let mut prior = Project::new(v.clone());
        prior.moments.push(Moment {
            id: 3,
            range: TimeRange { start_s: 10.0, end_s: 40.0 },
            signals: Signals::default(),
            score: 1.5,
            title: Some("Big play".into()),
        });
        prior.save(&dir.join("project.json")).unwrap();

        // save_project (the import-time persist) must keep the Moment and return it.
        let returned = save_project(&v, &dir).unwrap();
        assert_eq!(returned.moments.len(), 1, "returned the preserved Moment");
        assert_eq!(returned.moments[0].id, 3);
        // And it's still on disk (not clobbered to an empty Project).
        let reloaded = Project::load(&dir.join("project.json")).unwrap();
        assert_eq!(reloaded.moments.len(), 1);
        assert_eq!(reloaded.moments[0].title.as_deref(), Some("Big play"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clip_id_for_is_distinct_per_range_start() {
        assert_eq!(clip_id_for(TimeRange { start_s: 100.0, end_s: 130.0 }), 100_000);
        assert_ne!(
            clip_id_for(TimeRange { start_s: 100.0, end_s: 130.0 }),
            clip_id_for(TimeRange { start_s: 500.0, end_s: 530.0 })
        );
        // Same start -> same id (a re-render dedups onto one record, not a new one).
        assert_eq!(
            clip_id_for(TimeRange { start_s: 4259.5, end_s: 4289.5 }),
            clip_id_for(TimeRange { start_s: 4259.5, end_s: 9999.0 })
        );
    }

    #[test]
    fn persist_clip_records_every_batch_render_not_just_the_last() {
        // The bug: a hardcoded Clip id made persist_clip's dedup overwrite each batch
        // render onto one record. Three distinct Moments must yield three Clip records,
        // each linked to its Moment, with no phantom Moments invented.
        let dir = std::env::temp_dir().join("yc_persist_clip_batch");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let v = vod("local", "stream");

        // Seed three detected Moments (as a Detect would), distinct ranges.
        let mut prior = Project::new(v.clone());
        for (id, start) in [(1u64, 100.0), (2, 500.0), (3, 900.0)] {
            prior.moments.push(Moment {
                id,
                range: TimeRange { start_s: start, end_s: start + 30.0 },
                signals: Signals::default(),
                score: 1.0,
                title: None,
            });
        }
        prior.save(&dir.join("project.json")).unwrap();

        let lay =
            || yc_core::Layout::FullFrame { crop: yc_core::Crop { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 } };
        // Render all three (the batch), each at its Moment's range.
        for start in [100.0_f64, 500.0, 900.0] {
            let r = TimeRange { start_s: start, end_s: start + 30.0 };
            let clip = build_clip(
                r,
                lay(),
                "huge-word",
                None,
                None,
                Vec::new(),
                None,
                &dir.join(format!("{start}.mp4")),
            );
            persist_clip(&v, &clip, &dir).unwrap();
        }
        let p = Project::load(&dir.join("project.json")).unwrap();
        assert_eq!(p.clips.len(), 3, "all three batch renders recorded, not just the last");
        assert_eq!(p.moments.len(), 3, "no phantom Moments added");
        let linked: std::collections::HashSet<u64> = p.clips.iter().map(|c| c.moment_id).collect();
        assert_eq!(linked, [1, 2, 3].into_iter().collect(), "each Clip links to its Moment");

        // A re-render of one Moment replaces its own record (still three, not four).
        let r2 = TimeRange { start_s: 500.0, end_s: 530.0 };
        let clip2 =
            build_clip(r2, lay(), "karaoke", None, None, Vec::new(), None, &dir.join("500b.mp4"));
        persist_clip(&v, &clip2, &dir).unwrap();
        let p = Project::load(&dir.join("project.json")).unwrap();
        assert_eq!(p.clips.len(), 3, "re-render replaces its own record");
        let m2 = p.clips.iter().find(|c| c.moment_id == 2).unwrap();
        assert_eq!(m2.caption_style, "karaoke", "the re-render's data won");

        // A directly-promoted range with no detected Moment records its own Moment.
        let r4 = TimeRange { start_s: 2000.0, end_s: 2030.0 };
        let clip4 = build_clip(
            r4, lay(), "huge-word", None, None, Vec::new(), None, &dir.join("direct.mp4"),
        );
        persist_clip(&v, &clip4, &dir).unwrap();
        let p = Project::load(&dir.join("project.json")).unwrap();
        assert_eq!(p.clips.len(), 4, "the direct promote adds a fourth Clip");
        assert_eq!(p.moments.len(), 4, "and records a Moment for the un-detected range");

        let _ = fs::remove_dir_all(&dir);
    }

    // --- dependency downloads (ADR 0041) --------------------------------------

    /// Build a zip like a pinned release archive: entries nested under a
    /// versioned root dir, stored (no compression feature needed to write).
    fn test_zip(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;
        let file = fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn extract_zip_picks_by_suffix_and_sweeps_dlls() {
        let dir = std::env::temp_dir().join("yc_dl_extract");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("pkg.zip");
        test_zip(
            &zip_path,
            &[
                ("ffmpeg-9.9-essentials_build/bin/ffmpeg.exe", b"FF".as_slice()),
                ("ffmpeg-9.9-essentials_build/bin/ffprobe.exe", b"PR".as_slice()),
                ("ffmpeg-9.9-essentials_build/lib/helper.DLL", b"DL".as_slice()),
                ("ffmpeg-9.9-essentials_build/README.txt", b"no".as_slice()),
            ],
        );
        let picks = vec![
            ("/bin/ffmpeg.exe", dir.join("out").join("ffmpeg.exe")),
            ("/bin/ffprobe.exe", dir.join("out").join("ffprobe.exe")),
        ];
        extract_zip(&zip_path, &picks, Some(&dir.join("dlls"))).unwrap();
        assert_eq!(fs::read(dir.join("out").join("ffmpeg.exe")).unwrap(), b"FF");
        assert_eq!(fs::read(dir.join("out").join("ffprobe.exe")).unwrap(), b"PR");
        // The sweep keeps the entry's own basename (case preserved) and the
        // README is not extracted at all.
        assert_eq!(fs::read(dir.join("dlls").join("helper.DLL")).unwrap(), b"DL");
        assert!(!dir.join("out").join("README.txt").exists());
        // Extraction replaces an existing (stale) install.
        test_zip(&zip_path, &[("v2/bin/ffmpeg.exe", b"F2".as_slice()), ("v2/bin/ffprobe.exe", b"P2".as_slice())]);
        extract_zip(&zip_path, &picks, None).unwrap();
        assert_eq!(fs::read(dir.join("out").join("ffmpeg.exe")).unwrap(), b"F2");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_zip_fails_loudly_when_a_pick_is_missing() {
        let dir = std::env::temp_dir().join("yc_dl_missing");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("pkg.zip");
        test_zip(&zip_path, &[("root/bin/ffmpeg.exe", b"FF".as_slice())]);
        let picks = vec![
            ("/bin/ffmpeg.exe", dir.join("ffmpeg.exe")),
            ("/bin/ffprobe.exe", dir.join("ffprobe.exe")),
        ];
        let err = extract_zip(&zip_path, &picks, None).unwrap_err();
        assert!(err.to_string().contains("ffprobe"), "names the missing pick: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hex_digest_matches_known_sha256() {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        h.update(b"abc");
        assert_eq!(
            hex_digest(h.finalize().as_slice()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn replace_file_overwrites_an_existing_target() {
        let dir = std::env::temp_dir().join("yc_dl_replace");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let from = dir.join("new.part");
        let dest = dir.join("tool.exe");
        fs::write(&from, b"new").unwrap();
        fs::write(&dest, b"old").unwrap();
        replace_file(&from, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        assert!(!from.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
