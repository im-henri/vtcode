param(
    [Parameter(Mandatory=$true)][string]$BeforeBinary,
    [Parameter(Mandatory=$true)][string]$AfterBinary,
    [Parameter(Mandatory=$true)][string]$CorpusDirectory,
    [Parameter(Mandatory=$true)][string]$OutputDirectory,
    [ValidateRange(1,9)][int]$Rounds=3,
    [switch]$IncludeFrames
)
$ErrorActionPreference='Stop'
$before=(Resolve-Path -LiteralPath $BeforeBinary).Path
$after=(Resolve-Path -LiteralPath $AfterBinary).Path
$corpus=(Resolve-Path -LiteralPath $CorpusDirectory).Path
$output=[IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $output) {throw 'Output directory already exists; choose a new directory.'}
[void][IO.Directory]::CreateDirectory($output)
$manifest=Get-Content -LiteralPath (Join-Path $corpus 'manifest.json') -Raw | ConvertFrom-Json
$environmentNames=@('VTCODE_OPTIMIZATION_CORPUS','VTCODE_OPTIMIZATION_SYMBOL_REPORT','VTCODE_OPTIMIZATION_MENTION_REPORT')
$saved=@{}
foreach ($name in $environmentNames) {$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
$runs=[Collections.Generic.List[object]]::new()
try {
    $env:VTCODE_OPTIMIZATION_CORPUS=$corpus
    for ($round=1;$round -le $Rounds;$round++) {
        $order=if ($round % 2 -eq 1) {@('before','after')} else {@('after','before')}
        foreach ($side in $order) {
            $binary=if ($side -eq 'before') {$before} else {$after}
            $prefix=Join-Path $output "$side-$round"
            $env:VTCODE_OPTIMIZATION_SYMBOL_REPORT="$prefix-symbols.json"
            & $binary benchmark_vtcode_source_symbols --ignored --nocapture --test-threads=1 *> "$prefix-symbols.log"
            if ($LASTEXITCODE -ne 0) {throw "Symbol replay failed: $side round $round"}
            $env:VTCODE_OPTIMIZATION_MENTION_REPORT="$prefix-mentions.json"
            & $binary benchmark_chat_mention_lookup --ignored --nocapture --test-threads=1 *> "$prefix-mentions.log"
            if ($LASTEXITCODE -ne 0) {throw "Mention replay failed: $side round $round"}
            if ($IncludeFrames) {
                & $binary benchmark_unwrapped_input_frames --ignored --nocapture --test-threads=1 *> "$prefix-frames.log"
                if ($LASTEXITCODE -ne 0) {throw "Input/frame replay failed: $side round $round"}
                & $binary benchmark_rich_editor_rendering --ignored --nocapture --test-threads=1 *> "$prefix-rich.log"
                if ($LASTEXITCODE -ne 0) {throw "Rich frame replay failed: $side round $round"}
            }
            $runs.Add(@{side=$side;round=$round;symbols=(Get-Content -LiteralPath "$prefix-symbols.json" -Raw | ConvertFrom-Json);
                mentions=@(Get-Content -LiteralPath "$prefix-mentions.json" -Raw | ConvertFrom-Json)})
            Write-Output "Completed $side round $round"
        }
        $pair=@($runs | Where-Object {$_.round -eq $round})
        foreach ($case in $pair[0].symbols.cases) {
            $other=@($pair[1].symbols.cases | Where-Object {$_.case -eq $case.case})[0]
            if ($case.digest -ne $other.digest -or $case.symbols -ne $other.symbols -or $case.bytes -ne $other.bytes) {
                throw "Symbol output mismatch: $($case.case) round $round"
            }
        }
        if ($IncludeFrames) {
            $digests=@{}
            foreach ($side in @('before','after')) {
                $digests[$side]=@{}
                foreach ($line in Get-Content -LiteralPath (Join-Path $output "$side-$round-rich.log")) {
                    $taskMatch=[regex]::Match($line,'rich render case=(\w+).* cells=([0-9a-f]+)')
                    if ($taskMatch.Success) {$digests[$side][$taskMatch.Groups[1].Value]=$taskMatch.Groups[2].Value}
                }
            }
            if ($digests.before.Count -eq 0 -or $digests.before.Count -ne $digests.after.Count) {throw 'Missing rich-frame output digests'}
            foreach ($case in $digests.before.Keys) {
                if ($digests.before[$case] -ne $digests.after[$case]) {throw "Changed rich-frame output: $case"}
            }
        }
    }
    $cpu=try {(Get-ItemProperty -LiteralPath 'HKLM:\HARDWARE\DESCRIPTION\System\CentralProcessor\0' -Name ProcessorNameString).ProcessorNameString} catch {'unavailable'}
    $report=@{profile='fastrelease: opt-level=3, codegen-units=16, no LTO';cpu=$cpu;logicalProcessors=$env:NUMBER_OF_PROCESSORS;
        compiler=@(& rustc -Vv);rounds=$Rounds;framesIncluded=[bool]$IncludeFrames;
        corpusRevision=$manifest.revision;corpusFiles=$manifest.files.Count;corpusBytes=($manifest.files | Measure-Object -Property bytes -Sum).Sum;
        corpusManifestSha256=(Get-FileHash -LiteralPath (Join-Path $corpus 'manifest.json') -Algorithm SHA256).Hash;
        beforeSha256=(Get-FileHash -LiteralPath $before -Algorithm SHA256).Hash;
        afterSha256=(Get-FileHash -LiteralPath $after -Algorithm SHA256).Hash;runs=@($runs.ToArray())}
    [IO.File]::WriteAllText((Join-Path $output 'comparison.json'),($report | ConvertTo-Json -Depth 20),[Text.UTF8Encoding]::new($false))
} finally {
    foreach ($name in $environmentNames) {[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}
}
