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
REM     scripts\cargo-cuda.bat run --release -p yt-clipper --features face,align
REM
REM `face` is on by default here: M6 auto-framing AND the podcast speaker
REM detection (Active Speaker camera) both need Ultraface via ort. Drop it only
REM for a build that must not carry the ONNX Runtime binary.
REM `align` compiles the forced-alignment caption timing pass (ADR 0054) into
REM the same ort runtime `face` already ships. Since the ADR 0055 flip it is
REM the ensemble path's DEFAULT timing skeleton (YC_FORCED_ALIGN=0 is the
REM off-switch back to the whisper DTW fusion); whisper-engine renders are
REM untouched either way.

call "%~dp0cargo-cuda.bat" build --release -p yt-clipper -p yc-llm-judge --features face,align
exit /b %ERRORLEVEL%
