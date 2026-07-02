# What we've done

A running, readable log of completed features — **newest first**. Each session appends a dated entry here when a feature is finished (alongside the detailed `handoffs/`, which are gitignored). This file is committed so you can read it any time.

---

## 2026-07-02 — productization sweep: the Studio editor, Podcast Mode, natural clip lengths, better titles (focus.md, all 6 tasks)

The focus.md session: park `nextprompt.md`, make the app feel like a product.
Six tasks, one autonomous run (ADRs 0037/0038/0039; CONTEXT.md grew Studio /
Camera mode / Speaker track / Camera plan / Caption preset / Transcript
override).

- **Why every clip was ~30 s (task 5)**: `DetectParams::dur_s = 30.0` — a
  fixed window around every peak. Replaced with an **adaptive sustain window**
  (ADR 0037): the range grows over the span where the combined signal stays
  elevated (0.45×peak, floored at 0.5σ), pads the build-up + tail, floors at
  15 s and caps at a configurable max — GUI slider 30–180 s (the Shorts
  ceiling), `YC_MAX_CLIP_S` headless, default 90. Fixed-gap NMS became
  overlap suppression (trim weaker windows, drop swallowed peaks). Sharp
  spikes still read ~15–20 s; sustained arcs now surface at 40–90 s.
- **Titles that upload as-is (task 6)**: the judge prompt's title section
  rewritten — hook first and concrete, curiosity without overpromise, strong
  verbs / present tense, one ALL-CAPS word max, a named ban list (Insane /
  Epic / You Won't Believe / Gone Wrong / …), no hashtags/quotes/emoji, good
  shapes shown. Prompt-guard tests pin the new clauses like the scripted-
  cutscene mitigation.
- **Podcast Mode (task 3, ADR 0038)**: `yc_frame::speaker` — face tracks from
  streamed 5 fps frames (new `stream_frames_rgb`, one frame resident), per-bin
  **mouth activity** (fixed-grid luma diff in the lower face box), an RMS
  voice gate, and attribution with a 1.35× switch margin held 0.8 s. The
  timeline becomes a **cut-based CameraPlan**: min shot 2.4 s, flickers
  absorbed, rapid exchanges collapse into a group shot (2 people = stacked
  split screen). Rendered as trim/crop/concat in one ffmpeg pass
  (`camera.fg` via `-filter_complex_script`), captions burned once —
  validated end-to-end on the pinned ffmpeg (solo → split → solo, 1080×1920,
  frame-accurate cuts). `Job::AnalyzeSpeakers` auto-queues when Prepare sees
  2+ faces; release builds now carry `--features face`.
- **The Studio (tasks 1/2/4, ADR 0039)**: Promote opens a full-window editor
  page — toolbar (Back · title · **Preview/Original** Before/After toggle ·
  captions/safe-area · Export), **transcript editor** left (every caption a
  row: `m:ss.cc` editable timestamps, text, split/merge/censor/delete,
  add-at-playhead; edits burn **verbatim** via `transcript_override` — no
  whisper, no re-timing over operator words), preview center (Original =
  source frame + draggable/corner-resizable/scroll-zoom 9:16 crop box +
  rule-of-thirds + face overlays with Person A/B/C labels + click-to-retarget;
  Preview = composited 9:16 + caption overlay + safe-area guide + tracking
  chip "Tracking Person B · 96%"), properties right (5 **Camera modes** —
  Manual/Center/Auto face/**Active Speaker**/Group; framing; **6 caption
  presets** — Classic/TikTok/Podcast/Minimal/Gaming/MrBeast — over the
  extended CaptionStyle: outline width+colour, shadow, back box, bold, all
  serde-defaulted so old styles render byte-identical), timeline bottom
  (ruler, caption blocks, per-speaker lanes, cut markers, scrub +
  keyboard: space/arrows/±/0). **Export opens a summary** (length,
  resolution, captions, camera, tracking, estimated render time) before
  rendering. Prepare auto-queues `Job::Transcribe`, so captions are editable
  pre-render and the render is NVENC-only.
- **Shell modernized (task 1)**: theme grew a design system (section kickers,
  cards, primary/status-chip/segmented helpers, WELL/INFO/track colours);
  the library page reorganized (import card up top, defaults grid, moments
  list with duration+score badges, signal chips in the detail pane); status
  is a coloured chip; preflight moved to the rail bottom.
- **Validated**: 231 workspace tests green (29 new in `speaker`, adaptive-
  range + overlap suppression, camera filtergraph, presets, mm:ss.cc
  parsing, censor/split, ASS style-line byte-compat golden); headless
  end-to-end render on a synthetic VOD through the reworked pipeline; the
  camera-cut graph rendered + frame-inspected; GUI smoke-launched.
- **Known limits**: speaker attribution is visual (overlap → the most
  animated mouth wins; off-screen voices hold the shot) — diarization is the
  named upgrade path; preview playback is still the ~4 fps filmstrip (360p
  for >60 s clips); per-Creator memory still stores genre only, not full
  custom styles; transcript edits don't feed the dialect store (per-clip
  fixes vs durable curation stay separate lanes).

