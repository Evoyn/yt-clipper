# Session prompt — review-fixes slice 2 (perf): the three measured wastes (queued 2026-07-14; from docs/code-review-2026-07-13.md § Performance)

Slice 1 (correctness + diagnosability, six fixes) closed 2026-07-14 —
see `handoffs/2026-07-14-review-fixes.md` and the whatwedone entry.
Two operator-eye checks ride along from it: the escape burn
(`target/escape_burn/escape_burn.png`, regenerate with
`cargo run -p yt-clipper --example escape_burn_diag`) and one
close-mid-render + `tasklist | findstr ffmpeg` check. Collect their
verdict if not already given.

This slice = the review's three real performance findings. The review
doc's line numbers were verified 2026-07-13 and HAVE drifted (slice 1
edited youtube.rs/export.rs/ass.rs/main.rs/pipeline.rs) — re-grep every
site before editing. Standing rule up front: QUALITY OVER RUNTIME — none
of these may change what renders; they only stop paying for it twice.

## The three fixes

1. **Lance–Williams average-linkage in `cluster_cosine`**
   (`crates/frame/src/voice.rs` ~:233). Today every merge recomputes all
   pairwise member distances from scratch — O(n³·d) — and the function
   runs ~10× per camera plan (`build_lane` threshold sweep 7× + 1,
   `build_occupant_map` 2×). Keep a cached cluster-distance matrix and
   update it on merge with the average-linkage Lance–Williams recurrence
   (d(k, i∪j) = (|i|·d(k,i) + |j|·d(k,j)) / (|i|+|j|)) → O(n²).
   THE BAR: identical cluster assignments to the naive version on real
   fixture embeddings (same merges, same order — average linkage is
   exactly representable, so byte-equal outcomes, not "close"); property
   test naive-vs-cached on random embeddings; measure the speedup on a
   fully-voiced ~180 s fixture and record the number.
2. **Resident model sessions.** Whisper is deliberately resident
   (ADR 0007); these two rebuild per call:
   - the forced aligner reloads its ort session per clip —
     `crates/transcribe/src/ensemble.rs` ~:1056 (`Aligner::load` in the
     per-clip path);
   - htdemucs rebuilds per separation call —
     `crates/detect/src/sep.rs` ~:60 (`commit_from_file` inside
     `separate_vocals_wav`).
   Hold each session across a batch the way whisper is held (a
   `OnceCell`/held-in-worker-state shape — follow the whisper precedent,
   don't invent a new lifetime). BAR: second clip in a batch skips the
   load (measure the per-clip delta, expect multi-hundred-ms); output
   byte-identical; GPU/VRAM note if residency visibly raises the
   baseline (the 8 GB laptop card is shared with the operator's games —
   if resident sessions crowd it, say so and gate on the operator).
3. **yt-dlp retry de-loop** (`crates/ingest/src/youtube.rs` ~:557
   pre-drift). The degraded segment path re-runs a full `-j` extraction
   per attempt — up to 8 processes for one failing segment. Resolve
   formats ONCE, reuse across attempts; keep the retry semantics and
   tree-kill intact. BAR: a degraded fetch spawns exactly one extraction
   (count spawns in a diag or via the tail log), and the happy path is
   byte-identical.

## Deferred trio — fold into THIS slice's grill if cheap, else re-defer

- `first_score` digit-grab (`crates/detect/src/llm.rs` ~:114): "after
  the 3rd try, I'd say 8" yields 3. Prefer last-line / labeled-number
  parse; bounded blast radius (fallback-only, clamped).
- `read_range` 200-acceptance (`crates/ingest/src/dash.rs` ~:250): a 200
  whose length coincidentally equals the window streams as valid —
  require 206 (or reject 200 unless the request had no Range).
- Filtergraph `str::replace` label rewiring (`crates/render/src/export.rs`
  wrap fns): assert label-uniqueness before replace (a debug_assert +
  count==1 check) so a colliding label fails loudly instead of silently.

## Bars (pre-register before building)

- Clustering: naive-vs-cached identity (fixtures + property test), the
  measured speedup recorded in the handoff.
- speaker_diag regression bars ALL stay green (see
  `.claude/skills/verify/SKILL.md`): ANTITESA `camera_diag.fg` SHA
  `9e07d81f…`, camera audit `clean (0 findings)` on BOTH fixtures, the
  Deddy person-join/laughter/reaction bars.
- Residency: measured per-clip load delta gone on clip 2+; renders
  byte-identical; VRAM headroom noted.
- `cargo test --workspace` green (454 today); clippy
  `--workspace --all-targets` STAYS exit 0 (slice 1 turned this gate
  on — do not regress it); release build FOREGROUND;
  `"--features" "face,align,ser"` quoting rule in PS.

## The ritual

/grill-with-docs UNLESS the operator says "automatic". Perf work with
identical-output bars needs NO new ADR; if residency forces a real
lifetime/VRAM trade-off, ask the operator and record it (ADR 0007
amendment). Validate on the production path (fixtures + speaker_diag,
not synthetic-only). Standing rules: quality over runtime, PS 5.1
quoting, commit via `git commit -F <file>`, captions never corrected via
store JSON, AI and operator artifacts never share a track. Finish:
handoff + whatwedone.md + fresh nextprompt (the queue behind this:
`nextprompt-title-gen.md`, then the structural splits when their files
are touched anyway) + the starter line.
