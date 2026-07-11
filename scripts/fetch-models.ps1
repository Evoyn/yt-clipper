# Fetches the Whisper GGML model into /models. M1 default: large-v3 (f16, full precision,
# ~3.1 GB) per the M1 plan -- ADR 0003's quantized ship-default is revisited at M6. Records
# exactly what was fetched in models/VERSIONS.txt. Re-run deliberately to change models, never
# automatically (Offline constraint: network use is operator-initiated only).
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$models = Join-Path $root "models"
New-Item -ItemType Directory -Force $models | Out-Null

# Whisper GGML from the canonical whisper.cpp model repo on Hugging Face (public, no token).
$model = "ggml-large-v3.bin"
$uri = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$model"
$out = Join-Path $models $model

if (Test-Path $out) {
    $gb = [math]::Round((Get-Item $out).Length / 1GB, 2)
    Write-Host "$model already present ($gb GB) -- skipping. Delete it to re-fetch."
} else {
    Write-Host "Downloading $model (~3.1 GB) from Hugging Face..."
    # curl.exe streams straight to disk; Invoke-WebRequest buffers the whole file in memory on
    # PS 5.1. -L follows the HF CDN redirect; --fail turns an HTTP error into a nonzero exit.
    & curl.exe -L --fail --progress-bar -o $out $uri
    if ($LASTEXITCODE -ne 0) { throw "curl failed with exit code $LASTEXITCODE" }
}

# Silero VAD (ggml) from whisper.cpp's official VAD model repo (public, no token).
# Opt-in caption pre-segmentation (YC_VAD=1, ADR 0033); tiny (~0.9 MB).
$vad = "ggml-silero-v5.1.2.bin"
$vadUri = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/$vad"
$vadOut = Join-Path $models $vad

if (Test-Path $vadOut) {
    Write-Host "$vad already present -- skipping. Delete it to re-fetch."
} else {
    Write-Host "Downloading $vad (~0.9 MB) from Hugging Face..."
    & curl.exe -L --fail --progress-bar -o $vadOut $vadUri
    if ($LASTEXITCODE -ne 0) { throw "curl failed with exit code $LASTEXITCODE" }
}

# Qwen3-ASR 1.7B (official ggml-org GGUF conversion) for the ASR A/B trial
# (ROADMAP "ASR engine upgrade"). MULTIMODAL: needs BOTH the text model and the
# mmproj audio encoder, hosted by the pinned llama.cpp sidecar
# (scripts/fetch-llama-sidecar.ps1) - NOT by whisper.cpp or yc-llm-judge.
$qwenRepo = "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/main"
$qwenFiles = @("Qwen3-ASR-1.7B-Q8_0.gguf", "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf")
foreach ($qf in $qwenFiles) {
    $qOut = Join-Path $models $qf
    if (Test-Path $qOut) {
        $qgb = [math]::Round((Get-Item $qOut).Length / 1GB, 2)
        Write-Host "$qf already present ($qgb GB) -- skipping. Delete it to re-fetch."
    } else {
        Write-Host "Downloading $qf from Hugging Face..."
        & curl.exe -L --fail --progress-bar -o $qOut "$qwenRepo/$qf"
        if ($LASTEXITCODE -ne 0) { throw "curl failed with exit code $LASTEXITCODE" }
    }
}

# Forced-alignment caption timing (ADR 0053/0054; the ensemble DEFAULT on an
# `align` build since ADR 0055 - YC_FORCED_ALIGN=0 is the off-switch):
# cahya/wav2vec2-large-xlsr-indonesian exported to ONNX. No hosted ONNX exists
# for this model, so the pin is source weights + a local export
# (scripts/export-align-onnx.py via uv, ~1.2 GB download, ~1.3 GB out).
$alignDir = Join-Path $models "w2v2-align-id"
$alignOnnx = Join-Path $alignDir "model.onnx"
if (Test-Path $alignOnnx) {
    $agb = [math]::Round((Get-Item $alignOnnx).Length / 1GB, 2)
    Write-Host "w2v2-align-id/model.onnx already present ($agb GB) -- skipping. Delete it to re-export."
} else {
    $uv = Join-Path $env:USERPROFILE ".local\bin\uv.exe"
    if (-not (Test-Path $uv)) { $uv = "uv" }
    Write-Host "Exporting w2v2-align-id (downloads the HF weights on first run)..."
    & $uv run --with torch --with transformers --with onnx python (Join-Path $PSScriptRoot "export-align-onnx.py") $alignDir
    if ($LASTEXITCODE -ne 0) { throw "align model export failed with exit code $LASTEXITCODE (is uv installed? https://docs.astral.sh/uv/)" }
}

Write-Host "Recording versions..."
$gb = [math]::Round((Get-Item $out).Length / 1GB, 2)
$versions = @(
    "whisper model: $model (f16, M1 default)",
    "size: $gb GB",
    "source: $uri",
    "vad model: $vad (Silero, opt-in YC_VAD=1 - ADR 0033)",
    "vad source: $vadUri",
    "qwen3-asr: Qwen3-ASR-1.7B-Q8_0.gguf + mmproj (multimodal; ASR A/B trial - ROADMAP)",
    "qwen3-asr source: $qwenRepo",
    "align model: w2v2-align-id/model.onnx + vocab.json (cahya/wav2vec2-large-xlsr-indonesian, fp32 opset17 - ADR 0054/0055, ensemble default)",
    "align source: https://huggingface.co/cahya/wav2vec2-large-xlsr-indonesian (rev fe66c9f1) via scripts/export-align-onnx.py",
    "fetched: $(Get-Date -Format o)"
)
$versions | Out-File (Join-Path $models "VERSIONS.txt") -Encoding utf8

Write-Host "Done:"
Get-Content (Join-Path $models "VERSIONS.txt")