---

## 2026-07-02 (late night) — the ensemble becomes a product feature: per-Creator Caption engine picker

ADR 0035 slice 1 of 3 (picker → decode cache → harvest), the queued session you
picked in a live grill (the detail forks then resolved AFK — veto in the
handoff): the Qwen ensemble stops being an env secret and becomes a per-Creator
choice the app remembers.

- **`Caption engine` on the Creator** (`CaptionEngine::Whisper | QwenEnsemble`
  in yc-core): serde-default Whisper and skipped when unflipped, so every
  existing creators.json loads AND re-saves byte-identical. No default flip —
  a new Creator always starts on Whisper, even if your last render was
  ensemble.
- **An Engine picker in the import rail** next to Caption Style: seeded from
  the Creator store on import, saved back per Creator on each render (the
  ADR 0016 pattern), explicit pick always wins.
- **The switch warn, computed from the real appliers**: flip a curated
  Creator's engine and an inline gold line quantifies what carries —
  single-word + `at_s`-pinned corrections transfer, multi-word + context stay
  whisper-only (and pinned fixes "go dormant" in the reverse direction). The
  counts mirror the ensemble code's own filters, including the subtlety the
  ADR prose missed: a multi-word wrong WITH a pin carries (your "blok on"
  rulings). guntur69 today: 20 carry / 10 stay.
- **`YC_QWEN_ENS` became tri-state** (AFK-sharpened into ADR 0035 §6): `1`
  forces ensemble, `0` forces whisper, unset defers to the Creator — so the
  whisper gate fixtures stay runnable after you flip Deddy for real. The
  override is per-invocation and never saved back.
- **Engine flips re-transcribe**: the per-Prepare transcript cache now knows
  which engine made it, so picker-flip + Render actually changes words
  (before, it would have silently reused the old transcript).
