# Two-stage judge: whole-video digest + curiosity-gap title rules

The operator's `feature-implementation-plan.md` closes with **Title Generation
Improvements**: *Video → transcript → LLM reads complete transcript → high-CTR
Shorts title*, understanding main topic / conflict / curiosity / emotional
impact / controversy / keywords — prioritizing curiosity gap, strong hooks, and
honesty. Their example titles (in the plan file — the ground-truth style) are
Indonesian podcast shapes: pointed questions ("Ilmu vs Guru: Mana yang
Sebenarnya Lebih Penting?"), warnings ("Hati-hati Kebalik!"), hidden dangers,
quoted concepts. The operator queued this slice and said "do this
automatically", which per `nextprompt-title-gen.md` means: decide the
"complete transcript" fidelity by their written plan + cost-honesty, state it,
and pre-register the bars here BEFORE touching the prompt.

Today's titles show the gap on the saved ECA podcast table (25 Moments,
`LeR59VmXiSc`): dead topic descriptions ("Diskusi tentang generasi Z",
"Membicarakan Proses Membuat Lagu"), **5/25 in English** on an Indonesian VOD,
and one title containing the prompt's own banned filler ("Nutritional advice
gone wrong"). Root causes: each title inference sees ONLY its own ~30-90 s
transcript (no idea what the video is or who is talking), and the rubric is
game-streamer-flavored while the operator clips podcasts.

## Decision

**(a) Whole-video digest, two-stage, at detect time** — chosen over (b) a
whole-VOD transcription pass and (c) per-clip neighbor-transcript reading:

1. **Stage 1 (new): one digest inference.** The judge process — model already
   loaded, all candidate transcripts already in the `JudgeRequest` — first runs
   ONE ungrammared inference over the VOD's uploaded title + creator name + the
   candidate transcripts in timeline order (per-candidate and total char caps),
   producing a 3-5 sentence brief in the transcript's language: what kind of
   video (podcast / game stream / interview), who speaks, main topics,
   strongest tensions and claims. Context window for this call: 8192 (Qwen2.5's
   native 32k covers it; KV ≈ 0.5 GB fits the 8 GB card after the whisper drop).
2. **Stage 2: every per-candidate inference gets the digest** as
   whole-video context, plus a rewritten title rulebook: curiosity-gap shapes
   lifted from the operator's examples (pointed question, warning, bold
   claim/confession, hidden danger/mistake, single-quoted charged phrase), an
   explicit "never translate into English" language lock reinforced per-prompt,
   and a named ban on dead topic-description titles ("Membahas X", "Diskusi
   tentang Y"). The scoring rubric reframes to "a live game stream, a podcast,
   or a talk show" — the digest tells the model which — while the ADR 0009/0010
   scripted-cutscene mitigation stays, clause-for-clause, guarded by the same
   (extended, never deleted) pinned tests.

Why not (b): ~0.9× realtime whole-VOD whisper adds ~70 minutes to an 80-minute
VOD's detect (near-doubling it) to transcribe mostly the low-signal talk
discovery already rejected, and a 12k-word transcript fits no judge context
without a new chunked map-reduce summarization surface. The candidates are the
video's high-signal cross-section; their union is the "complete transcript" of
everything clip-worthy. If the operator's eye later finds the digest blind to
off-candidate context, (b) can layer in behind the same digest seam without
touching the per-candidate prompt again. Why not (c): 25× per-prompt token cost
with less synthesis than one digest.

Genre-awareness deliberately rides in the digest rather than a per-Creator
genre enum: no new Creator-store surface, no egui, and a mixed-genre creator
(Deddy does both podcasts and mukbang-style chaos) gets per-VOD truth instead
of a sticky per-Creator label.

## IPC and persistence changes

- `JudgeRequest` gains `vod_title` + `vod_creator` (serde-defaulted);
  `JudgeCandidate` gains `start_s` (serde-defaulted) so the digest reads
  excerpts in timeline order.
- The judge's stdout becomes `JudgeResponse { digest, verdicts }`. The app
  parses leniently — object first, bare `Vec<JudgeVerdict>` array fallback — so
  a stale judge exe beside a new app degrades to today's behavior (empty
  digest) instead of failing the signal.
