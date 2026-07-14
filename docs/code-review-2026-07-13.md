# Codebase Review — 2026-07-13

**Scope:** all 8 workspace crates, source only (`crates/*/src`; the 24 diag harnesses in
`examples/` were checked for prod leakage only — there is none). Documentation was
deliberately not read; every finding below comes from the code, `cargo clippy`,
`cargo test`, and the git log.

**Method:** first-hand read of the `app` crate (editor, pipeline, main, player,
review_queue, settings) plus four independent deep-review passes over `render`,
`transcribe`+`ingest`, `detect`+`frame`, and a cross-cutting workspace audit
(error handling, testing, concurrency, dependencies, persistence).

---

## Verdict

**Grade: B+ leaning A-. Good enough? Yes — better than most production codebases
of this size. Optimized enough? Mostly — the memory model is right, but there are
three concrete performance misses worth fixing.**

The debts are localized, predictable, and documented rather than systemic. The
highest-leverage work is not bug-hunting but decomposition of three god-modules
and a short list of cheap correctness/diagnosability fixes.

### Health metrics

| Metric | Value |
|---|---|
| Source lines (src only / incl. examples) | ~37,900 / ~44,600 |
| Unit tests | 451, **all passing**, full suite < 1 s, hermetic (no model/network/GPU) |
| Production panics (`panic!`/`unreachable!`/raw `unwrap` on prod paths) | **0** (all 33 panic sites + ~191 unwraps live in `#[cfg(test)]`) |
| Clippy | ~5 trivial style warnings; 2 pedantic `erasing_op` errors in **test** code fail `--all-targets` |
| `.clone()` calls workspace-wide | 209 (very low for 44k lines) |
| Commits | 247 in ~30 days, disciplined conventional-commit + ADR references |

### Per-crate grades

| Crate | Grade | One-liner |
|---|---|---|
| `ingest` | A- | Disciplined subprocess + adversarial-input parsing; opaque yt-dlp errors hold it back |
| `detect` | A- | Streaming, measured-constant, well-tested; unverified ONNX glue + per-call model reload |
| `core` | A- | Clean domain model, byte-stable serde round-trips; no schema version field |
| `render` | B+ | Golden-pin tests, clean errors; one reachable escaping bug + template duplication |
| `transcribe` | B+ | A-grade correctness and tests dragged by a 2,650-line god-module |
| `frame` | B+ | A-grade attribution pipeline; O(n³) clustering, 3,526-line module, one latent panic |
| `app` | B | Excellent pipeline worker; monolithic UI shell with zero tests on the Job→Progress spine |

---

## What's genuinely strong (keep doing this)

1. **Constants carry measured evidence.** Thresholds cite the measurements that set
   them (`PAN_STATIC_GROWTH`: "wander-and-settle needed 1.06×… genuine travel ~3.8×";
   `JOIN_MAX_CONTESTED`: "clean edges 16-17% contested vs poisoned 87-100%").
   Retuning is safe because the rationale travels with the number. Rare.
2. **Pure logic is split from I/O shells, and tests land on the logic.**
   `render` pins golden byte-identity outputs; `transcribe`'s vote/fusion algorithms
   test against measured fixtures; `review_queue` is UI-free precisely to be testable.
   Subprocess-touching code is a thin shell over tested pure functions.
3. **Process lifecycle handled like an adult.** Tree-kill via `taskkill /T`;
   stdout/stderr drained on separate threads (no pipe-buffer deadlock); typed decode
   watchdog; temp+rename atomic writes so a killed job never poisons a cache;
   `wait_killable` reaps children on both paths (no zombies).
4. **Cancellation semantics are engineered, not accidental.** The pipeline worker
   drains queued jobs on cancel and documents the 2026-07-03 incident that motivated
   it; `CancelToken` has correct, documented flag-before-registry lock ordering.
5. **Persistence discipline.** Additive-optional serde (`#[serde(default)]` +
   `skip_serializing_if`) with byte-stable round-trip assertions, `write_atomic`,
   and lossy loads that never block a render.