- **Validated on the production path** (3 headless Deddy control renders):
  env-unset whisper render byte-identical to last night's baseline (`fc /b`);
  store-flipped Deddy ran the ensemble with NO env (your 4 at_s pins applied:
  juga / blo'on ×2 / goblok) and save-back kept the flip + every other
  Creator; `YC_QWEN_ENS=0` forced whisper over the flipped Creator and
  reproduced the baseline byte-for-byte, without un-flipping the store.
- 204 fast tests green (+3); release `correct,face` build green.

**Your part**: flip guntur69 + Deddy to the ensemble yourself in the GUI (the
picker now exists for exactly that), eyeball the warn numbers, and the still-
pending interactive pass on last night's editor. Next queued: decode cache
(~90 s → ~1 s re-renders), or editor slice 2. Entry point: `nextprompt.md`.

---

## 2026-07-02 (night) — the editor grew eyes: caption preview playback + drag-anywhere placement

You asked for it mid-ritual ("preview video and a captions editor…drag to
move and change size") and it exists tonight — designed in an operator-AFK
grill (every fork went to the recommended option; veto freely, the handoff
lists them) and shipped as ADR 0036 slice 1:

- **The nudge editor now plays the clip**: the 7-frame scrub became a ~480p
  filmstrip with a real playhead — Play/Pause/scrub with the clip's actual
  audio (the same rodio path Moment review uses), so you can *watch and hear*
  the captions to catch the wrong ones.
- **Captions draw over the composite from the render's own model**: the new
  `preview_lines` + `word_states` in yc-render feed BOTH the ASS burn-in and
  the egui overlay — grouping, timing, position, size, karaoke snap are the
  same code, so the preview can't drift. (Glyphs/animation approximate; the
  MP4 stays ground truth.)
- **Drag the caption anywhere, scroll on it to resize** — center-snap guide,
  gold hover box, per-Clip persistence (`Clip.caption_placement`). Headless
  and old projects render byte-identical (`\pos(540,883)` verified on your
  Deddy control fixture end-to-end); placement only exists once you drag.
- An 8-angle review then hardened it in-session: no audio re-seek storm on
  scrub, no 120-frame decode wasted on headless/batch runs, shared clamps so
  preview size == burned size, galley caching for the 30 fps tick.
- 201 fast tests green; release `correct,face` build green.

**Slice 2 is designed, not built**: fixing a wrong word in the editor will
write an `at_s`-pinned per-clip correction (editing = curating), "add a
missing caption" becomes a clip-scoped insertion entry, harvested garbles
highlight in the preview, and a Transcribe job gives you captions before the
first render. **The ADR 0035 engine picker is still queued — untouched.**
Your part: an interactive pass (drag/resize/play), then rule on the AFK
decisions. Entry point as always: `nextprompt.md`.

---

## 2026-07-02 (evening) — the gate ran, caught two real bugs, and your rulings now aim at moments

The ADR 0034 generalization gate rendered both held-out clips and earned its
keep twice over (`b229708` + `df97c7f`, ADR 0035, 70 transcribe tests green,
all pushed):

- **The Deddy control silently wasn't testing the ensemble at all**: its
  stream folder ("… Tretan, Coki, Adriano") has commas, llama-mtmd-cli's
  `--audio` flag SPLITS on commas, all five variants died, and the render
  fell back to whisper looking perfectly fine. The sidecar now gets only our
  own ASCII wav names. Any comma'd or emoji VOD title would have hit this.
- **The eh-pile export exposed a fusion trap**: the vote correctly threw away
  107 of whisper's 115 "eh"s, but fusion still trusted the junk skeleton's
  accidental matches — 21 real words boxed into 0.12 s. The skeleton is now
  dropped when <1/3 of its anchors get claimed, words place on speech onsets
  read from the DENOISED audio (the loud mix has none), and each burst opens
  on its onset. `clip-36-43 (3).mp4` shows real words with real timing where
  the 107-eh pile used to be.
- **Your four Deddy rulings** (tiga→juga, blok-on→blo'on ×2, blok-on→goblok)
  shipped as time-anchored, clip-only corrections — and forced two new
  mechanics: multi-word pins that COLLAPSE units (blok+on → one blo'on, your
  apostrophe intact), and `at_s` as part of a correction's identity (three
  same-`wrong` pairs in one clip coexist; before, only the last survived).
  All four verified on `clip-30-00 (6).mp4`.
- **The productization grill → ADR 0035**: Caption engine as a per-Creator
  closed enum (Whisper | Qwen ensemble), shape-computed switch warnings,
  contested-vote harvest born time-anchored, one-shot sidecar stays, per-clip
  decode cache, no default flip. Implementation one slice per session:
  **picker → cache → harvest**.
- You closed the Diskusi benchmark loop: `clip-31-22 (13).mp4` is "almost
  perfect" — residuals accepted.

Next session: the engine picker slice (all decisions pre-made in ADR 0035).
Entry point as always: `nextprompt.md`.

---

## 2026-07-02 (loop, final) — your timestamps became curation; the Diskusi export is word- and time-faithful

Two more loop iterations after you watched the exports (`f884bd8..2956e41`,
186 tests green, all pushed):

- **Timing v2/v3**: the fusion no longer trusts whisper's (partly phantom)
  spans — it aligns on a two-decode anchor skeleton with similarity gating,
  places unanchored words on speech onsets, respreads flash-runs, and never
  silence-drops a vote-verified word. Your "not the same word the streamer
  said" complaint was the phantom-anchor bug; it has a regression test now.
- **Time-anchored corrections (`at_s`)**: your watch feedback ("tur biadab
  anjing is at 8s", "kreeng belongs at 16s"...) went straight into the
  per-clip store as five pins — including INSERTING "tur biadab anjing",
  words no engine ever heard. A correction with a timestamp now applies to
  exactly the right occurrence and pins the caption to the speech. This is
  the roadmap's "time-anchored curation" running for real (ADR 0034).
- Final export: **`clip-31-22 (13).mp4`** — 45 units, every slang word right,
  all five of your timing complaints fixed. Left for your ear: the
  enggak-udah opening, kau/kamu, one anying, and BANGET's edge flash.

Next session: watch `(13).mp4`, rule on the leftovers (a word + a rough time
per ruling), then the generalization gate (eh-pile + Deddy through the
ensemble). Entry point as always: `nextprompt.md`.

---

## 2026-07-02 (caption-perfection loop) — the Diskusi clip now renders with the words whisper never heard

You gave the ground truth and said don't stop. Here's where it landed
(`79ddcf2` + `27d970b`, ADR 0034, 175 tests green, all pushed):

**What's on screen now** (export `clip-31-22 (5).mp4`, rendered with the new
opt-in `YC_QWEN_ENS=1`): *ENGGAK UDAH KEBUKA · NANTI DEPAN SANA KAU **DICEGAT** ·
MANA · INI SATU · ANJING APA ITU · DEPAN SINI KAU YANG MAIN · KAU MAIN KAU MAIN
DULU · **KREENG KATANYA** · **BAJINGAN BAJINGAN** · **BANGKE** · **MANA TADI COK** · ANJING
**PUSING COK MAIN** · **PUSING KAN DIBILANG** · **ANJING NGERI BANGET BANGKE***. The old
render had 23 caption units with `pancingan pancingan`, a 4-second phantom
"pusing", and a hole; this one has 43 units and every swear right.

**How**: ~30 measured decode experiments showed no single decode (either
engine, any knob) gets the clip right — but different decode variants garble
DIFFERENT regions, so a 5-variant Qwen3-ASR vote + whisper-as-voter recovers
what no single decode holds (words), fused onto whisper's DTW spans (timing).
Plus the new **fuzzy store transfer**: your existing curation now applies
across engines (whisper's `dijekat→dicegat` caught the ensemble's `dijegat`),
which is the answer to ADR 0033's "engine swap breaks curation". Your ground
truth also filled 4 old review-queue to-dos and seeded 2 new corrections.

**What still differs from your text, honestly**: the scream region ("tur
biadab anjing / ayok" — scrambled repetition in every decode ever run), kau
vs kamu, one anying-vs-anjing, maen-vs-main, and the opening "ENGGAK UDAH"
(the engines may genuinely hear "nggak, udah kebuka" — your ear rules).
Two caption clusters flash fast (inserted words squeezed between whisper
anchors). Watch the export; details + next levers in
`handoffs/2026-07-02-caption-perfection-loop.md`.

---

## 2026-07-02 (Qwen3-ASR trial) — the A/B ran; your verdict is the only thing left

The ⭐ item is done up to its designed stopping point. A new inspector
(`asr_ab_diag`, commit `5f7d733`) puts **whisper raw / whisper as-it-renders-today /
Qwen3-ASR-1.7B** side by side on your three benchmark clips, staging the GPU models
strictly one-at-a-time and never touching a dialect store (ADR 0033 honored — a
built-in check proves the whisper column reproduces your curated Diskusi render
exactly: 23 units, `dicegat`). The Qwen runtime is a pinned llama.cpp sidecar
(`sidecars\llama`, b9859, past the known Qwen3-ASR repetition bug;
`scripts\fetch-llama-sidecar.ps1` re-fetches it deliberately).

What the run showed, headline first: **on the eh-pile clip Qwen transcribed real
speech across the whole window whisper fills with 107 "eh"s** — and its tail matches
whisper's tail word-for-word, so the middle is almost certainly real. On Diskusi it
recovered "bangke" and the 23–26s hole from the plain mix (what enh/suppress_nst
recovered, without touching the decode), though raw it mislabeled the clip **Malay**;
telling it "Indonesian" via a system hint fixed that AND moved `pancingan → bacingan`
— one letter from your curated `bajingan`. On the clear Deddy control both engines are
good; Qwen punctuates and catches the short interjections. **No word timestamps
survive** this path (karaoke timing would need Qwen3-ForcedAligner or whisper kept for
timing). English was deliberately skipped this session, per your call in the grill.

Transcripts: `target\asr-ab\report.md` (also shown in-session). Your ears decide —
the verdict branches are in `handoffs\2026-07-02-qwen3-asr-ab-trial-results.md`.
166 tests green, pushed.

---

## 2026-07-02 (session wrap) — full review done: 10 commits, and the next caption move is staged

Session summary: the full-project review you asked for is complete — 8 fix/feature
commits (all four backlog items + durability, cancel, and caption-pipeline bugs the
sweep confirmed), ADR 0033 (WHY decode changes keep breaking your curated fixes), and
the new direction committed to the roadmap: **trial Qwen3-ASR (open-sourced Jan 2026,
Indonesian + English, music-robust) against whisper on your two ground-truthed hard
clips + the Deddy podcast control — your ears judge, per language.** The 1.7B model +
its audio encoder are already downloaded into `models\`; the workspace was deliberately
NOT cleared (it holds the benchmark clips — fresh data from the new youtuber is
additive, each streamer gets their own folder). Next session: open `nextprompt.md` and
take the ⭐ item. 166 tests green, everything pushed.

---

## 2026-07-02 (overnight 9) — Cancel now actually stops the render and the audio extract

Hitting **Cancel** only ever killed downloads (yt-dlp). The two longest local
operations — the **NVENC export** and the whole-VOD **audio extraction** on import —
ignored it: the button set a flag, the work ran to completion anyway, and only then did
the app say "Cancelled". Both now poll the flag ~20x/second and kill the ffmpeg child
within ~50ms of the click (unit-tested with a real process). Cancelling a batch render
mid-encode no longer burns the rest of the encode first.

---

## 2026-07-02 (overnight 8) — three caption-pipeline fixes: cleaner review queue, no more truncated corrections on long clips, no cross-seam word fusing

- **The review queue stops collecting ghosts.** Words whisper hallucinates into a
  silent/music stretch were being harvested into your per-clip review queue even though
  the render itself then DROPS them (they never appear in the caption) — so you could
  spend curation time on a word that isn't in any clip. Harvest now runs after the
  keep/drop decision and only records words that actually render.
- **The LLM corrector's reply no longer gets cut off on dense clips.** Its reply
  echoes every word; the fixed 512-token cap covered ~100 words, but fast Indonesian
  speech runs 120–160 words per clip — past the cap, your curated fixes in the clip's
  tail were silently never applied. The cap now scales with the clip (up to 4x).
- **A word can no longer fuse across whisper's 30s seam.** If a decode window's first
  token arrives without its word marker, it used to glue onto the previous window's
  last word, stretching one caption across the boundary. Now a window boundary always
  starts a fresh word. (Verified: today's test clip decodes byte-identically — this is
  a safety net for the rare case.)

---

## 2026-07-02 (overnight 7) — your saved data can no longer be silently wiped by a crash

A project-wide review found a quiet hazard: every saved file (`project.json`, the
global `creators.json`, `review.json`, and — most precious — your **dialect correction
stores**) was written by truncate-then-write. A crash, kill, or power cut mid-write
leaves a half-file; and because every loader deliberately treats a bad file as empty
(so nothing ever blocks a render), the NEXT save would quietly rewrite it from empty —
your curation, gone, no error shown. Three fixes:

- **All saves are now atomic** (write a temp sibling, then rename): a torn write can
  no longer exist, the old file survives any crash.
- **The one save that rewrites the global `creators.json`** now refuses to save if it
  couldn't READ the file first (before, an unreadable file read as empty and the save
  wiped every other streamer's remembered settings).
- **A cancelled import can no longer poison the cache**: `analysis.wav` extraction now
  goes to a temp name and only becomes `analysis.wav` on success — before, a truncated
  wav from a cancel/crash was cached and silently reused for every future import of
  that VOD (wrong detection, wrong everything, no error).

---

## 2026-07-02 (overnight 6) — three new caption decode dials to try on hard clips (defaults untouched), and WHY decode changes keep breaking your curated fixes

You asked for help with bad captions on noisy / fast-slang clips. I wired up the three
standard whisper anti-hallucination settings the render never used, measured them on
your two documented hard clips via the honest caption checker, and learned something
that reframes the whole fight:

- **`YC_SUPPRESS_NST=1`** (suppress whisper's "non-speech" junk tokens) on the "Diskusi
  biasa" clip **recovered everything the rejected voice-cleaner recovered — from the
  plain mix**: the missed 23–26s speech ("Mana dah ini anjing…"), the real "bangke" at
  20s (was a mistimed 4-second "pusing"), "kamu main dulu". 23 → 40 words.
- **But it also re-scrambled the exact words your curated fixes are keyed to**
  (dijekat→dijegat, pancingan→Pacingan…), so `dicegat`, `bajingan bajingan`, `dah cok`,
  `biadab anjing` all fell back to fresh garbles — the same trade that got the
  denoiser shelved, twice. **The lesson (now written down as ADR 0033): your correction
  store is keyed to whisper's garbles under ONE exact decode — ANY decode change is a
  curation-breaking event.** That's why every "just improve the audio/decoder" attempt
  keeps souring: it un-does your curation on already-fixed clips.
- So: **all three dials ship OFF; a no-dials render is byte-identical to before**
  (proven by diffing the checker's output). Use them per-clip, on NEW clips, BEFORE
  curating: `YC_SUPPRESS_NST=1` (the promising one), `YC_CAPTION_NOCTX=1` (fresh slate
  per 30s window — only matters on clips longer than 30s), `YC_VAD=1` (Silero speech
  gating; needs the tiny model `fetch-models.ps1` now also fetches — measured a no-op
  on your clips because loud game audio reads as "speech", kept for truly-quiet ones).
- The infamous "eh"×115 pile on clip #7 is untouched by all three — it needs a
  repetition guard in our own timing pass, not a decoder setting (queued as follow-up).
- Bonus: the caption/correction checkers now actually SHOW their diagnostic logs
  (which dialect store loaded, the decode settings used) — they were silently
  swallowing them before.

**Recommended flow for a new noisy clip:** run the checker twice (with and without
`YC_SUPPRESS_NST=1`), keep whichever transcript misses less real speech, then curate
against THAT decode's garbles.

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
