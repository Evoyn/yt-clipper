# M2 spike (throwaway): fetch ONLY a padded section of a 1080p H.264 stream,
# fast, without hanging.
#
# Findings so far (2026 YouTube reality - SABR/PO-token era):
#   * android_vr client -> direct DASH but moov-not-at-front: ffmpeg CANNOT
#     range-seek it ("could not seek to position 600 / partial file"); seeking
#     means downloading the whole 2 GB file = the 10-min "hang".
#   * web client -> SABR-only formats, not directly downloadable (empty list).
#   * web_safari client -> HLS (m3u8) WITHOUT a PO token. HLS is fragmented, so
#     --download-sections grabs only the in-range segments natively. NO seek.
#
# This spike: player_client=web_safari, native HLS section download. The output
# SIZE is the signal: ~30-40 MB in a few s = WIN. HARD-KILL at 150 s as backstop.
# NOTE: no --downloader-args here (its space-containing value got split by
# Start-Process and broke the prior run); HLS-native needs no ffmpeg seek anyway.
#
# Run via:  ! powershell -ExecutionPolicy Bypass -File scripts\spike-segment.ps1
# Report: the format list (is 1080p there? what protocol?) + the Result block.
# Pure ASCII only (PowerShell 5.1 reads BOM-less UTF-8 as CP1252).
param(
    [string]$Url     = "https://www.youtube.com/watch?v=ZSegfmsrYmE",
    [string]$Section = "*600-660",
    [int]   $CapSecs = 150,
    [string]$Client  = "web_safari"
)
$ErrorActionPreference = "Stop"
$root     = Split-Path -Parent $PSScriptRoot
$sidecars = Join-Path $root "sidecars"
$ytdlp    = Join-Path $sidecars "yt-dlp.exe"
$ffprobe  = Join-Path $sidecars "ffprobe.exe"
$ffmpeg   = Join-Path $sidecars "ffmpeg.exe"
$out      = Join-Path $root "workspace\_ingest"
$xargs    = "youtube:player_client=$Client"
New-Item -ItemType Directory -Force $out | Out-Null

# deno on PATH so yt-dlp can solve nsig.
$deno = $null
if (Test-Path (Join-Path $sidecars "deno.exe")) { $deno = $sidecars }
elseif (Get-Command deno -ErrorAction SilentlyContinue) { $deno = "(on PATH)" }
else {
    $hit = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\DenoLand.Deno_*" -Recurse -Filter deno.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($hit) { $deno = $hit.DirectoryName; $env:PATH = "$($hit.DirectoryName);$env:PATH" }
}
Write-Host "deno: $(if ($deno) { $deno } else { 'NOT FOUND' })  |  player_client=$Client" -ForegroundColor Cyan

# What does web_safari offer? Video rows show 'WxH ... <protocol>'; want 1080p + m3u8.
Write-Host "`n=== Formats under player_client=$Client ===" -ForegroundColor Cyan
& $ytdlp -F --extractor-args $xargs $Url | Select-String -Pattern "\d+x\d+|audio only" | Select-Object -First 40

# Fetch the section. HLS -> native fragment download (no --downloader-args, so no
# space-split bug). ffmpeg is still used to MERGE video+audio (not to seek).
$fmt = "bestvideo[height<=1080][vcodec^=avc1]+bestaudio/bestvideo[height<=1080]+bestaudio/best[height<=1080]/best"
$ytArgs = @(
    "--download-sections", $Section,
    "-f", $fmt,
    "--extractor-args", $xargs,
    "--socket-timeout", "30",
    "--ffmpeg-location", $ffmpeg,
    "--no-playlist", "--newline",
    "-o", (Join-Path $out "spike.%(ext)s"),
    $Url
)
Write-Host "`n=== Fetching section $Section (hard cap ${CapSecs}s) ===" -ForegroundColor Cyan
Remove-Item (Join-Path $out "spike.*") -Force -ErrorAction SilentlyContinue
$logOut = Join-Path $out "spike.out.log"
$logErr = Join-Path $out "spike.err.log"
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$proc = Start-Process -FilePath $ytdlp -ArgumentList $ytArgs -NoNewWindow -PassThru -RedirectStandardOutput $logOut -RedirectStandardError $logErr
$exited = $proc.WaitForExit($CapSecs * 1000)
$sw.Stop()
if (-not $exited) {
    Write-Host "HARD-KILLED after ${CapSecs}s" -ForegroundColor Red
    & taskkill /T /F /PID $proc.Id 2>$null | Out-Null
    Start-Sleep -Milliseconds 500
}

Write-Host "`n=== Result ===" -ForegroundColor Cyan
Write-Host ("wall-clock : {0:N1}s" -f $sw.Elapsed.TotalSeconds)
Write-Host ("exited     : $exited" + $(if ($exited) { " (exit $($proc.ExitCode))" } else { "" }))
Get-ChildItem (Join-Path $out "spike.*") | Where-Object { $_.Extension -ne ".log" } |
    Select-Object Name, @{n='MB'; e={ [math]::Round($_.Length / 1MB, 1) }} | Format-Table -AutoSize
$spikeMp4 = Get-ChildItem (Join-Path $out "spike.*") | Where-Object { $_.Extension -in ".mp4", ".mkv" } | Select-Object -First 1
if ($spikeMp4) {
    Write-Host "ffprobe ($($spikeMp4.Name)):" -ForegroundColor Cyan
    & $ffprobe -v error -show_entries "format=duration:stream=codec_name,width,height" -of "default=noprint_wrappers=1" $spikeMp4.FullName
}
Write-Host "`n--- last yt-dlp lines ---" -ForegroundColor DarkGray
Get-Content $logErr, $logOut -ErrorAction SilentlyContinue | Select-Object -Last 16
