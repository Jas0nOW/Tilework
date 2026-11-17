param([Parameter(Mandatory)][string]$Executable, [switch]$TestSnapRecovery, [switch]$TestDrag)
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
    [DllImport("user32.dll", EntryPoint="SystemParametersInfoW")]
    public static extern bool SetSystemParameter(uint action, uint value, IntPtr input, uint flags);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr hwnd, int attribute, out Rect rect, int size);

    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [DllImport("user32.dll")] static extern bool GetCursorPos(out Point p);
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern void mouse_event(uint flags, uint x, uint y, uint data, UIntPtr info);
    [DllImport("user32.dll")] static extern IntPtr WindowFromPoint(Point p);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr hwnd, uint flags);
    [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr window);
    [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr window, IntPtr dc);
    [DllImport("gdi32.dll")] static extern uint GetPixel(IntPtr dc, int x, int y);
    static bool PreviewBlue(IntPtr dc, int x, int y) {
        uint color = GetPixel(dc, x, y);
        int red = (int)(color & 255), green = (int)(color >> 8 & 255), blue = (int)(color >> 16 & 255);
        return blue - red >= 90 && blue - green >= 40;
    }
    [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleDC(IntPtr dc);
    [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleBitmap(IntPtr dc, int width, int height);
    [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr dc, IntPtr obj);
    [DllImport("gdi32.dll")] static extern bool DeleteDC(IntPtr dc);
    [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr obj);
    [DllImport("gdi32.dll")] static extern bool BitBlt(IntPtr dest, int x, int y, int w, int h, IntPtr src, int sx, int sy, uint op);
    static Rect SamplePreview(IntPtr overlay, int x, int y) {
        Rect area = Bounds(overlay);
        int width = area.Right - area.Left, height = area.Bottom - area.Top;
        IntPtr screen = GetDC(IntPtr.Zero);
        IntPtr dc = CreateCompatibleDC(screen);
        IntPtr bitmap = CreateCompatibleBitmap(screen, width, height);
        IntPtr old = SelectObject(dc, bitmap);
        try {
            if (!BitBlt(dc, 0, 0, width, height, screen, area.Left, area.Top, 0x40CC0020)) throw new Exception("Preview capture failed");
            x -= area.Left;
            y -= area.Top;
            Rect result = new Rect { Left = x, Top = y, Right = x, Bottom = y };
            if (PreviewBlue(dc, x, y)) {
                while (result.Left > 0 && PreviewBlue(dc, result.Left-1, y)) result.Left--;
                while (result.Right < width && PreviewBlue(dc, result.Right, y)) result.Right++;
                while (result.Top > 0 && PreviewBlue(dc, x, result.Top-1)) result.Top--;
                while (result.Bottom < height && PreviewBlue(dc, x, result.Bottom)) result.Bottom++;
            }
            result.Left += area.Left;
            result.Right += area.Left;
            result.Top += area.Top;
            result.Bottom += area.Top;
            return result;
        } finally {
            SelectObject(dc, old);
            DeleteObject(bitmap);
            DeleteDC(dc);
            ReleaseDC(IntPtr.Zero, screen);
        }
    }
    public static System.Threading.Tasks.Task<Rect> Drag(IntPtr source, int x1, int y1, int x2, int y2, int sampleX, int sampleY) {
        return System.Threading.Tasks.Task.Run(() => {
            IntPtr oldDpi = SetThreadDpiAwarenessContext(new IntPtr(-4));
            GetCursorPos(out Point old);
            bool pressed = false;
            try {
                if (!SetCursorPos(x1, y1)) throw new Exception("SetCursorPos was refused.");
                System.Threading.Thread.Sleep(120);
                IntPtr hit = GetAncestor(WindowFromPoint(new Point { X = x1, Y = y1 }), 2);
                if (hit != source) throw new Exception("The fixture caption was obscured.");
                mouse_event(2, 0, 0, 0, UIntPtr.Zero);
                pressed = true;
                System.Threading.Thread.Sleep(110);
                for (int i = 1; i <= 20; i++) {
                    SetCursorPos(x1 + (x2 - x1) * i / 20, y1 + (y2 - y1) * i / 20);
                    System.Threading.Thread.Sleep(25);
                }
                IntPtr overlay = FindWindowW("TileworkOverlay", null);
                for (int i = 0; i < 50; i++) {
                    if (overlay != IntPtr.Zero && IsWindowVisible(overlay)) {
                        System.Threading.Thread.Sleep(180);
                        Rect preview = SamplePreview(overlay, sampleX, sampleY);
                        mouse_event(4, 0, 0, 0, UIntPtr.Zero);
                        pressed = false;
                        System.Threading.Thread.Sleep(150);
                        return preview;
                    }
                    System.Threading.Thread.Sleep(20);
                }
                throw new Exception("Tilework did not show the native drag preview.");
            } finally {
                if (pressed) mouse_event(4, 0, 0, 0, UIntPtr.Zero);
                SetCursorPos(old.X, old.Y);
                if (oldDpi != IntPtr.Zero) SetThreadDpiAwarenessContext(oldDpi);
            }
        });
    }
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
function Invoke-AuditDrag($Form, [int]$DropX, [int]$DropY, [int]$SampleX, [int]$SampleY) {
    $Form.Activate()
    $source = Get-AuditBounds $Form
    # The mouse driver runs on another thread while WinForms pumps the native move loop.
    $task = [TileworkAuditNative]::Drag($Form.Handle, ($source.Left + 160), ($source.Top + 16), $DropX, $DropY, $SampleX, $SampleY)
    if (!(Wait-AuditCondition { $task.IsCompleted })) { throw 'Mouse driver did not finish.' }
    $task.GetAwaiter().GetResult()
}
function Assert-Preview($Preview, $Tile, [string]$Label) {
    # The colored fill starts at the 3 px inset plus the 3 px white border.
    if ($Preview.Left -ge $Preview.Right -or $Preview.Top -ge $Preview.Bottom -or
        !(Test-Close ($Preview.Left - 6) $Tile.Left) -or !(Test-Close ($Preview.Top - 6) $Tile.Top) -or
        !(Test-Close ($Preview.Right + 6) $Tile.Right) -or !(Test-Close ($Preview.Bottom + 6) $Tile.Bottom)) {
        throw "Native $Label preview $Preview differed from committed tile $Tile."
    }
    Write-Output "PASS native $Label preview $Preview matched committed tile $Tile."
}
if ((Get-Process -Id $PID).SessionId -eq 0) { throw 'No interactive Windows session' }
if ([TileworkAuditNative]::FindWindowW('TileworkMain', 'Tilework') -ne [IntPtr]::Zero) { throw 'Tilework is already running' }
$token = 'TileworkAudit-' + [guid]::NewGuid().ToString('N').Substring(0, 10)
$beforeSnap = [TileworkAuditNative]::SnapEnabled()
if ($TestSnapRecovery -and !$beforeSnap) { throw 'Start the recovery smoke with Windows Snap enabled.' }
$testAppData = if ($TestSnapRecovery) {
    (New-Item -ItemType Directory -Path (Join-Path ([IO.Path]::GetTempPath()) $token)).FullName
} else {
    [IO.Path]::GetTempFileName()
}
$marker = Join-Path $testAppData 'Tilework/snap-off.flag'
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
    $start.Environment['LOCALAPPDATA'] = $testAppData
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
    if ($TestDrag) {
        $forms[0].Activate()
        $from = Get-AuditBounds $forms[0]
        $to = Get-AuditBounds $forms[1]
        $centerX = ($to.Left + $to.Right) / 2
        $centerY = ($to.Top + $to.Bottom) / 2
        $preview = Invoke-AuditDrag $forms[0] $centerX $centerY $centerX $centerY
        if (!(Wait-AuditCondition {
            (Test-Close (Get-AuditBounds $forms[0]).Left $to.Left) -and
            (Test-Close (Get-AuditBounds $forms[1]).Left $from.Left)
        })) { throw 'Native centre swap did not commit expected geometry.' }
        Assert-Preview $preview (Get-AuditBounds $forms[0]) 'centre swap'

        $forms[2] = New-AuditForm 'four'
        if (!(Wait-AuditCondition { Test-V3Third @($forms | ForEach-Object { Get-AuditBounds $_ }) })) {
            throw 'Could not restore the third test tile.'
        }
        $source = $forms[2]
        $target = $forms | Where-Object { $b = Get-AuditBounds $_; $b.Top -eq $work.Top -and $b.Bottom -eq $work.Bottom } |
            Select-Object -First 1
        $to = Get-AuditBounds $target
        $nestedX = $to.Left + ($to.Right - $to.Left) / 8
        $centerY = ($to.Top + $to.Bottom) / 2
        $preview = Invoke-AuditDrag $source $nestedX $centerY $nestedX $centerY
        if (!(Wait-AuditCondition {
            $b = Get-AuditBounds $source
            (Test-Close $b.Left $to.Left) -and (Test-Close $b.Right (($to.Left + $to.Right)/2)) -and
            (Test-Close $b.Top $to.Top) -and (Test-Close $b.Bottom $to.Bottom)
        })) { throw "Native nested split did not commit expected geometry: $(Get-AuditBounds $source), target $to" }
        Assert-Preview $preview (Get-AuditBounds $source) 'nested split'

        $centerX = ($work.Left + $work.Right) / 2
        $dropY = $work.Bottom - 12
        $sampleY = $work.Bottom - ($work.Bottom - $work.Top) / 4
        $preview = Invoke-AuditDrag $source $centerX $dropY $centerX $sampleY
        if (!(Wait-AuditCondition {
            $b = Get-AuditBounds $source
            (Test-Close $b.Left $work.Left) -and (Test-Close $b.Right $work.Right) -and (Test-Close $b.Bottom $work.Bottom)
        })) { throw "Native screen-edge drop did not fill the full width: $(Get-AuditBounds $source)" }
        Assert-Preview $preview (Get-AuditBounds $source) 'bottom-edge zone'
    }
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
    if ($TestSnapRecovery) {
        if (!(Wait-AuditCondition { (Test-Path -LiteralPath $marker) -and ![TileworkAuditNative]::SnapEnabled() })) {
            throw 'Tilework did not take ownership of Windows Snap in the temporary profile.'
        }
        $tilework.Kill()
        if (!$tilework.WaitForExit(5000)) { throw 'Tilework did not terminate.' }
        if ([TileworkAuditNative]::SnapEnabled() -or (Get-Content -Raw -LiteralPath $marker) -ne '1') {
            throw 'Forced termination did not leave the expected recovery marker and disabled Snap.'
        }
        if ($frame -ne [IntPtr]::Zero -and [TileworkAuditNative]::IsWindowVisible($frame)) {
            throw 'The focus overlay remained visible after the process terminated.'
        }
        $tilework = [Diagnostics.Process]::Start($start)
        if (!(Wait-AuditCondition {
            $window = [TileworkAuditNative]::FindWindowW('TileworkMain', 'Tilework')
            $owner = 0u
            [void][TileworkAuditNative]::GetWindowThreadProcessId($window, [ref]$owner)
            $owner -eq $tilework.Id
        })) { throw 'The replacement Tilework process did not start.' }
        if (!(Wait-AuditCondition { (Test-Path -LiteralPath $marker) -and ![TileworkAuditNative]::SnapEnabled() })) {
            throw 'The replacement Tilework process did not acquire its own recovery marker.'
        }
        Write-Output 'PASS forced termination retained the original Snap value and restart reached the manager.'
    }
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
    $afterSnap = [TileworkAuditNative]::SnapEnabled()
    $markerLeft = $TestSnapRecovery -and (Test-Path -LiteralPath $marker)
    if ($afterSnap -ne $beforeSnap -and $TestSnapRecovery) {
        [void][TileworkAuditNative]::SetSystemParameter(0x0083, 0, [IntPtr]::new([int]$beforeSnap), 0)
    }
    if ($TestSnapRecovery) {
        $resolvedAppData = (Resolve-Path -LiteralPath $testAppData).Path
        if ($resolvedAppData -ne (Join-Path ([IO.Path]::GetTempPath()) $token)) {
            throw 'Unexpected fixture app-data cleanup path.'
        }
        Remove-Item -LiteralPath $resolvedAppData -Recurse
    } else {
        Remove-Item -LiteralPath $testAppData
    }
    if ($afterSnap -ne $beforeSnap) { throw 'Tilework left Windows Snap in the wrong state after exit.' }
    if ($markerLeft) { throw 'Tilework left the recovery marker after graceful exit.' }
    Write-Output "PASS Snap state unchanged ($beforeSnap)."
    if ($shutdownFailure) { throw $shutdownFailure }
}
