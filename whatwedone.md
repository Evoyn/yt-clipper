# What we've done

A running, readable log of completed features — **newest first**. Each session appends a dated entry here when a feature is finished (alongside the detailed `handoffs/`, which are gitignored). This file is committed so you can read it any time.

---

## 2026-06-30 — voice isolation for captions (the W3 thread, unblocked)

You pasted the real bad-caption VOD (the 2 h horror co-stream "Horror Tanpa
Ekspresi bersama @guntur69") and asked to export the top 10. That gave the corpus
the voice-isolation work needed — and we measured our way to a shipped fix. Newest
first.

- **Captions can clean the streamer's voice before whisper now** (cleaned-voice
  captions, the big one) — on a noisy gameplay clip the game SFX used to drown the
  voice and whisper invented gibberish (clip #7 literally hallucinated *English* —
  "onions… comic book"). A **gentle speech denoiser** (DeepFilterNet, run as a
  sidecar like ffmpeg) now cleans the caption audio first, so whisper reads the
  voice above the noise. The result on #7: coherent Indonesian, no more English.
  **Crucially we measured before shipping** — *full*-strength denoise *destroyed*
  the speech (it gouged the quiet voice to silence and whisper looped "eh" 211
  times), so it's tuned to a gentle setting that recovers masked speech while
  leaving already-clean clips untouched. Off by default (`--features enh`); the
  clip's audible audio is always the original mix. (ADR 0029)
  **Caveat (found on the real render):** the spike validated on the clip's
  re-encoded audio, but production denoises the cleaner *Segment* audio — on which
  denoising over-processed and made #7's opening *worse*. enh is off-by-default and
  **not** validated on the real path. The remaining errors are linguistic, not
  noise — so the next lever is an **LLM correction pass** (spiked, 3/4 of your
  clip-7 fixes exact with zero over-correction; ADR 0030), to be integrated next.
- **A flaky download no longer kills a whole batch render** — the top-10 export
  died at clip 4 when YouTube returned a one-off "format not available" on the
  segment fetch; the batch treated it as fatal and threw away 6 good clips. The
  fetch now **retries** transient failures (they happen before any bytes download),
  so a single hiccup costs a few seconds, not the run. Re-running, all 10 exported —
  2 transient failures silently absorbed. (ADR 0028)
- **Exported your top 10** from the guntur69 VOD (Indonesian, huge-word captions,
  auto two-person facecam framing) — which doubled as the bad-caption corpus: the
  loudest clips had the worst captions, exactly the audio-masking ADR 0027 predicted.

## 2026-06-28 — round 2 (post-v1.1): UX polish, framing, caption accuracy

A second operator-feedback batch — running W1→W4 (hide consoles → two-person
facecam → caption accuracy → SaaS GUI) as an autonomous loop, same ritual. Newest
first.

