# Hide child-process console windows (Windows `CREATE_NO_WINDOW`)

The operator flagged a UX wart: during a render / ingest, **console windows pop up
and flicker** on screen. Cause: the release GUI is built
`#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` — a Windows
GUI binary with **no console of its own**. When such a process spawns a console
subsystem child (ffmpeg, ffprobe, yt-dlp, deno, the `yc-llm-judge` sidecar),
Windows gives the child a **brand-new console window** by default. Every clip
render spawns several, so consoles flash throughout.

## Decision

Add a small `core::NoConsole` extension trait on `std::process::Command` with one
method `no_console()` that, **on Windows**, sets the `CREATE_NO_WINDOW`
(`0x0800_0000`) creation flag, and is a **no-op everywhere else**. Call it on every
`Command` before spawning, across the three crates that shell out:

- `yc-ingest`: the `run` / `run_capture` helpers (yt-dlp, ffprobe) and the
  `taskkill` cancel tree-kill.
- `yc-ingest::extract_audio` / `extract_frames_rgb` (ffmpeg).
- `yc-render::run_export` (the NVENC ffmpeg).
- `app::pipeline`: the `yc-llm-judge` sidecar and the `sep` ffmpeg calls.

One shared seam (the trait) instead of a `cfg` block at each of the ten spawn
sites; the flag only affects the **console window**, not stdio — `Stdio::inherit()`
(the judge's / ffmpeg's stderr) still reaches the terminal in the debug console
build.

## Considered options

- **`NoConsole` trait in `core` (chosen).** All three crates already depend on
  `core`; one place owns the flag and the `cfg`, and call sites read
  `.no_console()`. Cross-platform by construction (a no-op off Windows).
- **`cfg(windows)` + `creation_flags` inline at each spawn.** Rejected: ten copies
  of the same platform shim, easy to forget on a new spawn.
- **Detach via `DETACHED_PROCESS`.** Rejected: it also detaches stdio handles, which
  would break the stderr inheritance the judge / ffmpeg rely on; `CREATE_NO_WINDOW`
  suppresses only the window.

## Consequences

- Purely a Windows-presentation change: no behavioural difference to the render,
  and the debug console build is unaffected (children there simply run without
  their own console; inherited stdio still flows to the app's console).
- A new spawn site must remember `.no_console()` — cheap, and the trait makes it a
  one-token addition.

## Outcome

**Shipped + verified (2026-06-28).** Trait in `core` + applied at all ten spawn
sites. `core` 7 tests green (+1: `no_console` stays chainable); the app checks
clean with `face` **and** `sep` (the cfg-gated sep spawns compile). **Live (real
release headless render — `windows_subsystem = "windows"`, the build that pops
consoles):** polled visible `ConsoleWindowClass` windows every 0.5 s across the
~75 s render (frame-extraction ffmpeg + whisper + NVENC export) → **0 new console
windows**, and the render completed (exit 0, output produced — so the flag didn't
break the spawns' stdio). Before the fix this same render flashed an ffmpeg console
per spawn.
