# Process CPU and GPU engine usage of clipuh.exe per UI scenario:
# idle (Settings, no input), continuous Settings wheel scrolling, Editor
# playback. Uses gpu_sample.ps1 (Windows GPU Engine counters).
#
#   pwsh scripts/perf/ui_resources.ps1 -Exe target/release/clipuh.exe -Label after
param([string]$Exe, [string]$Label = '', [string]$Clip = '00-46-56', [int]$Seconds = 8)
. "$PSScriptRoot/ui_driver.ps1"
$sampler = "$PSScriptRoot/gpu_sample.ps1"
$log = Join-Path ([IO.Path]::GetTempPath()) "clipuh-res-$Label-$PID.log"
Start-ClipApp $Exe $log
$id = $script:App.Id

function Measure-While([string]$Name, [scriptblock]$Action) {
    $out = Join-Path ([IO.Path]::GetTempPath()) "clipuh-res-sample-$PID.txt"
    $sampling = Start-Process pwsh -PassThru -WindowStyle Hidden -RedirectStandardOutput $out -ArgumentList `
        '-NoProfile', '-File', $sampler, '-ProcessId', $id, '-Seconds', $Seconds, '-Label', "`"$Label $Name`""
    & $Action
    $sampling.WaitForExit()
    Get-Content $out | Where-Object { $_ -match '^##|^process|^dwm|CPU' }
}

Send-Page 4; Start-Sleep 2
Measure-While 'idle (Settings)' { Start-Sleep ($Seconds + 2) }
Measure-While 'Settings wheel scrolling' {
    $end = (Get-Date).AddSeconds($Seconds + 1)
    while ((Get-Date) -lt $end) { Send-Wheel 1000 700 -Ticks 10 -Delta 120 -IntervalMs 40 }
}
Send-Page 2; Start-Sleep 3
Click-At 700 222; Start-Sleep 2       # neutral spot (title bar clicks can maximize)
Click-At 1000 180; Start-Sleep 1
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.SendKeys]::SendWait($Clip); Start-Sleep 2
Click-At 372 470
Measure-While 'Editor opening (thumbnails, waveforms)' { Start-Sleep ($Seconds + 2) }
Start-Sleep 10
Measure-While 'Editor paused (settled)' { Start-Sleep ($Seconds + 2) }
Send-Key 0x20
Measure-While 'Editor playback' { Start-Sleep ($Seconds + 2) }
Send-Key 0x20
Stop-ClipApp
