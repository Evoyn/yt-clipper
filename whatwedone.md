# What we've done

A running, readable log of completed features — **newest first**. Each session appends a dated entry here when a feature is finished (alongside the detailed `handoffs/`, which are gitignored). This file is committed so you can read it any time.

---

## 2026-06-28 — round 2 (post-v1.1): UX polish, framing, caption accuracy

A second operator-feedback batch — running W1→W4 (hide consoles → two-person
facecam → caption accuracy → SaaS GUI) as an autonomous loop, same ritual. Newest
first.

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
