# Fetches the pinned llama.cpp runtime into /sidecars/llama for the Qwen3-ASR
# sidecar (ROADMAP "ASR engine upgrade"): llama-mtmd-cli.exe (the one-shot
# multimodal CLI the asr_ab_diag harness drives) + llama-server.exe (the likely
# production transport) + every runtime DLL, including the CUDA runtime from the
# matching cudart zip so the sidecar runs WITHOUT the CUDA toolkit on PATH
# (Windows searches the exe's own directory first - the deep-filter
# self-containment precedent).
#
# Pinned to b9859 (2026-07-02): first pin AFTER the Qwen3-ASR repetition fix
# (b9173, ggml-org/llama.cpp#23073) - bump deliberately, never automatically
# (Offline constraint: network use is operator-initiated only). CUDA 13.3
# build to match this machine's toolkit (see cargo-cuda.bat); RTX 3070 Ti.
# Records what was fetched in sidecars/llama/VERSIONS.txt.
# Pure ASCII: PowerShell 5.1 reads BOM-less UTF-8 as CP1252.
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"  # IWR progress rendering cripples big downloads on PS 5.1

$tag = "b9859"
$cuda = "13.3"
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "sidecars\llama"
New-Item -ItemType Directory -Force $dest | Out-Null

$base = "https://github.com/ggml-org/llama.cpp/releases/download/$tag"
$zips = @(
    "llama-$tag-bin-win-cuda-$cuda-x64.zip",
    "cudart-llama-bin-win-cuda-$cuda-x64.zip"
)

$extract = Join-Path $env:TEMP "llama-sidecar-extract"
if (Test-Path $extract) { Remove-Item -Recurse -Force $extract }
New-Item -ItemType Directory -Force $extract | Out-Null

foreach ($zip in $zips) {
    $zipPath = Join-Path $env:TEMP $zip
    Write-Host "Downloading $zip ..."
    Invoke-WebRequest -Uri "$base/$zip" -OutFile $zipPath
    Write-Host "Extracting $zip ..."
    Expand-Archive -Path $zipPath -DestinationPath $extract -Force
    Remove-Item -Force $zipPath
}

# The harness needs mtmd-cli; server is staged for the production transport
# decision. Every DLL from both zips (ggml/llama/mtmd + cudart/cublas) lands
# flat beside the exes so DLL resolution never depends on the environment.
$keepExes = @("llama-mtmd-cli.exe", "llama-server.exe")
foreach ($exe in $keepExes) {
    $found = Get-ChildItem -Path $extract -Recurse -Filter $exe | Select-Object -First 1
    if ($null -eq $found) { throw "$exe not found in the release zips" }
    Copy-Item $found.FullName (Join-Path $dest $exe) -Force
}
Get-ChildItem -Path $extract -Recurse -Filter "*.dll" | ForEach-Object {
    Copy-Item $_.FullName (Join-Path $dest $_.Name) -Force
}
Remove-Item -Recurse -Force $extract

Write-Host "Recording versions..."
$versions = @()
$versions += "llama.cpp $tag (win-cuda-$cuda-x64 + cudart), pinned - see scripts/fetch-llama-sidecar.ps1"
$versions += "source: $base"
$versions += "fetched: $(Get-Date -Format o)"
$versions | Out-File (Join-Path $dest "VERSIONS.txt") -Encoding utf8

Write-Host "Done:"
Get-Content (Join-Path $dest "VERSIONS.txt")
Get-ChildItem $dest -Filter "*.exe" | ForEach-Object { Write-Host ("  " + $_.Name) }
