# Session prompt — speaker diarization ("remember the voice"), spike + gate

You are adding SPEAKER DIARIZATION to yt-clipper's podcast Active-Speaker
camera (F:\yt-clipper, pure-Rust egui app) — the upgrade path ADR 0038 has
documented from day one. Fresh session: read the context below, then run
/grill-with-docs to pin the design against CONTEXT.md + the ADRs BEFORE any
code. One implementation this session: the **diarization spike + its
measurement gate**. Render-path integration is a LATER session, only after
the spike measures a win on the production fixtures.

Non-negotiable method (it cracked three camera bugs on 2026-07-05): build
the feedback loop first, extract and LOOK at real data before theorizing,
validate on the SAME inputs production uses, and the operator's eyes/ears
are the referee. Do NOT theorize about attribution without running
`speaker_diag` on the fixtures.

## Read first (in this order)
1. CONTEXT.md — the Camera plan / Speaker track / Studio terms.
2. docs/adr/0038-podcast-active-speaker-camera.md — whole file, especially
   the 2026-07-05 refinements (same-seat track merge, angle-aware
   attribution shots, and why A/B/C/D labels are SEATS, not identities).
3. handoffs/2026-07-05-cut-flash-trim-precision.md (parts 1–3).
4. crates/frame/src/speaker.rs — `attribute_speakers`,
   `merge_same_seat_fragments`, `plan_shots`.
5. crates/app/examples/speaker_diag.rs — THE harness. Extend it; never
   bypass it.

## What exists today (as of 2026-07-05, commit 5335161)
Attribution = per-bin mouth-motion (16×10 luma grid in the lower face box)
gated by an RMS VAD; winner needs a 1.35× margin held 0.8 s. It picks WHO
among the visible seat tracks; the source's scene cuts decide WHERE (shots
re-frame per angle). Same-seat track fragments merge (near + temporally
complementary + each fragment co-occurs with someone outside the pair —
that context gate is what keeps solo-camera framings of two DIFFERENT
people from fusing; do not weaken it).

## Production fixtures (in workspace/, deliberately kept)
- **ANTITESA** (solo-cam multicam, follow-visible regime, frame-exact cuts):
  `workspace/Leon Hartono/ANTITESA Cacing Cacing Naga Naga! - Ft. Andrew
  Susanto/data`, range `790.0 859.0` (project.json clip 6).
- **Deddy** (two-person angles + jump cuts, attribution regime, 12 cuts):
  `workspace/Deddy Corbuzier/BGN B NYA…. 😂 SEREM BGT NIH PODCAST ASUU‼️
  Tretan, Coki, Adriano/data`, range `1559.6094450950623
  1629.7294450950621` (clip 9). Post-fix baseline: 2 seat tracks (right
  seat 100 % presence), mean attribution confidence 0.38, switches at 27.0
  / 41.3 / 50.9 s.
- Harness: `cargo run --release -p yt-clipper --example speaker_diag
  --features face -- "<data dir>" <start_s> <end_s> [bins.csv]`
  (prints tracks / pair forensics / attribution timeline / plan, writes the
  real filtergraph as `camera_diag.fg` + a per-bin CSV).

## Operator ground truth (2026-07-05 evening, post-fix validation)
The operator watched both fix-validation renders:
- **ANTITESA: SIGNED OFF** (cuts clean — the trim-precision fix holds).
- **Deddy: WHERE signed off, WHO residual** — "the cut is good you fix it,
  but some of the cut error and not framing the one who talking. maybe we
  need the diarization." So this session's premise is operator-confirmed,
  not speculative: with tracking and framing structurally correct, the
  remaining errors are attribution picking the wrong seat at some switches.
**First thing in the grill: ask the operator for 2–3 timestamps in
"Diskusi politik dan nutrisi (fix-validation).mp4" where the wrong person
is framed** — those become labeled test moments for the spike (the baseline
switch list to check against: 27.0 / 41.3 / 50.9 s, mean conf 0.38).

## The gaps diarization fills (priority order — measure, then claim)
1. **Overlapping speech / rapid interjections** — mouth motion ties; the
   margin flips late or wrongly.
2. **Off-screen voice** — today attribution HOLDS the incumbent (a guess).
3. **Low-motion talkers / far faces** — the motion signal is weak (Deddy
   mean conf 0.38 even post-fix; a clean solo clip runs far higher).
4. **"Remember the voice"** — stable person identity across seats/angles so
   labels survive edits (today labels are screen positions by design).

## Constraints (decided history — do not re-litigate)
- Fully offline; Windows; 8 GB RTX 3070 Ti; this signal should be
  CPU-friendly (it runs beside face tracking; VRAM staging rules apply).
- Pure Rust: `ort` ONNX in-process (Ultraface precedent) or a pinned
  sidecar exe (llama.cpp precedent). NO python runtime, ever.
- Audio input: `data/analysis.wav` (16 kHz mono) sliced by the clip range —
  the same samples the VAD reads (`yc_ingest::read_range_samples`).
- Must NOT touch the caption decode path (ADR 0033: decode config is
  coupled to the dialect store) and must not change `SPEAKER_FPS`.
- Adopted models get an ADR 0041 registry pin (URL + SHA-256, Diagnostics
  Downloads row).
- OFF by default until the operator gate passes (ADR 0029 lesson: the
  analysis-side spike ALWAYS over-promises; only production-path runs
  count, and the operator's ear decides).

## Candidate routes (verify facts online during the grill; licenses matter)
- **sherpa-onnx speaker-embedding models** (WeSpeaker / 3D-Speaker ECAPA
  ONNX exports; small, CPU-fast, Apache-2.0) — the shape that drops into
  `ort` like Ultraface did. Leading candidate; verify Bahasa-Indonesia
  robustness (fixtures are Indonesian).
- NVIDIA TitaNet via ONNX export; pyannote only if a clean ONNX export
  exists (else reject: python).
Check per candidate: license, embedding dim, minimum chunk length, how it
behaves on 1.5–2 s voiced windows, cosine-similarity separation on the two
fixtures' voices.

## The spike (this session's deliverable)
1. Extend `speaker_diag`: embed voiced windows (~1.5–2 s, hopped), cluster
   (agglomerative over cosine), print a per-bin VOICE-CLUSTER lane beside
   the mouth-motion lane + an agreement %.
2. Join voice-clusters ↔ seat-tracks by co-occurrence on bins where both
   signals are confident.
3. Measure on BOTH fixtures: attribution coverage, confidence, and the
   switch list vs the operator's ear at ~10 sampled moments each; find at
   least one overlapping-speech or off-screen moment where voice beats
   mouth motion. Zero regression on either fixture is the bar.
4. Draft the ADR (options incl. rejected ones) and the fusion rule: voice
   joins the ensemble as a Signal — margin tiebreak + off-screen override —
   it does NOT replace the visual join.
5. Stop at the gate: present the diag lanes (or a rendered A/B) to the
   operator. Integration is the NEXT session's item.

## Ritual
/grill-with-docs first; finish with /handoff to `handoffs/<date>-<slug>.md`
+ a dated entry prepended to whatwedone.md; commit as Evoyn with the
model's Co-Authored-By trailer; `git push origin main` has standing
permission. Windows PS 5.1 quirks and commit-via-`-F`-file per the standing
memory notes.
