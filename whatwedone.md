# What we've done

A running, readable log of completed features — **newest first**. Each session appends a dated entry here when a feature is finished (alongside the detailed `handoffs/`, which are gitignored). This file is committed so you can read it any time.

---

## 2026-06-28 — operator-feedback pass (post-v1)

Working the operator's 8 priority items from real-world testing of v1, one feature
per iteration (grill → implement → live-verify → commit). Newest first.

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
