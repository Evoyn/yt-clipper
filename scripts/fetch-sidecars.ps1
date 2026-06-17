# Fetches the binary sidecars into /sidecars: ffmpeg (gyan.dev release-essentials,
# includes NVENC + libass), yt-dlp (official release exe), and deno (yt-dlp's JS
# runtime for nsig / anti-throttle - without it YouTube media URLs 403; ADR 0006).
# Records exactly what was fetched in sidecars/VERSIONS.txt - re-run deliberately to
# update, never automatically (Offline constraint: network use is operator-initiated
# only). Pure ASCII: PowerShell 5.1 reads BOM-less UTF-8 as CP1252.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$sidecars = Join-Path $root "sidecars"
New-Item -ItemType Directory -Force $sidecars | Out-Null

Write-Host "Downloading yt-dlp..."
Invoke-WebRequest -Uri "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe" `
    -OutFile (Join-Path $sidecars "yt-dlp.exe")

Write-Host "Downloading ffmpeg (release-essentials)..."
$zip = Join-Path $env:TEMP "ffmpeg-release-essentials.zip"
Invoke-WebRequest -Uri "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip" -OutFile $zip
$extract = Join-Path $env:TEMP "ffmpeg-extract"
if (Test-Path $extract) { Remove-Item -Recurse -Force $extract }
Expand-Archive -Path $zip -DestinationPath $extract
$bin = Get-ChildItem -Path $extract -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1
Copy-Item $bin.FullName (Join-Path $sidecars "ffmpeg.exe")
Copy-Item (Join-Path $bin.DirectoryName "ffprobe.exe") (Join-Path $sidecars "ffprobe.exe")
Remove-Item -Recurse -Force $extract
Remove-Item -Force $zip

Write-Host "Downloading deno (yt-dlp JS runtime for nsig)..."
$denoZip = Join-Path $env:TEMP "deno-win.zip"
Invoke-WebRequest -Uri "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip" `
    -OutFile $denoZip
$denoExtract = Join-Path $env:TEMP "deno-extract"
if (Test-Path $denoExtract) { Remove-Item -Recurse -Force $denoExtract }
Expand-Archive -Path $denoZip -DestinationPath $denoExtract
$denoBin = Get-ChildItem -Path $denoExtract -Recurse -Filter "deno.exe" | Select-Object -First 1
Copy-Item $denoBin.FullName (Join-Path $sidecars "deno.exe")
Remove-Item -Recurse -Force $denoExtract
Remove-Item -Force $denoZip

Write-Host "Recording versions..."
$versions = @()
$versions += (& (Join-Path $sidecars "ffmpeg.exe") -version | Select-Object -First 1)
$versions += "yt-dlp " + (& (Join-Path $sidecars "yt-dlp.exe") --version)
$versions += (& (Join-Path $sidecars "deno.exe") --version | Select-Object -First 1)
$versions += "fetched: $(Get-Date -Format o)"
$versions | Out-File (Join-Path $sidecars "VERSIONS.txt") -Encoding utf8

Write-Host "Done:"
Get-Content (Join-Path $sidecars "VERSIONS.txt")
