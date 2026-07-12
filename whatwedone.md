# What we've done

A running, readable log of completed features — **newest first**. Each session appends a dated entry here when a feature is finished (alongside the detailed `handoffs/`, which are gitignored). This file is committed so you can read it any time.

---

## 2026-07-12 (lane 3) — Captions no longer linger over group laughs: the hold trim measured floor-exact, and your A/B is staged

You said "do this automatically" and went to shower, so this session ran the
re-ranked queue head (lane 3 — laughter-aware holds, the one caption lane
that applies to EVERY engine): trim a held word at the moment the room
erupts, using the same shared-reaction mask the camera already trusts
(ADR 0045/0046). Measure-first, bars committed before the first run
(ADR 0062).

- **The defect, measured on the fresh raw emit of your clip 3** (first burn
  of the true code path since your no-JSON ruling): held words ride group
  laughs for 3.64 s total — GEMOY floats 1.15 s into the big 51–56 s laugh,
  TUH rides 0.80 s, NGELEDEKIN 0.50 s over the mocking laugh. The mask sees
  every laugh your ear named (3/3, even the faint 12 s one), and on the
  Deddy 3p control it lands exactly on the 25.5–30.25 span your ear
  confirmed in ADR 0045 — independent reproduction.
- **The trim (pure, tiny)**: a cue's gap-fill hold ends at the first laugh
  onset inside it, never below the 0.40 s readability floor, nothing
  deleted, no onset moved (your ADR 0050 rule is structural: pop-on-laugh
  cues are untouchable). It removed EVERY trimmable second on all five
  corpus clips; controls are clean (guru/Helmy zero trims; ANTITESA trims
  exactly its real ~62 s laugh).
- **Why it did NOT ship: the pre-registered bar split.** R3's letter said
  "residual ≤ 30%" and the floor physically protects 41% (in dense overlap
  a word pops < 0.40 s before the laugh — untouchable without minting
  flashes). Bars are bars: production is byte-identical, the wiring spot
  carries a NOT-wired comment, and the emit-side trim sits behind an
  explicit YC_LAUGH_TRIM=1 so no artifact can silently diverge.
- **Your call is staged (~2 min)**: watch A (raw) vs B (trimmed holds) —
  only seven cue ends differ, timestamps listed in
  `nextprompt-laugh-verdict.md`. "B is better" re-pins the bar to its
  honest form and wires the trim for all future videos on both engines;
  "A is better" banners the lane refused.
- Suites green everywhere (388 workspace + face,align/align flavors); the
  production-order emit reproduced the instrument's trims value-for-value,
  twice decode-deterministic.

---

## 2026-07-12 (ruling landed) — Your eye passed the 1080p gate, and new Creators now start on the ensemble

