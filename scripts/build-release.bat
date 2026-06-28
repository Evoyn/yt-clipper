@echo off
REM Build the production RELEASE binaries (M8 packaging) -- the app + the
REM yc-llm-judge sidecar, optimized (thin-LTO; see the workspace Cargo.toml).
REM The operator otherwise runs debug builds; release materially speeds the
REM whisper + llama GPU work at runtime.
REM
REM Usage (from the repo root):  scripts\build-release.bat
REM
REM Output:  target\release\yt-clipper.exe  +  target\release\yc-llm-judge.exe
REM The judge sidecar must sit beside the app exe at runtime (both land in
REM target\release, so that holds). AppPaths walks up from target\release to the
REM repo root for sidecars\ models\ assets\ workspace\ (ADR 0005), so run with:
REM     scripts\cargo-cuda.bat run --release -p yt-clipper
REM Add  --features face  for the M6 auto-framing path (Ultraface via ort).

call "%~dp0cargo-cuda.bat" build --release -p yt-clipper -p yc-llm-judge
exit /b %ERRORLEVEL%