- `core::Project` gains `digest: Option<String>` (serde-defaulted,
  skip-if-none): the brief is cached per-VOD for the review trail and any
  future re-title flow. The worker logs it at info level so headless `--detect`
  output shows it.
- Digest failure (overflowed context, model hiccup) is soft: warn, empty
  digest, per-candidate prompts degrade to today's no-context shape.

## Pre-registered bars (before any run)

A = the saved ECA table (`data/project.json`, the operator's production state —
the diff base `nextprompt-title-gen.md` names). B = one fresh headless
`--detect` on `LeR59VmXiSc` (id) with the release build of this change, same
cached `analysis.wav`, GPU idle-gated. Operator data byte-backed-up first and
restored SHA-identical after (the ADR 0063 pattern).

1. **Parse:** 25/25 Moments carry a non-empty title (an empty title is the
   lenient-fallback fingerprint), each ≤60 chars after `normalize_title`.
2. **Language:** 0 English titles on this Indonesian VOD (A has 5/25).
3. **Honesty/filler:** no title contains a banned filler phrase (A has one);
   no title verbatim-copies an example shape out of the prompt (parroting
   check).
4. **Tests:** every pre-existing pinned prompt clause still asserted and
   green; suites for `yc-detect`, the app, and `yc-core` pass; `cargo clippy
   --workspace --all-targets` exits 0; `yc-llm-judge` builds with its real
   CUDA features.
5. **Cost:** the digest adds exactly ONE inference per detect; the LLM-judge
   stage's wall-clock grows ≤ 60 s over the 25-candidate batch.
6. **The operator's eye gates the ship:** the before/after table lands in this
   ADR's Outcome for their verdict; rank moves are expected (the rubric now
   knows podcasts exist) and are listed, not hidden. A one-commit revert
   restores the old prompt if they refuse it.

Scores/ranks are NOT pinned frozen: the reframed rubric may legitimately move
podcast-flavored candidates. The bars above are the mechanical floor; the
operator's eye on the table is the judgment call.

## Outcome (2026-07-18)

**All five mechanical bars PASS; bar 6 (your eye on the table below) is open.**
B ran headless `--detect` on the ECA podcast (`LeR59VmXiSc`, cached
`analysis.wav`, release build with `face,align,ser`, GPU at desktop idle);
your `project.json`/`review.json` were byte-backed-up first and restored
SHA-identical after (`DA7B6640…`/`CDBF3B17…`; the fresh outputs live in the
session scratchpad, the backup pair stays at `data/_backup_title_ab/`).

1. **Parse 25/25** — every Moment titled, longest 55 chars, none empty.
2. **Language 0/25 English** (A had 5/25) — the per-prompt closing reminder
   held even though the digest itself came out in English (see residuals).
3. **Filler/parroting 0/25** — A's "Nutritional advice gone wrong" is now
   "Dibilang Narkoba, Gue Jawab Kau Tau?"; no title copies a prompt example
   (the `Gemini vs Scorpio: Mana Lebih Baik?` shape-adaptation is the
   intended behavior, not a copy).
4. **Tests** — yc-detect 57, app 66, judge 1, all pinned clauses extended and
   green; clippy `--workspace --all-targets` exit 0.
5. **Cost** — the digest inference took **4.4 s** (model loaded 13:45:26.3 →
   digest ready 13:45:30.6, 636-char brief), 13x under the 60 s bar; the 25
   verdicts followed in ~73 s (~2.9 s each), whole detect ~13 min.

The digest it wrote (englished, see residuals): *"discussion show where Deddy
Corbuzier, Pandy, Nino, and Vadi discuss … 'red flags' in relationships …
jokes about Gemini and Capricorn … being recorded without consent …"* — the
show, the guests, and the running topics are all real, and the titles below
visibly lean on them.

### Before/after, paired by start time (rank = that run's ordering)

