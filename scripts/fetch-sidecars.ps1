# Fetches the two binary sidecars into /sidecars: ffmpeg (gyan.dev release-essentials,
# includes NVENC + libass) and yt-dlp (official release exe). Records exactly what was
# fetched in sidecars/VERSIONS.txt — re-run deliberately to update, never automatically
# (Offline constraint: network use is operator-initiated only).
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

Write-Host "Recording versions..."
$versions = @()
$versions += (& (Join-Path $sidecars "ffmpeg.exe") -version | Select-Object -First 1)
$versions += "yt-dlp " + (& (Join-Path $sidecars "yt-dlp.exe") --version)
$versions += "fetched: $(Get-Date -Format o)"
$versions | Out-File (Join-Path $sidecars "VERSIONS.txt") -Encoding utf8

Write-Host "Done:"
Get-Content (Join-Path $sidecars "VERSIONS.txt")
