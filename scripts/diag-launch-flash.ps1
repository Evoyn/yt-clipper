# diag-launch-flash.ps1 - regression check for the launch flash (2026-07-20):
# with a maximized window persisted (app.ron window settings maximized:true),
# the app used to APPEAR unpainted, VANISH ~30 ms later, and re-APPEAR ~1 s
# on (winit applies a restored `maximized` via ShowWindow(SW_MAXIMIZE), which
# force-shows eframe's hidden-until-first-paint window; fixed in app main.rs
# by deferring the maximize past the first painted frame).
#
# Launches yt-clipper.exe and logs every top-level window appear/vanish event
# (pid, process, class, title, rect), then closes ONLY the launched instance
# via PostMessage WM_CLOSE.
#
# PASS: exactly one APPEAR of cls='Window Class' and zero VANISH events for
#       it while it runs (with %APPDATA%\yt-clipper\data\app.ron holding
#       maximized:true - the failing condition).
# FAIL: an APPEAR/VANISH pair for the same hwnd before the final APPEAR.
#
# There is no Rust-test seam for this: the bug spans winit's Win32 ShowWindow
# semantics and eframe's paint loop. This watcher IS the regression test.
# ASCII only, PS 5.1.

param(
    [string]$Exe = "F:\yt-clipper\target\release\yt-clipper.exe",
    [double]$WatchSeconds = 12.0
)

$ErrorActionPreference = "Stop"

Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public class WinEnum {
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lp);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr wp, IntPtr lp);

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int L, T, R, B; }

    delegate bool EnumWindowsProc(IntPtr h, IntPtr lp);

    public class Win {
        public long Hwnd; public uint Pid; public string Cls; public string Title;
        public int X; public int Y; public int W; public int H;
    }

    public static List<Win> Snapshot() {
        var list = new List<Win>();
        EnumWindows(delegate(IntPtr h, IntPtr lp) {
            if (!IsWindowVisible(h)) return true;
            uint pid; GetWindowThreadProcessId(h, out pid);
            var cb = new StringBuilder(256); GetClassName(h, cb, 256);
            var tb = new StringBuilder(512); GetWindowText(h, tb, 512);
            RECT r; GetWindowRect(h, out r);
            list.Add(new Win { Hwnd = h.ToInt64(), Pid = pid, Cls = cb.ToString(),
                Title = tb.ToString(), X = r.L, Y = r.T, W = r.R - r.L, H = r.B - r.T });
            return true;
        }, IntPtr.Zero);
        return list;
    }
}
"@

function ProcName([uint32]$p) {
    try { (Get-Process -Id $p -ErrorAction Stop).ProcessName } catch { "gone" }
}

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$prev = @{}
foreach ($w in [WinEnum]::Snapshot()) { $prev[$w.Hwnd] = $w }

Write-Output ("baseline: {0} visible top-level windows" -f $prev.Count)
$proc = Start-Process -FilePath $Exe -WorkingDirectory (Split-Path $Exe) -PassThru
Write-Output ("launched pid {0} at t={1}ms" -f $proc.Id, $sw.ElapsedMilliseconds)

$events = New-Object System.Collections.ArrayList
$seenPids = @{}
while ($sw.Elapsed.TotalSeconds -lt $WatchSeconds) {
    $now = $sw.ElapsedMilliseconds
    $cur = @{}
    foreach ($w in [WinEnum]::Snapshot()) { $cur[$w.Hwnd] = $w }
    foreach ($h in $cur.Keys) {
        if (-not $prev.ContainsKey($h)) {
            $w = $cur[$h]
            $pn = ProcName $w.Pid
            if (-not $seenPids.ContainsKey($w.Pid)) { $seenPids[$w.Pid] = $pn }
            [void]$events.Add(("t={0,6}ms APPEAR hwnd=0x{1:X} pid={2} proc={3} cls='{4}' title='{5}' rect=({6},{7} {8}x{9})" -f `
                $now, $w.Hwnd, $w.Pid, $pn, $w.Cls, $w.Title, $w.X, $w.Y, $w.W, $w.H))
        }
    }
    foreach ($h in $prev.Keys) {
        if (-not $cur.ContainsKey($h)) {
            $w = $prev[$h]
            $pn = ProcName $w.Pid
            [void]$events.Add(("t={0,6}ms VANISH hwnd=0x{1:X} pid={2} proc={3} cls='{4}' title='{5}'" -f `
                $now, $w.Hwnd, $w.Pid, $pn, $w.Cls, $w.Title))
        }
    }
    $prev = $cur
    Start-Sleep -Milliseconds 25
}

Write-Output "---- window events ----"
foreach ($e in $events) { Write-Output $e }

# Is the launched process still alive? Did any OTHER yt-clipper pid appear?
$alive = $null
try { $alive = Get-Process -Id $proc.Id -ErrorAction Stop } catch {}
if ($null -ne $alive) {
    Write-Output ("launched pid {0} still alive" -f $proc.Id)
} else {
    Write-Output ("launched pid {0} EXITED (code {1})" -f $proc.Id, $proc.ExitCode)
}
$clips = @()
try { $clips = @(Get-Process yt-clipper -ErrorAction Stop) } catch {}
foreach ($c in $clips) {
    Write-Output ("yt-clipper process now: pid={0} start={1}" -f $c.Id, $c.StartTime.ToString("HH:mm:ss.fff"))
}

# Close ONLY processes named yt-clipper that I saw appear during this run
# (the launched pid, plus any respawn of it). WM_CLOSE = 0x0010.
$targets = @()
foreach ($c in $clips) { $targets += $c.Id }
if ($targets.Count -gt 0) {
    foreach ($w in [WinEnum]::Snapshot()) {
        if ($targets -contains [int]$w.Pid) {
            [void][WinEnum]::PostMessage([IntPtr]$w.Hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
            Write-Output ("posted WM_CLOSE to hwnd=0x{0:X} pid={1}" -f $w.Hwnd, $w.Pid)
        }
    }
    Start-Sleep -Milliseconds 2500
    foreach ($t in $targets) {
        try {
            $p = Get-Process -Id $t -ErrorAction Stop
            Write-Output ("pid {0} ignored WM_CLOSE; force-stopping my own launch" -f $t)
            Stop-Process -Id $t -Force -Confirm:$false
        } catch { Write-Output ("pid {0} closed cleanly" -f $t) }
    }
}