| start | A rank | A title (your saved table) | B rank | B title (this change) |
|---|---|---|---|---|
| 0m12s | #7 | Diskusi tentang generasi Z | #24 | Musuh Rezim? Apa Artinya? |
| 3m01s | #5 | Mengobrol tentang zodiak Gemini | #9 | Gemini vs Scorpio: Mana Lebih Baik? |
| 5m27s | #23 | Diskusi Natalan Antara Penonton | #3 | Corbuzier nyebut Capricorn pintar |
| 7m36s | #17 | Momen Anak Pinjam Jaket | #7 | Ditangkap Kamera Tanpa Izin! |
| 10m09s | #21 | Fadli bertemu bully di rumahnya | #13 | Fadi dikasih jaket begitu? |
| 11m51s | #10 | Nino sedih lihat dia dari zero | #8 | Fadi Botak dan Celahnya |
| 15m52s | #4 | Faddy ngomongin komentar fans | #2 | 'Tokoh Antagonis di Sinetron Jadi Jahat di Dunia Nyata' |
| 19m03s | #11 | Mas Panji bercanda tentang komentar | #1 | Dasar Pemabuk Gitu! |
| 21m15s | #14 | Debating on Twitter, gue ngerti now | #12 | Ken, kita berdua debrief ya |
| 29m10s | #2 | Deddy mode on, Fadi scared | #16 | Om Deddy Menyapu Fadi! |
| 31m01s | #12 | Bercanda tentang menjadi korban | #15 | Becanda yang menjatuhkan orang |
| 32m16s | #15 | Mabuk-mabukan dan Tips Memabuk | #21 | Manfaat Mabuk Menurut Dia |
| 33m41s | #6 | Kepala kayak biji wijen, lucu banget! | #11 | Kepala kayak biji wijen? |
| 36m44s | #16 | Nutritional advice gone wrong | #20 | Dibilang Narkoba, Gue Jawab Kau Tau? |
| 38m00s | #1 | Bapak 42 Tahu Gak? | #6 | Bapak 42 Tahu Gak? |
| 39m25s | #22 | Echa bikin album lagu baru | #14 | Echa Bikin Album Lagu Baru! |
| 42m33s | #13 | Pertanyaan Unik dari Orang Tua | #10 | Pilih ASKA atau Celebrity? |
| 44m02s | #8 | Om Deddy Ternyata Berbeda Di Depan Kamera | #4 | Deddy Bercerita Tentang Rasa Sayang yang Menyeramkan |
| 45m21s | #20 | Daddy's gossip style revealed | #17 | Daddy Gosip Banget |
| 48m12s | #18 | Membicarakan Proses Membuat Lagu | #23 | Terkait Proses Bikin Lagu |
| 51m11s | #19 | Momen Awal Menyapa di YouTube | #19 | Nge-Prank Co-Host itu Begini! |
| 52m09s | #9 | Diskusi tentang Om Deddy dan onde-onde | #5 | Om Deddy jadi onde-onde |
| 54m19s | #25 | Bicara tentang sabun multi tujuan | #25 | Bicara Produk Ekologis |
| 77m28s | #24 | Tanya Lagu Baru Lin Buat Gamilla | #22 | Kursi Bolong di Acara Ini |
| 79m01s | #3 | Membahas Lagu Botak Botak Mania | #18 | Botak Botak Mania - Jadi Album? |

Rank moves are the reframed rubric doing its job (a podcast rubric scoring a
podcast); starts drift a few seconds on some rows from whisper refine
nondeterminism + sentence snapping, not from this change.

### Residuals (honest, for the next slice or your call)

- **The digest came out in English** despite `DIGEST_SYSTEM` asking for the
  transcript's language. It is internal context only and the titles stayed
  Indonesian, but a per-prompt language reminder (the same fix that worked
  for titles) would pin it.
