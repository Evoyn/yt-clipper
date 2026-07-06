---
name: verify
description: Drive yt-clipper (egui GUI) and its diag harnesses to verify changes on the real surfaces. Use when verifying production analysis/render changes end-to-end.
---

# Verifying yt-clipper changes

## Surfaces, fastest first

1. **Diag harnesses (production-path twins)** — same constants/functions as
   the pipeline, terminal output, fixture-driven:
   ```
   cargo build --release -p yt-clipper --example speaker_diag --features face
   ./target/release/examples/speaker_diag.exe "workspace/Deddy Corbuzier/BGN B NYA…. 😂 SEREM BGT NIH PODCAST ASUU‼️ Tretan, Coki, Adriano/data" 1559.6094450950623 1629.7294450950621
   ./target/release/examples/speaker_diag.exe "workspace/Leon Hartono/ANTITESA Cacing Cacing Naga Naga! - Ft. Andrew Susanto/data" 790.0 859.0
   ```
   Regression bars: ANTITESA `data/camera_diag.fg` SHA-256
   `9e07d81ff777bafceaebaf2992b4c90d407d2b8c5933a441f0159153527e8dad`;
   camera audit `clean (0 findings)` on BOTH fixtures. Deddy person-join
   bars (ADR 0044): occupant map `4 persons + 2 singleton sighting(s)` cut
   at 0.40; exactly one VALID edge (`V2 x cam0 ... -> P1`); off-screen
   1.5 s (positive absence only, spans 20.8-21.4 + 68.6-69.1).
   Laughter bars (ADR 0045; ear-truth spans are operator knowledge,
   supplied by env): Deddy with `YC_LAUGH_TARGET=25.5-30.25
   YC_LAUGH_EXSPLIT=14.2-22.1` prints `BARS: PASS at tau 0.1` (target 89%,
   every monologue 0%, tau 0.2 also passes at 63%); ex-split 14.2-22.1
   mass 0%. The 22.1-27.0 span is a DEAD stale annotation — never judge
   against it. Bar 0: `tagselftest` on the release test wavs — 4.wav must
   rank Laughter top at scale Unit (raw ~0.86, sigmoid-terminated export);
   wavs re-fetchable from the pinned HF mirror's `test_wavs/` (ADR 0046).
   Shared-reaction wiring (ADR 0046, shipped): the Deddy integrated plan is
   `15 shots (1 differ from the 15-shot mouth-only baseline)` — shot #5
   `27.0s..30.2s group` (the reaction split; the ONLY track/time diff), and
   the Person B solo chain frames at `435x773` (#6/#10/#14 `(1149,102)`,
   #11 `(1015,155)` — the accepted post-split anchor re-seed, ADR 0046).
   ANTITESA never computes the mask (follow-visible, attribution gate).
   Renders (never clobber old gate artifacts — pick fresh names for new gates):
   `YC_INTEG_RENDER=1` -> ../diar_integration.mp4; `YC_SMOOTH_RENDER=1` ->
   ../camera_smoothing.mp4; `YC_PERSON_RENDER=1` -> ../diar_person.mp4;
   `YC_REACTION_RENDER=1` -> ../diar_reaction.mp4
   (ALL FOUR are PASSED gate artifacts — do not re-run casually).
   GOTCHA: screenshots of the GUI while the operator works — prefer the
   PrintWindow capture (no focus steal); CopyFromScreen shows whatever app
   is frontmost.

2. **GUI (debug build boots in ~8 s)** — `cargo build --workspace --features face`
   then background-launch `./target/debug/yt-clipper.exe` (use run_in_background;
   plain `&` dies with the sandbox shell). Screenshot via PowerShell
   `System.Drawing` CopyFromScreen; click/scroll via `user32` SetCursorPos +
   mouse_event (2/4 = L down/up; 0x0800 wheel, delta 4294967177 = -120).
   The window title is `yt-clipper`. GOTCHA: egui panels scroll — re-screenshot
   before every click; coordinates rot. The Diagnostics expander sits in the
   left panel ("Diagnostics - all tools ready"); the dependency rows + green
   dots verify registry/downloads changes without any import.
   Kill with `Stop-Process -Name yt-clipper`.

3. **Studio flow (needs a real import)** — "Open a local file…" opens a native
   dialog (paste path from clipboard — emoji paths survive Set-Clipboard + ^v).
   Import runs whisper + the Qwen judge on the GPU (minutes) — do NOT drive it
   blind while the operator's machine is busy; prefer asking the operator or
   the harness twins. AnalyzeSpeakers is editor-only (no headless path).

## Suites
`cargo test --workspace` AND `--features face` (both must stay green; the
non-face build has stub fn signatures that drift silently otherwise).
Count of record: **337 both ways** (2026-07-07). The historic "268
non-face" was a partial-run artifact — 268 = 334 minus exactly yc-core 14
+ yc-ingest 21 + yc-llm-judge 1 + yt-clipper 30; no yc-frame test module
was ever feature-gated, so both suites run the same tests.
