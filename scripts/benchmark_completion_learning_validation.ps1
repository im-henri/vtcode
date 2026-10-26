param(
    [Parameter(Mandatory = $true)][string]$Binary,
    [Parameter(Mandatory = $true)][string]$Fixtures,
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [string]$VerifierBinary,
    [switch]$IncludeCold
)

# Requires an already-copied, frozen manifest. No discovery or source selection.
# This script measures quality; latency and memory have separate workloads.
$ErrorActionPreference = 'Stop'
$taskBinary = (Resolve-Path -LiteralPath $Binary).Path
$taskFixtures = (Resolve-Path -LiteralPath $Fixtures).Path
if (-not $VerifierBinary) { $VerifierBinary = $taskBinary }
$taskVerifier = (Resolve-Path -LiteralPath $VerifierBinary).Path
$taskOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $taskOutput) { throw "Refusing existing output path: $taskOutput" }
New-Item -ItemType Directory -Path $taskOutput | Out-Null
$taskRunPath = Join-Path $taskOutput 'run.json'
$taskEnvNames = @('VTCODE_LEARNING_RENAME_MODE', 'VTCODE_LEARNING_RENAME_FIXTURES',
    'VTCODE_LEARNING_REPLAY_WARM_INDEX', 'VTCODE_LEARNING_RENAME_MANIFEST',
    'VTCODE_LEARNING_APPEND_VALIDATION_REPORT', 'VTCODE_LEARNING_RENAME_REPORT',
    'VTCODE_LEARNING_VALIDATION_SUMMARY')