Two rulings in one sentence ("its 1080p now, use ensemble for new
creators"), both executed:

- **ADR 0060's gate: PASSED on your eye** — the native DASH Promote is the
  production Segment path; the 360p floor is officially the emergency
  fallback only.
- **ADR 0061: new Creators default to the qwen ensemble.** The seed for a
  never-seen Creator (GUI picker, import reset, headless resolution) is now
  `qwen_ensemble` — vote-cleaned words + the eye-approved aligned timing on
  every first import, 5 sidecar decodes per clip accepted. **Existing
  Creators are untouched**: Helmy Yahya Bicara and "local" keep whisper
  (their records carry no engine key, and reinterpreting old files would be
  a silent flip — the serde default deliberately stays Whisper, pinned by a
  new unit test). Want local files on the ensemble too? One click in the
  rail picker persists it (ADR 0035).
- This re-ranks the caption arc: the whisper-only lanes (the ADR 0058
  refusal, recall parity) now serve only explicitly-whisper Creators; the
  queue banner points the next grill at lane 3 (laughter-aware holds,
  every-engine fuel) vs an ADR 0058 re-measure.
- Suites green across the workspace; release rebuilt.

---

## 2026-07-12 (native DASH) — 1080p Segments are BACK: the fetch YouTube can't take away without breaking its own player

You said "do the native dash fix" and it shipped the same sitting,
spike-first (ADR 0060 — all four pre-registered bars passed on both fixture
VODs before a line of production wiring):

- **The mechanism**: YouTube's DASH files carry a `sidx` index that maps
  time → byte ranges. We fetch the tiny head (~12 KB of index), compute
  exactly which bytes cover the padded Segment, and read just those with
  plain ranged HTTP — no tokens, no ffmpeg-seeking-over-HTTP (the M1 hang),
  no whole-file pull. Video (1080p avc1) and audio (m4a) sections are muxed
  locally with a spike-pinned recipe that preserves their sync to ≤1 ms and
  lands the same file shape the old HLS path produced, so every downstream
  consumer (probe, anchor measurement, editor, export) needed ZERO changes.
- **Measured**: ranged reads at 7-8 MB/s (no throttle class), sidx duration
  deviation ≤0.01%, A/V delta error 0.000-0.001 s, a 60-70 s section in
  ~19-35 s. Clip 3's own range on the old VIOR VOD was one of the fixtures.
- **The tiers now**: HLS if YouTube ever revives it (auto-heal) → native
  DASH 1080p (today's path) → progressive 360p (the morning's stopgap,
  demoted to emergency floor). A tier-2 failure warns and falls through —
  a Promote never breaks because the fast path did.
- 369 workspace tests green (+5 new: sidx parse, window planning, format
  pickers, hierarchical/multi-sidx refusal, truncation tolerance).
  `dash_spike` stays in-tree as the re-runnable gate instrument.
- **Your eye is the last gate**: release is rebuilt — promote a Moment on
  the ECA podcast; the segment should land 1920×1080 in ~20-40 s, Studio
  opens, export plays in sync. Say the word if anything reads off.

---

## 2026-07-12 (hotfix) — YouTube killed the Segment fetch under you mid-test; Promote works again (360p stopgap, you picked it), 1080p fix queued

You promoted a Moment on the new ECA podcast and the editor wouldn't open.
Diagnosis: **YouTube switched off the `web_safari` HLS client** (the SABR
wave ADR 0006 predicted) — it now returns storyboard thumbnails only, for
new AND old VODs, on old AND new yt-dlp. Every Promote on every VOD was
dead; your import/detection was fine (25 Moments saved).

- **Measured before deciding**: no token-free client serves HLS anymore
  (ios dead, tv inside YouTube's DRM experiment, android/web SABR-only,
  mweb PO-token-gated); 1080p DASH *sections* re-create the old M1 hang
  (ffmpeg can't seek moov-at-end files — a "30 s section" pulls the whole
  1.5 GiB; measured twice); **itag 18 progressive still section-fetches
  perfectly** — a real 53 s moment range in 4 s / 3.6 MB, clean audio+video.
- **You picked (in-session)**: 360p stopgap now + a native DASH section
  fetcher as the next slice. Shipped (ADR 0059): the fetch now tiers by
  section-seekability — muxed HLS first (auto-heals to 1080p the moment
  YouTube re-serves it anywhere), else muxed progressive (360p today);
  DASH merges are structurally excluded (a unit test pins the selector so
  the hang class can't sneak back). The dead client pin is gone; the
  yt-dlp sidecar + its Diagnostics pin bumped to 2026.07.04 (validated on
  the exact build; old exe kept as .bak).
- **Quality honesty**: interim exports are test-grade (360p + itag-18
  audio) — good for editor/framing/caption verification, not for
  publishing. Captions are untouched (they read analysis.wav). The 1080p
  fix is the new queue head: `nextprompt-segment-native-dash.md` (sidx
  parse + ranged reads — ADR 0006's title, finally for real; whole-VOD
  cache is your ranked fallback if the spike refutes ranged reads).
- Release rebuilt (the running app was holding the exe lock — closed
  gracefully, moments all saved). Launch `target\release\yt-clipper.exe`
  and Promote again; the ECA VOD's 25 Moments are waiting.

---

## 2026-07-12 (general arc, lane 1) — Whisper-engine alignment measured, and it REFUSED itself at its own bar: without the vote, the aligner amplifies whisper's wrong tokens

You said "do this automatically" and went to watch anime, so this session ran
the general-arc queue head (nextprompt-caption-general.md) by the recorded
rubric: lane 1 — wire the eye-approved forced aligner into the whisper
engine, so every future video gets the accurate timing skeleton on BOTH
engines. It was run measure-first, and the measurement said no:

- **Built (all in-tree, zero production change)**: the production entry
  point (`forced_align_retime` — same words in, aligned times out, every
  fail-soft gate the ensemble has plus Indonesian-only) and its instrument
  (`whisper_align_diag` — one production whisper decode, DTW arm vs ALIGN
  arm, every mover named). Bars were written into ADR 0058 BEFORE the first
  run, per the house rule.
- **Measured on the 5-clip corpus (idle GPU, after waiting out your game)**:
  the three turn-taking controls + the densest 3-person overlap clip are
  clean-to-better (medians 0.03–0.06 s; the movers are the known un-smears
  your eye already passed on the ensemble; zero silence-drop changes; all
  714 words aligned, zero words changed). **But the defect clip failed its
  bar**: whisper double-transcribed the brand ("SDC" AND "Susu Deddy Corp"),
  and the aligner — faithful to a wrong token list — dragged the whole
  brand phrase ~2.7 s early onto the wrong audio. 6/8 mis-onsets stayed
  6/8; the catastrophic class got worse, not zero.
- **The insight worth the session**: where whisper's tokens match the
  vote's, the aligner lands BIT-IDENTICAL onsets to your approved burns
  (fadil 32.77, pinguin 50.60, jalanannya 52.76 — exact). The ensemble's
  timing win was never the aligner alone — it's the vote cleaning the words
  FIRST. Wiring the aligner under unvoted whisper words ships the amplifier
  without the cleaner.
- **So nothing shipped**: the pipeline is byte-identical (a NOT-wired
  comment marks the spot), suites green (360 + 30 + 98 both flavors). The
  A/B caption files (`clip_wdtw.ass` / `clip_walign.ass`) sit beside each
  fixture's analysis.wav if you ever want to look.
- **Your two unlocks, on the record (ADR 0058)**: lane 2 (give whisper a
  witness/vote to clean its tokens — then re-run this slice's ready-made
  bars), or the policy call you own: default NEW Creators to the ensemble
  engine (5 sidecar decodes + models per clip). The nextprompt is updated
  to put that choice at the head.

---

## 2026-07-12 (ruling) — You banned JSON caption curation: the stores are now raw, the code is the only tuning surface

You ruled it twice, sharper the second time: "dont touch any json to make
correction for the captions, leave it just like what the llm produce it, so
we can tune the code." Executed immediately:

- **All 4 hand-placed entries removed** from the clip-3 per-clip store — the
  staged DEDDY CORP pin AND the three older ones (gue, gemes, jalanannya).
  The store now holds only the machine-harvested to-do rows, which is
  exactly "what the llm produced". A sha-recorded backup sits in the
  yc_deddy kit and every pin value is preserved verbatim in ADR 0055/0056,
  so nothing is lost — but they are history, not live state.
- **What this means for clip 3**: a re-render now produces the pipeline's
  raw output (jalanannya back at the aligner's 52.76, gemoy unrenamed,
  dedikornya as voted). Your approved burns stay in the stream folder as
  reference videos; they are no longer reproducible by re-render — by
  design. In exchange, clip 3 becomes an unpolluted fixture: every
  instrument in the upcoming general-accuracy arc measures the true code
  path, no store layer in the way.
- **The DEDDY CORP verdict is withdrawn** (nothing to judge — the entry is
  gone); nextprompt-deddy-verdict.md is bannered RESOLVED.
- The rule is saved to agent memory as standing law: caption defects get
  fixed in code (alignment, vote rules, decode configs, LLM pass, engine
  choice), gated on your eye on a burn — never by store entries. The in-app
  review queue remains yours to use if you ever want it; sessions won't.

Next: the general caption-accuracy arc — nextprompt-caption-general.md.

---

## 2026-07-12 (second) — DEDIKORNYA now reads DEDDY CORP: the SDC brand spelled right, your eye pending

You said "do this automatically" again, so this session applied the same pick
rubric as last time: connective words stays a recorded NON-GOAL until your eye
asks for it, whisper parity needs its own grill, the 3p re-render is your call
— which leaves the one item whose ask is already on record: the **dedikornya →
"deddy corp"** spelling (ADR 0056 routed it to the curation lane when its
admission guard correctly refused to re-admit whisper's respelling).

- **The fix**: a per-clip store pin renames the agglutinated vote token in
  place — on glass the brand now reads **SUSU → DEDDY → CORP** (three huge
  words, one at a time; no grouped line — your ADR 0057 ruling held the pen).
- **Two mechanisms were measured and the obvious one LOST**: feeding "deddy
  corp" as a normal (global) correction re-times the words BEFORE alignment,
  and the two extra-token change re-routed the aligner through the
  laughter-masked 25–28 s stretch — KALAU/SAMPAI walked onto the laugh at 26 s
  your ear already named (the mis@28/phantom@26 class). Withdrawn on the spot;
  the shipped shape is an `at_s` pin that renames AFTER all timing, leaving
  every other cue byte-identical (your GUE/GEMES/JALANANNYA pins land
  byte-exact; 119 → 120 cues and nothing else moves).
- **Placement honesty**: the aligner's true "deddy" onset (24.61) is
  unreachable by the pin — the only speech onset nearby belongs to SUSU
  (24.28), and pinning below ~25.08 snaps onto it and drags SUSU early. So
  DEDDY pops at **25.10** (measured-verbatim landing, ~0.5 s into the spoken
  phrase) and CORP holds the genre's standard 1.2 s. If your ear wants it
  shifted, the pin value is the lever — verbatim side only; the geometry is
  written into the store entry's note.
- **Your call is staged**: watch `_deddy-corp clip3 (store curation -
  dedikornya at_s 25.10).mp4` against the approved AB2 burn, then read
  `nextprompt-deddy-verdict.md` — approve / nudge by ear / respell / full
  restore are all pre-committed (backups checksummed in the kit).

---

## 2026-07-12 (verdict) — You ruled: no grouping. ADR 0057 reversed the same night, one-word-at-a-time stays

You watched the A/B and said "i dont like grouped" — the pre-committed
reversal branch ran in one sitting: the caption code is restored byte-exact
to the state your eye approved yesterday (the revert is total because sparse
words were never touched by design), the burn artifact is back to the
approved one (checksum-verified), the instrument and glossary term are
removed, and both burns stay in the stream folder for reference. 372 tests
green on the restored code.

What the record keeps (the honest negative, the ADR 0050 pattern): grouping
genuinely fixed the numbers — 62% of cues flashing under 0.40 s fell to 1% —
but the numbers were never the gate. Your eye keeps huge-word's
one-word-at-a-time purity, and the sub-0.40 flash at dense speech is
accepted as the genre's nature (with the early-cue and dropped-word classes
already fixed by the align and recall arcs, it is the tradeoff you chose).
The ADR 0049 too-fast class is CLOSED; the roadmap is fully dispatched.
Next session: the remaining menu (connective words, dedikornya spelling,
3p clip re-render, whisper parity) — nextprompt-recall-menu.md.

---

## 2026-07-12 — The flashing captions are gone: cramped word-runs share a compact line (ADR 0057, your eye pending)

You said "do this automatically" and went to watch anime, so this session took
the one menu item you had already approved in principle — the ADR 0049 fix #2
too-fast lane (2026-07-08: "grouping bends the huge-word look in dense
stretches", accepted as a tradeoff to weigh at the gate) — and ran it
measure-first, end to end.

- **The defect, on your approved clip 3 burn**: 62% of caption cues were on
  screen under 0.40 s — the fast banter at 14–19 s was 12 consecutive
  sub-readable flashes. The floor exists in the code, but the one-cue-on-screen
  clamp defeats it whenever people talk faster than 2.5 words/s.
- **The fix (ADR 0057)**: a cramped run now falls back to a compact 2–3 word
  line at the line-genre size. Same words, same order, nothing added or
  dropped; a group can never bridge a pause; sparse words render byte-identical
  to before. Your three pins land byte-exact (GUE 28.14, GEMES 51.48,
  JALANANNYA 54.40) and PINGUIN keeps its approved 50.60.
- **Measured across the corpus** (new `caption_regroup_diag`, CPU-only):
  clip 3 sub-0.40 cues **62% → 1%** (median dwell 0.32 → 0.65 s);
  Tretan/Coki 3p **80% → 5%**; guru gembul solo **72% → 3%**; ANTITESA 2p
  **73% → 1%**. Production-path proof: a fresh 5-decode emit reproduced the
  instrument's prediction except ONE pre-registered centisecond-edge case
  (recorded in the ADR). 375 workspace tests + 4 new, all green.
- **Your call is staged**: watch `_toofast-regroup clip3 (ADR 0057 -
  grouped).mp4` against the approved AB2 burn, then read
  `nextprompt-toofast-verdict.md` — every verdict branch is pre-committed,
  including one-const levers (compact size, max words) and the full reversal
  path if one-word-at-a-time was better after all.

---

## 2026-07-11 (fifth) — Your eye CLOSED the ADR 0056 gate: JALANANNYA at 54.40

You picked B over A, called 53.60 "still a touch early", and the second
iteration read right: the pin re-aimed to 54.4 — deliberately past the onset
snap's 0.75 s radius so nothing could pull it back toward 53.60 — landed the
cue at exactly **54.40**, and your eye passed
`_recall-hole-fill clip3 (ADR 0056 - AB2 jalanannya 54.4).mp4`. The pin
(`jalanannya @3646.4`, clip-scoped, delete-anytime) is live in the per-clip
store beside your gue/gemes pins, and the burn artifact matches it, so any
re-render of this clip reproduces exactly what you approved.

That closes the whole recall arc (ADR 0052 → 0056) on your eye. Next is
whichever menu item you pick: connective words (`lucu aku banget mereka`),
the dedikornya→"deddy corp" spelling, the 3p overlap clip's admissions, the
whisper-engine parity question, or the too-fast dwell lane (ADR 0049 #2).

---

## 2026-07-11 (fourth) — Your JALANANNYA A/B is ready: 52.76 vs 53.60, one sitting decides

You said "do this automatically" and went gaming, so this session pre-staged the one
verdict branch that had a pre-committed fix — "JALANANNYA reads early → the at_s pin"
— as an A/B pair in the VIOR stream folder. Watch both, pick one:

- **A**: `_recall-hole-fill clip3 (ADR 0056 - production).mp4` — JALANANNYA at
  **52.76** (the aligner's placement; the pending ADR 0056 burn, untouched).
- **B**: `_recall-hole-fill clip3 (ADR 0056 - AB jalanannya at 54).mp4` — NEW, same
  clip with the one-line pin at your by-ear ~54: JALANANNYA lands at **53.60**
  (snapped to the measured speech onset; whisper's own skeleton said 53.5). The
  emit's cue diff shows exactly ONE cue moved — everything else is byte-identical.

The pin is **not** applied anywhere: the store is byte-exact shipped state, so
production still renders A until your eye says otherwise. If B reads right, the pin
block sits ready-to-paste in `nextprompt-recall-eye-verdict.md` (clip-scoped,
delete-anytime, the gue shape). Both GPU passes (one emit, one NVENC burn) ran
behind the idle gate only while your game was closed.

Also: CONTEXT.md now defines **Time pin** and **Recall admission** — the two terms
ADR 0051/0055/0056 lean on that the glossary never had. Ops scar for the record:
this harness kills background waits after ~25–40 min (three gate-wait attempts died
mid-sleep, each leaving an orphan bash+sleep pair to sweep) — the shape that works
is wakeup-polling plus a short foreground gate+burn burst; saved to memory.

---

## 2026-07-11 (third) — The 51–56s caption hole is filled and the floating GEMOY is dead (ADR 0056)

The two things you named on the forced-align burn — the pinguin/jalanannya gap and
the stray GEMOY floating at 56 — are both fixed on a fresh burn waiting for your
eye: `_recall-hole-fill clip3 (ADR 0056 - production).mp4` in the VIOR stream
folder. The 48–58s stretch now reads GEMOY·PINGUIN·**GEMES**·**JALANANNYA**·
**NGEFANS** where before it was gemoy·pinguin·(5 seconds of nothing)·GEMOY-at-56.

- **What the instrument found first** (two full decode runs, byte-identical =
  no decode luck): the hole was one six-word phrase from the co-speaker
  (`jalanannya lucu aku ngefans banget mereka`) that ONLY whisper and the raw-mix
  qwen heard — all four denoised variants are deaf to it because the denoiser
  treats the overlapping co-speaker as noise. The strict-majority vote killed it
  2-vs-3. (Your pinguin was never actually dropped this time: it's voted and
  placed at 50.6, same as the surgical clip you approved.)
- **The fix** (`admit_recall`, ensemble step 2b): a distinctive word whisper
  heard (5+ chars, not a filler word) that the vote carries fewer copies of gets
  re-admitted, text-only, into its whisper position. Three guards keep it honest,
  each one earned by a real false positive the instrument caught before you had
  to: no filler words, no re-spelling words the vote already has (whisper's
  `deddy corp` vs the vote's `dedikornya` stays curation, and your gt's own
  `duduk-duduk`→`dodo` garble stays OUT), and nothing at the clip edges (the
  range cuts words in half; whisper transcribes the stumps).
- **Controls**: your guru gembul solo clip and the ANTITESA 2-person clip admit
  NOTHING (the clean-speech bar). On the dense Tretan/Coki overlap clip it admits
  3 words incl. `maksudnya` — which 4 of 6 decoders heard and the vote still
  dropped — i.e. the rule doing its job on exactly the clips that need it.
- **The GEMOY float**: with jalanannya back at 52.76 the aligner has no room to
  float the garbled token — it lands at 51.2, and a new clip-scoped pin renames
  it **GEMES** (the real `gemoy` at 49.7 is untouched; the twin-token trap from
  ADR 0054 is structurally gone now that the position is stable).
- 372 tests green. Decode measured deterministic run-to-run, so the "different
  votes on different days" mystery reduces to: the code changed between the
  07-07 render and now, the decodes themselves don't wobble.
- **One thing to check with your ear**: JALANANNYA shows at 52.76; you called it
  ~54. If it reads early on glass, say so — it's a one-line `at_s` pin like the
  gue one.

## 2026-07-11 (later) — Forced alignment is the DEFAULT ensemble timing now (ADR 0055)

You passed the burn gate in the morning ("timing for rust port is right"), so this
session flipped the switch. You were gaming, so per your "do this automatically" the
one open decision — flip first vs recall lane first — went with the recommended order:
**flip now, recall next** (ADR 0055 records why; `YC_FORCED_ALIGN=0` is the exact
escape hatch if you disagree).

- **Your ensemble renders now time by the wav2vec2 aligner with no knob.** Unset =
  on (production `align` build + the fetched model); `YC_FORCED_ALIGN=0` brings back
  the old DTW fusion render-for-render. Whisper-engine renders untouched either way.
- **Each ensemble render got a whole GPU decode cheaper**: the second whisper pass
  (suppress_nst) only fed the old DTW skeleton, so it now runs only when the DTW
  fallback would actually be used.
- **Cross-clip widening measured before trusting the flip beyond huge-word**
  (fresh production decodes, the gate instrument):
  - clip 3 re-check: the drift-class numbers reproduce exactly (siapa +0.08,
    SDC −0.07; 117/117 aligned); the leftovers (pinguin@50.6, missing jalanannya)
    are the recall/text lane, same on both engines.
  - guru gembul karaoke-genre clip (your solo-lecture regime): 164/164 aligned,
    aligner-vs-DTW shift median 0.09 s, max 0.66 s — it agrees wherever timing was
    already good, dwell doesn't regress. Karaoke/rolling builders were also audited
    for span-shape assumptions (none; +1 unit test).
  - Tretan/Coki 3-person overlap clip (the densest in the workspace, ~3 words/s):
    211/211 aligned, shift median 0.08 s, dwell 80%→80% — the movers (21 words,
    up to 2.5 s) are the drift class the aligner exists to fix; nothing else budges.
- **Your time-pins still beat the aligner** (the gue class it can miss): a real
  gue@3620 pin now sits in clip 3's per-clip store (clip-scoped, never promotes —
  the note inside says delete if unwanted), and both fusions log the override:
  the aligner had gue at 28.55, the pin lands it at 28.12 — exactly where your ear
  and ADR 0051's measurement put it.
- Ops note: your gaming session and the ensemble share the GPU badly — decodes hit
  their crawl-watchdog twice and simply re-ran clean once the GPU freed. That's the
  watchdog doing its job; don't raise the budget.

Next: the recall lane (`nextprompt-recall-hole-fill.md`) — fill the 51–56 s token hole
(pinguin/jalanannya/gemes) that today's vote drops; the floating GEMOY dies with it.

## 2026-07-11 — Forced-alignment timing is now IN THE APP, native Rust, opt-in (ADR 0054)

The Python spike from last session became shipped code: the wav2vec2 aligner now runs
inside yt-clipper itself (ONNX via `ort`, the same runtime the face models already use —
no Python anywhere). It's behind `YC_FORCED_ALIGN=1` on an `align` build (build-release.bat
now compiles it in), OFF by default — your normal renders are byte-identical until you
flip the knob.

- **The port is exact, not approximate.** The Rust Viterbi reproduces torchaudio's
  alignment span-for-span (650/650 identical on the spike's emission), and the full Rust
  chain (ONNX session → log-softmax → Viterbi) lands every one of the spike's 111 word
  onsets with **0.0000 s** difference from the Python that you approved on the burn.
- **On a fresh production decode of clip 3** (real vote, no hand-picked text) the two
  worst mis-onsets fixed themselves: "siapa" was 1.5 s early → now +0.08 s off your ear;
  "SDC" was 2.2 s early → now −0.07 s. 117/117 words aligned, ~25 s on CPU per clip.
  ("jalanannya" and the second "pinguin" are missing from TODAY'S vote on both engines —
  that's the recall/text lane, ADR 0052, a different problem timing can't invent.)
- **Turn-taking control (ANTITESA, the "this is good" regime)**: the aligner agrees with
  the current timing where it's already right — median difference 0.08 s across 184 words,
  9 in 10 words within 0.35 s, and dwell doesn't regress. Eight words moved >0.5 s (named
  in ADR 0054) — those are for your ear at the flip gate.
- **Input contract pinned by measurement**: the model card says normalize the audio; the
  spike (which you approved) didn't. Measured both: normalization fixes "gue" but breaks
  "pinguin" (grabs the earlier duplicate). Raw input ships; the export script + module doc
  both say why, so nobody "fixes" it later.
- **Your ruling on the burn (same morning): "timing for rust port is right" — gate PASSED.**
  The two things you flagged are both text-lane, and I measured them before touching anything:
  the second "pinguin" (and "jalanannya", and the real ~51s word "gemes") were DROPPED by
  today's vote — that's the recall lane (ADR 0052), timing can't invent words. The stray
  GEMOY you saw at ~56 is the vote's garbled spelling of that "gemes" floating into the
  5-second hole those drops left; I measured the two obvious auto-fixes and both are dead
  ends (alignment confidence doesn't separate it from real fast words — 0.295 vs siapa's
  0.006; a time-pin would grab the REAL gemoy at 49.7 instead — the twin trap). Honest
  verdict in ADR 0054: fill the hole (recall lane) and the float has no room; then the
  store fixes the spelling. Next session grills one question first: flip the default now
  and do recall next (my recommendation), or recall first
  (nextprompt-forced-align-default-flip.md).

## 2026-07-08 — The dropped words + a real fix for the timing drift: forced alignment (ADR 0052/0053)

Chased the words captions miss on overlapping speech ("otot", "kreatin", "pinguin", the
"siapa tau" before "SDC"). Two findings, one small and one big.

- **The dropped words are recoverable — the ensemble already hears them.** I built a
  measure-first instrument (`caption_recall_diag`) that decodes all six views (whisper + the
  5 Qwen variants) and localizes where a word is lost. It found these aren't a "the audio is
  too masked to hear" problem — 3 of the 4 are heard by most decoders and the **current**
  code already places them near the right time. The versions you'd seen drop them were older
  renders. So the recall itself is mostly already there (ADR 0052).
- **But a naive re-render felt worse — and you were right.** It recovered the words yet
  brought back the mis-timing (your ADR 0051 pins were never saved, so a fresh render has
  none) and added new phantoms from ensemble drift. So we did it **surgically** instead:
  your clean shipped render + your pins + only the missing words inserted — no regeneration,
  no new phantoms. You approved that clip. It proves the target, but it's a one-off (it
  doesn't carry to other videos).
- **The big win — the timing fix that GENERALIZES.** You asked the right question: how does
  this carry forward? Hand-pins don't. So I researched and **spike-tested** the WhisperX
  approach: **wav2vec2 forced alignment** — deriving each word's time from the audio itself
  instead of whisper's drift-prone DTW. On clip 3 it **reproduced your hand-tuning
  automatically, with zero pins**: "siapa" 3.66→**5.08**, "SDC" 20.78→**22.93** (the two
  whisper got worst), and every recovered word placed accurately — 7 of 8 within ~0.8s, and
  nothing fell through the cracks under laughter. One word ("gue", said many times) it timed
  wrong — the same duplicated-word case pins struggle with. Burned it for your eye
  (`_recall-lane clip3 (FORCED-ALIGN spike - no pins).mp4`), you approved the direction.
- **What's next:** port forced alignment to native Rust (ONNX via `ort`, no Python) as the
  new timing source, behind a flag, gated on your turn-taking clips before it becomes
  default. That would make good caption timing **automatic on every video** and fold the
  drop + mis-onset problems into one mechanism — a much bigger win than fixing clip 3 by
  hand. `keratin→kreatin` stays a dialect-store spelling fix. (ADR 0053)

## 2026-07-08 — Mis-onset re-anchor: your time-pins now work on both caption engines (ADR 0051)

The real fix for the mis-onset problem (a word shown early, onto a nearby laugh). First I
measured **where** the early placement comes from: it's **whisper's own word timing**, not
the ensemble — the shipped ensemble captions are byte-identical to plain whisper at every
mis-timed cue. So the fix belongs in the shared caption path.

- **Auto-detecting mis-onset is not safely possible — measured, not guessed.** I built an
  instrument that tried to spot the early words from the laughter mask + whisper's timing.
  It doesn't work: 3 of your 5 mis-timed words sit on *clean* audio at their wrong moment,
  the mask fires on the opening pile you told me to keep, and the real speech is often
  *under* the laughter. An early word before a gap is acoustically identical whether it's
  mis-placed or correctly placed before a pause — only your ear separates them. An
  auto-mover would shift correctly-placed words (the ADR 0050 trap). Recorded, not chased.
- **The fix that IS safe: your time-pin.** You can pin a word's correct moment in the
  dialect store and the code snaps it onto the real speech onset — but that only worked on
  the **ensemble** engine; on the default **whisper** engine a time-pin silently did
  nothing. Now it works on **both**. Zero risk: only a word you pin ever moves.
- **Proved on your eye.** A BEFORE/AFTER re-burn of clip 3 (the BEFORE byte-identical to
  your shipped Short) with 5 pins: SIAPA 3.66→5.06, SDC 20.78→22.78, GUE 25.84→28.12,
  FADIL 31.32→32.26, JALANANNYA 53.48→53.60 — each on its real onset, and no other word
  moved (the other GUE/FADILs stayed put). You ruled the timing fixed.
- **Also fixed:** a caps word like "SDC" now matches your pin on the whisper engine (it was
  silently case-mismatched before).
- **Caveat:** a common word said many times (like "gue") is finicky to pin — the pin grabs
  the occurrence nearest your time, so pin it toward where it *currently* shows. Distinctive
  words (names, acronyms) — the ones worth a durable pin — just work.
- **What's left (recorded for the next slices):** the drops you caught (siapa tau ~20s,
  otot kayaknya, kreatin-kreatin, pinguin) are the **recall lane** — next session. The
  wrong-text ones (proten→protein) are your store curation. "ya"@12 is the one phantom
  candidate.

## 2026-07-08 — Reaction-phantom suppression: TRIED, then REVERSED on your eye (ADR 0050)

Tried ADR 0049's "fix #1": drop the flash pile of words the caption stage seemed to
cram onto the opening group-laugh ("YA SIAPA TAU MAU COBA AKU"). It passed the
instrument gate — but when you watched the real BEFORE/AFTER burn, you ruled it wrong:
**"Siapa tau mau coba ku bawa" is real speech**, just spoken fast and placed too early
— deleting it made the clip **worse** than shipped. So I reverted the whole thing the
same day; the captions are byte-identical to before again.

- **The lesson (worth more than the code):** an instrument agreeing with a
  hand-labelled "phantom" is NOT your eye on the burned clip. The opening was never
  laughter-noise — it was real words on the wrong clock. Gate on the burn, not the label.
- **What this means for the real fix:** almost every caption problem you named is
  **mis-onset** (right word, wrong time: siapa→5s, gue→28s, fadil→32s, SDC/siapa-tau→23s,
  jalannya→54s), **wrong text** (lu→lucu, ya→yakan, aslinya→ASI, pamu→Pak muh, gemot→gemes,
  duduk-duduk→dodo, proten→protein), or **dropped words** (otot kayaknya, kreatin-kreatin,
  pinguin). Hardly any is a phantom. So the mis-onset re-anchor is now fix #1.
- **Kept from the detour:** the `caption_overlap_diag` instrument and a new `segment_seek`
  helper that re-burns a clip.ass over the fetched segment at the render's exact seek — so
  we can eyeball any future caption change on the real surface without a GPU re-decode.

## 2026-07-08 — Captions under overlapping speech: measured the defect, gate PASSED (ADR 0049)

You found the shipped 4-person VIOR captions bad when several people talk at
once (delayed/leading, dropped words, flashing too fast), while turn-taking
captions stay good. Instead of guessing a fix, this session **measured** where
and why — the same measure-first discipline as the camera lanes (an unmeasured
"fix" is the `enh` overclaim, caught twice). One implementation: the instrument
+ the gate; the fixes follow as their own measured slices. Committed + pushed.

- **A caption-overlap inspector** (`caption_overlap_diag`) reads what actually
  shipped — the burned `clip.ass` cues — and joins them against the overlap
  signal the analysis already computes (who's talking + the laughter mask) and
  your by-ear ground truth. Pure, no GPU, reproducible; kept in-tree for every
  fix to re-measure against.
- **The verdict, on your clip 3 (fans Fadhil):** four distinct failures, and a
  cross-clip control proved **all four are overlap-specific** — turn-taking
  clips flash 0–2% of cues sub-readably, the overlap clips **54–78%**:
  - **too-fast** (52% of cues): overlapping speakers pack 2–3× the words/second,
    and the timing floor gets defeated so half the words flash by.
  - **mis-onset**: the mis-placed cues appear ~1.5 s **early, onto the laughter**
    (you confirmed this matches — captions race ahead at the laughs, then the
    real speech feels un-captioned).
  - **phantom**: words hallucinated onto laughter (an opening "YA SIAPA TAU MAU"
    pile that isn't really spoken).
  - **drop**: words masked by the overlap and never transcribed — a recall
    problem no re-timing can fix.
- **The root**: the caption stage is blind to the overlap signal (who's speaking
  + the laughter mask) that the rest of the analysis already has. Feed it in and
  three of the four collapse. **Fix order (approved, each its own gated slice):**
  phantom suppression → too-fast grouping → mis-onset re-anchor → the drop/recall
  lane. Nothing shipped into the render this session — the caption path is
  byte-untouched. (ADR 0049)

## 2026-07-07 — Solo-presence WIRING: the verified check heals the plan in production (ADR 0048)

Wired the solo-presence measurement (ADR 0047's instrument) into the
**production Camera plan**: a solo shot that frames no verifiable subject
face is now caught at analyze time and rewritten to an honest framing,
instead of shipping a stale crop on set dressing. Attribution regime only
(the Occupant map is the robustness anchor; follow-visible's A-class waits,
so its plans stay byte-identical). One implementation this session, shipped
after the operator's manual re-test (which sharpened it twice).

- **The wiring** (`crates/frame/speaker.rs` + `crates/app/pipeline.rs`): draft
  `plan_shots` → a per-solo-shot **presence seek pass** (full-res YuNet over
  each low-measured solo crop, reusing the occupant-map seek machinery; an
  arm-1 pre-filter skips well-measured shots so the fixtures pay ~0 seeks) →
  `evaluate_solo_presence` (**Bar P**, pure, unit-tested) → `rewrite_for_presence`
  (the ladder: split → hold-verified → wide) → the audit consumes the artifact.
  Pure decision in `yc_frame`, seeks in the pipeline glue.
- **Bar P is two-sided** (the operator's re-test caught the miss): a solo flags
  when majority-unmeasured AND no crop-face in `[0.35, 1.9]×` its reference —
  too small (poster/figurine) OR too large (a back-of-head/guitar blob ~2×).
  ADR 0047 had missed the oversized class by reading raw heights without
  normalizing per seat, and mis-filed the 22–25 s windows as benign.
- **A split never shows one person twice** (operator rule): two panels framing
  the same source region (a tight-angle mis-track projecting a wide-shot seat
  onto set dressing — the guitar beside the guest) collapse to the honest
  centered wide — universal in `group_layout`, so every split obeys it.
- **Gate PASSED**: every VIOR ship-truth defect heals (clip 3
  #8/#12/#13/#14/#17/#27, clip 2 #2/#3/#7/#11) with zero false positives; both
  fixtures untouched (ANTITESA `camera_diag.fg` byte-pin `9e07d81f…`, Deddy
  15-shots + reaction split `vstack` intact); clip 1 (follow-visible)
  byte-identical; suites **349 both ways**. No caption/transcribe code touched.

## 2026-07-07 — Solo-presence instrument + gate PASSED (the shakedown's worst defect, measured)

Built the measurement the shakedown's worst defect was missing — **does
a solo shot verifiably frame its attributed person's real, currently-
visible face?** — pinned its bars in a `/grill-with-docs` session BEFORE
the instrument existed (the ADR 0045 pattern), then ran it on all three
VIOR clips + both fixtures and **passed the gate** (ADR 0047).

- **The instrument** (`speaker_diag`'s new `== solo presence` table,
  diagnostics-only — production untouched): per solo shot it measures the
  measured-bin share + largest unmeasured run, the in-crop share, and —
  the load-bearing part — **full-res seek scans** that ask, at span
  quantiles + the biggest gap's midpoint, "is a real face visible in
  what actually renders?" (YuNet over the *planned crop*) and "is what
  the tracker matched a real face, and whose?" (YuNet+SFace on the
  track's own box, cosined against the occupant-map persons).
- **The gate** (bars declared before the seeks were read): a solo shot
  flags when it is ≥50% unmeasured AND no seek finds a subject-scale face
  in the crop; plus an in-crop containment bar. **Both fixtures clean**
  (Deddy + ANTITESA solo shots never flag; every standing bar green —
  ANTITESA `camera_diag.fg` byte-identical, Deddy laughter PASS, 337
  tests both ways). Every ship-truth defect flagged, **zero false
  positives**, and it surfaced **two empty-crop windows the operator's
  eye had missed** (Clip 2 ~5–10 s and ~41–45 s).
- **The finding that shapes the next slice: the occupant map is the
  robustness anchor.** In attribution regime (Clips 2, 3) the per-shot
  identity check cleanly tells a stale crop parked on set dressing from
  one that luckily still holds the subject — the same face-like-blob
  class that fools the audit. Follow-visible (Clip 1) builds no map, so
  YuNet false-fires a "face" on a *guitar body* (render-confirmed) and
  the crop-face bar is fooled; that A-class defect is catchable only by
  the cruder unmeasured-share bar. The defect is *easier* to verify where
  the map exists — the inverse of the naive read.
- **Ship-truth corrected two shakedown ledger entries**: the burned-in
  source timecodes proved the prior session's "13.09 blank" and "22.19
  guitar blob" were a screenshot→timestamp decode that crossed clips —
  both dissolved, two genuinely-new hits recorded, one reclassified to a
  Wide-layout framing wart. The publisher record stays honest.
- **CONTEXT.md**: added **Solo presence** (the verified fact a solo Shot
  owes before it ships). Wiring the fallback grammar + audit extension is
  the next slice (`nextprompt-presence-wiring.md`).

## 2026-07-07 — Ship shakedown: first fresh-material production run — 2 of 3 clips shipped

The production tool ran as a production tool for the first time on
fresh material (Deddy Corbuzier VIOR episode, o1SBOz5UK2Q, 5-person
panel): headless `--detect` ranked 25 Moments (persisted; the GUI's M8
re-import restored them), the top 3 went promote → Studio (Active
Speaker, Qwen-ensemble captions) → render, and the operator judged each
against the new **Publish bar** (CONTEXT.md term, ruled publisher-honest:
ship when a naive viewer would notice nothing; every defect ledgered
with its clip-relative timestamp regardless). **Clips 1 and 3 SHIPPED;
Clip 2 HELD** (two fully-empty camera windows in 47 s).

- **Worst defect, twin-confirmed across all 3 clips and both regimes**
  (9 publisher-visible hits; the queued slice —
  `nextprompt-solo-presence-gate.md`): a solo Shot never verifies that
  what it frames is the attributed person's real, currently-visible
  face. Sub-class A: subject undetected → the stale crop persists →
  set dressing/sliced heads ship and the camera audit is BLIND (no
  measurement, no finding). Sub-class B: in a new angle the seat's
  position lands on set dressing, the track latches onto a face-like
  blob (a guitar body, a pegboard), and the audit is FOOLED ("subject
  in crop" passes). Operator's causal read verified on the harness twin:
  the camera doesn't reframe after the previous person because nothing
  measured triggers the dead-zone.
- **The instruments earned their keep where their regime allowed**:
  Clip 3 planned 31 shots with 6 reaction/off-screen diffs, caught a
  head-thrown-back laugh ON-frame, and claimed 0.9 s off-screen — while
  Clip 1 planned FOLLOW-VISIBLE (multicam close-up section; map + mask
  structurally out of play) and Clip 2's voice join produced **0.0 s**
  (rapid banter fragments into 1–2-window embedding scraps — ADR 0044's
  texture generalizing; no off-screen rescue exactly when banter needs
  it). Fresh material mixes regimes per-section; the fixtures never did.
- **Infrastructure findings with measured evidence**: the production
  binary has been face-only through the whole camera arc (`ser`/arousal
  never in a release build despite the gate-passed model installed —
  detect ranks without one Signal); YouTube's `web_safari` segment fetch
  flaps in ~10-minute windows on player-rollout days and defeats the
  4×3 s retry policy (3 separate 4/4 failures today), with the GUI
  showing only a bare "Failed" chip (M8 error-surfacing gap, now
  evidenced).
- Session evidence of record (local): the defect ledger
  `handoffs/2026-07-07-ship-shakedown-ledger.md` (F1–F9 + twin
  verdicts) and `handoffs/2026-07-07-ship-shakedown.md` (operational
  traps: regime mixing, fetch-flap probe recipe, GUI drive recipe,
  twin commands + deterministic segment refetch args).

## 2026-07-07 — Shared-reaction wiring: the laughter mask ships into production (ADR 0046)

ADR 0045's PASSED instrument gained its production caller: the
shared-reaction mask now computes inside `Job::AnalyzeSpeakers`, the split
grammar consumes it, the Studio timeline shows it, and the tagger heals
from Diagnostics ▸ Downloads. Gate: a fresh `diar_reaction.mp4` A/B'd
against `diar_person.mp4` on the operator's eyes — **PASSED, shipped**.

- **The lane**: `SpeakerAnalysis.reaction: Option<Vec<f32>>` — a SIBLING of
  the voice lane (a missing CAM++ model can't kill it), computed in the
  attribution regime only (the occupant-map gate), so a follow-visible Clip
  structurally never computes it (~10 s CPU saved; the ANTITESA byte-pin
  holds by construction). Conventions ride the call site: `Unit` scale,
  `Probs` output (both selftest-pinned, ADR 0045). Missing/broken tagger =
  a Camera-panel note, never a failed job.
- **The grammar**: a piece with ≥ 2.0 s of bins at tau 0.1 flips to the
  visible pair's split screen — absolute burst-seconds (a share rule
  under-fires on the measured burst texture), piece-level, mirroring the
  off-screen split exactly (decided before framing, no anchor written).
  Deddy: shot #5 (27.0–30.2, wall-to-wall mask) flips; #4 (1.25 s, its
  22.1–25.5 majority is ear-confirmed speech), #6 (0.3 s), and the
  ex-split 14.2–22.1 (0%) all hold — realized exactly as pre-declared.
- **One deviation, surfaced and ruled**: killing #5's solo anchor re-seeded
  the whole downstream Person B chain (size-reuse had propagated #5's
  843 px crop height; #6/#10/#11/#14 now frame at 773 px — B's talking
  posture, not his mid-laugh posture; ≤ 43 px movement, audit clean). The
  operator accepted the re-seed over having a split piece mint solo
  framing state.
- **Downloads (ADR 0041 pattern)**: two rows + two `Install::File` specs
  from k2-fsa's own HF mirror at immutable revision `3c795f58…` — chosen
  over the GitHub tarball (living release, checksum.txt already stale once,
  .tar.bz2 would need new archive code); both hashes verified
  byte-identical to the gate-passing install. fp32 ships; int8 (~68 MB,
  hash recorded) waits on a measured re-run of the bars.
- **Studio**: the voice row is now the evidence row — reaction mask in
  gold beside seat claims and off-screen red; renders when either lane
  exists.
- **Bars**: suites green both ways — count of record corrected to **337
  both ways** (the historic "268 non-face" was a partial-run artifact;
  no frame test was ever feature-gated). Laughter bars PASS verbatim,
  person-join intact, audits clean, ANTITESA fg pin `9e07d81f…` intact,
  Bar 0 reproduces. Also healed: a broken persisted window geometry
  (saved minimized at −32000) that booted the GUI at 16×16.

---

## 2026-07-06 (evening) — Laughter-class instrument spike: bars declared, annotation overturned, gate PASSED

ADR 0044's named path measured (ADR 0045): an AudioSet-class tagger
(icefall Zipformer-M, sherpa-onnx pinned release — the CAM++ sourcing
pattern) scores every 0.25 s of the same analysis.wav the voice lane reads,
for the new **Shared reaction** class (CONTEXT.md). Harness-only; zero
production behavior change.

- **Conventions seen, not assumed**: the tagselftest on the release's own
  13 test wavs pinned `Unit` sample scale (rank-perfect vs the published
  reference on all 13; `Int16` audibly breaks — laughter tags as Music)
  and caught that the export is sigmoid-terminated (raw [0,1] with hard
  zeros) — the first cut double-sigmoided; `TagOutput::Probs` pinned. The
  release's checksum.txt is stale (asset re-uploaded); pinned by the live
  hash `6c89b86c…` instead, discrepancy recorded.
- **Bars declared in the grill BEFORE code**: ≥50% laughter mass on the
  known stretch, ≤10% on every monologue segment, gap ≥5×, at one tau
  from {0.1..0.5}. First judge: **FAIL** on the recorded span 22.1–27.0
  (peaks 25%) — but every monologue at 0% and the mask sitting at
  25.5–30.25, exactly where ADR 0044's own text (the V4/V8/V9
  fragmentation segment = 27.0–30.2) said the laughter was. The operator
  ear-checked both spans: **the annotation was wrong, the instrument
  right** (second overturn on this fixture). Same bars re-judged on the
  ear-corrected span: **PASS at tau 0.1 (89% / 0% / ∞), tau 0.2 also
  passes** — and the ex-split 14.2–22.1 measures 0%: that piece has no
  laughter fuel, stated plainly.
- **Texture for the wiring slice**: real overlapped podcast laughter
  scores 0.1–0.3 per step (bursts, not a wall; clean test-wav laughter
  0.86) — the grammar threshold must be burst-derived, low tau over the
  zero-noise floor.
- **Bars**: suites green both ways (334 face / 268 non-face — the +7 are
  the new `yc_frame::reaction` pure tests); Deddy person-join bars intact
  through every run; ANTITESA follow-visible, audit clean, fg byte-pin
  `9e07d81f…` intact, laughter lane quiet there. **Operator gate: PASSED —
  production wiring queued** (`nextprompt-laughter-wiring.md`).

## 2026-07-06 (afternoon) — Studio timeline resizable, voice row visible, gate PASSED

The operator's fresh-binary ask ("timeline is so small it doesnt visible
there, maybe make a timeline size dragable or have a scroll?") shipped as a
drag-to-resize strip. UI-only; zero analysis/render behavior change.

- **The strip**: the Studio's bottom panel is now resizable exactly like
  the side panels (drag its top edge), default 230 px, range 150–420. The
  chrome rows (ruler, caption blocks, cut markers) keep their fixed sizes;
  the seat lanes + voice row split ALL the remaining height equally,
  floored at the old 13 px — so the minimum panel degrades to exactly the
  old layout, and dragging taller always visibly fattens the lanes. A
  proposed 34 px lane cap died during implementation: on ANTITESA's 3
  lanes it was already saturated at the default height, so dragging would
  have grown only dead space — no cap; the panel's own max bounds it.
- **Persistence**: eframe's `persistence` feature (one Cargo.toml line) —
  the timeline height, the side-panel widths, and the window geometry all
  survive restarts via egui memory in
  `AppData\Roaming\yt-clipper\data\app.ron`. Proven with a boot → resize
  to 1111×777 → graceful close → boot round-trip restoring the size
  byte-exactly. Deliberately no ADR: one panel builder + one cargo
  feature, trivially reversible (grill call).
- **Bars**: suites 327 green both ways; Deddy person-join bars intact
  (occupant map 4+2 at cut 0.40, exactly one VALID edge V2×cam0→P1,
  off-screen 1.5 s, audit clean); ANTITESA follow-visible + audit clean +
  fg byte-pin `9e07d81f…` intact. **Operator gate: PASS — the voice row's
  spans plainly visible on ANTITESA at their chosen height.**

## 2026-07-06 (mid-day) — Person-scoped voice join in production, gate PASSED ("honest evidence over lucky output")

The ADR 0043 spike became production (ADR 0044): the voice⇄seat join
rebuilt over the occupant map, gated on `diar_person.mp4` vs the passed
`diar_integration.mp4`.

- **The join**: purity-gated (cluster, camera) edges — contested
  co-occurrence (2+ mouths over the floor) is no evidence at all; measured
  per-edge, clean edges run 16–17% contested vs 87–100% on every poisoned
  one (`JOIN_MAX_CONTESTED` 0.65 mid the gap). Clean edges resolve through
  the map to PERSONS; a person claims its mapped seat in any segment (face
  evidence beats the single-visit rule); contradicting clean edges = the
  cluster is provably impure, refused an identity, camera-local claims
  kept. Off-screen became **positive absence only** — fully-known camera,
  person not among the occupants — never ignorance. Singletons are unknown
  occupants (block absence proofs, never merge cameras). The CV sweep
  scores the join that ships (thr moved 0.60 → 0.55 on Deddy).
- **What it found**: the false 14–20 s "off-screen dominant" call is dead
  (V1 refused, holds honestly), and the instrument OVERTURNED the ear at
  20.8–21.4 s — the interjection voice is the peci man (P1), positively
  absent, on clean two-camera evidence (16%/17% contested), with the
  source itself J-cutting to him at 69.1 s. The interjection cut therefore
  correctly does NOT fire. The two approved splits lose their poisoned
  fuel and revert to solos; three honest replacements for the laughter
  split were measured and all failed to discriminate (contested share
  74–100% everywhere; absence ≤35%; cluster composition inverted) —
  laughter is out-of-domain for a speech embedder, so no unmeasurable
  grammar shipped. **Operator gate: PASS — ship it.** The laughter-class
  instrument queues as the named path to win the splits back.
- **Wiring**: occupant map in `Job::AnalyzeSpeakers` (attribution regime +
  both models + viable lane only; ~15 s of targeted seeks per 70 s clip;
  soft-degrades to the seat-scoped join, never a failed job); pure
  machinery in `yc_frame::occupant` shared by pipeline and harness; YuNet +
  SFace as registry rows with Downloads healing (pins re-verified
  byte-identical against the full-hash zoo URLs); Camera panel prints
  per-lane off/degraded notes.
- **Regression**: ANTITESA fg byte-pin `9e07d81f…` intact through the whole
  rebuild (the map never computes in follow-visible, structurally); 327
  tests green both ways (+12); camera audits clean on both fixtures; the
  re-promoted fixture re-armed the pin at clean HEAD before any code.
- Queued: draggable/scrollable Studio timeline (the voice row is invisible
  at today's strip height — operator request from the fresh-binary first
  look).

## 2026-07-06 (morning) — Face re-id spike: the occupant map, gate PASSED ("each row is one human")

Two gates closed and one instrument was born (ADR 0043; ADR 0042 gained its
verdict section):

- **The diarization integration gate PASSED** — "it still looks good" — with
  ear-truth annotations that rewrote the 14–22 s story: the actual talker was
  the split's bottom-panel man, ON screen. Frame forensics then proved the
  source has exactly TWO real cameras (four humans) that the seat-geometry
  signature had fragmented into eleven angles, and that the drinker's
  cup-churn had poisoned the mouth reference the voice join cross-validated
  against — the measured circularity limit, now with pixels.
- **The occupant map** (`speaker_diag` face lane): full-res targeted-seek
  crops per (segment, seat), YuNet 5-landmark alignment (survey: SFace
  Apache-2.0 + YuNet MIT, pinned via the zoo's own LFS oids; InsightFace
  rejected on license like Rev.ai; box-only alignment REJECTED by the
  faceselftest — a different-person +0.397 beat a same-person +0.351),
  SFace embeddings averaged per entry, clustered at the largest dendrogram
  gap. Deddy: 4 persons exactly, 23/23 confident entries match the
  frame-verified truth, 13 signature angles → the 2 real cameras over 11/13
  segments, two pose-extreme singletons split conservatively (never
  mis-joined). **Operator gate on the contact sheet: PASSED.**
- **The replay finding**: joining over merged cameras with today's rule makes
  it WORSE (8.9→22.8 s claimed against the poisoned reference; even the
  laughter blob joins) — but the map exposes the poison as a visible
  contradiction: V1 joins seat A in both cameras while those seats hold
  different humans. Next slice (the production per-person join) designs
  against that: person-consistency gate + purity-weighted co-occurrence,
  own render gate.
- Housekeeping: the release exe the operator ran at 04:36 predated the
  integration commit by 2 h (why the Studio "showed no difference") —
  rebuilt; their 04:36 promote also overwrote the ANTITESA fixture segment
  with a different 40 s Moment, so the standing fg byte-pin is unmeasurable
  until they re-promote 790–859 (code-stability proven via stash A/B:
  HEAD reproduces the drifted bytes exactly). Suites 315/315 both ways.

## 2026-07-06 (small hours) — The voice lane joins production: fused attribution, off-screen splits, voice-gated interjection cuts

ADR 0042's gate passed, so this session made the spike real — one
implementation, the integration slice, with the grill settling four
decisions first (recorded in ADR 0042's new "Integration" section):

- **The fused lane IS production attribution.** `SpeakerAnalysis` carries
  `voice: Option<VoiceLane>` (per-bin joined seat / off-screen / overridden)
  and `speaking`/`confidence` are the fused result — `plan_shots` keeps its
  signature, every consumer reads the one lane the camera follows. The
  join/sweep/fusion logic lifted VERBATIM into pure `yc_frame::voice`
  functions, and `speaker_diag` now CALLS the production functions (~570
  copied lines deleted) — the ADR 0029 "harness measures a copy" hazard is
  structurally closed.
- **The two operator-approved behaviors live in `plan_shots` itself.** An
  angle piece whose voiced time is ≥50% known off-screen voice for ≥1.2 s
  (the demo's measured thresholds) becomes the visible pair's split — 
  decided BEFORE framing so it writes no anchor, and the return to that
  angle reuses the pre-split crop verbatim (measured: dpos 0px across both
  Deddy splits). A sub-2.4 s run survives absorption only when it carries
  ≥ the 0.8 s confirm hold of voice-OVERRIDDEN bins — and only a
  same-angle-joined voice can override, so "speaker on screen" is
  structural, not a check.
- **The Studio shows the evidence**: a voice row under the seat lanes
  (seat-colored claims, red off-screen spans), chip tags (`· voice`,
  `Off-screen voice · split`), a Camera-panel voice status line (or the
  reason the lane is off), and the export summary counts off-screen splits.
  The CAM++ model is an ADR 0041 download row (pin verified byte-for-byte +
  live URL this session); missing/broken model = mouth-only analysis, never
  a failed job.
- **Validated on the production path**: Deddy reproduces every spike number
  (thr 0.60, lane 8.9 s @ 93%, off-screen suspects 15.7/18.9/20.8 s) and the
  integrated plan differs from baseline by EXACTLY the two approved splits,
  camera audit clean; ANTITESA `camera_diag.fg` byte-identical to the pinned
  hash (follow-visible untouched, plus a unit test pinning voice-blindness
  there). 306 tests green in both feature configs. `diar_integration.mp4`
  rendered with the production export command for the operator's gate —
  old gate artifacts untouched.

Notable measured truth: the 20.8 s interjection does NOT earn its cut on
this fixture — its angle is single-visit, so the voice can't prove the
speaker on-screen; the off-screen split covers it (matching the approved
demo render). Making that cut fire needs person identity across angles:
face re-id, the next spike.

## 2026-07-05 (late night) — Framing memory: the jump-cut camera twitch is gone

The operator's ask after the diarization gate renders — "can we make the
jitter tracking more smooth" — turned out to be the attribution regime
re-deriving every angle piece's crop from that piece's own bins: Person A's
zoom breathed 894→702→884→736 px across 27 s of one seat talking. One
session, one mechanism (ADR 0038's new "framing memory" refinement):

- **Measured first**: speaker_diag gained adjacent-piece and
  best-prior-anchor forensics; on BOTH fixtures the numbers split bimodally
  — returns to an already-framed camera sit ≤0.26 face-heights / ≤7%
  height off, real changes ≥0.39 fh / ≥21.5% — so the dead-zone (0.30 fh /
  12%) was picked from the measured gap, not invented. (The session
  prompt's "subject barely moved" hunch was half-wrong: adjacent pieces
  are different ANGLES and genuinely move ~100 px; it's the RETURNS that
  barely move, which is what the memory acts on.)
- **Framing memory** (`piece_framing`, attribution regime's solo pieces
  only): per seat, whole-clip, FIXED anchors. Inside the dead-zone → the
  remembered crop VERBATIM (zero-twitch jump cut); same face height at a
  genuinely new position → the remembered SIZE re-placed (a lean moves the
  camera, never the zoom); otherwise fresh, always at a source cut where a
  re-frame is perceptually free. A per-axis band guard means a reused
  framing can never crop through a bobbing face. Angle identity is
  deliberately NOT computed — the geometry match subsumes it, so ADR
  0042's different-humans-per-seat constraint holds by construction, and
  the brittle 60 px signature stays a voice-lane diagnostic.
- **Result on Deddy**: boundaries, WHO and pans identical to the
  signed-off plan; 7 of 13 solo re-derives became exact reuses (Person A's
  0..27 s alternation is now exactly the source's two framings, 894 and
  702 px); a pan's next same-seat shot holds the pan's landing crop
  exactly. ANTITESA: byte-identical filtergraph (hash-checked). 293 tests
  green (5 new).
- **Operator retest, same night**: "22-26s still have jitter to the left"
  — that window is the follow-PAN piece, which the memory never touches
  (and which rendered identically pre/post). The per-bin CSV showed an
  out-and-back lunge (365 px out, most of the way back, face undetected
  the first 1.3 s) modelled as one linear glide — the camera slid left
  while the subject returned right. Fix: a pan must have its head→tail
  drift EXPLAIN the excursion (`PAN_EXTRA_FRAC` 0.6 — genuine drifts
  measure the off-drift remainder ≤0.36× their drift, the lunge 1.7×);
  an excursion holds ONE static crop grown over the whole band. Piece #4's
  glide became a stationary full-height frame; the three genuine drift
  pans and every reuse unchanged; ANTITESA still byte-identical; 294
  tests green.
- **Operator retest #2, same night**: "30-40 still have the same jitter"
  — the 11 s pan there passed the monotonicity test (net +56 px) but the
  series is wander-and-settle: ~5 px/s of PERMANENT camera micro-motion
  over a mostly-still subject, whose band fits a 1.06× static crop. Pans
  are now **static-first** (`PAN_STATIC_GROWTH` 1.25): a pan must be
  monotonic AND uncontainable; wander holds a static frame. All four
  Deddy pans became static — the plan has ZERO pans; seat A settles to 3
  framings and seat B to 4 across the whole 70 s clip.
- **The detector the operator asked for** ("detect first so we don't
  have this kind of bug again if I export new videos"):
  `audit_camera_plan` flags camera creep / re-frame without cause /
  subject adrift on every plan — printed by the diag harness, logged by
  `AnalyzeSpeakers`, and shown in the Studio Camera panel + export
  summary BEFORE a render. Zero findings on both fixtures is the
  regression bar (asserted clean post-fix); each defect class has a
  true-positive unit test. 298 tests green.
- **Gate PASSED (same night)**: the operator watched the round-3
  `camera_smoothing.mp4` (22:58) across all three symptom windows
  (jump-cut twitch, 22-26 s, 30-40 s) — "i think thats good, i see it
  and it fixed." Framing memory + static-first pans + the camera audit
  are the signed-off production camera behavior.

Next queued: the diarization integration slice
(nextprompt-diarization-integration.md).

## 2026-07-05 (night) — Diarization spike: a voice lane that hears who the camera can't see

The operator's verdict on the Deddy render ("the cut is good… not framing
the one who talking, maybe we need the diarization") kicked off the ADR
0038 upgrade path. One session, spike only — zero production-path changes,
gated on the operator's eyes/ears (ADR 0042):

- **Model**: 3D-Speaker CAM++ zh_en advanced (Apache-2.0, 28 MB,
  SHA-256-pinned from sherpa-onnx's release; ~2 s CPU per 70 s clip). The
  fbank frontend the ONNX expects is implemented pure in `yc_frame::voice`
  (unit-tested); a known-speaker selftest guards the whole path — and
  rejected the other candidate (WeSpeaker CAM++ LM scrambled same/different
  speakers under every documented convention).
- **The lane**: 1.5 s voiced windows embedded + cosine-clustered; the
  threshold swept per clip and scored OUT-OF-SAMPLE through the
  cluster→seat join (in-sample scoring was measured circular: overfit
  singletons "agree" 100% while carrying nothing).
- **The discovery**: on Deddy, seats are not people — voice cluster V1
  articulates as one angle's left man and speaks again where the camera
  shows a different pair, while the framed man DRINKS FROM A CUP (the cup's
  luma churn faked "talking"). The actual speaker was **off-screen** — the
  gap mouth motion cannot close. Joins are therefore per camera angle, the
  whole-clip join is banned in the attribution regime, and "known voice, no
  seat in this angle" prints as an off-screen suspect (it nails the proven
  stretch).
- **Measured**: ANTITESA 95% voice/mouth agreement over 39.6 s, 7/9
  switches confirmed, camera plan untouched (zero regression; the harness's
  baseline render is byte-count-identical to the operator's validated
  export). Deddy: both mouths move 90%+ of the time (attribution runs on
  margins), 55% of voiced time is shared laughter/cross-talk (correctly
  unattributable), and the conservative fusion changes zero shots — the
  rescued 1.1 s interjection dies to the 2.4 s min-shot grammar by design.
- **The gate artifacts** (in the Deddy stream folder): `diar_baseline.mp4`
  vs `diar_offscreen_demo.mp4` — the demo shows the visible pair's split
  screen where a known voice is off-screen (14.2–22.1 s: no more solo shot
  of a man sipping coffee while somebody else talks). Two grammar decisions
  for the operator: off-screen ⇒ split screen? interjections < 2.4 s ⇒ ever
  cut? Integration is the next session, after the verdict.

---

## 2026-07-05 (evening) — Deddy clip: 2 people were 4 tracks; camera now re-frames per angle

Operator's new bug batch on a Deddy Corbuzier export (4-person episode, edit
cuts between two-person angles + jump cuts): the camera drifted right/down,
attribution "sometimes didn't know who spoke", and the editor showed the
wrong person label (twin boxes on one head). The new `speaker_diag`
inspector (production glue 1:1) measured one shared root: **2 on-screen
people had become 4 tracks** — a detection gap plus a shift past the match
radius mints a duplicate track (fragments 20/104 px apart, co-visible
0.0-0.1 s, one with a 31.8 s hole). Fragmentation split the mouth-motion
signal (mean confidence 0.21, fake "switches" between one person's own
halves) and framing windows fell into fragment gaps (pans gliding toward
stale positions — a −114 px glide while the subject moved +2 px).

Fixes (pure `yc_frame::speaker`, 46 tests green): **same-seat fragments
merge** (near + alternating + each fragment shares the frame with someone
outside the pair — that context gate is what keeps Leon-style solo-camera
framings, which show nobody else, from ever fusing two people), and the
attribution regime **re-frames each shot at the source's own cut frames**
(WHO stays attribution, WHERE is per-angle; slivers fold; an occluded piece
keeps the previous framing). Measured on the same clip: 4 tracks → the 2
real seats (right seat 100 % presence, no gaps), confidence 0.21 → 0.38,
15 per-angle pieces whose remaining pans match real measured motion to the
pixel, re-render sweep 14 single switch spikes / 0 flash doubles, and the
cropped-off-forehead framing regains headroom. Validation render:
`Diskusi politik dan nutrisi (fix-validation).mp4` beside the export —
eyeball, then delete. Honest limit stated to the operator: mouth-motion is
already the "lips" signal; per-voice identity (diarization) stays the
documented upgrade path; 100 % is not a thing any detector delivers, but
the structural failures are gone. (ADR 0038)

**Operator verdict, same evening:** ANTITESA fix **signed off**; Deddy
cuts/framing **signed off**, with a residual — some switches still frame
the non-talker (WHO, not WHERE). Diarization is therefore
operator-confirmed as the next lever; the ready session prompt is
`nextprompt-diarization.md`, updated with this verdict (it opens by asking
the operator for wrong-person timestamps as labeled test moments).

## 2026-07-05 (later) — the same blank in the Studio preview + one camera control

Operator confirmed the fixed export's cuts are clean, then caught the **same
blank in the editor preview**. Same disease, different quantizer: the preview
picked the camera crop by the **playhead clock**, but the screen shows the
~2-4 fps filmstrip frame *nearest* the playhead when paused (up to hundreds
of ms away — parked just past a ✂ marker, the new crop sat over the old
scene for as long as you looked), and during playback the live pipe decoded
at a hardcoded 24 fps over the 23.976 source (the same per-cut coin flip the
render had). Fix, one principle: **the crop, tracking chip, face highlight,
and face-click retarget are all picked at the content time of the frame
actually on screen** (`display_time`: shown strip frame's time paused, newest
live frame's midpoint playing), never the playhead — and the live decode now
runs on the **source's own probed fps grid**, so the preview switch always
lands on the same displayed frame as the content switch, at any preview
resolution. The playhead stays the audio/caption clock, untouched.

Also merged the redundant camera controls (operator ask): the **"Detect
speakers" button is gone** — picking **Active Speaker / Group** already runs
the same analysis automatically on first use (the chips now say so on hover);
Failed keeps Retry. One control, one concept. (ADR 0038)

## 2026-07-05 — the flash-at-a-cut P1: one rounded digit, not a timing model

**The Active Speaker "blank at a cut" (an empty seat / half-out person for one
frame at some shot switches) is fixed at the root.** Per the frame-level
evidence rule, the actual frames were extracted and *looked at* before any
code changed. The bug was never detection, planning, or analysis fps — the
render wrote shot boundaries into `camera.fg` rounded to **3 decimals**. A
boundary is a real source frame's pts, and ffmpeg `trim`'s start is inclusive:
when the rounding lands **above** the cut frame's pts (a coin flip per cut —
7 of the ANTITESA clip's 14), the incoming shot rejects its own first frame
and the outgoing shot keeps it — one frame of the **new** scene rendered
through the **old** shot's crop. That is the "empty seat" flash, and why only
*some* cuts showed it. Boundaries now print at full precision (shortest
round-trip `{}`), with a regression test pinned to the measured production
pts (13.302833 — the old rounded 13.303 fails it).

Validated on the production path end to end: re-rendered the operator's exact
clip (same `segment.mp4`, seek, plan, crops, NVENC args) with fixed
boundaries — the export's scene-score series drops from **7 double-spike
flashes to 0**, all 14 switches land on their exact predicted output frame,
and frame strips at the three worst cuts show clean A→B switches. A
validation render sits next to the original export as
`Bicara tentang rebalance investasi (fix-validation).mp4` — eyeball, then
delete it.

Also **reverted an unvalidated uncommitted attempt** found in the tree (a
+60 ms `CUT_LEAD_S` hold and dissolve-boundary machinery in
`yc_frame::speaker`): measured against the real footage, every detected cut
IS the settled first frame of the new shot — no transition frames exist — so
the hold would have stranded 1-2 new-scene frames in the old crop at **every**
cut, making the flash universal instead of fixing it. (ADR 0038)

## 2026-07-04 — two P1 bugs: caption/audio desync root-caused to a segment-fetch snap; Active Speaker made production-ready

Two operator-reported P1s, both fixed at the root, no rendering (operator tests
the exports). Diagnosed with the `diagnose` skill: built an envelope
cross-correlation harness (throwaway, deleted) that gave a sub-10 ms,
0.99-correlation pass/fail signal against the real workspace media.

**P1-1 — captions drift out of sync (Leon Hartono / ANTITESA, not Guru Gembul).**
Not a gradual drift and not in the caption machinery: a **constant ~5.84 s
offset** (measured flat across the whole clip, corr 0.99). Root cause: the
promote path *assumed* a fetched Segment's timeline starts at the requested
section start (`in_segment_offset`), but yt-dlp's HLS `--download-sections`
snaps to a stream **fragment boundary** — on the Leon VOD the Segment actually
began **5.84 s early** (and ended that much short), while Guru's landed
sample-exact. Captions are cut from `analysis.wav` at the true VOD range, so the
snap shifted video+audio under fixed captions. It looked per-video because the
snap is per-VOD. **Fix:** a new `yc_ingest::align::measure_segment_anchor` —
cross-correlate the Segment's audio envelope against `analysis.wav`, two
independent windows that must agree (no false lock on repetitive audio), and
read the true VOD anchor; `resolve_segment` (pipeline) seeks the **measured**
offset and, when the snapped section leaves the clip's tail uncovered (the Leon
case also truncated the export ~4 s), **refetches once** with the request
widened. No confident lock falls back to today's assumption. Validated on both
real Segments: Leon delta **+5.844 s** (now corrected), Guru **+0.000 s**
(unchanged). This same snap was also desyncing the **speaker analysis** (video
frames at the wrong offset vs. audio VAD at the true range), so the fix directly
helps P1-2's "camera on a non-speaker" on affected VODs.

**P1-2 — Active Speaker tracking (cuts off movers, frames non-speakers, jumpy).**
Root cause was framing every shot from a **whole-clip** face position with no
motion inside a shot (ADR 0038 refinement). Three fixes in `yc_frame::speaker`:
(1) tracks match on **last-seen** position, not all-time mean, so a person who
leans/shifts stays one track instead of shedding a phantom; (2) each shot frames
its subject from that track's **path over the shot's own bins** (median + P10–P90
center band, crop grown to contain the band), so it frames where they *are
during that shot* and a bob never crops the face — AutoFace sized over the whole
path likewise; (3) a subject who **drifts** past a 12 %-of-crop dead-zone gets a
`Shot::pan_to` — a bounded within-shot **follow** that glides the same-sized crop
(render: one time-expression `crop` filter in `camera.fg`; preview: the same
`Crop::lerp`, so ADR 0036 holds). Cuts stay the grammar *between* speakers; a
short first run folds into the next shot so the camera never opens on a flash.
Recorded as an ADR 0038 refinement + CONTEXT glossary update.

**P1-2 follow-up — multicam sources + prop faces (same day).** Operator retest
on ANTITESA surfaced the real root: that VOD's *source is already a multicam
edit* (cuts between a one-person camera on each guest, never a shared wide
shot), plus a framed **photo on the set** that Ultraface tracked as a third
speaker. A production diagnostic (`speaker_diag`, real segment) proved it:
3 tracks incl. a 61×76 "face" at the table, mean 1.2 faces/bin. Two fixes in
`yc_frame::speaker`: (a) **reject printed-face props** in `finish` — a track
both much smaller than the tallest AND far less lively than the liveliest
(relative motion bar; codec-noise shimmer measured ~1/4 of a real mouth cleared
an absolute floor); (b) **detect the source regime** by `mean_visible_faces` —
a static wide shot (≈ track count in frame) keeps the attribution plan; a
multicam/solo source (≈1 face) uses the new `plan_follow_visible`, which ignores
audio (the source already cut to its subject) and frames the *largest visible
face* per bin, mirroring the source's cuts so the crop is never parked on an
off-screen position (the empty-crop screenshot). Re-verified on the real
segment: book dropped (2 clean tracks A/B), regime = multicam, each person
framed on their own position. ADR 0038 + CONTEXT updated.

**P1-2 follow-up 2 — source-shot framing (same day, after a render retest).**
Watching the render, the operator caught three more: a ~1 s blank right after
each cut, the crop not re-centering when the source cut to a wider framing, and
left/right jitter. A production diagnostic (per-bin face position + cut/jitter
dump) on the real segment showed the source cuts every 2–6 s between **four
camera framings** of the two guests, with brief wide two-shots — and my
follow-visible plan was framing from *leading-edge* positions (lagging each cut
~1 s) and picking the *largest* face per bin (flickering when two were visible).
Reworked `plan_follow_visible`: (a) **static median framing per source shot** —
correct from frame 1, no leading-edge pan, no per-bin jitter; (b) subject =
**one face → follow it, two+ → a `Group` split** (show everyone the source
shows) instead of a largest-face contest, so wide shots split cleanly and never
flicker; (c) `group_layout_span` splits only the people *on screen in that
shot*, not every framing of them. Re-verified on the real segment: 10 clean
shots matching the source's cuts, the wide stretch now a two-person split.

**P1-2 follow-up 3 — the blank crop was dropped tracks, not detection (same day).**
A third render retest (the 69 s rebalance clip) still showed a ~1 s blank at
cuts and a wide two-shot that didn't re-center. The operator asked for
"detection every second, quality over speed" — but a production diagnostic
proved detection was already perfect: at the blank stretch the detector returns
both faces at **p=1.00** (5 fps). The faces were *found then discarded*: each
multicam camera framing of a person is its own position track, and track
survival was gated on **20 % of the clip** — a 10 s wide-shot inside a 90 s clip
is ~11 %, so those tracks were dropped and the camera had nothing to follow
there (it held the previous single-cam crop over empty space → the blank). Fixed
by gating survival on **absolute on-screen time** (1.5 s) instead of a clip
fraction, raising `MAX_TRACKS` to 6, and adding a **hard size floor** to the prop
filter (a set photo that catches passing hands was beating the relative-motion
test). Result on the real 85 s segment: 4 clean tracks (book gone), the wide
two-shot now a proper two-person split, every solo shot on the right person —
no blanks. Detection rate untouched (the fix was track retention).

**P1-2 follow-up 4 — frame-accurate cuts + clean splits (same day).** A fourth
retest: a 1-5 frame blank at every cut, and the wide shot sometimes a three-way
column / empty-panel split. Diagnosis: (a) the **5 fps analysis grid quantized
cuts to 0.2 s**, so the render held the old crop up to ~5 frames past the source
cut (the blank) — fixed by raising **`SPEAKER_FPS` 5 → 24** (frame-accurate cuts;
~8 s analysis for this clip; all downstream windows derive from the one rate; the
frame tests made fps-agnostic via an `nbins` helper); (b) **`group_layout_span`
counted any track with a single frame in the span**, so a person's second camera
framing (multicam → multiple position tracks per person) inflated a two-person
split into a column — fixed with a **40 % presence bar** so a split shows only
the people actually in the shot. Re-verified on the real 85 s segment: cuts land
frame-accurately (0.75, 2.50, 7.96 s… not 0.2 s multiples), wide shot a clean
two-person split.

**P1-2 follow-up 5 — the source's real cut frames as shot boundaries (same day).**
Even at 24 fps a ~2-frame blank lingered at cuts: any fixed sample rate
quantizes a cut to a bin, and the source rate needn't match the grid. The
frame-precise fix: stop inferring cuts from sampled detections and read the
source's **own cut frames** — `do_analyze_speakers` runs one cheap ffmpeg pass
(`select='gt(scene,0.2)',metadata=print`, pixel-level scene detection, no model)
for the exact clip-relative time of every hard cut, and the multicam
`plan_by_scene_cuts` makes each inter-cut span one shot (framed on whoever is on
screen, a split when 2+ share it, adjacent same-subject spans merged). Every cut
now lands on the exact source frame regardless of sample/source rate — 100 % cut
accuracy. Verified on the real segment: shot boundaries are the detected cuts
themselves (0.751, 2.502, 7.966, 9.551…), wide shot a clean two-person split.
`plan_follow_visible` stays the fallback when no cuts are detected.

**P1-2 follow-up 6 — the residual "blank at cut" was preview drift, not the plan
(same day).** The operator still saw a blank flash at cuts and felt the audio lag
— but in the **editor preview**, not necessarily the export. Root cause found by
elimination: `player.rs` and `main.rs`'s audio path are **unchanged this whole
session** (so the audio lag is not a regression), and the render is deterministic
(scene-cut boundaries, `-map 0:a:0` from the same seek — no drift). The preview,
though, starts the playhead-driven crop and the rodio audio the instant Play is
hit, while the live ffmpeg decoder needs ~0.1-0.5 s to spawn+seek+decode frame
one — so the crop *leads* the video and every cut flashes blank, and A/V drifts.
Fix (`editor.rs` + `player.rs`, preview only): **drive the playhead by the live
decoder's delivered-frame count** (`PreviewPlayer::video_secs` = frames /
`PLAY_FPS`), not wall-clock — the crop then advances frame-for-frame with the
video and *cannot* switch to the next shot before that frame is on screen (a
first-frame resync-only fix, tried first, only cured the spawn latency, not the
ongoing drift of a decoder that isn't perfectly real-time — the operator nailed
it: "cut after the play line passes the cut"). On the first frame the audio is
(re)anchored to it; a 1.5 s / spawn-failure fallback keeps wall-clock so
filmstrip playback is never frozen. The exported render was already correct;
this makes the *preview* reflect it. (GUI-only — not covered by headless tests.)

Verification: 6 crates compile with `--features face`, **all tests green**
(core 14, frame 42, render 41, ingest 20, app 29 — incl. anchor, span-framing,
follow-pan, lerp, prop-reject + hard-floor, multicam follow/cut/group,
short-framing-survives, fps-agnostic timing, and scene-cut boundary/merge tests);
every non-GUI stage validated on the real ANTITESA segments via production
diagnostics (removed after use); the preview A/V resync is GUI-only and needs an
in-app check.

---

## 2026-07-03 (night) — the ensemble hang: diagnosed to WDDM VRAM spill, Cancel made real, watchdog fallback

The operator caught it live: an ensemble caption pre-pass "stuck" (one decode
ran **18 minutes**; healthy is 10–15 s), a Render queued blind behind it, and
a Cancel that did nothing. Forensics from the stream folder + three controlled
repros pinned the crawl: **WDDM VRAM oversubscription** — with the GPU free
the same clip's full ensemble export takes ~100 s; an *idle* 2.5 GB VRAM
holder changes nothing (WDDM evicts cold pages); an *active* one collapses
even whisper ~20x, because CUDA silently spills to shared system memory
instead of erroring. The incident's competitor: **the operator was watching a
Discord livestream** — the exact profile the evidence bracketed (video decode
+ compositing touch their VRAM every frame so WDDM can't evict them, few
hundred MB + hot dwm/overlay, near-zero compute), which is why whisper
(~3.5 GB) still fit while mtmd (~6.8 GB peak, ~400 MB headroom on a quiet
desktop) spilled and crawled ~75x. Fixes (ADR 0034 amendment):

- **Cancel now reaches the ensemble.** All three child kinds (ffmpeg cut,
  deep-filter, llama-mtmd-cli) run under the cancel poll and are killed
  mid-run (`wait_killable` semantics; mtmd waits on a 50 ms `try_wait` loop
  over piped, thread-drained stdio). `apply` polls between stages; a cancelled
  child aborts the job as Cancelled instead of soft-falling into a render.
- **A cancel also drains the queue.** The incident's queued Render would have
  reset the token and started a fresh full transcribe+render seconds after
  Cancel; the worker now flushes everything queued behind a cancelled job.
- **Watchdog on every decode** (30 s + 1.5x audio, floor 120 s ≈ 6–10x
  healthy): the first timeout aborts the WHOLE ensemble (typed
  `DecodeTimeout` — the conditions would hold for all remaining variants) and
  the export continues on whisper captions, with the reason in the status bar
  — bounded ~2–4 min worst case instead of a 90-minute crawl.
- **The longest stage now shows progress**: "Qwen ensemble — decode 2/5" per
  variant in the status bar; the editor's Captions panel says to watch it.
  A Render clicked while the pre-pass runs now reads "Render queued — waiting
  for captions" instead of claiming NVENC is running.

252 workspace tests green (3 new). In-process whisper decodes still have no
watchdog (June-19 scar: the abort callback collapses CUDA-graph throughput) —
under heavy contention whisper can still crawl, but Cancel now bites at every
stage boundary and the ensemble can no longer anchor a pile-up. **Operator
verification owed:** Cancel mid-ensemble in the GUI, and the practical rule —
close/pause GPU-hungry apps (games, active encodes) before an ensemble
export.

## 2026-07-03 (late) — B/C/D shipped: sentence-boundary clips, in-app downloads, the brand

The three sessions grilled this afternoon, built in one sitting (commits
`1879991..a53874b`, 249 workspace tests green):

- **Sentence-boundary clip bounds, 45–180 s (ADR 0040)** — ADR 0037's revisit
  clause fired: detected Moments cut words mid-sentence. Refine now
  transcribes each candidate over a ±15 s padded window and snaps bounds to
  sentence start/end (whisper punctuation en/id/ja, inter-word-pause
  fallback). Mid-sentence edges snap *outward*; a trailing edge in dead air
  retreats to the last complete sentence; a leading edge in silence keeps the
  ADR 0020 pre-roll. The floor rose 15 → 45 s and is reached by pulling in
  *adjacent real sentences* — when speech runs dry the shorter clip ships
  (context over duration, never dead-air padding). The cap drops the crossing
  sentence; a run-on falls back to a breath pause, then a word boundary —
  never mid-word. NMS spacing + overlap survival inherit the 45 s floor
  (the same "one Moment per clip-length" semantics, rescaled); the GUI
  slider floor rose 30 → 45. Manual Moments are exempt by construction. New
  pure module `yc_detect::sentence`, 12 synthetic-transcript tests.
  **Operator still owes the real-detect ear-check.**
- **In-app dependency downloads (ADR 0041)** — every missing Diagnostics row
  now carries a **Download** button (+ "Download all missing (N GB)").
  Downloads run on the serial worker: streamed to a `.part` beside the
  destination, SHA-256-verified against the pin, then atomically renamed or
  unzipped into place — a mismatch installs nothing and says why. A real
  progress bar rides the status bar; Cancel aborts mid-stream; a fresh deno
  re-resolves without a restart. All 14 pins (gyan ffmpeg 8.1.2, yt-dlp
  2026.06.09, deno v2.9.1, HF revision-pinned whisper/Silero/Qwen3-ASR pair,
  bartowski's Qwen2.5-7B Q5_K_M — the judge GGUF's true origin, found by
  hash — llama.cpp b9859 pair, deep-filter v0.5.6, Anton, Ultraface, zenodo
  w2v2) were cross-verified byte-identical against the operator's working
  set. New deps: ureq (rustls), sha2, zip.
- **The brand: "Punch-out" (concept A of four rendered)** — the operator
  picked from an artifact page of four directions (punch-out / spike /
  clipped-play / YC monogram, each on light + ink + gold, with lockups and
  taskbar-size tests). A vertical Short punched out of a wide VOD frame,
  gold on ink: master SVGs in `assets/branding/`, a GDI+ raster script
  (`scripts/render-brand.ps1`) producing the multi-res `.ico` (embedded into
  the exe via new `build.rs` + winresource) and a raw-RGBA window/taskbar
  icon (no image decoder ships), a vector-painted `theme::brand_mark` in the
  brand bar beside the wordmark — now **YT CLIPPER** in Anton gold — and the
  README hero.
- **README rewritten to current reality** — Studio, Podcast Mode, ensemble
  captions, the dependency registry + in-app downloads, the brand hero
  (dark/light `<picture>`), and build docs that offer Diagnostics →
  Download as the script-free path.

## 2026-07-03 — operator bug batch: WYSIWYG caption size, 60 fps always, pre-pass cancel, full diagnostics, QoL

The operator filed 6 bugs + a features list; the grill scoped one review-round
batch (8 commits `77ef27e..c27a932`, 233 workspace tests green) and queued
three follow-up sessions. Fixed:

- **Exported captions no longer smaller than the preview** (top-filed bug),
  measured root cause: libass sizes a face by its **OS/2 win cell** (VSFilter
  compat), egui 0.34/skrifa by the **em square** — Anton (upem 2048, win cell
  3550) drew **1.73×** bigger in the preview. A one-glyph burn through the
  production ffmpeg proved the mapping (Fontsize 150 → 75 px cap height =
  the win-cell prediction). Per ADR 0036 the burn is ground truth, so the
  *preview* now multiplies by `upem/winCell`, unit-test-pinned against the
  shipped TTF's own tables (ADR 0036 § Calibration). Captions in the editor
  now look smaller — that's the render's truth; scroll-to-resize placement is
  the size lever and carries to the export.
- **60 fps everywhere**: the GPU-stage 10 fps repaint throttle is deleted
  (operator's explicit call; measurement knowingly skipped) — spinners stay
  fluid through whisper/NVENC/LLM/ensemble. If a detect ever crawls again
  (the June-19 scar), the revert is one commit: `b60d39b`.
- **Back/Esc from the Studio cancels the caption pre-pass**: stage-boundary
  semantics — killable children (ensemble sidecars, ffmpeg, deep-filter) die
  now, results are discarded, downstream stages skip; an in-flight in-process
  whisper decode finishes invisibly and is thrown away (the June-19
  abort-callback scar stands: installing the hook collapses CUDA-graph
  throughput). The worker also flushes pre-pass jobs *queued behind* the
  cancel (the top-bar Cancel had the same hole), and a cancelled speaker
  analysis no longer leaves the Camera panel spinning forever.
- **Diagnostics is now a dependency registry**: every external tool/model the
  pipeline resolves — the Qwen3-ASR GGUF pair, llama-mtmd-cli, deep-filter,
  Silero VAD, the LLM judge + its model, face/SER models — in one table
  (`dependency_registry`) driving the page. Required rows go red when
  missing; optional rows show a neutral "not installed". The ensemble's
  mtmd/Qwen paths moved out of pipeline.rs's ad-hoc derivation into AppPaths
  so the page and the render can't disagree. Session C hangs per-row
  Download buttons off this same table.
- **QoL sweep**: review audio stops when the editor opens (promote forgot
  what Back/Render remembered); the ensemble's 4 spawn sites got
  `no_console` (the "terminal window flashes during processing" report —
  ensemble.rs postdated the ADR 0025 sweep); **Open / Folder buttons +
  Ctrl+O** on the Done chip launch the exported Short; **no em dashes** in
  any UI string, and generated Titles normalize em/en dashes to hyphens at
  the parse boundary (test-pinned — titles also name the exported files).

Grill decisions recorded for the queued follow-ups: **(B)** sentence-boundary
clip bounds 45–180 s (ADR 0037's revisit clause fired; boundaries from
whisper punctuation at refine via a padded window; grow through real speech
only, accept shorter when it runs dry; 180 s cap drops the trailing
incomplete sentence; manual marks stay verbatim), **(C)** in-app dependency
downloads off the new registry (ureq + sha2 + zip, pinned official URLs;
CONTEXT.md's "Offline" term already widened), **(D)** full brand pass
(black/yellow logo → exe ico + window icon + top-bar wordmark + README hero,
concepts to be brought as rendered SVG variants).

Details: `handoffs/2026-07-03-operator-bug-batch.md`.

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
  named upgrade path; per-Creator memory still stores genre only, not full
  custom styles; transcript edits don't feed the dialect store (per-clip
  fixes vs durable curation stay separate lanes).
- **Same-day follow-up (operator review round)**: playback upgraded from the
  4 fps filmstrip to a **live streaming decode** (ffmpeg `-re` → rgb24 pipe →
  one reused texture, 24 fps @ 640p; filmstrip stays for paused/scrub) —
  measured 120 frames / 5 s at 0.98x realtime; plus the review-round fixes
  (theme pinned dark both-slots + white strong text, chip grid, scrub/seek
  audio restarts, review-queue snapshot, clickable timeline captions,
  engine-aware transcribe placeholder, editor loading state, path-free
  diagnostics).

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