6. **Error spine.** Every worker job outcome maps to `Progress::Failed/Cancelled/Done`
   → `Status` → rendered in the UI. No silent job-error swallowing. NaN-safe
   `partial_cmp(...).unwrap_or(Ordering::Equal)` at ~20 sort sites.
7. **The ADR-referencing comment trail** is a genuine navigability multiplier —
   non-obvious decisions cite the decision record and often the measurement.

---

## Correctness findings

Ordered by (reachability × blast radius). None are crashes in the normal path today.

1. **No ASS escaping anywhere** — `crates/render/src/ass.rs:416` (manual/operator
   text) plus the three Dialogue emitters (`:433`, `:722`, `:772`). Caption text is
   burned with only `.to_uppercase()`. An operator-typed manual caption containing
   `{`, `}`, `\`, or a newline opens/closes an ASS override block or splits the
   Dialogue line — corrupting or blanking the event. Low-probability for
   single transcribed words; **directly reachable via manual captions (ADR 0065)**.
   Fix: a one-line `escape_ass()` (`\`→`\\`, `{`→`\{`, `}`→`\}`, newline→`\N`)
   applied at four sites.
2. **Hard window-close orphans the render child** — `crates/app/src/pipeline.rs:443`.
   `spawn` returns no `JoinHandle` and nothing kills the worker's ffmpeg/NVENC child
   on app exit; only an explicit Cancel runs `wait_killable`'s kill. Resource leak on
   the most common abort path (closing the window mid-render).
3. **`voiced_bins` panics on an empty grid** — `crates/frame/src/speaker.rs:665`.
   `sorted[(n_bins*0.9) as usize .min(n_bins.saturating_sub(1))]` indexes `sorted[0]`
   on an empty vec when `n_bins == 0`. Latent panic on a `pub` production entry point
   for a degenerate/zero-length clip.
4. **Field failures are undiagnosable (two sites).**
   - `run_export` discards ffmpeg's stderr — `crates/render/src/export.rs:495-506`.
     On failure the only diagnostic is the exit status; under `CREATE_NO_WINDOW` the
     inherited stderr goes nowhere.
   - ingest's `run` inherits yt-dlp's stderr — `crates/ingest/src/youtube.rs:175-195`.
     The most failure-prone subprocess in the app ("Requested format is not
     available", nsig failures) produces the least diagnosable error.
   `decode_one` (`ensemble.rs:599-608`) already captures stderr tails — these two
   should match it.
5. **Lenient LLM score fallback can grab a non-score digit** —
   `crates/detect/src/llm.rs:114`. `first_score` returns the first integer ≤ 10
   anywhere in raw output, so "after the 3rd try, I'd say 8" yields 3. Bounded
   (fallback-only, clamped) but a real misread entering ranking.
6. **`read_range` can accept a non-206 whole-file response** —
   `crates/ingest/src/dash.rs:250`. A `200` reply whose content-length coincidentally
   equals the requested window streams as valid — the exact whole-file pull the
   module exists to prevent. Astronomically unlikely; still a logical hole in the one
   guard that matters.
7. **Invariant `.expect()` clusters on production paths** (documented-invariant
   assertions, fine today, fragile under refactor):
   - `transcribe/ensemble.rs` ~16 sites (e.g. `:731`, `:749`) — a broken
     edit-distance invariant panics the caption worker (lost job, not a crash).
   - `frame/speaker.rs:703/870/1193`, `frame/voice.rs:599/722` — panics a render job
     if an upstream stage emits an unexpected shape.
   - `editor.rs:1783/1796/1815` — `self.playing.expect("playing")` inside the UI
     thread; guarded today, but any future branch nulling `playing` mid-frame is a
     **GUI hard crash** (highest blast radius of the family).
   - `ingest/dash.rs:211` — a malformed sidx window panics rather than falls back.
8. **Filtergraph label rewiring by blind `str::replace`** —
   `crates/render/src/export.rs:213/251-255/324-326`. The wrap functions re-terminate
   graphs by replacing `[out]`/`[aout]`/`[0:a]` substrings. Correct today only
   because those labels are unique by construction — an unguarded invariant that
   breaks silently the day a filter emits a colliding label.

---

## Performance findings ("optimize enough?")

The big picture is healthy: no whole-VOD buffers (loudness streams bin-by-bin,
detection/framing are per-clip), RMS accumulates in f64 before narrowing to f32,
karaoke color tags are hoisted out of per-word loops, and clone density is low.
The misses are targeted:

1. **`cluster_cosine` is O(n³·d) and invoked ~10× per camera plan** —
   `crates/frame/src/voice.rs:233`. Average-linkage clustering recomputes all
   pairwise member distances from scratch on every merge (`:243-256`); the
   `build_lane` threshold sweep calls it 7× (`:760`) + once more (`:817`), and
   `build_occupant_map` twice (`occupant.rs:236,238`). On a fully-voiced ~180 s clip
   (~240 windows × ~192-d embeddings) that is billions of f32 multiplies per plan.
   **Fix: cached cluster-distance matrix (Lance–Williams update) → O(n²).**
   This is the single biggest computational waste in the codebase.
2. **Model residency is inconsistent.** Whisper is deliberately kept resident across
   a batch (ADR 0007), but:
   - the ONNX forced-aligner reloads from disk per clip —
     `crates/transcribe/src/ensemble.rs:1056` (`Aligner::load` builds a fresh
     `ort::Session` every invocation);
   - the htdemucs separation session rebuilds per call —
     `crates/detect/src/sep.rs:60` (`commit_from_file` inside `separate_vocals_wav`).
   Every promoted clip pays the multi-hundred-ms load the resident pattern was
   invented to avoid.
3. **yt-dlp retry path re-runs a full `-j` extraction per attempt** —
   `crates/ingest/src/youtube.rs:526/557-565`. The degraded path can spawn up to
   8 yt-dlp processes for one failing segment (4 cold extractions + 4 downloads).
4. **56 MB chat replay slurped with `read_to_string`** — `crates/detect/src/chat.rs:16`.
   The parser walks line-by-line anyway; a `BufReader` bounds memory on the largest
   single input the crate touches.
5. **`merge_same_seat_fragments` rescans everything after each merge** —
   `crates/frame/src/speaker.rs:553/600-614`, roughly O(fragments²·bins·seats)
   before the persistence cap. Fine for real tracks; balloons on detection-noise-heavy
   input. Union-find over precomputed pair predicates is near-linear.

---

## Structural debt

**Three-and-a-half god-modules.** Each is an *organized* monolith — rationale
comments, factored closures, composable stages — so this is packaging debt, not
tangle. But each is past the size a newcomer (or future-you) can hold:

| File | Size | The problem |
|---|---|---|
| `crates/app/src/editor.rs` | 8,408 lines | One function, `ui_strip` (`:3434-5136`), is ~1,700 lines; `ui_transcript_panel`, `ui_properties`, `caption_overlay`, `apply_timeline_drag` are 330-450 each |
| `crates/app/src/pipeline.rs` | 3,817 lines | Coherent per-stage functions, but every pipeline stage lives in one file |
| `crates/frame/src/speaker.rs` | 3,526 lines | Five stages (tracking → VAD → attribution → shot planning → presence audit) in one file, unlike `detect/` which is already cleanly split |
| `crates/transcribe/src/ensemble.rs` | 2,658 lines | Mixes subprocess orchestration, audio DSP, vote algorithms, store application, and timing fusion; wants to be `ensemble/{decode,vote,fuse,store}.rs` |

**Duplication worth unifying:**

- Four overlapping edit-distance implementations in `ensemble.rs`: `align` (`:637`),
  `align_weighted` (`:1681`), `similar_word` (`:1746`), `edit1` (`:1008`) — the two
  DP aligners are near-identical, differing only in cost/tie-break.
- A hand-mirrored decode loop + budget formula between `apply` (`:229/:236-282`) and
  `decode_variants` (`:427/:430-465`) that must stay identical by discipline alone.
- The `Dialogue:` line template ×4 in `ass.rs` (`:411`, `:433`, `:722`, `:772`);
  the Stacked seam-height math byte-for-byte twice in `export.rs` (`:35-36`, `:64-65`).
- The ffprobe `key=value` parser ×3 across `ingest` (`youtube.rs:644/:719`,
  `align.rs:146`).
- (Noted: the two `align.rs` files are **not** duplicates — CTC forced alignment vs
  envelope cross-correlation. Unrelated algorithms sharing a filename.)

**Testing gaps (against otherwise excellent coverage):**

- `app/main.rs` (2,555 lines, **0 tests**) owns the eframe loop, worker wiring, and
  `Status` state machine — the UI↔worker `Progress` contract has no automated test,
  and there is no `tests/` integration dir anywhere exercising
  ingest→transcribe→detect→render.
- Zero-test modules elsewhere: `detect/sep.rs` (the overlap-add reconstruction is
  pure enough to test), `frame/infer.rs`, `app/theme.rs`, `app/player.rs`.
- The `ort 2.0.0-rc.12` inference glue across `detect`/`frame`/`transcribe` carries
  explicit "verified on first `--features` build, expect signature fixups" notes —
  the pure math around the model calls is tested; the model calls themselves aren't.

**Workspace hygiene:**

- `ort = "2.0.0-rc.12"` pinned independently in three Cargo.tomls
  (`detect`, `frame`, `transcribe`) — these link the *same* ONNX runtime DLL; an
  independent bump skews the ABI. `hound` ×2 and `ureq` ×2 also unhoisted.
  Move all three into `[workspace.dependencies]`.
- No schema version field in persisted models (`core/src/lib.rs:120/169` —
  `Project`/`CreatorStore`/`ReviewCache` load lossily). Safe only while every change
  stays additive; a future breaking rename silently discards user data instead of
  migrating. Add a version tag (or an unknown-key guard) before the first breaking
  schema change.
- `cargo clippy --workspace --all-targets` currently **fails** on two deliberate
  `0 * w` row-math expressions in test code (`frame/face_id.rs:485` and one in
  `transcribe`). Either `#[allow(clippy::erasing_op)]` those tests or drop the
  `0 *` — as-is, a strict lint gate can't be turned on in CI.