- **2/25 titles still topic-describe** ("Terkait Proses Bikin Lagu", "Bicara
  Produk Ekologis") — both bottom-quartile moments with thin transcripts;
  down from ~10/25 in A.
- **"Ken, kita berdua debrief ya"** (21m15s) reads like a transcript garble
  ("Ken" is likely a mis-heard name) — a caption-accuracy issue upstream of
  titles, not a title-rule failure.

**Your eye gates the ship** (bar 6): if these titles are wrong for your
channel, `git revert` of the implementation commit restores the old prompt
wholesale — the ADR and the A/B table stay as the record either way.

## Outcome, same day: the eye loop — two verdicts → v2.2 (final, approved)

Bar 6 ran live, twice, and closed affirmatively:

1. **Verdict 1 on the table above: "we need more catchy and hooking"**, the
   11 example styles re-pasted. Structural read of those examples: nearly
   all are TWO-BEAT — a hook (question / charged claim / scare-quoted
   concept) plus a payoff tease that raises stakes ("Hati-hati Kebalik!",
   "Ini Awal Mula Masalahnya", "(Banyak yang Salah Kaprah)", a trailing
   "..."), charged with stakes words (Bahaya Tersembunyi, Kesalahan Fatal,
   Akar Masalah, Kritik Keras), 40-60 chars. **Iteration 2** encoded that
   and the re-run went 24/25 two-beat, avg length 28→37.4 — but FAILED the
   pre-registered parrot bar (2 verbatim 'Hati-hati Kebalik!' pastes),
   shouted whole phrases in ALL-CAPS, and garbled words on thin banter
   ("COBUJER", "TRAKTOR", "BJI WIJEN").
2. **Verdict 2 on iteration 2's titles: "i like it now."** The energy —
   including the caps — is approved. The drafted whole-phrase caps leash
   was therefore DROPPED (pinned as such in the tests; do not re-add).
3. **v2.2 shipped** = iteration 2's construction + three honesty leashes
   that do not dampen it: BEAT 2 must be built from THIS clip's own words
   (template payoffs are elided from the prompt entirely — only '...'
   stubs remain), names and quoted words copy letter-for-letter, and a
   thin transcript earns one clean honest beat instead of an invented
   second one; a capitalized word must still be a correctly spelled real
   word. Final ECA run: **all bars pass** — 25/25 titled ≤60, 0 English,
   0 filler, **0 parroting**, 25/25 two-beat, avg 35.2 chars; digest
   reproduced at 636 chars, cost unchanged. **Operator smoke-tested the
   result and ruled it good** — the arc is closed.

### The three runs, paired by start time (baseline → approved energy → shipped v2.2)

| start | baseline (saved) | iteration 2 (verdict: "i like it now") | v2.2 (shipped) |
|---|---|---|---|
| 0m12s | Diskusi tentang generasi Z | MUSUH GEN Z? KENAPA TUH? | Deddy Corbuzier: Lo Musuh Gen Z? |
| 3m01s | Mengobrol tentang zodiak Gemini | GEMINI APA SIH? Maksudnya Ada Red Flag! | Gemini Tidak Banyak Red Flag? Kenapa Aska? |
| 5m27s | Diskusi Natalan Antara Penonton | GEMINI VS CAPRICORN: Mana yang Lebih Pintar? | Gemini vs Capricorn: Mana yang Lebih Pintar? |
| 7m36s | Momen Anak Pinjam Jaket | Capricorn Bro Wiss! Dia Pakai Jaket Gue? | Capricorn Serang Pak Yaya Dengan Jaket Gue? |
| 10m09s | Fadli bertemu bully di rumahnya | FRAMING GILATAN! Kenapa Dia Muka Baik? | Fadi Masuk Benteng, Gue Usilin Dia Balik! |
| 11m51s | Nino sedih lihat dia dari zero | KASIAN NIH KAYAK GAK SIH? Fadi dan Nino | Fadi Botak? Ini Jawabannya! |
| 15m52s | Faddy ngomongin komentar fans | Faddy yang Kuat? Aku Gak Tahan! | Faddy yang Kuat? Aku Gak Tahan! |
| 19m03s | Mas Panji bercanda tentang komentar | MAS PANJI GAK ADA YANG NYERANG? Hati-hati Kebalik! | Dasar Pemabuk Gitu! Ada yang Nyerang? |
| 21m15s | Debating on Twitter, gue ngerti now | Ken, kita berdua debrief ya?! | Ken, kita debrief ya... |
| 29m10s | Deddy mode on, Fadi scared | OM DEDDY MULAI CHALLENGE! | Fadi vs Deddy: Siapa yang Benar-benar Siap? |
| 31m01s | Bercanda tentang menjadi korban | Pemabuk? Dasar Lo Gitu! | Becanda yang menjatuhkan orang... |
| 32m16s | Mabuk-mabukan dan Tips Memabuk | GEMINI VS CAPRICORN: APA MANFAATNYA? | Nino: Tips Menjadi Pemabuk, Gue Yang Memabuk! |
| 33m41s | Kepala kayak biji wijen, lucu banget! | KEPALA LU KAYAK BJI WIJEN? Aiii Jago Dia! | Kepala Lu Kaya Wijen? Astaga! |
| 36m44s | Nutritional advice gone wrong | KURANG GIZI? MAKSA DIA DENGAN TRAKTOR! | Dedi: 'Kau Tau' Gitu Ya? |
| 38m00s | Bapak 42 Tahu Gak? | GAGALAN GAYA HIDUP? Gak Pakai Kacamata Usia 42+ | Bapak 42 Tahu Gak? Dia Nanya Umur! |
| 39m25s | Echa bikin album lagu baru | ECHA BIKIN ALBUM LAGU BARU?! | Echa Bikin Album Lagu Baru! Wow! |
| 42m33s | Pertanyaan Unik dari Orang Tua | Pilih ASKA Sama Celebrity atau Manusia? Gila! | Pilih ASKA Sama Celebrity Atau Manusia? |
| 44m02s | Om Deddy Ternyata Berbeda Di Depan Kamera | EMANG GUE SAYANG? Ada Intimasi di Sini... | Deddy: Gue Sayang Lu, Lu Jaman-Jaman |
| 45m21s | Daddy's gossip style revealed | Daddy GOSIP? Ini Bahan Gossipnya! | Daddy Gak Berdua? Ini Alasannya! |
| 48m12s | Membicarakan Proses Membuat Lagu | Terkait Proses Bikin Lagu, Gue Sampai Sayang! | Deddy Sayang Banget sama Panji! |
| 51m11s | Momen Awal Menyapa di YouTube | Kurang Ajar? Kenapa Dia Sering Ledek Co-Host! | Kritik Keras: Jangan Sampai Ngeledekin Co-Host! |
| 52m09s | Diskusi tentang Om Deddy dan onde-onde | OM DEDDY COBUJER? Hati-hati Kebalik! | Om Deddy Jadi Onde-Onde Cobujer! |
| 54m19s | Bicara tentang sabun multi tujuan | Bicara tentang Sabun Ekologis | Biodegradable Sabun Untuk Apa? |
| 77m28s | Tanya Lagu Baru Lin Buat Gamilla | LOH, KURSINYA BOLONG! KENAPA NGGAK DIBAYAR? | Nah Kalau Misalkan... Ada yang Minta Tiket? |
| 79m01s | Membahas Lagu Botak Botak Mania | Bobo - Eh Engga, Nama... (Botak Botak Mania?) | Dedi Tawar Nulis Lagu Bersama! |

Residual carried forward: "Cobujer"/"Dedi" spelling drift in two v2.2
titles mirrors the TRANSCRIPT's own phonetic garbling of the name — the
letter-for-letter rule is copying faithfully from a garbled source. That is
caption-accuracy work (name dictionary / dialect store), upstream of title
rules. Operator data was byte-restored SHA-identical after every run.

## Considered options

- **Whole-VOD transcription pre-pass (b).** Rejected for this slice on
  cost-honesty (above); the digest seam leaves it open as a later escalation.
- **Per-Creator genre field in creators.json.** Rejected: new store surface +
  GUI for a fact the digest infers per-VOD for free.
- **Per-clip neighbor transcripts (c).** Rejected: token cost without
  synthesis.
- **Second title-only inference round (score first, title with digest
  second).** Rejected: doubles per-candidate inferences for no information the
  single call can't use — the digest is in-context either way.
