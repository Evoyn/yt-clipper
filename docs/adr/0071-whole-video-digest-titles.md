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