- **The GUI is a two-pane "clip workspace" now** (SaaS redesign, done) — the old
  single scrolling column became a proper app: a **top bar** (gold "yt-clipper"
  brand + a live status — spinner/Done/Failed), a **left rail** (diagnostics that
  collapse when all's well, Import, and a compact selectable Moments list), and a
  **detail/preview pane** on the right (the VOD waveform + the selected Moment's
  signals, title, transcript, audio, and a "Promote → Frame & Render" button). On
  the dark "midnight studio" theme. (ADR 0024)
- **Captions decode with beam search now** (caption-accuracy, part 1) — the render
  path was using whisper's *fastest, lowest-quality* decoder; since you said render
  time is no object, it now uses **beam search** (whisper's quality decoder) for the
  captions, while detection stays fast. Diagnosing first showed whisper is already
  accurate on clear clips — the wrong captions are mostly from **game noise** on
  gameplay clips, so the bigger fix is **voice isolation** (cleaning the audio before
  whisper); that + an optional LLM auto-correct are queued for your steer with a real
  bad-caption clip. (ADR 0027)
- **Two-person cams frame both people now** — on a co-stream cam (e.g. "Horror
  Tanpa Ekspresi bersama @guntur69"), auto-framing used to zoom into one of the two
  people, tight enough to look soft. It now detects **everyone** on the cam and
  frames their **union** in the stacked facecam panel, so both are in shot (and the
  wider crop is sharper). A solo cam is unchanged. Verified on your actual VOD: both
  streamers framed side-by-side. (ADR 0026)
- **No more console windows flashing during a render** — the GUI spawns ffmpeg /
  yt-dlp / the judge as helpers, and on Windows each was popping its own console
  window mid-render. They now run hidden. Verified on a real release render: zero
  console windows appeared. (ADR 0025)

## 2026-06-28 — operator-feedback pass (post-v1)

Working the operator's 8 priority items from real-world testing of v1, one feature
per iteration (grill → implement → live-verify → commit). Newest first.

- **The app has a real look now** — replaced egui's default gray with a "midnight
  studio" dark theme: a cool-slate palette with the **caption gold (#FFD100)** as
  the one accent, and the **yt-clipper heading in gold Anton** (the same font the
  captions burn) as the signature. Roomier spacing, a clear type scale, cleaned-up
  copy. Live-verified by launching the GUI and screenshotting it. Layout and every
  control are unchanged — just the skin. (ADR 0024)
- **Common viewer names stop cluttering the review queue** — a correctly-read name
  (Budi, Siti, Wahyuni, John, Kayla) used to get flagged as a "garble" because it
  isn't in the Indonesian/English word lists. Bundled a common-given-names list
  (`names.words.txt`, ~5,100 Indonesian + international names, always loaded) so
  common names are now recognized; only an **unusual** handle still harvests (the
  one actually worth curating — and item 6 now tags it with where it was said).
  Complements your per-streamer `names.json` roster. (ADR 0023)
- **The dialect review queue (`id.json`) is curatable now** — each auto-harvested
  word used to be just `{wrong, right:""}` with no clue where it came from. Now its
  note records the **source**: the generated Short title + the absolute VOD
  timestamp, e.g. `from "He LOST it on the boss" at 1:23:45`, so you can jump
  straight to the clip and decide what it should say. (ADR 0022)
- **Quiet speech and viewer names get captioned now** — on a clip with loud
  reactions, a quietly-spoken word (a whispered name) was being dropped because the
  "is this silence?" bar was set relative to the loudest moment, so quiet-but-real
  speech fell under it (and the dropped word left a gap that wobbled the timing).
  Added an absolute floor below the relative bar: real quiet speech survives, true
  silence still drops. Measured on a real clip: the word "Terus" (dropped at peak
  0.0091, just under the bar) is now kept, with no timing gap. (ADR 0021)
- **Loud clips start earlier, for the build-up** — a jumpscare or loud donation
  clip used to begin a fixed 5s before the loud peak. Now the pre-roll scales with
  how loud the peak is — up to ~10s for a loud-driven Moment, so the lead-up is
  captured — while a chat-driven Moment keeps the ~5s (chat lags the event). Window
  spacing was widened so the longer leads never make two candidate clips overlap.
  Measured on a real 105-min VOD: all 25 detected Moments are loud-driven and now
  open with the full 10s pre-roll. (ADR 0020)
- **Captions no longer jump ahead of the audio** — sometimes a word appeared a
  beat before it was spoken. whisper's word-onset timing occasionally runs early;
  now each caption's start is nudged **forward** to where the audio actually rises
  (bounded to 0.2s, never past the next word), so a caption never precedes its
  sound. One fix covers all three caption styles. Measured on a real clip: 11 of
  27 words were leading (by up to 0.2s); all corrected. (ADR 0019)
- **Karaoke captions snap per word** — the karaoke style used a smooth
  left-to-right fill that looked like "a loading bar filling." Now each word
  **snaps** to the accent colour as a whole the moment it's spoken (and stays
  lit), instead of sweeping. Same timing, punchier read. Live-verified: a frame
  strip over "DAN LAGI, TIDAK ADA" shows whole words turning gold one-by-one,
  none ever half-filled. (ADR 0018)
- **Layout is now selectable** — your preferred stacked framing (gameplay on top,
  facecam below) stopped appearing because M6 auto-detect falls back to full-frame
  whenever it can't confidently find a corner cam (and `--batch` never opens the
  nudge editor to fix it by hand). Added an explicit **Layout** menu — *Auto* /
  *Stacked* / *Full cam* / *Full gameplay* — in the GUI top bar, and a matching
  `--batch`/`--headless` CLI token (`stacked` etc.). *Auto* keeps the smart
  auto-detect; pick *Stacked* and every clip is framed game-on-top, cam-below,
  including in batch. Live-verified on a real horror clip: forced *stacked* and
  *gameplay* render visibly different layouts. (ADR 0017)

## 2026-06-28 — v1 complete (M0–M8), one autonomous `/loop` run

Took the project from post-M6 caption work to **functionally complete v1**. Everything below is committed (13 commits); ship with `git push origin main`. All 111 unit tests pass and every feature was live-verified on real Indonesian gaming VODs.

- **Output organization + generated titles** — each clip lands in `workspace/<creator>/<stream-title>/`, named by a catchy ≤60-char title the LLM judge writes from the transcript, so re-promotes never overwrite. Intermediates moved under `data/`. (ADR 0015)
- **Karaoke-fill caption genre** + caption-style **selection** — pick huge-word / rolling-pop / karaoke-fill globally, per-Creator, or per-Clip in the editor.
- **Per-Creator defaults** (`creators.json`) — a streamer's caption style is remembered and pre-selected next import. (ADR 0016)
- **Japanese captions** — character-chunk grouping, so EN / ID / **JA** all caption end-to-end.
- **Re-import restores your work** — detected Moments + their transcripts + LLM reasons reload from `project.json` + `review.json`, no re-detect.
- **No-speech promote no longer crashes** — a silent clip used to abort whisper's DTW; now it just renders without captions.
- **Batch render** — a headless `--batch <vod> [lang] [genre] [k]` mode + a GUI multi-select "Render N selected (auto-framed)" queue, rendering Shorts sequentially.
- **Multi-word dialect corrections** — the correction dict now handles phrases ("point blank" → "Point Blank"), not just single words. (ADR 0014)
- **Release build verified** — `scripts\build-release.bat` produces the fast optimized binaries.

(Earlier milestones M0–M6 — skeleton, YouTube ingest, detection ensemble, arousal + LLM-judge signals, auto-framing — predate this run; see `docs/ROADMAP.md`.)
