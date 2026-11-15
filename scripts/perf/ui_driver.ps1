# GUI driver for UI smoothness measurements (dot-source it).
#
# Starts clipuh.exe with GPUI's built-in frame timing (ZED_MEASUREMENTS=1:
# one "frame duration: <t>" line on stderr per drawn frame, covering layout,
# paint and present), positions the window, sends shortcuts and synthetic
# mouse-wheel input, and summarizes the frame log per scenario.
#
# Synthetic input moves the real cursor and keyboard focus: do not use the
# machine while a run is in progress.

Add-Type -Namespace Ui -Name Native -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, int data, System.IntPtr extra);
[DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, System.IntPtr extra);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr h);
[DllImport("user32.dll")] public static extern bool ShowWindow(System.IntPtr h, int c);
[DllImport("user32.dll")] public static extern bool MoveWindow(System.IntPtr h, int x, int y, int w, int hh, bool r);
[StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
[DllImport("user32.dll")] public static extern bool GetWindowRect(System.IntPtr h, out RECT r);
[DllImport("winmm.dll")] public static extern uint timeBeginPeriod(uint p);
'@
Add-Type -AssemblyName System.Drawing
[Ui.Native]::timeBeginPeriod(1) | Out-Null

$script:App = $null
$script:Log = $null

function Start-ClipApp([string]$Exe, [string]$Log) {
    $env:ZED_MEASUREMENTS = '1'
    $script:Log = $Log
    $script:App = Start-Process $Exe -PassThru -RedirectStandardError $Log
    Remove-Item Env:ZED_MEASUREMENTS
    for ($i = 0; $i -lt 100 -and $script:App.MainWindowHandle -eq 0; $i++) {
        Start-Sleep -Milliseconds 100; $script:App.Refresh()
    }
    Start-Sleep -Seconds 2
    $h = $script:App.MainWindowHandle
    [Ui.Native]::ShowWindow($h, 9) | Out-Null
    [Ui.Native]::MoveWindow($h, 100, 60, 1600, 1000, $true) | Out-Null
    Focus-ClipApp
    Start-Sleep -Seconds 2
}

function Focus-ClipApp {
    $script:App.Refresh()
    [Ui.Native]::SetForegroundWindow($script:App.MainWindowHandle) | Out-Null
    Start-Sleep -Milliseconds 300
}

function Stop-ClipApp {
    if ($script:App -and -not $script:App.HasExited) { Stop-Process -Id $script:App.Id -Force }
}

# Ctrl+<digit> page shortcut (1 Dashboard, 2 Library, 3 Editor, 4 Settings).
function Send-Page([int]$Digit) {
    Focus-ClipApp
    [Ui.Native]::keybd_event(0x11, 0, 0, 0)
    [Ui.Native]::keybd_event([byte](0x30 + $Digit), 0, 0, 0)
    [Ui.Native]::keybd_event([byte](0x30 + $Digit), 0, 2, 0)
    [Ui.Native]::keybd_event(0x11, 0, 2, 0)
    Start-Sleep -Milliseconds 800
}

function Send-Key([byte]$Vk) {
    Focus-ClipApp
    [Ui.Native]::keybd_event($Vk, 0, 0, 0); [Ui.Native]::keybd_event($Vk, 0, 2, 0)
}

# Client point relative to the window's top-left corner.
function Click-At([int]$X, [int]$Y, [switch]$Double) {
    $r = New-Object Ui.Native+RECT; [Ui.Native]::GetWindowRect($script:App.MainWindowHandle, [ref]$r) | Out-Null
    [Ui.Native]::SetCursorPos($r.L + $X, $r.T + $Y) | Out-Null
    Start-Sleep -Milliseconds 80
    $n = if ($Double) { 2 } else { 1 }
    for ($i = 0; $i -lt $n; $i++) {
        [Ui.Native]::mouse_event(0x2, 0, 0, 0, 0); [Ui.Native]::mouse_event(0x4, 0, 0, 0, 0)
        Start-Sleep -Milliseconds 60
    }
}

# Drag from (X1,Y) to (X2,Y) in Steps moves of StepMs each.
function Drag-At([int]$X1, [int]$X2, [int]$Y, [int]$Steps = 120, [int]$StepMs = 8) {
    $r = New-Object Ui.Native+RECT; [Ui.Native]::GetWindowRect($script:App.MainWindowHandle, [ref]$r) | Out-Null
    [Ui.Native]::SetCursorPos($r.L + $X1, $r.T + $Y) | Out-Null
    [Ui.Native]::mouse_event(0x2, 0, 0, 0, 0)
    for ($i = 1; $i -le $Steps; $i++) {
        $x = $X1 + [int](($X2 - $X1) * $i / $Steps)
        [Ui.Native]::SetCursorPos($r.L + $x, $r.T + $Y) | Out-Null
        Start-Sleep -Milliseconds $StepMs
    }
    [Ui.Native]::mouse_event(0x4, 0, 0, 0, 0)
}

# Mouse wheel at a window point: $Ticks events of $Delta (120 = one notch,
# small deltas emulate a precision touchpad), $IntervalMs apart, then the
# same back up.
function Send-Wheel([int]$X, [int]$Y, [int]$Ticks, [int]$Delta = 120, [int]$IntervalMs = 16) {
    $r = New-Object Ui.Native+RECT; [Ui.Native]::GetWindowRect($script:App.MainWindowHandle, [ref]$r) | Out-Null
    [Ui.Native]::SetCursorPos($r.L + $X, $r.T + $Y) | Out-Null
    foreach ($sign in -1, 1) {
        for ($i = 0; $i -lt $Ticks; $i++) {
            [Ui.Native]::mouse_event(0x800, 0, 0, $sign * $Delta, 0)
            Start-Sleep -Milliseconds $IntervalMs
        }
    }
}

function Save-Shot([string]$Path) {
    $r = New-Object Ui.Native+RECT; [Ui.Native]::GetWindowRect($script:App.MainWindowHandle, [ref]$r) | Out-Null
    $bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
    $bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose()
}

function Get-FrameMark { (Get-Content $script:Log -ErrorAction SilentlyContinue | Measure-Object -Line).Lines }

# Frame-time statistics of the frames logged since $Mark over $Seconds.
function Get-FrameStats([int]$Mark, [double]$Seconds, [string]$Label) {
    $lines = Get-Content $script:Log | Select-Object -Skip $Mark
    $ms = foreach ($l in $lines) {
        if ($l -match 'frame duration: ([\d.]+)(µs|us|ms|s)') {
            $v = [double]$Matches[1]
            switch ($Matches[2]) { 'ms' { $v } 's' { $v * 1000 } default { $v / 1000 } }
        }
    }
    $ms = @($ms | Sort-Object)
    $n = $ms.Count
    if ($n -eq 0) { return [pscustomobject]@{ Scenario = $Label; Frames = 0 } }
    $p = { param($q) $ms[[Math]::Min($n - 1, [int][Math]::Floor($q * $n))] }
    [pscustomobject]@{
        Scenario  = $Label
        Frames    = $n
        'Frames/s' = [Math]::Round($n / $Seconds, 1)
        MedianMs  = [Math]::Round((& $p 0.5), 2)
        P95Ms     = [Math]::Round((& $p 0.95), 2)
        P99Ms     = [Math]::Round((& $p 0.99), 2)
        MaxMs     = [Math]::Round($ms[$n - 1], 2)
        'Over16.7' = ($ms | Where-Object { $_ -gt 16.7 }).Count
    }
}
