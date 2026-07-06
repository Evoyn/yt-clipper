# Shared-reaction wiring: the laughter mask ships into the analysis, the split grammar, and the Downloads registry

ADR 0045 left a PASSED instrument with no production caller: the pinned
AudioSet tagger discriminates the Deddy laughter stretch at tau 0.1 with 89%
target mass over a zero-noise monologue floor, and the operator ruled the
wiring queued. This ADR records the wiring slice's decisions: where the mask
lives, what arms a split, how the tagger heals from Downloads, and the gate
that shipped it. (Terms: CONTEXT.md **Shared reaction**, **Camera plan**.)

## The lane: a sibling of the voice lane, attribution regime only

- `SpeakerAnalysis.reaction: Option<Vec<f32>>` — the laughter-family
  probability per analysis bin, projected from the tagger's 0.25 s steps.
  A SIBLING of `voice`, not a member of `VoiceLane`: the tagger needs no
  VAD, no clustering, no occupant map, and no CAM++ model, so a missing
  voice model must not kill the reaction mask (mouth-only analysis + a
  reaction split is a valid state). CONTEXT.md already said it: "its own
  acoustic evidence class, measured per time bin *beside* the Voice lane."
- `do_analyze_speakers` computes it in its own block gated on the existing
  `attribution` bool — the occupant-map precedent. A follow-visible Clip
  structurally never computes the mask (the plan's early-return paths never
  read it either): the ANTITESA byte-pin holds by construction, and
  follow-visible clips never pay the ~10 s CPU tagging cost (measured, 281
  steps over a 70 s clip, fp32 on one core).
- The conventions ride the call site exactly like CAM++'s:
  `SampleScale::Unit` + `TagOutput::Probs`, both selftest-pinned (ADR 0045
  — Int16 audibly breaks the model; the export is sigmoid-terminated).
