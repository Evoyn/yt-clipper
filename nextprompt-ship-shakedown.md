# Session prompt — Ship real clips (production shakedown)

You are running yt-clipper (F:\yt-clipper, pure-Rust egui app) as a
PRODUCTION tool this session: the operator picks real podcast VODs, the
app produces publishable clips, and every defect the fresh material
surfaces becomes measured evidence for the next slice. Fresh session:
read the context below, then run /grill-with-docs BEFORE anything else —
the grill here is short (scope + fixtures), not a design session.

## Why this exists

The operator ruled the app production-ready for the solo podcast workflow
(2026-07-07, after the ADR 0046 reaction-split gate passed on their eyes).
Every camera instrument — active-speaker cuts, framing memory (ADR 0038),
voice lane + fused attribution (ADR 0042), person-scoped join + off-screen
split (ADR 0044), shared-reaction split (ADR 0045/0046) — was gated on the
SAME TWO fixtures (Deddy attribution-regime, ANTITESA follow-visible).
Fresh material is the only remaining evidence source. Shipping clips IS
the shakedown.

## The session's shape

1. Operator supplies one or more NEW podcast VODs (URL or local) + which
   Moments they actually want to publish.
2. Full production flow per clip: import → detect → review → promote →
   Studio (watch the Camera panel notes + the timeline evidence row —
   seat colours / off-screen red / reaction gold) → render.
3. The operator judges each clip AS A PUBLISHER (would I post this?).
   Anything that reads wrong gets recorded with its clip-relative
   timestamp and WHAT the eye saw (wrong subject / twitch / missed split /
   wrong split / caption issue) — do not fix live unless it is trivial
   and gate-safe; the ritual is one implementation per session.
4. Defects found → reproduce on the harness twin (speaker_diag on that
   clip's data dir; the evidence table names which lane lied) → the worst
   one becomes the next session's nextprompt, spike-shaped if it needs a
   new instrument.
5. Clean pass → the app has its first fresh-material validation; next
   session candidates from the record: int8 tagger measurement (ADR 0046
   follow-up, hash pinned a6254a4c…), applause/cheering class (needs
   fixture material), M8 error-state polish.

## Read first

1. handoffs/2026-07-07-reaction-wiring.md — today's state + traps.
2. docs/adr/0046-shared-reaction-wiring.md — what just shipped.
3. verify SKILL.md — the bars any live fix must keep (fixture runs,
   byte-pins, 337-both-ways suites, gate artifacts never clobbered).

## Hard rules

- Fixture bars stay green through ANY change (Deddy person-join +
  laughter bars, ANTITESA fg pin 9e07d81f…, audits clean, suites 337/337).
- AnalyzeSpeakers is editor-only (no headless path) — the Studio flow is
  the real surface; drive the GUI per the PrintWindow recipe, never
  CopyFromScreen while the operator works.
- Renders on new VODs go to their own workspace dirs — never touch the
  four gate artifacts (camera_smoothing / diar_integration / diar_person /
  diar_reaction .mp4).
- Detect/import runs GPU-heavy jobs (whisper + judge) — do not drive them
  blind while the operator's machine is busy; coordinate.

## Ritual

/grill-with-docs first (short: which VODs, how many clips, publish bar);
/verify before committing any fix; finish with /handoff to
`handoffs/<date>-<slug>.md` + a dated entry prepended to whatwedone.md;
commit as Evoyn with the model's Co-Authored-By trailer (message via -F
file); `git push origin main` has standing permission. Windows PS 5.1
quirks per the standing memory notes.
