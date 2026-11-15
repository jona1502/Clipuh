# UI smoothness benchmark: frame-time statistics per scenario.
#
#   pwsh scripts/perf/ui_bench.ps1 -Exe target/release/clipuh.exe -Label after
#
# Uses the user's normal config (whatever replay state it has). Scenarios:
# idle on Library, Settings wheel scroll, Settings touchpad-style scroll,
# Library wheel scroll, page navigation. Editor scenarios need a clip open and
# are driven separately (see docs/performance/ui-smoothness.md).
param([string]$Exe, [string]$Label = '', [int]$Runs = 1)
. "$PSScriptRoot/ui_driver.ps1"
$log = Join-Path ([IO.Path]::GetTempPath()) "clipuh-ui-$Label-$PID.log"
Start-ClipApp $Exe $log
$results = @()
for ($run = 1; $run -le $Runs; $run++) {
    Send-Page 2
    Start-Sleep 2
    $m = Get-FrameMark; Start-Sleep 5
    $results += Get-FrameStats $m 5 "idle (Library)"

    Send-Page 4
    Start-Sleep 1
    $t = [Diagnostics.Stopwatch]::StartNew(); $m = Get-FrameMark
    Send-Wheel 1000 700 -Ticks 40 -Delta 120 -IntervalMs 16
    Start-Sleep -Milliseconds 300
    $results += Get-FrameStats $m $t.Elapsed.TotalSeconds "Settings wheel"

    $t = [Diagnostics.Stopwatch]::StartNew(); $m = Get-FrameMark
    Send-Wheel 1000 700 -Ticks 150 -Delta 20 -IntervalMs 8
    Start-Sleep -Milliseconds 300
    $results += Get-FrameStats $m $t.Elapsed.TotalSeconds "Settings touchpad"

    Send-Page 2
    Start-Sleep 1
    $t = [Diagnostics.Stopwatch]::StartNew(); $m = Get-FrameMark
    Send-Wheel 900 600 -Ticks 40 -Delta 120 -IntervalMs 16
    Start-Sleep -Milliseconds 300
    $results += Get-FrameStats $m $t.Elapsed.TotalSeconds "Library wheel"

    $t = [Diagnostics.Stopwatch]::StartNew(); $m = Get-FrameMark
    foreach ($p in 1, 4, 2, 4, 1, 2) { Send-Page $p }
    $results += Get-FrameStats $m $t.Elapsed.TotalSeconds "Navigation"
}
Stop-ClipApp
$results | ForEach-Object { $_ | Add-Member -NotePropertyName Build -NotePropertyValue $Label -PassThru } |
    Format-Table Build, Scenario, Frames, 'Frames/s', MedianMs, P95Ms, P99Ms, MaxMs, 'Over16.7' -AutoSize | Out-String -Width 200
