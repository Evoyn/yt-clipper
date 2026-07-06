# Session prompt — Shared-reaction wiring (the laughter class joins production)

You are wiring yt-clipper's PASSED laughter-class instrument into the
production analysis (F:\yt-clipper, pure-Rust egui app). Fresh session: read
the context below, then run /grill-with-docs BEFORE any code. One
implementation this session: **the Shared reaction mask ships** — computed
in `Job::AnalyzeSpeakers`, consumed by the split grammar, healed by the
Downloads page — gated on a fresh A/B render on the operator's eyes.

## Why this exists (what the spike proved)

ADR 0045: the pinned AudioSet tagger (Zipformer-M, sherpa-onnx release)
passed every pre-declared bar on the ear-corrected Deddy stretch — PASS at
tau 0.1 (89% target mass, 0% on EVERY monologue segment of both fixtures,
gap infinite; tau 0.2 also passes at 63%). Along the way it overturned the
session record's laughter annotation (the truth is 25.5–30.25, not
22.1–27.0 — the operator's ear confirmed) and measured the ex-split piece
14.2–22.1 at 0%: THAT piece has no laughter fuel and must not change via
this class. Real overlapped laughter scores 0.1–0.3 per 2 s step (bursts,
not a wall); the clean test wav scores 0.86. Everything already runs in
`yc_frame::reaction` + the `speaker_diag` laughter lane — the harness is
the production twin, so wiring is moving call sites, not inventing.

## Open the grill with

1. The grammar rule: what arms a shared-reaction split? The burst evidence
   says masked-seconds at low tau (e.g. >= 2 s masked at tau 0.1 within a
   piece), NOT a wall-share threshold. Which Deddy piece(s) should change —
   the plan piece covering ~25.5–30.25 becomes the visible pair's split
   screen ("nobody on screen is THE speaker", the off-screen split's exact
   semantics and anchor rules); 14.2–22.1 must NOT change (0% measured).
   Pin the piece-level rule + expected shot diff BEFORE wiring.
2. Lane representation: where does the mask live — `VoiceLane` gains a
   `reaction: Vec<bool>` (bins), or `SpeakerAnalysis` a sibling field? Who
   computes it in the pipeline (before/after the voice lane; it needs no
   VAD, no map — cheapest stage). Studio timeline: does the voice row mark
   reaction spans this slice, or is that a follow-up?
3. Registry + Downloads (ADR 0041 pattern): the tagger becomes a
   Diagnostics row + DownloadSpec. The tarball is 300 MB for a 259 MB
   fp32 model.onnx — the release also ships model.int8.onnx (~66 MB?);
   shipping int8 requires re-running Bar 0 + the fixture bars on int8
   (measure, don't assume). Missing model = no reaction mask, note line,
   never a failed job (the CAM++ precedent).
4. The gate: a FRESH render artifact (e.g. `diar_reaction.mp4` — never
   clobber diar_person/diar_integration/diar_baseline/camera_smoothing),
   A/B'd against `diar_person.mp4` on the operator's eyes; plus the
   pre-declared shot-diff expectation from grill point 1.

## Read first (in this order)

1. handoffs/2026-07-06-laughter-spike.md — traps (pinned conventions,
   stale annotation, checksum.txt staleness, run costs) + numbers of
   record.
2. docs/adr/0045-laughter-class-instrument-spike.md — the instrument, the
   bars, the burst-not-wall texture, the verdict terms.
3. crates/frame/src/reaction.rs + the laughter lane in
   crates/app/examples/speaker_diag.rs — the code that becomes production.
4. crates/frame/src/speaker.rs `plan_shots` (the off-screen split grammar
   the reaction split mirrors) + crates/app/src/pipeline.rs
   `do_analyze_speakers` (where the mask computes).
5. docs/adr/0041-in-app-dependency-downloads.md — the registry row shape.

## Acceptance

- The grill's pre-declared shot-diff on Deddy realized exactly (the
  reaction split appears where declared, nowhere else); ANTITESA
  byte-pin `9e07d81f…` INTACT (follow-visible never computes the mask —
  decide + pin structurally in the grill).
- All verify SKILL.md bars: person-join (map 4+2, one VALID edge,
  off-screen 1.5 s), laughter bars (PASS at tau 0.1 with the corrected
  spans), audits clean, suites green both ways (334 face / 268 non-face
  today).
- Operator rules the render gate on their eyes; no pass, no ship.

## Fixtures + harness (same as ever)

- Deddy: `workspace/Deddy Corbuzier/BGN B NYA…. 😂 SEREM BGT NIH PODCAST
  ASUU‼️ Tretan, Coki, Adriano/data`, range `1559.6094450950623
  1629.7294450950621`, laughter env `YC_LAUGH_TARGET=25.5-30.25
  YC_LAUGH_EXSPLIT=14.2-22.1` (the 22.1–27.0 span is DEAD — never judge
  against it).
- ANTITESA: `workspace/Leon Hartono/ANTITESA Cacing Cacing Naga Naga! -
  Ft. Andrew Susanto/data`, range `790.0 859.0`.
- `cargo run --release -p yt-clipper --example speaker_diag --features
  face -- "<data dir>" <start> <end>`.
- GUI drive: PrintWindow flag 2 via MainWindowHandle, never CopyFromScreen
  while the operator works.

## Ritual

/grill-with-docs first; /verify before committing; finish with /handoff to
`handoffs/<date>-<slug>.md` + a dated entry prepended to whatwedone.md;
commit as Evoyn with the model's Co-Authored-By trailer (message via -F
file); `git push origin main` has standing permission. Windows PS 5.1
quirks per the standing memory notes.
