# Rasterize the "Punch-out" brand mark (assets/branding/logo.svg geometry)
# into the Windows icon set (ADR 0024 gold on Midnight-Studio ink):
#   assets/branding/yt-clipper.ico   - multi-res (16..256, PNG-compressed entries)
#   assets/branding/window-icon-64.rgba - raw 64x64 RGBA for eframe's IconData
# Pure GDI+ drawing (no SVG parser, no external tools). Re-run after any
# geometry change; keep in sync with logo.svg and theme::brand_mark.
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$outDir = Join-Path $root "assets\branding"
if (-not (Test-Path $outDir)) { New-Item -ItemType Directory -Force $outDir | Out-Null }

$gold = [System.Drawing.Color]::FromArgb(255, 0xFF, 0xD1, 0x00)
$ink  = [System.Drawing.Color]::FromArgb(255, 0x14, 0x16, 0x1B)

function New-RoundedPath([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

# Draw the mark once at 1024 px on transparent ground (240-unit viewBox * s).
$master = 1024
$s = $master / 240.0
$bmp = New-Object System.Drawing.Bitmap($master, $master, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)

# 1) The wide VOD frame: gold outline, no fill.
$framePath = New-RoundedPath (24 * $s) (82 * $s) (192 * $s) (104 * $s) (16 * $s)
$framePen = New-Object System.Drawing.Pen($gold, (12 * $s))
$g.DrawPath($framePen, $framePath)

# 2) The vertical Short punched out of it: gold fill, ink punch-out ring.
$barPath = New-RoundedPath (124 * $s) (53 * $s) (78 * $s) (139 * $s) (16 * $s)
$goldBrush = New-Object System.Drawing.SolidBrush($gold)
$g.FillPath($goldBrush, $barPath)
$ringPen = New-Object System.Drawing.Pen($ink, (10 * $s))
$g.DrawPath($ringPen, $barPath)

# 3) The play tip.
$inkBrush = New-Object System.Drawing.SolidBrush($ink)
$tri = @(
    (New-Object System.Drawing.PointF((151 * $s), (102 * $s)))
    (New-Object System.Drawing.PointF((151 * $s), (142 * $s)))
    (New-Object System.Drawing.PointF((184 * $s), (122 * $s)))
)
$g.FillPolygon($inkBrush, $tri)
$g.Dispose()

function Resize-Png([System.Drawing.Bitmap]$src, [int]$size) {
    $dst = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $dg = [System.Drawing.Graphics]::FromImage($dst)
    $dg.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $dg.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $dg.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $dg.DrawImage($src, (New-Object System.Drawing.Rectangle(0, 0, $size, $size)))
    $dg.Dispose()
    return $dst
}

# --- the .ico: classic DIB entries up to 128 (safest for every consumer,
# including .NET's Icon and rc.exe), PNG-compressed only at 256 (Vista+) -----
function Get-BgraRows([System.Drawing.Bitmap]$b) {
    $r = New-Object System.Drawing.Rectangle(0, 0, $b.Width, $b.Height)
    $d = $b.LockBits($r, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $raw = New-Object byte[] ($b.Width * $b.Height * 4)
    [System.Runtime.InteropServices.Marshal]::Copy($d.Scan0, $raw, 0, $raw.Length)
    $b.UnlockBits($d)
    return , $raw   # comma keeps it a byte[] (PS would unroll it otherwise)
}

function New-DibEntry([System.Drawing.Bitmap]$b) {
    # BITMAPINFOHEADER + bottom-up BGRA + an all-zero 1bpp AND mask.
    $wpx = $b.Width; $hpx = $b.Height
    $raw = Get-BgraRows $b
    $maskStride = [int](([math]::Ceiling($wpx / 32.0)) * 4)
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter($ms)
    $bw.Write([UInt32]40); $bw.Write([Int32]$wpx); $bw.Write([Int32]($hpx * 2))
    $bw.Write([UInt16]1); $bw.Write([UInt16]32); $bw.Write([UInt32]0)
    $bw.Write([UInt32]($wpx * $hpx * 4 + $maskStride * $hpx))
    $bw.Write([Int32]0); $bw.Write([Int32]0); $bw.Write([UInt32]0); $bw.Write([UInt32]0)
    for ($y = $hpx - 1; $y -ge 0; $y--) {
        $bw.Write([byte[]]$raw, [int]($y * $wpx * 4), [int]($wpx * 4))
    }
    $bw.Write((New-Object byte[] ($maskStride * $hpx)))
    $bw.Flush()
    return , $ms.ToArray()   # comma keeps it a byte[]
}

$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$entries = @()
foreach ($size in $sizes) {
    $b = Resize-Png $bmp $size
    if ($size -ge 256) {
        $ms = New-Object System.IO.MemoryStream
        $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $entries += , $ms.ToArray()
        $ms.Dispose()
    } else {
        $entries += , (New-DibEntry $b)
    }
    $b.Dispose()
}
$icoPath = Join-Path $outDir "yt-clipper.ico"
$fs = [System.IO.File]::Create($icoPath)
$w = New-Object System.IO.BinaryWriter($fs)
$w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
    $w.Write([Byte]$dim); $w.Write([Byte]$dim)          # width, height (0 = 256)
    $w.Write([Byte]0); $w.Write([Byte]0)                # palette, reserved
    $w.Write([UInt16]1); $w.Write([UInt16]32)           # planes, bpp
    $w.Write([UInt32]$entries[$i].Length); $w.Write([UInt32]$offset)
    $offset += $entries[$i].Length
}
foreach ($p in $entries) { $w.Write($p) }
$w.Flush(); $fs.Close()
Write-Host ("wrote " + $icoPath + " (" + (Get-Item $icoPath).Length + " bytes, " + $sizes.Count + " sizes)")

# --- the eframe window icon: raw RGBA, decoded by nothing at runtime ---------
$win = Resize-Png $bmp 64
$rect = New-Object System.Drawing.Rectangle(0, 0, 64, 64)
$data = $win.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$bytes = New-Object byte[] (64 * 64 * 4)
[System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
$win.UnlockBits($data)
# GDI+ hands back BGRA; eframe wants RGBA.
for ($i = 0; $i -lt $bytes.Length; $i += 4) {
    $b = $bytes[$i]; $bytes[$i] = $bytes[$i + 2]; $bytes[$i + 2] = $b
}
$rgbaPath = Join-Path $outDir "window-icon-64.rgba"
[System.IO.File]::WriteAllBytes($rgbaPath, $bytes)
Write-Host ("wrote " + $rgbaPath + " (" + $bytes.Length + " bytes)")

$win.Dispose(); $bmp.Dispose()