---

## Prioritized actions

Cheap correctness/diagnosability first, then the perf wins, then structure:

1. **`escape_ass()`** at the four emit sites (`ass.rs`) — closes the only
   reachable-today bug. ~1 hour.
2. **Capture stderr tails** in `run_export` and ingest `run` — turns undiagnosable
   field failures into actionable errors. ~1 hour.
3. **Kill the worker's child on app exit** (eframe `on_exit` → cancel + kill, or a
   Drop guard around the child pid). ~1 hour.
4. **Guard `voiced_bins` empty input**; swap the raw invariant-expects on the UI
   playback path (`editor.rs:1783+`) for graceful fallbacks. ~1 hour.
5. **Lance–Williams clustering** in `voice.rs` + **resident aligner/sep sessions** —
   the real performance wins. ~1-2 sessions.
6. **Hoist `ort`/`hound`/`ureq` to workspace deps**; fix the two test-code
   `erasing_op` lints so `clippy --all-targets` gates clean. ~30 min.
7. **De-loop the yt-dlp retry** (`resolve_formats` once, reuse across attempts).
8. Longer-term, as slices when touching them anyway: split `ensemble.rs` into
   `decode/vote/fuse/store`, split `speaker.rs` the way `detect/` already is, unify
   the edit-distance family, extract row renderers from `ui_strip`, and put one
   integration test over the Job→Progress contract.
9. Before the first breaking schema change: **version field in persisted JSON**.

---

*Review method note: findings were produced by one first-hand pass plus four
independent review passes and verified against real line numbers on 2026-07-13;
line references will drift as files change.*
