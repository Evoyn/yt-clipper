# Session prompt — ship the arousal Signal in release builds (operator pick 2026-07-12)

> **ARC CLOSED (2026-07-12, same day — shipped).** The slice ran start to
> finish in one session, operator away ("do this automatically"): every
> pre-registered bar passed (ADR 0063 Outcome has the A/B table + costs),
> `ser` is wired into `build-release.bat`, the corrupt-model fail-soft
> landed unit-covered, and the operator's saved Moments were restored
> byte-identical. Do NOT queue sessions from this file (focus-md pattern:
> historical once the arc closes). The staged eye-gate lives in ADR 0063's
> table — a one-line `build-release.bat` revert reverses the wire if the
> eye refuses. **The queue head is now `nextprompt-editor-suite.md`** (the
> operator's own `feature-implementation-plan.md`, 16 editor features +
> title-gen — grill first, slice second).

The operator closed the caption-general arc and picked this thread
("i want you to make based on your recommended"), then had this prompt
verified-and-sharpened against the actual code the same day — the facts
below were READ, not guessed (re-verify only if the cited files changed).

## The defect (verified in code, 2026-07-12)

`scripts/build-release.bat:24` builds `--features face,align` — **no
`ser`**. The ser-gated refine pass (pipeline.rs ~1036–1052) is therefore
compiled out of every production binary, which degrades TWO Signals, both
verified:

1. **Ranking**: the default `Weights` reserve **0.15 for arousal**
   (yc-detect lib.rs:109); without it `combined_score` renormalizes over
   the rest — Moments rank loud-vs-flat blind (the exact false-positive
   class ADR 0008 exists to kill).
2. **The LLM judge**: its prompt corroborates against `arousal_z`
   (yc-detect llm.rs:75,101) — a ser-less build has fed it None the whole
   time, so even the llm Signal has been judging without the
   voice-activation line.

Everything else is ALREADY in place (all verified): `AppPaths::ser_model`
= `models/w2v2-emotion/model.onnx` and the model IS installed on this
machine; the Diagnostics ▸ Downloads heal row exists ("arousal Signal
(ser builds, ADR 0008) (~610 MB)", installs model.onnx + model.yaml —
main.rs ~1378/1581); the GUI already displays the arousal value per
Moment (main.rs 177, 2291); and **the mixed-audio gate PASSED 2026-06-18**
(ADR 0008 Outcome: reactions peak 0.94–1.10 and decay to ~0, loud-but-flat
demoted, on a real 66-min VOD). This slice VALIDATES-and-SHIPS a
gate-passed Signal into the release binary; it does not re-open the gate.

## Verified constraints and sharp edges

- **Fail-soft on a MISSING model is structural**: the pass runs only under
  `#[cfg(feature = "ser")]` AND `paths.ser_model.is_file()`; absent ⇒
  today's ranking exactly (renormalize). BUT a PRESENT-and-corrupt file
  hard-fails the whole detect job (`Ser::load(&paths.ser_model)?`,
  pipeline.rs ~1042). Decide in-session: accept (Downloads re-heals) or
  soften to warn+skip — if softened, that's part of the ONE
  implementation, unit-covered, not an extra.
- **Cost of record** (ADR 0008, 2026-06-18 hardware): ~0.35–0.46 s CPU per
  4 s window, ~163 s for a 25-candidate refine batch, after the whisper
  drop (no VRAM). Re-measure today's wall-time and record it beside the
  old number.
- **License** (ADR 0008): the audeering w2v2 model is
  research/non-commercial — already ruled acceptable for this offline
  personal tool; release inclusion changes nothing. Do not re-litigate.
- **Discovery-arousal stays DEFERRED** (ADR 0008's own measured deferral:
  real reactions and game-cutscene voice-acting interleave in one score
  band; the only recorded revisit shape is a corroboration-gated
  augmenter, itself gated on vocal separation). This slice is
  REFINE-ONLY. Do not resurrect discovery.
- **Weights are not a lever this session**: the 0.15 default ships as-is;
  retuning the mix is its own future measured slice.

## Instruments (verified — do not invent new ones)

- `crates/detect/examples/arousal_probe.rs` — dense local arousal trace
  (the ADR 0008 gate's per-moment evidence; needs `--features ser`).
- `crates/detect/examples/arousal_scan.rs` — whole-VOD distribution +
  off-diagonal report (gate fuel; discovery context only).
- `crates/detect/examples/detect_vod.rs` — the no-GPU discovery
  tuner/ranked-dump the ADR used for the refine re-rank table.
- `crates/app/examples/arousal_adds.rs` — the DISCOVERY-precision
  transcript probe. NOT this slice's tool (discovery deferred); listed so
  nobody re-guesses its purpose.

## The slice (ONE implementation)

1. **Persistence semantics first**: read how `--detect` / the GUI detect
   job writes `project.json` (main.rs) BEFORE any run. Never leave the
   operator's saved Moments altered — the ECA podcast workdir (under
   "Deddy Corbuzier", the "JADI, COWOK RED FLAG…" stream folder) holds
   their 25 saved Moments. Byte-backup `project.json`, restore after,
   verify sha.
2. **Capture the A side BEFORE rebuilding**: the current
   `target\release\yt-clipper.exe` is the ser-less binary — copy it aside
   (or record its detect table first); the rebuild overwrites it.
3. **Build B**: `scripts\cargo-cuda.bat` with QUOTED
   "--features" "face,align,ser" (PS 5.1 trap: a bare comma makes an
   array and silently drops features). Suites: workspace default,
   `-p yc-detect --features ser`, app `face,align,ser` flavor.
4. **Probe**: `arousal_probe` over a current fixture's analysis.wav range
   — scores move, per-window CPU wall recorded.
5. **A/B the refine ranking on the real VOD** (pre-register "sane" BEFORE
   looking): every refined Moment carries `Some(arousal)`; rank moves are
   explainable by the arousal column (the known classes: reactions up,
   loud-but-flat down); zero words of code changed in scoring itself.
   Artifact: the before/after ranked top-N table with per-Signal columns.
6. **Wire**: add `ser` to build-release.bat + extend its comment block
   (the file documents each feature's reason — keep that convention).
   Rebuild release; boot check (GUI opens, Diagnostics SER row green,
   a detect shows the "Refining moments (arousal, CPU)" stage).
7. **Operator gate**: the before/after top-N table + the measured cost
   line, for their eye — a re-ranked detect is a behavior change; the eye
   rules (ADR 0050/0057 lesson). Stage it in the handoff if they're away.

## Hard rules (standing)

- Measure before building; pre-register what "sane" means before reading
  the A/B; fail-soft verified, not assumed.
- GPU: detect's whisper/llm stages want the idle gate (probe decode is
  the decider); the arousal pass itself is CPU. Sweep orphans; background
  waits die ~25–40 min — wakeup-poll instead.
- PS 5.1: ASCII scripts; QUOTE the features list; commit -F file;
  git-bash mangles workspace paths — use the PowerShell tool.
- Caption code is OUT OF SCOPE (that arc is closed — see
  nextprompt-caption-general.md's banner). So is weight retuning and
  discovery-arousal (above).

## Ritual

/grill-with-docs first (operator may be present; if they said "do this
automatically", the pick + this verified plan are the recorded rubric —
proceed through the slice in order). Still on the operator's table, one
click each, their act alone: flipping Helmy Yahya Bicara + "local" to the
ensemble engine (ADR 0035/0061). Finish with /handoff + whatwedone.md
entry; commit as Evoyn with the model trailer; `git push origin main` has
standing permission; ALWAYS end with the next
`read nextprompt-<slug>.md and follow it.` line.
