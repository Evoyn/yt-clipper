# Session prompt — Laughter-class instrument spike (win the Deddy splits back honestly)

You are giving yt-clipper a laughter/shared-reaction detector spike
(F:\yt-clipper, pure-Rust egui app). Fresh session: read the context below,
then run /grill-with-docs BEFORE any code. One implementation this session:
**a measured spike of an acoustic laughter-class instrument** — ADR 0044's
named path to revive the two Deddy splits with true evidence. Spike means
NUMBERS FIRST: the harness prints discrimination measurements on the Deddy
fixture; production wiring only if a pre-declared gate passes and the
operator rules it.

## Why this exists (the debt being paid)

ADR 0044 shipped the honest person join: the two good-looking Deddy splits
(14.2–22.1, 27.0–30.2) reverted to solos because their off-screen fuel was
proven false. Three candidate replacements were measured and ALL failed to
discriminate the laughter stretch (contested share 74–100% on EVERY
segment; absence ≤35%; cluster composition INVERTED — V0 covers 77–100% of
monologues, ~0% of the laughter, whose voiced time fragments into V4/V8/V9
scraps). CAM++ is a speech embedder; laughter is out-of-domain. The splits
return ONLY via a real acoustic instrument. Do NOT re-derive a grammar from
the existing lanes — that path is measured dead.

## Open the grill with

1. Instrument: an off-the-shelf ONNX audio-event tagger through the
   existing `ort` stack (AudioSet-class models tag laughter directly;
   fits the ADR 0041 registry + Downloads pattern like YuNet/SFace) vs
   hand-rolled spectral heuristics vs whisper non-speech tokens.
   Recommend ONE and defend it (the registry-pinned tagger is the shape
   every prior instrument took; heuristics are tune-to-answer bait).
2. Success bars BEFORE any code (the ADR 0043/0044 lesson): what numbers
   make the instrument real — e.g. laughter mass on the known laughter
   stretch ≥X%, false-positive mass on the monologue stretches ≤Y%,
   measured per 0.25 s bin on the SAME analysis.wav the production lane
   reads. Pin X and Y in the grill, then measure.
3. What a passing class feeds (design only, no wiring this session): a
   shared-reaction bin mask that arms the split grammar? suppresses
   contested bins? a new lane note? — settle the target semantics so the
   spike measures the right thing.
4. Confirm scope: harness-only (`speaker_diag` prints the mask + the
   per-segment evidence table extended with a laughter column), zero
   production behavior change, no render gate unless the numbers pass and
   the operator asks.

## Read first (in this order)

1. handoffs/2026-07-06-timeline-ux.md — traps (orphaned-build heal,
   app.ron persistence, GUI-drive recipe) + gate state.
2. docs/adr/0044-person-scoped-voice-join.md — the measured failures the
   spike must beat, the verdict, the named-path terms.
3. handoffs/2026-07-06-person-join.md — the numbers of record + the ear
   overturn (do not tune toward the stale 20.8 s annotation).
4. crates/frame/src/voice.rs — the lane the mask would sit beside;
   crates/app/examples/speaker_diag.rs — where the spike prints.

## Acceptance

- Pre-declared discrimination bars (grill point 2) measured and printed by
  the harness on the Deddy fixture, laughter stretch vs monologue
  stretches — pass or fail stated plainly; the operator rules what's next.
- Zero production behavior change. Suites green both ways (327 today).
- The ANTITESA fg byte-pin `9e07d81f…` and the Deddy person-join bars
  (occupant map 4+2, one VALID edge V2 x cam0 -> P1, off-screen 1.5 s)
  stay untouched — run both harness fixtures before committing
  (.claude/skills/verify/SKILL.md has the bars).

## Fixtures + harness (same as ever)

- Deddy: `workspace/Deddy Corbuzier/BGN B NYA…. 😂 SEREM BGT NIH PODCAST
  ASUU‼️ Tretan, Coki, Adriano/data`, range `1559.6094450950623
  1629.7294450950621`.
- ANTITESA: `workspace/Leon Hartono/ANTITESA Cacing Cacing Naga Naga! -
  Ft. Andrew Susanto/data`, range `790.0 859.0`.
- `cargo run --release -p yt-clipper --example speaker_diag --features
  face -- "<data dir>" <start> <end>`.
- GUI drive: PrintWindow flag 2 via MainWindowHandle, never CopyFromScreen
  while the operator works (recipe in the timeline-ux handoff).

## Ritual

/grill-with-docs first; /verify before committing; finish with /handoff to
`handoffs/<date>-<slug>.md` + a dated entry prepended to whatwedone.md;
commit as Evoyn with the model's Co-Authored-By trailer (message via -F
file); `git push origin main` has standing permission. Windows PS 5.1
quirks per the standing memory notes.
