# What we've done

A running, readable log of completed features — **newest first**. Each session appends a dated entry here when a feature is finished (alongside the detailed `handoffs/`, which are gitignored). This file is committed so you can read it any time.

---

## 2026-07-02 (overnight 5) — the GPU-job repaint throttle now actually throttles

The app was supposed to drop to ~10 fps while whisper/NVENC runs (so the UI doesn't
fight the render for the one graphics card) — that throttle shipped back on June 19. It
turns out it never engaged: the little loading **spinner** is an animated egui widget
that demands a fresh frame every frame it's visible, which quietly overrode the
throttle the whole time. The status bar now draws an identical-looking spinner that
doesn't do that, so a running job finally repaints at 10 fps (the spinner just animates
a bit less silkily — that's the trade, and the point). Detects/renders with the GUI open
should feel less like the GPU is being strangled.

---

## 2026-07-02 (overnight 4) — Cancel's hot flag no longer takes a lock

Internal engineering (the M5 follow-up): the "did the operator hit Cancel?" check —
polled constantly during detection and while waiting on the LLM sidecar — was locking a
Mutex on every call. It's now a lock-free atomic flag; the child-process registry (the
part that actually kills a download mid-flight) keeps its lock, with the ordering that
makes "cancel lands exactly as a child spawns" safe spelled out and unit-tested for the
first time. No behavior change.

---

## 2026-07-02 (overnight 3) — importing a known streamer now remembers their language

The Language picker gained an **"Auto (Creator's saved)"** option — and it's the new
default. Import a streamer the app has rendered before and it applies the language you
last used for them (from `creators.json`, which was recording languages all along but
never reading them). Picking English/Bahasa/Nihongo explicitly still always wins, same
for the CLI (`--headless <url> ... id` forces Id; omit the token for Auto). The Moments
header now shows which language the import resolved to, so Auto is never a mystery.
(ADR 0016; CONTEXT.md updated.)

---

## 2026-07-02 (overnight 2) — the caption corrector now knows WHO is talking and WHAT the stream is

The smart LLM caption pass used to be handed a useless "topic": a description of a
settings file ("Generic Indonesian base store...") instead of anything about the clip.
That blind context is part of why it fumbled the doubled-word fix. Now it gets the real
thing — the clip's title, the streamer's name, and the stream's title (which names the
game and guests). The offline preview tool (`correct_diag`) reads the same names from
the project file and sends the exact same request, so it stays honest, and prints the
topic so you can see it. **Wants your A/B the next time you render with correction on**
(the richer topic can change which words Qwen decides to fix). (ADR 0030.)

---

## 2026-07-02 (overnight 1) — a caption fix can now be scoped to ONE clip, without touching the streamer's other clips

Autonomous overnight run, item 1. Earlier today's clip-31-22 fix (`pancingan → bajingan`)
had a catch: any per-clip correction automatically **promotes** to the streamer's shared
file — so a fix that's only right for one clip would wrongly apply to *all* their clips
(I had to undo that by hand). Now a correction can be marked **clip-only**: it fixes just
that clip and never spreads. Backward-compatible (every existing file still loads),
unit-tested. This makes today's manual workaround automatic and durable. (ADR 0031.)

---

## 2026-07-02 — tried to fix the "Diskusi biasa" clip's slang; mapped why the corrector can't reach it (and made the checker honest)

Goal: finish item "A" — teach the caption corrector this clip's leftover slang
(`pancingan→bajingan`, `banget→bangke`, `tadi luar→dah cok`, `biasa kamu→biadab anjing`)
and A/B it with you. Outcome: **none of the four could be fixed cleanly — but I can now
show you exactly why.** That's a real result, just not the hoped-for one. Your instinct
to render before signing off is what caught it.

- **Two are two-word phrases** (tadi luar, biasa kamu). The smart LLM pass only ever swaps
  **one word for one word** — it structurally can't do a two-word fix. (Two-word fixes only
  exist as a blunt global find-replace, which we won't use on real words.)