$taskPrevious = @{}
foreach ($taskName in $taskEnvNames) { $taskPrevious[$taskName] = [Environment]::GetEnvironmentVariable($taskName, 'Process') }
$taskInputs = [System.Collections.Generic.List[object]]::new()
$taskTestLists = @{}
$taskRun = [ordered]@{
    status = 'running'; startedUtc = [DateTime]::UtcNow.ToString('o'); mode = 'Full'
    binary = $taskBinary; verifierBinary = $taskVerifier; fixtures = $taskFixtures
    fixtureProvenance = $null
    includeCold = [bool]$IncludeCold; qualityGatePassed = $null
    inputs = $taskInputs; caseManifests = [System.Collections.Generic.List[object]]::new()
    commands = [System.Collections.Generic.List[object]]::new()
    scope = 'Warm native primary cohort; cold/Unknown paired results are separate and never add primary events. Resource gates are separate.'
}
function Save-ValidationRun {
    $taskRun | ConvertTo-Json -Depth 14 | Set-Content -LiteralPath $taskRunPath -Encoding utf8
}
function Add-FrozenInput([string]$Path, [string]$Kind, [string]$Expected = '') {
    $taskHash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ($Expected -and $taskHash -ne $Expected) { throw "Frozen $Kind hash mismatch: $Path" }
    $taskInputs.Add([pscustomobject]@{ path = $Path; kind = $Kind; sha256 = $taskHash })
}
function Assert-FrozenInputs {
    foreach ($taskInput in $taskInputs) {
        if ((Get-FileHash -LiteralPath $taskInput.path -Algorithm SHA256).Hash -ne $taskInput.sha256) {
            throw "Frozen input changed during validation: $($taskInput.path)"
        }
    }
}
function Resolve-ValidationTestName([string]$Executable, [string]$Name) {
    # Resolve the exact name so pre-VTComplete baseline binaries remain usable.
    if (-not $taskTestLists.ContainsKey($Executable)) {
        $taskListing = @(& $Executable --list)
        if ($LASTEXITCODE -ne 0) { throw "Cannot list tests in $Executable" }
        $taskTestLists[$Executable] = @($taskListing | Where-Object { $_ -match ': test$' } |
            ForEach-Object { $_ -replace ': test$', '' })
    }
    $taskMatches = @($taskTestLists[$Executable] | Where-Object {
        $_.EndsWith("::learning_replay_tests::source_rename_tests::$Name", [StringComparison]::Ordinal)
    })
    if ($taskMatches.Count -ne 1) { throw "Expected exactly one validation test named $Name in $Executable" }
    return $taskMatches[0]
}
function Invoke-ValidationTest([string]$Executable, [string]$Name, [string]$Label, [bool]$Ignored) {
    Assert-FrozenInputs
    $taskArguments = @((Resolve-ValidationTestName $Executable $Name), '--exact', '--nocapture', '--test-threads=1')
    if ($Ignored) { $taskArguments += '--ignored' }
    $taskStdout = Join-Path $taskOutput "$Label.stdout.log"
    $taskStderr = Join-Path $taskOutput "$Label.stderr.log"
    $taskEntry = [ordered]@{
        binary = $Executable; arguments = $taskArguments; stdout = $taskStdout; stderr = $taskStderr
        warmIndex = $env:VTCODE_LEARNING_REPLAY_WARM_INDEX; caseManifest = $env:VTCODE_LEARNING_RENAME_MANIFEST
        appendReport = $env:VTCODE_LEARNING_APPEND_VALIDATION_REPORT; renameReport = $env:VTCODE_LEARNING_RENAME_REPORT
        summary = $env:VTCODE_LEARNING_VALIDATION_SUMMARY; startedUtc = [DateTime]::UtcNow.ToString('o')
    }
    $taskRun.commands.Add($taskEntry)
    Save-ValidationRun
    $taskProcess = Start-Process -FilePath $Executable -WindowStyle Hidden -PassThru `
        -ArgumentList $taskArguments -RedirectStandardOutput $taskStdout -RedirectStandardError $taskStderr
    $taskProcessHandle = $taskProcess.Handle # Retain a handle so ExitCode survives fast process exit.
    $taskProcess.WaitForExit()
    $taskEntry.exitCode = $taskProcess.ExitCode
    $taskText = [System.IO.File]::ReadAllText($taskStdout)
    $taskEntry.finishedUtc = [DateTime]::UtcNow.ToString('o')
    $taskEntry.exactlyOnePassed = $taskText -match '(?m)^running 1 test\r?$' -and
        $taskText -match '(?m)^test result: ok\. 1 passed; 0 failed; 0 ignored;'
    Save-ValidationRun
    if ($taskProcess.ExitCode -ne 0) { throw "Test $Name exited $($taskProcess.ExitCode); retained logs: $taskStdout, $taskStderr" }
    if (-not $taskEntry.exactlyOnePassed) { throw "Test $Name did not run exactly one passing test; inspect $taskStdout" }
    Assert-FrozenInputs
}
function Invoke-ValidationCohort([string]$Label, [bool]$Warm) {
    $env:VTCODE_LEARNING_REPLAY_WARM_INDEX = if ($Warm) { '1' } else { '0' }
    $env:VTCODE_LEARNING_RENAME_MANIFEST = Join-Path $taskOutput "$Label-cases.json"
    $env:VTCODE_LEARNING_APPEND_VALIDATION_REPORT = Join-Path $taskOutput "$Label-append.json"
    $env:VTCODE_LEARNING_RENAME_REPORT = Join-Path $taskOutput "$Label-rename.json"
    $env:VTCODE_LEARNING_VALIDATION_SUMMARY = $null
    Invoke-ValidationTest $taskBinary 'source_rename_selection_manifest' "$Label-selection" $false
    # Freeze the case manifest and persist its hash BEFORE either predictor runs.
    Add-FrozenInput $env:VTCODE_LEARNING_RENAME_MANIFEST "$Label-case-manifest"
    $taskRun.caseManifests.Add([pscustomobject]@{
        cohort = $Label; path = $env:VTCODE_LEARNING_RENAME_MANIFEST
        sha256 = (Get-FileHash -LiteralPath $env:VTCODE_LEARNING_RENAME_MANIFEST -Algorithm SHA256).Hash
    })
    Save-ValidationRun
    Invoke-ValidationTest $taskBinary 'learning_runtime_source_append_validation' "$Label-append" $true
    Invoke-ValidationTest $taskBinary 'learning_source_rename_replay_quality_comparison' "$Label-rename" $false
    Add-FrozenInput $env:VTCODE_LEARNING_APPEND_VALIDATION_REPORT "$Label-append-report"
    Add-FrozenInput $env:VTCODE_LEARNING_RENAME_REPORT "$Label-rename-report"
}

try {
    Add-FrozenInput $taskBinary 'prediction-binary'
    Add-FrozenInput $taskVerifier 'verification-binary'
    Add-FrozenInput $taskFixtures 'fixture-manifest'
    $taskManifest = [System.IO.File]::ReadAllText($taskFixtures) | ConvertFrom-Json
    $taskRun.fixtureProvenance = $taskManifest.provenance
    if ($taskManifest.protocol -ne 'completion-final-validation-v1' -or $taskManifest.sources.Count -ne 2) {
        throw 'Fixture manifest must use completion-final-validation-v1 with exactly two copied sources'
    }
    foreach ($taskSource in $taskManifest.sources) {
        $taskPath = if ([System.IO.Path]::IsPathRooted($taskSource.path)) { $taskSource.path } else {
            Join-Path ([System.IO.Path]::GetDirectoryName($taskFixtures)) $taskSource.path
        }
        $taskPath = (Resolve-Path -LiteralPath $taskPath).Path
        Add-FrozenInput $taskPath "copied-source:$($taskSource.label)" $taskSource.sha256
    }
    $env:VTCODE_LEARNING_RENAME_MODE = 'Full'
    $env:VTCODE_LEARNING_RENAME_FIXTURES = $taskFixtures
    Save-ValidationRun
    Invoke-ValidationCohort 'warm' $true
    $env:VTCODE_LEARNING_VALIDATION_SUMMARY = Join-Path $taskOutput 'warm-summary.json'
    Invoke-ValidationTest $taskVerifier 'learning_runtime_validation_primary_summary' 'warm-summary' $true
    $taskSummary = [System.IO.File]::ReadAllText($env:VTCODE_LEARNING_VALIDATION_SUMMARY) | ConvertFrom-Json
    $taskRun.qualityGatePassed = ($taskSummary.qualityGatePassed -is [bool]) -and $taskSummary.qualityGatePassed
    if ($taskSummary.mode -ne 'Full' -or $taskSummary.warmIndex -ne $true) { throw 'Summary is not the frozen Full warm candidate' }
    Add-FrozenInput $env:VTCODE_LEARNING_VALIDATION_SUMMARY 'warm-primary-summary'
    if ($IncludeCold) { Invoke-ValidationCohort 'cold' $false }
    Assert-FrozenInputs
    if (-not $taskRun.qualityGatePassed) { throw 'Primary quality gate failed; all reports and logs retained' }
    $taskRun.status = 'quality-gate-passed; resource gates not assessed'
} catch {
    $taskRun.status = 'failed'
    $taskRun.failure = $_.ToString()
    throw
} finally {
    $taskRun.finishedUtc = [DateTime]::UtcNow.ToString('o')
    try { Save-ValidationRun } finally {
        foreach ($taskName in $taskEnvNames) {
            if ($null -eq $taskPrevious[$taskName]) {
                if (Test-Path -LiteralPath "Env:$taskName") { Remove-Item -LiteralPath "Env:$taskName" }
            }
            else { [Environment]::SetEnvironmentVariable($taskName, $taskPrevious[$taskName], 'Process') }
        }
    }
}
Write-Output $taskRunPath
