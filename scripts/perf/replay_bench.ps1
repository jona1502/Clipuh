# Replay GPU/CPU benchmark. Runs the headless `replay_smoke` example against
# the primary monitor and samples GPU engine counters while it buffers.
#
#   pwsh scripts/perf/replay_bench.ps1 -Exe target/release/examples/replay_smoke.exe -Workload anim
#
# -Workload anim   : a 240 fps WPF animation keeps the desktop changing at
#                    the display refresh rate (game-like frame delivery).
# -Workload refresh: an Edge page (isolated profile) animating with
#                    requestAnimationFrame at the full display refresh rate.
# -Workload static : nothing moves; WGC delivers almost no frames.
# Without -Exe only the workload is sampled (the "replay off" baseline).
param(
    [string]$Exe = '',
    [ValidateSet('anim', 'refresh', 'static')][string]$Workload = 'anim',
    [int]$Fps = 60,
    [ValidateSet('performance', 'balanced', 'quality')][string]$Profile = 'balanced',
    [int]$Warmup = 10,
    [int]$Seconds = 20,
    [string]$Label = ''
)
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$total = $Warmup + $Seconds + 5
$anim = $null
$profileDir = $null
if ($Workload -eq 'anim') {
    $anim = Start-Process pwsh -ArgumentList '-NoProfile', '-File', "$here/animate.ps1", '-Seconds', ($total + 70) -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 3
}
if ($Workload -eq 'refresh') {
    # Isolated throwaway Edge profile; never touches the user's browsers.
    $profileDir = Join-Path ([IO.Path]::GetTempPath()) "clipuh-perf-edge-$PID"
    $edge = "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe"
    $page = 'file:///' + ((Resolve-Path "$here/anim.html").Path -replace '\\', '/')
    $anim = Start-Process $edge -ArgumentList "--user-data-dir=$profileDir", '--no-first-run', '--window-size=1000,700', '--window-position=40,40', "--app=$page" -PassThru
    Start-Sleep -Seconds 4
}
$replay = $null
$log = Join-Path ([IO.Path]::GetTempPath()) "replay-bench-$PID.log"
if ($Exe) {
    $replay = Start-Process $Exe -ArgumentList "run=$($total)", 'duration=30', "fps=$Fps", 'desktop=1', 'mic=0', "profile=$Profile" -PassThru -NoNewWindow -RedirectStandardOutput $log
}
Start-Sleep -Seconds $Warmup
& "$here/gpu_sample.ps1" -ProcessId ($replay ? $replay.Id : 0) -Seconds $Seconds -Label "$Label workload=$Workload fps=$Fps profile=$Profile replay=$([bool]$Exe)"
if ($replay) {
    $replay.WaitForExit(180000) | Out-Null
    "--- replay_smoke status lines (last 4) ---"
    Get-Content $log | Select-String 'ReplayStatus' | Select-Object -Last 4 | ForEach-Object { $_.Line }
    Get-Content $log | Select-String 'usage|clip |diagnostics|ReplaySaved|outcomes' | ForEach-Object { $_.Line }
    "log: $log"
}
if ($anim -and -not $anim.HasExited) { Stop-Process -Id $anim.Id -ErrorAction SilentlyContinue }
if ($profileDir) {
    Get-CimInstance Win32_Process -Filter "Name = 'msedge.exe'" |
        Where-Object { $_.CommandLine -like "*$profileDir*" } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 1
    Remove-Item -Recurse -Force $profileDir -ErrorAction SilentlyContinue
}
