# Session prompt — review-fixes slice 1: close the correctness + diagnosability list (operator pick 2026-07-13; from docs/code-review-2026-07-13.md)

A full-codebase review landed in `docs/code-review-2026-07-13.md`
(read it first — every finding below carries file:line evidence there;
line numbers were verified 2026-07-13 and will drift). Overall verdict
B+/A-: the debts are localized, not systemic. The operator picked
**fixing from the review doc** as the next arc. Sliced honestly:
**slice 1 = the six cheap correctness/diagnosability fixes** (each
~30-60 min, mechanical, low-risk, no design forks). The performance
slice (Lance–Williams clustering, resident aligner/sep sessions,
yt-dlp retry de-loop) is the NAMED next slice — its own nextprompt
when this one closes. The structural splits (`ensemble.rs`,
`speaker.rs`, `ui_strip`) and the persisted-schema version field stay
long-term, taken as slices when those files are touched anyway.
(`nextprompt-title-gen.md` stays queued behind this arc.)

## Slice 1 — the six fixes (all in the review doc, § Correctness / § Workspace hygiene)

1. **`escape_ass()` — the one reachable-today bug.** Caption text is
   burned into ASS with only `.to_uppercase()`; operator-typed manual
   captions (ADR 0065) containing `{`, `}`, `\`, or a newline corrupt
   or blank the Dialogue event. Add one helper (`\`→`\\` FIRST, then
   `{`→`\{`, `}`→`\}`, newline→`\N`) and apply it at the FOUR emit
   sites in `crates/render/src/ass.rs` (manual text ~:416; the three
   Dialogue emitters ~:433/:722/:772). Escape at the ASS-WRITE
   boundary ONLY — `preview_lines` feeds both the burn and the editor
   overlay (ADR 0036), and escaping upstream would paint `\{` on the
   canvas. While there: the review notes the Dialogue template is
   copy-pasted ×4 — fold the four into one `dialogue(...)` helper if
   it stays a pure refactor (byte-identity bar below protects it).
2. **Capture ffmpeg stderr in `run_export`**
   (`crates/render/src/export.rs:495-506`). Today stderr is inherited
   — under `no_console()` in the GUI it goes NOWHERE and a field
   export failure reports only an exit status. Pipe it and keep a
   bounded tail in the `ensure!` context. CAUTION: ffmpeg writes
   progress to stderr continuously — a piped stderr MUST be drained
   concurrently (the pipe-buffer deadlock). The in-house precedent is
   `decode_one` (`crates/transcribe/src/ensemble.rs:570-608`): drain
   thread + tail ring. Mirror it.
3. **Capture yt-dlp stderr in ingest `run`**
   (`crates/ingest/src/youtube.rs:175-195`). Same disease: the most
   failure-prone subprocess ("Requested format is not available",
   nsig breakage) yields the least diagnosable error
   (`"{program} failed ({status})"`). Same fix, same precedent, keep
   the tree-kill (`taskkill /T`) semantics intact.
4. **Kill the worker's child on app exit**
   (`crates/app/src/pipeline.rs:443` — `spawn` returns no
   JoinHandle; a hard window-close mid-render orphans the
   ffmpeg/NVENC child; only explicit Cancel kills today). The cheap
   honest mechanism: `wait_killable` already polls the `CancelToken`
   ~20×/s and kills registered children — so flipping the token from
   eframe's exit hook (`App::on_exit` in eframe 0.34, or the
   viewport-close event) reuses the entire existing kill path. Verify
   which hook actually fires on titlebar-X on Windows before wiring.
5. **Panic guards.** (a) `voiced_bins`
   (`crates/frame/src/speaker.rs:665`) indexes `sorted[0]` on an
   empty grid — guard `n_bins == 0` with the honest degenerate return
   (an empty grid has no voiced bins). Unit-test it. (b) The three
   UI-thread `self.playing.expect("playing")` in editor playback
   (`crates/app/src/editor.rs:1783/1796/1815`) — a future branch
   nulling `playing` mid-frame is a whole-GUI crash; restructure to
   `if let`/early-return so the invariant lives in the control flow,
   not a panic.
6. **Workspace hygiene pair.** (a) Hoist `ort = "2.0.0-rc.12"`
   (pinned independently in detect/frame/transcribe — same ONNX
   runtime DLL, an independent bump skews the ABI), `hound` ×2, and
   `ureq` ×2 into `[workspace.dependencies]`. (b) Fix the two
   test-code `erasing_op` lints (`crates/frame/src/face_id.rs:485`
   `src[(0 * w + 2) * 3]` + its twin in transcribe — clippy names it)
   so `cargo clippy --workspace --all-targets` finally exits 0 and
   can gate. `#[allow(clippy::erasing_op)]` with a one-line "row 0
   spelled out for symmetry" comment is honest; so is folding the
   zero. Either way the suite must gate clean afterwards.

DEFERRED (named, not this session): `first_score` digit-grab
(`detect/llm.rs:114`, bounded), `read_range` 200-acceptance hole
(`ingest/dash.rs:250`, astronomically unlikely), the `str::replace`
filtergraph rewiring guard — fold them into the perf slice's grill if
cheap, else they wait.

## Bars (pre-register before building)

- **Byte-identity, the standing bar**: caption text with NO special
  chars → ASS output byte-identical to today (the existing golden
  pins in `ass.rs` tests must stay green untouched — they ARE this
  bar). New unit tests pin each escape (`\`, `{`, `}`, newline, and a
  mixed string) through a real emitter, not just the helper.
- A deliberately failing ffmpeg export (bad arg) yields an error
  CONTAINING the stderr tail; a normal long export neither hangs nor
  balloons memory (drain thread proven by the existing long-clip
  path). Same two checks for a failing yt-dlp run.
- Close the app mid-render (real export running): no `ffmpeg.exe`
  survivor in tasklist afterwards; a normal close with no job stays
  clean (no spurious Cancelled artifacts on disk).
- `voiced_bins(&[], …)` (empty grid) returns instead of panicking —
  unit test.
- `cargo clippy --workspace --all-targets` exit 0. `cargo tree -e
  normal | findstr ort` shows ONE ort version. Feature builds still
  link: `"--features" "face,align,ser"` (PS quoting rule).
- `cargo test --workspace` green (451 tests today; new ones on top).

## The ritual

/grill-with-docs UNLESS the operator says "automatic" — then decide by
this file + the review doc, build, keep ADR 0036 preview-parity, ADR
0066-0070 contracts intact. Mechanical fixes need NO new ADR; if any
fix forces a real design fork (e.g. exit-hook choice has UX teeth),
record it as an ADR amendment. Release builds FOREGROUND. Validate:
cargo test --workspace, clippy on touched files then the full
`--all-targets` gate (bar above), `scripts\build-release.bat`, and the
operator's eye on ONE burn: a manual caption containing `{test} \ and
a brace` renders as typed (this is the escape fix's honest gate — a
unit test is not the operator's eye, per the standing rule). Standing
rules: quality over runtime, PS 5.1 quoting, commit via `git commit -F
<file>`, captions never corrected via store JSON, AI and operator
artifacts never share a track. Finish: handoff + whatwedone.md + fresh
nextprompt (the perf slice: Lance–Williams in `voice.rs:233` ~10×/plan,
resident aligner `ensemble.rs:1056` / htdemucs `sep.rs:60`, yt-dlp
retry `youtube.rs:557`) + the starter line.
