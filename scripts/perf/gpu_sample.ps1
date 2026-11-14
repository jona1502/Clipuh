# Samples Windows "GPU Engine" performance counters and reports average
# utilization per engine type for one process, for dwm.exe and for the whole
# system, plus the process' CPU usage and working set.
#
#   pwsh scripts/perf/gpu_sample.ps1 -ProcessId 1234 -Seconds 20
#
# Per-process numbers sum the engine instances of the same type (as Task
# Manager's engine graphs do); the system column sums all processes.
param(
    [int]$ProcessId = 0,
    [int]$Seconds = 20,
    [string]$Label = ''
)

$dwm = (Get-Process dwm -ErrorAction SilentlyContinue | Select-Object -First 1).Id
$cores = [Environment]::ProcessorCount
$samples = @()
$cpuStart = $null
$wsMax = 0
if ($ProcessId) {
    $proc = Get-Process -Id $ProcessId -ErrorAction Stop
    $cpuStart = $proc.TotalProcessorTime
}
$started = Get-Date

for ($i = 0; $i -lt $Seconds; $i++) {
    $set = Get-Counter '\GPU Engine(*)\Utilization Percentage' -ErrorAction SilentlyContinue
    if (-not $set) { continue }
    $row = @{}
    foreach ($s in $set.CounterSamples) {
        if ($s.InstanceName -notmatch '^pid_(\d+)_.*engtype_(.+)$') { continue }
        $owner = [int]$Matches[1]
        $engine = $Matches[2]
        $value = [double]$s.CookedValue
        $scopes = @('system')
        if ($ProcessId -and $owner -eq $ProcessId) { $scopes += 'process' }
        if ($owner -eq $dwm) { $scopes += 'dwm' }
        foreach ($scope in $scopes) {
            $key = "$scope|$engine"
            $row[$key] = ($row[$key] ?? 0) + $value
        }
    }
    $samples += $row
    if ($ProcessId) {
        $p = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
        if ($p) { $wsMax = [Math]::Max($wsMax, $p.WorkingSet64) }
    }
}

$keys = $samples | ForEach-Object { $_.Keys } | Sort-Object -Unique
$result = foreach ($key in $keys) {
    $scope, $engine = $key -split '\|', 2
    $values = $samples | ForEach-Object { $_[$key] ?? 0 }
    $stats = $values | Measure-Object -Average -Maximum
    [pscustomobject]@{
        Scope   = $scope
        Engine  = $engine
        AvgPct  = [Math]::Round($stats.Average, 2)
        MaxPct  = [Math]::Round($stats.Maximum, 2)
    }
}
"## $Label ($($samples.Count) samples)"
$result | Where-Object { $_.MaxPct -gt 0.05 } | Sort-Object Scope, Engine | Format-Table -AutoSize | Out-String
if ($ProcessId) {
    $p = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if ($p) {
        $elapsed = ((Get-Date) - $started).TotalSeconds
        $cpu = ($p.TotalProcessorTime - $cpuStart).TotalSeconds / $elapsed / $cores * 100
        "process CPU avg: {0:N2} % of all cores ({1:N2} % of one core)" -f $cpu, ($cpu * $cores)
        "process working set max: {0:N1} MiB" -f ($wsMax / 1MB)
    }
}
