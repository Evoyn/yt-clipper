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

Write-Host "Recording versions..."
$gb = [math]::Round((Get-Item $out).Length / 1GB, 2)
$versions = @(
    "whisper model: $model (f16, M1 default)",
    "size: $gb GB",
    "source: $uri",
    "vad model: $vad (Silero, opt-in YC_VAD=1 - ADR 0033)",
    "vad source: $vadUri",
    "fetched: $(Get-Date -Format o)"
)
$versions | Out-File (Join-Path $models "VERSIONS.txt") -Encoding utf8

Write-Host "Done:"
Get-Content (Join-Path $models "VERSIONS.txt")
