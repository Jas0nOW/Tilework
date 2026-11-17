param([Parameter(Mandatory)][string]$Executable)
# Use an interactive standard-user session with Tilework closed.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class TileworkAuditNative {
    [StructLayout(LayoutKind.Sequential)] public struct Rect {
        public int Left, Top, Right, Bottom;
        public override string ToString() => $"{Left},{Top}..{Right},{Bottom}";
    }
    [StructLayout(LayoutKind.Sequential)] public struct MonitorInfo {
        public int Size;
        public Rect Monitor, Work;
        public int Flags;
    }
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
    [DllImport("user32.dll")] public static extern IntPtr MonitorFromWindow(IntPtr hwnd, int flags);
    [DllImport("user32.dll")] public static extern bool GetMonitorInfoW(IntPtr monitor, ref MonitorInfo info);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string className, string title);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd, uint message, IntPtr wp, IntPtr lp);
    [DllImport("user32.dll")] public static extern bool SystemParametersInfoW(uint action, uint value, out int output, uint flags);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr hwnd, int attribute, out Rect rect, int size);
    public static Rect Bounds(IntPtr hwnd) {
        if (DwmGetWindowAttribute(hwnd, 9, out Rect rect, Marshal.SizeOf<Rect>()) != 0)
            throw new Exception("Could not query DWM extended frame bounds");
        return rect;
    }
    public static Rect Work(IntPtr hwnd) {
        MonitorInfo info = new MonitorInfo { Size = Marshal.SizeOf<MonitorInfo>() };
        if (!GetMonitorInfoW(MonitorFromWindow(hwnd, 2), ref info)) throw new Exception("Could not query work area");
        return info.Work;
    }
    public static bool SnapEnabled() {
        if (!SystemParametersInfoW(0x0082, 0, out int on, 0)) throw new Exception("Could not read Windows Snap state");
        return on != 0;
    }
}
"@
function Wait-AuditCondition([scriptblock]$Condition, [int]$Milliseconds = 7000) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    while ($watch.ElapsedMilliseconds -lt $Milliseconds) {
        [System.Windows.Forms.Application]::DoEvents()
        if (& $Condition) { return $true }
        Start-Sleep -Milliseconds 35
    }
    return $false
}
function Get-AuditBounds($Form) { [TileworkAuditNative]::Bounds($Form.Handle) }
function Test-Close($a, $b) { [Math]::Abs($a - $b) -le 3 }
function Test-FullWork($r) {
    (Test-Close $r.Left $work.Left) -and (Test-Close $r.Top $work.Top) -and
    (Test-Close $r.Right $work.Right) -and (Test-Close $r.Bottom $work.Bottom)
}
function Test-Halves($a, $b) {
    $ordered = @($a, $b) | Sort-Object Left
    $left = $ordered[0]; $right = $ordered[1]
    (Test-Close $left.Left $work.Left) -and (Test-Close $right.Right $work.Right) -and
    (Test-Close $left.Right $right.Left) -and (Test-Close $left.Right (($work.Left + $work.Right) / 2)) -and
    (Test-Close $left.Top $work.Top) -and (Test-Close $right.Top $work.Top) -and
    (Test-Close $left.Bottom $work.Bottom) -and (Test-Close $right.Bottom $work.Bottom)
}
function Test-V3Third($rects) {
    $midX = ($work.Left + $work.Right) / 2
    $midY = ($work.Top + $work.Bottom) / 2
    $left = @($rects | Where-Object { (Test-Close $_.Left $work.Left) -and (Test-Close $_.Right $midX) } | Sort-Object Top)
    $right = @($rects | Where-Object { (Test-Close $_.Left $midX) -and (Test-Close $_.Right $work.Right) } | Sort-Object Top)
    if ($left.Count -eq 2 -and $right.Count -eq 1) { $stack = $left; $full = $right[0] }
    elseif ($right.Count -eq 2 -and $left.Count -eq 1) { $stack = $right; $full = $left[0] }
    else { return $false }
    (Test-Close $full.Top $work.Top) -and (Test-Close $full.Bottom $work.Bottom) -and
    (Test-Close $stack[0].Top $work.Top) -and (Test-Close $stack[0].Bottom $midY) -and
    (Test-Close $stack[1].Top $midY) -and (Test-Close $stack[1].Bottom $work.Bottom)
}
function New-AuditForm([string]$Name) {
    $form = [System.Windows.Forms.Form]::new()
    $form.Text = "$token $Name"
    $form.StartPosition = [System.Windows.Forms.FormStartPosition]::Manual
    $form.Location = [System.Drawing.Point]::new(110, 110)
    $form.Size = [System.Drawing.Size]::new(450, 300)
    $form.Show()
    return $form
}
if ((Get-Process -Id $PID).SessionId -eq 0) { throw 'No interactive Windows session' }
if ([TileworkAuditNative]::FindWindowW('TileworkMain', 'Tilework') -ne [IntPtr]::Zero) { throw 'Tilework is already running' }
$token = 'TileworkAudit-' + [guid]::NewGuid().ToString('N').Substring(0, 10)
$blockedLocalAppData = [IO.Path]::GetTempFileName()
$beforeSnap = [TileworkAuditNative]::SnapEnabled()
$oldDpi = [TileworkAuditNative]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
$tilework = $null
$forms = @()
$shutdownFailure = $null
try {
    $forms += New-AuditForm 'one'
    $work = [TileworkAuditNative]::Work($forms[0].Handle)
    $start = [Diagnostics.ProcessStartInfo]::new((Resolve-Path -LiteralPath $Executable).Path)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.Environment['LOCALAPPDATA'] = $blockedLocalAppData
    $start.ArgumentList.Add('--filter')
    $start.ArgumentList.Add($token)
    $tilework = [Diagnostics.Process]::Start($start)
    if (!(Wait-AuditCondition { Test-FullWork (Get-AuditBounds $forms[0]) })) {
        throw "Single-window layout failed. Work area $work, window $(Get-AuditBounds $forms[0])"
    }
    Write-Output "PASS native single-window placement. Work area $work."
    $forms += New-AuditForm 'two'
    if (!(Wait-AuditCondition { Test-Halves (Get-AuditBounds $forms[0]) (Get-AuditBounds $forms[1]) })) { throw 'Two-window geometry failed' }
    Write-Output 'PASS native window-open events and two-column placement.'
    $forms += New-AuditForm 'three'
    if (!(Wait-AuditCondition { Test-V3Third @($forms | ForEach-Object { Get-AuditBounds $_ }) })) { throw 'Third-window geometry failed' }
    Write-Output 'PASS native V3 third-window vertical split.'
    $forms[2].Close()
    if (!(Wait-AuditCondition { Test-Halves (Get-AuditBounds $forms[0]) (Get-AuditBounds $forms[1]) })) { throw 'Window-close reflow geometry failed' }
    Write-Output 'PASS native window-close reflow.'
    $forms[1].WindowState = [System.Windows.Forms.FormWindowState]::Minimized
    if (!(Wait-AuditCondition { Test-FullWork (Get-AuditBounds $forms[0]) })) { throw 'Minimize reflow geometry failed' }
    $forms[1].WindowState = [System.Windows.Forms.FormWindowState]::Normal
    if (!(Wait-AuditCondition { Test-Halves (Get-AuditBounds $forms[0]) (Get-AuditBounds $forms[1]) })) { throw 'Restore reflow geometry failed' }
    Write-Output 'PASS native minimize/restore reflow.'
    $front = $forms[0]
    $front.Activate()
    $frame = [TileworkAuditNative]::FindWindowW('TileworkFocusBorder', $null)
    $framePid = 0u
    [void][TileworkAuditNative]::GetWindowThreadProcessId($frame, [ref]$framePid)
    if ($framePid -eq $tilework.Id -and (Wait-AuditCondition { [TileworkAuditNative]::GetForegroundWindow() -eq $front.Handle } 800)) {
        if (!(Wait-AuditCondition { [TileworkAuditNative]::IsWindowVisible($frame) })) { throw 'Focus overlay did not become visible' }
        $actual = [TileworkAuditNative]::Bounds($frame)
        $expected = Get-AuditBounds $front
        if (!(Test-Close $actual.Left $expected.Left) -or !(Test-Close $actual.Top $expected.Top) -or !(Test-Close $actual.Right $expected.Right) -or !(Test-Close $actual.Bottom $expected.Bottom)) {
            throw "Focus overlay bounds differ. Overlay $actual; window $expected"
        }
        Write-Output 'PASS native visible focus overlay bounds against active tiled window.'
    } else { Write-Output 'NOT TESTED native focus overlay. Windows did not grant foreground activation.' }
} finally {
    if ($tilework -and !$tilework.HasExited) {
        $main = [TileworkAuditNative]::FindWindowW('TileworkMain', 'Tilework')
        $mainPid = 0u
        [void][TileworkAuditNative]::GetWindowThreadProcessId($main, [ref]$mainPid)
        if ($mainPid -eq $tilework.Id) { [void][TileworkAuditNative]::PostMessageW($main, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }
        if (!$tilework.WaitForExit(5000)) {
            $tilework.Kill()
            $shutdownFailure = 'Tilework required forced termination'
        } elseif ($tilework.ExitCode -ne 0) {
            $shutdownFailure = "Tilework exited with code $($tilework.ExitCode)"
        } else {
            Write-Output 'PASS native manager shutdown (exit 0).'
        }
    }
    foreach ($form in $forms) { $form.Dispose() }
    if ($oldDpi -ne [IntPtr]::Zero) { [void][TileworkAuditNative]::SetThreadDpiAwarenessContext($oldDpi) }
    Remove-Item -LiteralPath $blockedLocalAppData
    $afterSnap = [TileworkAuditNative]::SnapEnabled()
    if ($afterSnap -ne $beforeSnap) { throw 'Unexpected Windows Snap state change' }
    Write-Output "PASS Snap state unchanged ($beforeSnap)."
    if ($shutdownFailure) { throw $shutdownFailure }
}
