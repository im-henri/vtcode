param(
    [Parameter(Mandatory = $true)]
    [string]$Binary,
    [string]$OutputDirectory = 'target/vtcomplete-learning-memory'
)

# Supplementary whole-process measurement. Run separately from the authoritative
# latency run: this poller can perturb scheduling and is not an allocator trace.
$ErrorActionPreference = 'Stop'
$taskBinary = (Resolve-Path -LiteralPath $Binary).Path
$taskOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $taskOutput -Force | Out-Null
$taskStdout = Join-Path $taskOutput 'stdout.log'
$taskStderr = Join-Path $taskOutput 'stderr.log'
$taskOldLatencyReport = $env:VTCODE_LEARNING_LATENCY_REPORT
$taskLatencyReport = Join-Path $taskOutput 'sampled-run-latency.json'
$env:VTCODE_LEARNING_LATENCY_REPORT = $taskLatencyReport

try {
    $taskProcess = Start-Process -FilePath $taskBinary -WindowStyle Hidden -PassThru `
        -ArgumentList @('benchmark_learning_replay_worker_latency', '--ignored', '--nocapture', '--test-threads=1') `
        -RedirectStandardOutput $taskStdout -RedirectStandardError $taskStderr
    $taskSamples = [System.Collections.Generic.List[object]]::new()
    $taskClock = [System.Diagnostics.Stopwatch]::StartNew()
    $taskMaxPrivate = 0L
    $taskMaxWorking = 0L
    $taskOsPeakWorking = 0L
    while (-not $taskProcess.HasExited) {
        try {
            $taskProcess.Refresh()
            $taskPrivate = $taskProcess.PrivateMemorySize64
            $taskWorking = $taskProcess.WorkingSet64
            $taskPeakWorking = $taskProcess.PeakWorkingSet64
            $taskMaxPrivate = [Math]::Max($taskMaxPrivate, $taskPrivate)
            $taskMaxWorking = [Math]::Max($taskMaxWorking, $taskWorking)
            $taskOsPeakWorking = [Math]::Max($taskOsPeakWorking, $taskPeakWorking)
            $taskSamples.Add([pscustomobject]@{
                elapsedMs = $taskClock.Elapsed.TotalMilliseconds
                privateBytes = $taskPrivate
                workingSetBytes = $taskWorking
            })
        } catch [System.InvalidOperationException] {
            if (-not $taskProcess.HasExited) { throw }
        }
        Start-Sleep -Milliseconds 5
    }
    $taskProcess.WaitForExit()
    $taskClock.Stop()
    $taskReport = [ordered]@{
        binary = $taskBinary
        binarySha256 = (Get-FileHash -LiteralPath $taskBinary -Algorithm SHA256).Hash
        exitCode = $taskProcess.ExitCode
        elapsedMs = $taskClock.Elapsed.TotalMilliseconds
        samplingIntervalRequestedMs = 5
        sampledPeakPrivateBytes = $taskMaxPrivate
        sampledPeakWorkingSetBytes = $taskMaxWorking
        osPeakWorkingSetBytesObserved = $taskOsPeakWorking
        sampleCount = $taskSamples.Count
        scope = 'Shared test process: baseline/new core, real worker queue, fixtures, background indexing and runtime together; not incremental model heap.'
        limitations = 'Private bytes is sampled and may miss short peaks. Working set includes resident shared pages. Sampling may perturb this run latency; use the separate unsampled run for acceptance latency.'
        samples = $taskSamples
    }
    $taskReportPath = Join-Path $taskOutput 'process-memory.json'
    $taskReport | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $taskReportPath -Encoding utf8
    Write-Output $taskReportPath
    if ($taskProcess.ExitCode -ne 0) { throw "Memory workload exited $($taskProcess.ExitCode); inspect $taskStdout and $taskStderr" }
} finally {
    $env:VTCODE_LEARNING_LATENCY_REPORT = $taskOldLatencyReport
}
