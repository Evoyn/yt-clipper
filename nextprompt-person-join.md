# Session prompt — Per-person voice join: production, on the occupant map

You are upgrading yt-clipper's voice⇄seat join from angle-scoped to
PERSON-scoped (F:\yt-clipper, pure-Rust egui app). Fresh session: read the
context below, then run /grill-with-docs BEFORE any code. One
implementation this session: **the production join rebuilt over occupant
cameras** — person-consistency gate, purity-weighted co-occurrence, an
occupant-aware off-screen flag — measured in speaker_diag against the
operator's ear-truth spans, gated on a fresh A/B render (never clobber
diar_integration.mp4 / diar_baseline.mp4 / camera_smoothing.mp4).

## Open the grill with
1. **The ANTITESA fixture state**: did the operator re-promote the
   790.0–859.0 Moment? (Their 04:36 promote overwrote the fixture segment
   with a different 40 s Moment — the standing fg byte-pin `9e07d81f…` is
   unmeasurable until restored. If restored: re-run the harness, confirm or
   re-derive the pin BEFORE any code.) And their **Studio voice-row first
   look** (the release exe is fresh now; their earlier "no difference" was
   a stale binary).
2. The design tree ADR 0043 left measured-but-open: (a) does the
   person-consistency gate REFUSE an inconsistent cluster entirely or
   split its evidence per camera, (b) purity weighting — hard gate on
   both-mouths share vs weight, and what the uncontested-evidence floor
   is (V2's clean evidence is 39% contested; V0/V1's poisoned evidence
   91–95%), (c) how the off-screen flag consults the map ("voice's person
   visibly seated here" must beat "no learned seat here" — the 20.8 s
   case), (d) singleton persons (P4/P5-class pose extremes): tolerate or
   second-pass absorb, (e) does the occupant map computation join
   `Job::AnalyzeSpeakers` now (cost: ~13 s harness-measured per 70 s clip,
   or amortize crops into the tracking pass), and what soft-degrades when
   face-id models are absent (mouth+voice-as-today, never a failed job).

## Read first (in this order)
1. docs/adr/0043-face-reid-spike-occupant-map.md — ALL of it (the
   two-camera truth, the poisoning replay, the measured design inputs,
   the operator's ear-truth spans = the acceptance bar).
2. handoffs/2026-07-06-face-reid-spike.md — gate state + traps (fixture
   numbers of record; the naive merged join is a demonstration, NOT a
   candidate; Deddy ground-truth table).
3. docs/adr/0042-voice-diarization-spike-campplus.md — Integration + the
   new verdict section.
4. crates/frame/src/voice.rs — build_lane (angle_override is already
   there), fuse_attribution, segment_bounds.
5. crates/frame/src/face_id.rs + the face lane / replay in
   crates/app/examples/speaker_diag.rs.

## The acceptance bar (operator ear-truth + frames, Deddy clip-relative)
- 14–20 s: the green-shirt man (seat B of the pair-1 camera) talks ON
  screen — attribution should reach him or hold honestly; the false
  "off-screen V1 dominant" call must not survive.
- 20.8–21.4 s: the cap man (seat A, pair-1 camera) interjects ~1 s ON
  screen — the operator asked for this cut ("cut when voice is sure");
  with person evidence it finally can fire. It must NOT fire from
  poisoned evidence.
- 22–27 s: cross-talk — hold (unattributable is correct).
- 27–30.2 s: laughter — the split stays.
- ANTITESA: zero behavior change (follow-visible; fg byte-pin once the
  fixture is restored).

## Non-negotiable method (unchanged)
Measurement loop first — extend speaker_diag, never bypass it (it CALLS
the production functions; keep that). Validate on BOTH fixtures. Suites
green BOTH ways (315 today). Renders for the operator's eyes use FRESH
names. Thresholds are measured constants — derive them from printed
evidence, never tune to the answer.

## Fixtures + harness (same as ever)
- Deddy: `workspace/Deddy Corbuzier/BGN B NYA…. 😂 SEREM BGT NIH PODCAST
  ASUU‼️ Tretan, Coki, Adriano/data`, range `1559.6094450950623
  1629.7294450950621`.
- ANTITESA: `workspace/Leon Hartono/ANTITESA Cacing Cacing Naga Naga! -
  Ft. Andrew Susanto/data`, range `790.0 859.0` (VERIFY the segment was
  re-promoted first — see grill item 1).
- `cargo run --release -p yt-clipper --example speaker_diag --features
  face -- "<data dir>" <start> <end> [csv]`; faceselftest mode for any
  new face-model candidate.
- Verify recipe (GUI drive incl.): `.claude/skills/verify/SKILL.md`.

## Ritual
/grill-with-docs first; /verify before committing; finish with /handoff to
`handoffs/<date>-<slug>.md` + a dated entry prepended to whatwedone.md;
commit as Evoyn with the model's Co-Authored-By trailer (message via -F
file); `git push origin main` has standing permission. Windows PS 5.1
quirks per the standing memory notes.
