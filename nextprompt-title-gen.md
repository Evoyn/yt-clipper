# Session prompt — title generation improvement (editor-suite plan, non-egui slice; queued 2026-07-12)

The operator's `feature-implementation-plan.md` (repo root — THEIR file) closes
with "Title Generation Improvements": *Video → transcript → LLM reads complete
transcript → high-CTR Shorts title*, understanding main topic / conflict /
curiosity / emotional impact / controversial statements / keywords —
prioritizing curiosity gap, strong hooks, honesty (no misleading clickbait).
Their example titles (IN THE PLAN FILE — read them, they are the ground-truth
style) are Indonesian podcast shapes: questions ("Ilmu vs Guru: Mana yang
Sebenarnya Lebih Penting?"), warnings ("Hati-hati Kebalik!"), quoted concepts.
The operator explicitly queued this as the next slice after the egui timeline
work (2026-07-12): "maybe do the title generation improvement, something egui
not related".

## What EXISTS today (verified against code 2026-07-12 — do not re-guess)

- Titles are generated **per-Moment at detect time** by the `yc-llm-judge`
  sidecar (ADR 0010/0015): one greedy temp-0 inference per refine candidate,
  GBNF-constrained to `{"score", "reason", "title"}` — `crates/detect/src/llm.rs`
  owns `SYSTEM` (rubric + title rules), `GRAMMAR`, `build_prompt`, the lenient
  parser, and tests that PIN prompt clauses (a reworded prompt must keep the
  game-narration mitigation + the hook-first title rules or those tests fail —
  extend them, never delete).
- The model sees ONLY: the candidate's own refined transcript, its z-scored
  chat/loudness/arousal, and the transcript language. Since ADR 0063 the
  arousal_z line is real in release builds.
- Title rules in `SYSTEM` today: ≤60 chars, transcript's language, hook-first,
  banned generic filler, no hashtags/emoji; `normalize_title` strips em-dashes.
- **There is NO whole-VOD transcript.** Discovery is signal-based; whisper
  transcribes candidate windows only (refine). "LLM reads complete transcript"
  therefore means one of: (a) the union/digest of all candidate transcripts —
  exists for free at detect time; (b) a new whole-VOD transcription pass —
  expensive (~0.9x realtime on an 80-min VOD) and new pipeline surface; or
  (c) per-clip regeneration reading neighbours' transcripts. The operator's
  plan says "complete transcript" — grill which fidelity they actually want
  vs the cost they'll pay, BEFORE building.
- The judge binary is the IPC boundary (`JudgeRequest` on stdin) — prompt
  changes are cheap (shared `llm.rs` consts); pipeline changes (a summary
  pre-pass, a second inference round) touch `yc-llm-judge` + the app worker.
- The rubric is game-streamer-flavored ("clutch play", chat corroboration).
  The operator's examples are PODCAST titles — a Creator-genre-aware rubric
  (podcast vs gaming) may matter as much as whole-video context. The Creator
  store (ADR 0016) is where a per-Creator genre would live if the grill wants
  one.

## Candidate shape (test in the grill, don't assume)

Two-stage at detect time: (1) one cheap digest inference over the
concatenated candidate transcripts (+ VOD title/metadata) → "what is this
video about, who's talking, main tension" (a few sentences, cached on the
Project); (2) each Moment's title inference gets that digest as context +
sharpened title rules (curiosity-gap shapes from the operator's examples,
per-genre). Bars would be: titles still ≤60 chars + parseable 25/25, no
regression on the pinned mitigation tests, and the operator's eye on a
before/after title table for a real VOD (the ECA podcast has a saved table
to diff against). Cost bar: the digest adds ONE inference per detect.

## The ritual

Session start: /grill-with-docs on this slice (unless the operator again says
"do it automatically" — then decide (a)/(b)/(c) by their written plan +
cost-honesty, state the choice out loud, and pre-register the bars in an ADR
before touching the prompt). One implementation this session. Standing rules:
quality over runtime, validate on the production path (headless `--detect` on
a real VOD, not a toy prompt harness), PS 5.1 quoting, `git commit -F` for
messages, gate on the operator's eye. Finish: handoff + whatwedone.md +
fresh nextprompt.
