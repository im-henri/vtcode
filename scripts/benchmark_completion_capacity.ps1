param(
    [Parameter(Mandatory=$true)][string]$Binary,
    [Parameter(Mandatory=$true)][string]$Corpus,
    [string]$OutputDirectory = 'target/completion-capacity-measurement',
    [ValidatePattern('^[a-zA-Z0-9_:]+$')][string]$TestFilter = 'benchmark_vtcode_index_capacity',
    [switch]$SampleMemory
)

$ErrorActionPreference = 'Stop'
$taskBinary = (Resolve-Path -LiteralPath $Binary).Path
$taskCorpus = (Resolve-Path -LiteralPath $Corpus).Path
$taskOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $taskOutput -Force | Out-Null
$taskOldCorpus = $env:VTCODE_CAPACITY_SOURCE
$taskOldReport = $env:VTCODE_CAPACITY_REPORT
$env:VTCODE_CAPACITY_SOURCE = $taskCorpus
$env:VTCODE_CAPACITY_REPORT = Join-Path $taskOutput 'capacity.json'
$taskArguments = @($TestFilter,'--ignored','--nocapture','--test-threads=1')
try {
    if (-not $SampleMemory) {
        & $taskBinary @taskArguments *> (Join-Path $taskOutput 'run.log')
        if ($LASTEXITCODE -ne 0) { throw "Benchmark failed; inspect $taskOutput/run.log" }
        Write-Output $env:VTCODE_CAPACITY_REPORT
        return
    }
    # Separate sampled run: process sampling must not establish latency claims.
    $taskProcess = Start-Process -FilePath $taskBinary -ArgumentList $taskArguments `
        -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $taskOutput 'stdout.log') `
        -RedirectStandardError (Join-Path $taskOutput 'stderr.log')
    $taskClock = [System.Diagnostics.Stopwatch]::StartNew()
    $taskPeakPrivate = 0L
    $taskPeakWorking = 0L
    $taskSamples = 0
    while (-not $taskProcess.HasExited) {
        try {
            $taskProcess.Refresh()
            $taskPeakPrivate = [Math]::Max($taskPeakPrivate,$taskProcess.PrivateMemorySize64)
            $taskPeakWorking = [Math]::Max($taskPeakWorking,$taskProcess.PeakWorkingSet64)
            $taskSamples++
        } catch [System.InvalidOperationException] {
            if (-not $taskProcess.HasExited) { throw }
        }
        Start-Sleep -Milliseconds 5
    }
    $taskProcess.WaitForExit()
    $taskClock.Stop()
    $taskResult = [ordered]@{
        binarySha256 = (Get-FileHash -LiteralPath $taskBinary -Algorithm SHA256).Hash
        test = $TestFilter
        exitCode = $taskProcess.ExitCode
        elapsedMs = $taskClock.Elapsed.TotalMilliseconds
        sampledPeakPrivateBytes = $taskPeakPrivate
        peakWorkingSetBytesObserved = $taskPeakWorking
        requestedSamplingIntervalMs = 5
        samples = $taskSamples
        referenceProcessor = $env:PROCESSOR_IDENTIFIER
        logicalProcessors = $env:NUMBER_OF_PROCESSORS
        scope = 'Whole benchmark process, including runtime, input buffers, indexing scratch and allocator retention; not incremental retained model heap.'
        limitations = 'Sampling may miss short peaks and perturb timings. Use the separate unsampled run for latency. The 10k workload retains its source corpus as test input.'
    }
    $taskResult | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $taskOutput 'process-memory.json') -Encoding utf8
    if ($taskProcess.ExitCode -ne 0) { throw "Sampled benchmark failed; inspect $taskOutput" }
    Write-Output (Join-Path $taskOutput 'process-memory.json')
} finally {
    $env:VTCODE_CAPACITY_SOURCE = $taskOldCorpus
    $env:VTCODE_CAPACITY_REPORT = $taskOldReport
}