- Missing model or labels CSV → a Camera-panel note ("Shared reaction off —
  tagger missing"), never a failed job; a broken load/tag likewise degrades
  to the mask-less analysis. The CAM++ error-shape precedent.
- The Studio's evidence row (the voice row) paints the mask at
  `REACTION_TAU` in gold beside the seat claims and off-screen red, and the
  row now renders when EITHER lane exists — the operator sees WHY a split
  fired on a machine with no voice model.

## The grammar: piece-level, absolute burst-seconds, low tau

A piece of an attribution run flips to the visible pair's split screen when
its bins scoring `>= REACTION_TAU (0.1)` total `>= REACTION_MIN_S (2.0 s)` —
`plan_shots` treats it exactly like the off-screen split (ADR 0042): decided
before framing, `group_layout_span` over the piece's bins, NO framing anchor
written.

- **Absolute seconds, not a share of the piece**: real overlapped laughter
  is bursts of 0.1–0.3 among speech (ADR 0045), so a wall-share rule
  under-fires (a genuine 3 s laugh inside a 12 s piece is 25% share). The
  measured pieces separate cleanly on seconds: 3.2 s (flips) vs 1.25 s
  (holds) vs 0.3 s vs 0 s.
- **Tau 0.1, not higher**: the monologue floor held 0% at EVERY tau on both
  fixtures, so low tau costs nothing on the clean side; at tau 0.2 the
  flipping piece's mass (~2.0 s) would sit exactly on the bar — no margin.
- **Piece-level, not sub-piece**: minting new shot boundaries at mask-run
  edges is new grammar machinery (boundaries not from source cuts,
  MIN_PIECE_S interactions) not paid for by the one measured fixture, whose
  cut-dense pieces are ≤ 11 s. Revisit if a long uncut piece with a short
  burst ever shows up in the wild.
- Rejected: putting the mask inside `VoiceLane` (false coupling, above);
  computing it for follow-visible clips too (nothing consumes it; the
  byte-pin would rest on plan-code discipline instead of structure).

## The pins: k2-fsa's own HF mirror at an immutable revision

Two optional Diagnostics rows ("reaction tagger", "AudioSet labels") backed
by two `Install::File` specs from
`huggingface.co/k2-fsa/sherpa-onnx-zipformer-audio-tagging-2024-04-09` at
revision `3c795f58cd1fe15a42cee103519e7a5cbbd93415`:

- `model.onnx` 259,079,136 B, SHA-256 `a8f11014905fbaab81644514b79e719f3f
  cfa3ad45d29a25b46e34eb03c48ed8`; `class_labels_indices.csv` 14,675 B,
  SHA-256 `cdd1049833c4b86127c2773ac0d14a2754b6a6d0d1798002ed5c66e699708429`.
  Both verified byte-identical to the gate-passing install on 2026-07-07
  (the model's HF LFS oid IS its SHA-256; the CSV was fetched and hashed).
- **Chosen over the GitHub release asset** (the ADR 0045 pin of record,
  tarball `6c89b86c…`): that tag is a LIVING release — k2-fsa re-uploads
  assets in place and its checksum.txt was already stale once — and it
  ships only `.tar.bz2`, which the downloader would need a new archive code
  path (+ bzip2/tar deps) for. The HF revision URL is immutable (the
  strongest pin class in ADR 0041, the whisper/Silero/Qwen pattern), serves
  the bare files, and saves 41 MB. Same org, same bytes, better transport.
- **fp32 ships; int8 stays unshipped.** The mirror carries `model.int8.onnx`
  (68,387,871 B, LFS `a6254a4c8a76deecb5ff22a1ad85eda8085a23bb56f6c672db5cc
  7da1bdfa257`) — a future swap needs Bar 0 + both fixtures' bars re-run on
  int8 first (measure, don't assume). The bars passed on fp32; fp32 is what
  ships.
- Two rows, not one: the lane loads two files, and a single row keyed on
  the model would show green while a missing CSV silently turned the lane
  off (the Qwen model+mmproj precedent).

## The gate (2026-07-07): declared first, measured, one amendment, ruled

Pre-declared from the baseline run before any code: shot #5 (27.0–30.2,
Person B solo, wall-to-wall mask) flips to the split; #4 (22.1–27.0, 1.25 s
mask — its 22.1–25.5 majority is ear-confirmed SPEECH) holds solo; #6
(0.3 s) and the ex-split 14.2–22.1 (0%, the honest no-fuel piece) hold; the
split arrives at the 27.0 source cut, where a re-frame is perceptually free.

Measured on the wired binary: the flip realized exactly; every bar of record
held (laughter bars PASS at tau 0.1 verbatim, person-join map 4+2 / one
VALID edge / off-screen 1.5 s, Bar 0 selftest, camera audits clean, ANTITESA
`camera_diag.fg` byte-pin `9e07d81f…` intact, suites green both ways).

One deviation, surfaced and ruled: the declared ripple ("#11, maybe #13")
was really the whole downstream Person B solo chain — in the baseline, #6
had SIZE-REUSED the anchor minted by #5 (piece_framing keeps a remembered
zoom when only the position moved), so #6/#10/#11/#14 all carried #5's
843-px crop height; with #5 now a group piece writing no anchor (the
invariant group pieces already obey), the chain re-seeds from #6's own bins
(773 px — B's height while actually talking, not mid-laugh). Movement
≤ 43 px, height −8.3%, zero breathing inside the new chain, audit clean.
The operator chose accepting the re-seed over having a split piece mint
solo framing state (which would bend "group pieces leave none" and remember
a zoom derived from laughing posture). The amended diff of record:
#5 solo→split; #6/#10/#14 `(1129,75) 474x843 → (1149,102) 435x773`; #11
`(973,112) → (1015,155)` same size; #12/#13 byte-identical.

Render gate: `YC_REACTION_RENDER=1` → `../diar_reaction.mp4` (fresh
artifact, never clobbering diar_person/diar_integration/diar_baseline/
camera_smoothing), A/B'd against `diar_person.mp4` on the operator's eyes —
PASSED, the shared-reaction split ships.

## Consequences

- The two good-looking Deddy splits ADR 0044 reverted come back HALF-way,
  honestly: 27.0–30.2 returns as a shared-reaction split on measured
  laughter; 14.2–22.1 stays a solo (0% laughter mass — whatever wins it
  back, it is not this class).
- The suite count of record is **337 both ways**: the historic "268
  non-face" was a partial-run artifact (268 = 334 minus exactly yc-core 14
  + yc-ingest 21 + yc-llm-judge 1 + yt-clipper 30) — yc-frame's test
  modules were never feature-gated. Corrected in verify SKILL.md.
- A future instrument for applause/cheering extends the same lane (the
  LAUGHTER_FAMILY constant is the only laughter-specific piece); the class
  name stays **Shared reaction** for exactly that reason.
- Pin-rot duty: the HF revision URLs are immutable, but if k2-fsa ever
  deletes the repo the SHA-verified download fails loudly and the fix is a
  new URL+hash pair in `download_specs`, nowhere else (ADR 0041).
