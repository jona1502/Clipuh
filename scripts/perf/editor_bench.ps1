# Editor playhead benchmark (black box). Searches the Library for -Clip, opens it in the
# Editor, starts playback and samples a 1 px screen strip through the
# timeline to locate the white playhead line over time, while GPUI logs frame
# durations. Also drags across the video track (scrub) for the scrubbing scenario.
#
#   pwsh scripts/perf/editor_bench.ps1 -Exe target/release/clipper-app.exe -Label after
#
# Layout coordinates assume the 1600x1000 window set by ui_driver.ps1 and a
# clip with three audio tracks; adjust -StripY for other clips.
param([string]$Exe, [string]$Label = '', [int]$Seconds = 8, [int]$StripY = 905,
      [int]$X0 = 500, [int]$X1 = 1520, [int]$ZoomClicks = 0,
      [string]$Clip = '00-46-56')
. "$PSScriptRoot/ui_driver.ps1"
$log = Join-Path ([IO.Path]::GetTempPath()) "clipuh-editor-$Label-$PID.log"
Start-ClipApp $Exe $log
Send-Page 2; Start-Sleep 3
Click-At 700 222; Start-Sleep 2       # neutral spot (title bar clicks can maximize)
Click-At 1000 180; Start-Sleep 1           # search field
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.SendKeys]::SendWait($Clip); Start-Sleep 2
Click-At 372 470; Start-Sleep 4           # scissors of the first (only) card -> Editor
Save-Shot (Join-Path ([IO.Path]::GetTempPath()) "clipuh-editor-open-$Label.png")
for ($i = 0; $i -lt $ZoomClicks; $i++) { Click-At 1551 657; Start-Sleep -Milliseconds 300 }

function Sample-Playhead([double]$Seconds) {
    $r = New-Object Ui.Native+RECT; [Ui.Native]::GetWindowRect($script:App.MainWindowHandle, [ref]$r) | Out-Null
    $w = $X1 - $X0
    $bmp = New-Object System.Drawing.Bitmap $w, 1
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $out = New-Object System.Collections.Generic.List[object]
    while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
        $g.CopyFromScreen($r.L + $X0, $r.T + $StripY, 0, 0, $bmp.Size)
        $t = $sw.Elapsed.TotalMilliseconds
        $best = -1; $sum = 0.0; $wsum = 0.0
        for ($x = 0; $x -lt $w; $x++) {
            $c = $bmp.GetPixel($x, 0)
            $v = [Math]::Min($c.R, [Math]::Min($c.G, $c.B))   # whiteness
            if ($v -gt 120) { $sum += $x * $v; $wsum += $v }
        }
        if ($wsum -gt 0) { $out.Add([pscustomobject]@{ T = $t; X = $sum / $wsum }) }
    }
    $g.Dispose(); $bmp.Dispose()
    $out
}

function Summarize([object[]]$s, [string]$Name) {
    $moves = @(); $back = 0; $last = $null; $lastMoveT = $null; $gaps = @()
    foreach ($p in $s) {
        if ($null -ne $last -and [Math]::Abs($p.X - $last.X) -gt 0.05) {
            if ($p.X -lt $last.X) { $back++ }
            $moves += [Math]::Abs($p.X - $last.X)
            if ($null -ne $lastMoveT) { $gaps += $p.T - $lastMoveT }
            $lastMoveT = $p.T
        }
        $last = $p
    }
    $span = if ($s.Count -gt 1) { ($s[-1].T - $s[0].T) / 1000 } else { 1 }
    $gs = @($gaps | Sort-Object); $n = $gs.Count
    [pscustomobject]@{
        Scenario        = $Name
        Samples         = $s.Count
        'Samples/s'     = [Math]::Round($s.Count / $span, 1)
        'Moves/s'       = [Math]::Round($moves.Count / $span, 1)
        'MedianStepPx'  = if ($moves) { [Math]::Round(($moves | Sort-Object)[[int]($moves.Count / 2)], 2) } else { 0 }
        'MedianGapMs'   = if ($n) { [Math]::Round($gs[[int]($n / 2)], 1) } else { 0 }
        'P95GapMs'      = if ($n) { [Math]::Round($gs[[Math]::Min($n - 1, [int]($n * 0.95))], 1) } else { 0 }
        'MaxGapMs'      = if ($n) { [Math]::Round($gs[-1], 1) } else { 0 }
        Backward        = $back
        'Px/s'          = if ($s.Count -gt 1) { [Math]::Round(($s[-1].X - $s[0].X) / $span, 1) } else { 0 }
    }
}

# Playback.
Send-Key 0x20                             # Space: play
Start-Sleep -Milliseconds 500
$m = Get-FrameMark
$play = Sample-Playhead $Seconds
$frames = Get-FrameStats $m $Seconds 'Editor playback'
Send-Key 0x20                             # pause
Start-Sleep 1

# Scrub: drag across the video track.
$m = Get-FrameMark
$t = [Diagnostics.Stopwatch]::StartNew()
Drag-At 600 1400 740 -Steps 220 -StepMs 8
$scrub = Get-FrameStats $m $t.Elapsed.TotalSeconds 'Editor scrub (drag)'
Stop-ClipApp

"## $Label"
Summarize $play 'playhead during playback' | Format-List | Out-String
$frames, $scrub | Format-Table Scenario, Frames, 'Frames/s', MedianMs, P95Ms, P99Ms, MaxMs, 'Over16.7' -AutoSize | Out-String -Width 200