- **The other two are words whisper said twice in a row** (`pancingan pancingan`,
  `banget banget`). The LLM fixes a word "only where it fits the meaning" — and on two
  identical back-to-back words it can't tell them apart, so on the **real render it fixed
  only one of the two** (`PANCINGAN BAJINGAN` — worse than leaving both). So the pass is
  reliable for slang that appears **once** (last clip's cok/tur/teh), not for whisper's
  doubled words.
- `banget` we wouldn't touch regardless — it's one of the commonest Indonesian words
  ("very"), so a rule on it would misfire everywhere.

**Nothing was added to the streamer's correction file** — this clip is, as you said, near
whisper's limit, and its residual slang isn't cleanly reachable. Your already-confirmed
fixes (dicegat, anjing ngeri) still work here.

**Fixed the checker that briefly fooled us.** The offline correction preview (`correct_diag`)
was reading the wrong dictionary (the empty base, not your streamer's file) **and** using a
different "topic" than the real render — so it claimed the pancingan fix worked (2 of 2)
when the render only did 1 of 2. Both fixed: it now reads your layered store and sends the
*exact* request the render sends, so it can't give false confidence again (same class of
"the tool was lying" bug we killed in the caption checker last session). Also flagged for
later: the real render hands the LLM a near-useless "topic" (a description of a settings
file instead of the streamer/game context) — worth fixing so it has real context next time.

---

## 2026-07-01 (later 4) — the caption inspector (caption_diag) now tells the truth

Last session the caption-checking tool `caption_diag` **lied**: on the guntur69 clip it
claimed 44 of 55 caption words were being *dropped* — including the curated ones
(dimarahin, cowok, tidur, horor) — when the render actually drops none of them. That false
report is what steered a wrong recommendation. The bug was in the tool, not the captions.

The tool used to *guess* which words the render kept by matching timings, and that guess
broke on fast, closely-spaced words (a rapid "eh eh eh" run) — one bad match cascaded into
a wall of fake "DROP"s. Now it reads the render's **actual** keep/drop decision directly,
so it can't guess wrong. Re-run on the same clip: **55 of 55 kept, 0 dropped** — correct.
It also prints a self-check line (kept + dropped = total) so a desync can never hide again.
No change to any rendered Short — this only fixes the diagnostic you use to sanity-check
captions before curating.

---

## 2026-07-01 (later 3) — tested cleaned-voice (enh) on a hard clip: recovers, but trades for garbles

You flagged that the "Diskusi biasa" clip still has bad captions — missed speech (23–26s)
and misplaced words. I tested the cleaned-voice denoiser (enh) on it end-to-end. Result:
it **does** recover the missed speech and fix the timing (BANGKE at 20s, "kamu main dulu",
the 23–26s hole) — **but it scrambles other clean words** (it turned the correct "dicegat"
back into a garble "dijegan"). That's the same trade-off that shelved enh before, now
confirmed on a 2nd clip's real render. So enh **stays off**; the mixed audio + your
corrections is the cleaner result. This clip's missing bits are a fundamental audio limit
(fast speech buried under game SFX) — the real fix would be the streamer's separate mic,
which the VOD doesn't have. Both versions are kept side by side to compare:
`clip-31-22.mp4` (normal) vs `clip-31-22 (2).mp4` (cleaned-voice).

---

## 2026-07-01 (later 2) — review queue surfaces fresh per-clip harvests

The Caption review queue used to show only the per-Creator backlog. Now it also surfaces
the to-dos harvested into each rendered clip's own file — so after you render new clips,
their new garbles appear in the queue automatically (grouped by clip, deduped against what
you've already confirmed). Also fixed a lurking bug where a stale clip-level to-do could
silently cancel a confirmed correction of the same word at render time.

---

## 2026-07-01 (later) — bug fix: batch renders all recorded in project.json

A batch render ("Render N selected" in the GUI, or `--batch`) now records **all N clips**
in `project.json` — before, it only ever kept the **last** one. Every render was tagged
with the same hardcoded id, so each overwrote the previous record. Now each clip is keyed
to its Moment, so all N coexist; a re-render of the same clip still replaces its own entry
(no duplicates). Internal bookkeeping only — no change to the rendered Shorts themselves.

---

## 2026-07-01 — Caption review queue in the app (curate to-dos without opening JSON)

You said id.json is hard to curate. Now you don't open it. Import a streamer's VOD and
the detail pane shows a **Caption review queue** — every word whisper misheard that's
still waiting on you (17 for Ino/@guntur69 right now), grouped by the clip each came from.

- **Fill the right word, hit Save.** No JSON. Each row shows the garble, how unsure
  whisper was, and a **click-to-hear** link that jumps straight to that moment in the
  YouTube VOD so you can check what was actually said.
- **A saved fix sticks for the whole streamer right away** — it writes their
  `workspace/<creator>/id.json` directly (no re-render needed), so it applies to every
  future clip of theirs.
- **Tick "context"** for a real word the streamer means as slang or a name (like
  cowok → cok) so it goes through the smart LLM pass, not the blunt global find-replace.
- Save is greyed out while a job runs, so it can never collide with a render.

No flags, no build features — it's just there after you import. (ADR 0032.)

---

## 2026-06-30 (latest+) — you signed off the correction pass (90%); last residue fixed

You watched the curated-only `clip7_B_WITH-correction.mp4` and called it **90% correct**.
The two remaining notes are both handled:
- **"itu" residue ("yang horor itu" → "yang horor"):** you confirmed whisper *always*
  tacks a spurious "itu" after "horor" for this streamer, so a deterministic per-Creator
  rule (`horor itu → horor`) now drops it — verified: the stray "itu" is gone, the
  real "itu tur" stays. The pass still never deletes a real word on its own (the safety
  that keeps it from eating good captions); this is your confirmed, targeted rule.
- **"diam dulu" not captioned:** that's whisper never hearing it (game masks the voice),
  not the correction — unfixable from the mixed VOD without a separate mic track.

The correction pass is **validated and stays off-by-default by your choice** (tick the
GUI checkbox, or `YC_CORRECT=1` headless). Curate going forward in the small per-clip
files; confirmed fixes auto-promote to the streamer's store.

---

## 2026-06-30 (latest) — per-Creator + per-clip caption dictionaries; GUI correction toggle

You asked for an id.json per exported clip (easy to curate) plus a per-Creator one, and
a GUI toggle for the LLM correction. Both done.

- **Each export now gets its own small correction file.** Beside every rendered Short, a
  `<ClipName>.id.json` collects just *that clip's* uncertain words (each noted with the
  title + VOD timestamp it came from). You curate that short, focused list instead of one
  giant global pile.
- **Confirmed fixes stick for the whole streamer.** When you fill in the right word in a
  clip's file, the next render copies it up to that Creator's own
  `workspace/<creator>/id.json` — so it applies to every future clip of theirs,
  automatically. Curate once, fixed forever for that streamer.
- **Three layers stack, most-specific wins:** the shared bundled dictionary, then the
  Creator's file, then the clip's file. A fix for one streamer no longer leaks to others.
- **Migrated your existing fixes:** all of Ino/@guntur69's confirmed slang/names + harvest
  to-dos moved out of the shared `assets/dialect/id.json` into their own
  `workspace/Ino Gemink Live Streaming/id.json`, so the bundled base is now purely
  generic. Your per-Creator stores are version-controlled (a `.gitignore` exception),
  so your curation is backed up; the rest of `workspace/` stays ignored.
- **A "Correct captions (LLM)" checkbox** in the GUI (default **off** — opt-in) runs the
  correction pass for a render; for headless/batch use `YC_CORRECT=1`. Still needs a build
  that includes the feature + the LLM sidecar. (ADR 0031)

---

## 2026-06-30 (later) — the LLM caption-correction pass, integrated + tuned to your A/B

The remaining caption errors are *linguistic* — local slang, names, and garbles that
no audio cleanup fixes — so we built the correction pass designed last session
(ADR 0030), you A/B'd a real render, and we tuned it to exactly what your ground-truth
showed. It is **OFF by default** and **awaiting your sign-off**.

- **What it does now (deliberately narrow + safe):** after whisper, the local LLM
  applies *only your confirmed slang/name corrections, in context* — the one thing a
  blind dictionary can't do (e.g. `cowok -> cok`, `tidur -> tur`, `teh -> eh`-reaction
  vs "teh"=tea). Plain garbles are fixed by the **dictionary** once you confirm them,
  not by the LLM guessing.
- **Why narrow:** your first A/B caught the LLM *guessing wrong* on words it can't know
  (it turned the guest "Guntur" into "buntut", `dakenyang` into "dakanya") and
  *collapsing your real repeated "eh" reactions*. So we removed the guessing and the
  collapse — the LLM now never invents, and repeated reactions stay.
- **Re-rendered clip-7, every error you flagged is fixed:** "Hai buntut dakanya" ->
  **"Guntur dah kenyang"**, "teh" -> **"eh"**, the repeated **"eh" reactions kept**,
  plus `cok` / `tur` / `dimarahin`. Two clips are in the guntur69 folder to compare:
  `clip7_A_NO-correction.mp4` vs `clip7_B_WITH-correction.mp4`.
- **Two known limits (not linguistic):** the stray "itu" in "yang horor itu" stays
  (a real word — the pass never deletes), and "diam dulu diam dulu" is still missing
  because whisper never heard it (an audio-masking problem, not a word problem).
- **The dictionary is now self-improving for you:** every garble whisper is unsure of
  gets queued in `assets/dialect/id.json` with where it came from; you fill the right
  word once and it's fixed forever after. We seeded your clip-7 confirmations. (ADR 0030)

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
