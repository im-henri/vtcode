param(
    [string]$SourceDirectory = 'src',
    [string]$CorpusDirectory = 'src',
    [string]$OutputDirectory = 'target/completion-heap',
    [string]$ExportRecordsDirectory = '',
    [string]$FixedRecordsDirectory = '',
    [switch]$RecursiveCorpus
)
$ErrorActionPreference = 'Stop'
$taskRoot = (Get-Location).Path
$taskSource = (Resolve-Path -LiteralPath $SourceDirectory).Path
$taskCorpus = (Resolve-Path -LiteralPath $CorpusDirectory).Path
$taskOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
$taskSources = Join-Path $taskOutput 'src'
New-Item -ItemType Directory -Path $taskSources -Force | Out-Null
Get-ChildItem -LiteralPath $taskSource -Filter 'completion*.rs' -File | Copy-Item -Destination $taskSources
Copy-Item -LiteralPath (Join-Path $taskSource 'vtcomplete_identifier.rs') -Destination $taskSources
# Instrument a temporary copy of the benchmark, never production sources.
$taskProbePath = Join-Path $taskSources 'completion_index_capacity_tests.rs'
$taskProbe = [System.IO.File]::ReadAllText($taskProbePath)
if ($RecursiveCorpus) {
    # The fixed original capacity comparison used src/*.rs. Include
    # nested modules here, preserving relative identities (several are mod.rs).
    $taskPaths = @'
    let mut all = Vec::new();
    let mut directories = vec![source.clone()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap(); let path = entry.path();
            if entry.file_type().unwrap().is_dir() { directories.push(path); }
            else if path.extension().is_some_and(|extension| extension == "rs") { all.push(path); }
        }
    }
    all.sort();
'@
    $taskPathsPattern = '(?s)    let mut all: Vec<_> = std::fs::read_dir\(&source\).*?    all.sort\(\);'
    if (-not [regex]::IsMatch($taskProbe,$taskPathsPattern)) { throw 'Recursive corpus marker missing' }
    $taskProbe = [regex]::Replace($taskProbe,$taskPathsPattern,$taskPaths)
    $taskProbe = $taskProbe.Replace('path.file_name().unwrap()', 'path.strip_prefix(&source).unwrap()')
    $taskProbe = $taskProbe.Replace('"vtcode-src"','"vtcode-src-recursive"')
}
foreach ($taskMarker in @('let shared = Arc::new(Shared {','let retained = worker.memory + catalog.memory_bytes;','"ledger": ledger,')) {
    if (-not $taskProbe.Contains($taskMarker)) { throw "Heap probe marker missing: $taskMarker" }
}
$taskProbe = $taskProbe.Replace('let shared = Arc::new(Shared {', 'let heap_start = crate::memory::live(); crate::memory::reset_peak(); let shared = Arc::new(Shared {')
$taskProbe = $taskProbe.Replace('let retained = worker.memory + catalog.memory_bytes;', 'let retained = worker.memory + catalog.memory_bytes; let live_heap = crate::memory::live().saturating_sub(heap_start); let peak_heap = crate::memory::peak().saturating_sub(heap_start);')
$taskProbe = $taskProbe.Replace('"ledger": ledger,', '"ledger": ledger, "liveRequestedHeapBytes":live_heap, "peakRequestedHeapBytes":peak_heap,')
# Freeze the logical file evidence, independently of its storage layout. This
# instrumentation runs after the live/peak snapshot, so export scratch is excluded.
$taskPacked = Test-Path -LiteralPath (Join-Path $taskSource 'completion_storage.rs')
$taskDecode = if ($taskPacked) { 'let decoded = record.decoded(&catalog);' } else { 'let decoded = record;' }
$taskLogicalProbe = @'
        let mut logical_digest = Sha256::new();
        let mut logical_terms = 0usize;
        let mut logical_contexts = 0usize;
        let mut logical_tokens = 0usize;
        for record in worker.files.values() {
            DECODE_RECORD
            let mut value = serde_json::to_value(&decoded).unwrap();
            let terms = value["terms"].as_array_mut().unwrap();
            terms.sort_unstable_by(|a,b| a["word"].as_str().cmp(&b["word"].as_str()));
            logical_terms += terms.len();
            for term in terms {
                let contexts = term["contexts"].as_array_mut().unwrap();
                contexts.sort_unstable_by_key(|v| v[0].as_u64());
                logical_contexts += contexts.len();
            }
            logical_tokens += value["examples"]["tokens"].as_array().unwrap().len();
            let relative = record.path.strip_prefix(&root).unwrap();
            let logical = serde_json::json!({"name":relative.to_string_lossy(),
                "terms":value["terms"],"examples":value["examples"]});
            logical_digest.update(serde_json::to_vec(&logical).unwrap());
            logical_digest.update([0]);
            if let Some(directory) = std::env::var_os("VTCODE_HEAP_EXPORT") {
                let directory = std::path::PathBuf::from(directory).join(name);
                std::fs::create_dir_all(&directory).unwrap();
                let output = directory.join(relative).with_extension("json");
                std::fs::create_dir_all(output.parent().unwrap()).unwrap();
                std::fs::write(output,
                    serde_json::to_vec(&decoded).unwrap()).unwrap();
            }
        }
        let logical_sha256 = format!("{:x}", logical_digest.finalize());
        drop(catalog);
'@
$taskLogicalProbe = $taskLogicalProbe.Replace('DECODE_RECORD',$taskDecode)
$taskProbe = $taskProbe.Replace('        drop(catalog);',$taskLogicalProbe)
$taskProbe = $taskProbe.Replace('"ledger": ledger,','"logicalSha256":logical_sha256,"logicalTerms":logical_terms,"logicalContexts":logical_contexts,"logicalTokens":logical_tokens,"ledger": ledger,')
if ($FixedRecordsDirectory) {
    if (-not $taskPacked) { throw 'Fixed-record replay requires the current packed implementation' }
    $taskProbe = $taskProbe.Replace('let settings = IndexSettings::default();','let settings = IndexSettings {max_memory_mb:256,..IndexSettings::default()};')
    $taskFixedInstall = @'
let extracted = $1;
            let fixed = std::path::PathBuf::from(std::env::var_os("VTCODE_HEAP_FIXED").unwrap()).join(name)
                .join(path.strip_prefix(&source).unwrap()).with_extension("json");
            let mut record:FileRecord = serde_json::from_slice(&std::fs::read(fixed).unwrap()).unwrap();
            record.path = extracted.path;
            worker.install(record, &settings);
'@
    $taskInstallPattern = '(?s)worker\.install\(\s*(FileRecord \{.*?\}),?\s*&settings,?\s*\);'
    if (-not [regex]::IsMatch($taskProbe,$taskInstallPattern)) { throw 'Fixed-record install marker missing' }
    $taskProbe = [regex]::Replace($taskProbe,$taskInstallPattern,$taskFixedInstall)
    # The ablation preserves every exported count and example. Caps apply in
    # ordinary runs; they must not discard logical evidence in this comparison.
    $taskStoragePath = Join-Path $taskSources 'completion_storage.rs'
    $taskStorage = [System.IO.File]::ReadAllText($taskStoragePath)
    $taskStorage = $taskStorage.Replace('const CONTEXT_BUDGET: usize = 512 * 1024;', 'const CONTEXT_BUDGET: usize = usize::MAX;')
    $taskStorage = $taskStorage.Replace('CONTEXT_BUDGET.min(retained_limit(settings) / 16)', 'CONTEXT_BUDGET')
    $taskStorage = $taskStorage.Replace('(4 * 1024 * 1024).min(retained_limit(settings) / 4)', 'usize::MAX')
    [System.IO.File]::WriteAllText($taskStoragePath,$taskStorage)
}
[System.IO.File]::WriteAllText($taskProbePath,$taskProbe)
$taskHarness = @'
#![allow(dead_code)]
#[path="src/completion_context.rs"] mod completion_context;
#[path="src/completion_source.rs"] mod completion_source;
#[path="src/completion_history.rs"] mod completion_history;
#[path="src/vtcomplete_identifier.rs"] mod completion_match;
#[path="src/completion_index.rs"] mod completion_index;
mod background { #[derive(Default,Clone)] pub(crate) struct UiWake; impl UiWake { pub(crate) fn notify(&self) {} } }
mod worker_priority { pub(crate) fn lower_current_thread() {} }
mod settings { pub(crate) fn project_settings_path(p:&std::path::Path)->std::path::PathBuf {p.join(".vtcode")} }
mod memory {
    use std::alloc::{GlobalAlloc,Layout,System};
    use std::sync::atomic::{AtomicUsize,Ordering::Relaxed};
    static LIVE:AtomicUsize=AtomicUsize::new(0);
    static PEAK:AtomicUsize=AtomicUsize::new(0);
    pub fn live()->usize {LIVE.load(Relaxed)}
    pub fn peak()->usize {PEAK.load(Relaxed)}
    pub fn reset_peak(){PEAK.store(live(),Relaxed)}
    fn added(n:usize){let next=LIVE.fetch_add(n,Relaxed)+n; PEAK.fetch_max(next,Relaxed);}
    pub struct Allocator;
    unsafe impl GlobalAlloc for Allocator {
        unsafe fn alloc(&self,l:Layout)->*mut u8 {let p=unsafe{System.alloc(l)};if !p.is_null(){added(l.size())}p}
        unsafe fn alloc_zeroed(&self,l:Layout)->*mut u8 {let p=unsafe{System.alloc_zeroed(l)};if !p.is_null(){added(l.size())}p}
        unsafe fn dealloc(&self,p:*mut u8,l:Layout){unsafe{System.dealloc(p,l)};LIVE.fetch_sub(l.size(),Relaxed);}
        unsafe fn realloc(&self,p:*mut u8,l:Layout,n:usize)->*mut u8 {
            let q=unsafe{System.realloc(p,l,n)};
            if !q.is_null(){if n>=l.size(){added(n-l.size())}else{LIVE.fetch_sub(l.size()-n,Relaxed);}}q
        }
    }
}
#[global_allocator] static ALLOCATOR:memory::Allocator=memory::Allocator;
'@
$taskHarnessPath = Join-Path $taskOutput 'heap.rs'
[System.IO.File]::WriteAllText($taskHarnessPath,$taskHarness)
$taskDeps = Join-Path $taskRoot 'target/fastrelease/deps'
$taskRustcArgs = @('--edition=2024','--test','-C','opt-level=3','-A','dead_code',$taskHarnessPath,'-L',"dependency=$taskDeps",'-o',(Join-Path $taskOutput 'heap.exe'))
foreach ($taskDependency in @('serde','serde_json','ropey','sha2','ignore','notify','rustc_hash','tempfile','windows_sys')) {
    $taskLibraries = @(Get-ChildItem -LiteralPath $taskDeps -Filter "lib$taskDependency-*.rlib")
    $taskLibrary = $null
    foreach ($taskCandidate in $taskLibraries) {
        if ($taskDependency -notin @('serde','serde_json')) { break }
        $taskId = $taskCandidate.BaseName.Substring(3)
        $taskFingerprint = Join-Path $taskRoot "target/fastrelease/.fingerprint/$taskId/lib-$taskDependency.json"
        if (Test-Path -LiteralPath $taskFingerprint) {
            $taskFeatures = (Get-Content -LiteralPath $taskFingerprint -Raw | ConvertFrom-Json).features
            if (($taskDependency -eq 'serde' -and $taskFeatures.Contains('derive') -and $taskFeatures.Contains('rc')) -or
                ($taskDependency -eq 'serde_json' -and $taskFeatures.Contains('raw_value'))) { $taskLibrary=$taskCandidate; break }
        }
    }
    if ($null -eq $taskLibrary) { $taskLibrary = $taskLibraries | Sort-Object LastWriteTime | Select-Object -First 1 }
    if ($null -eq $taskLibrary) { throw "Build fastrelease test dependencies first: missing $taskDependency" }
    $taskRustcArgs += @('--extern',"$taskDependency=$($taskLibrary.FullName)")
}
$taskOldManifest = $env:CARGO_MANIFEST_DIR
$taskOldCorpus = $env:VTCODE_CAPACITY_SOURCE
$taskOldReport = $env:VTCODE_CAPACITY_REPORT
$taskOldExport = $env:VTCODE_HEAP_EXPORT
$taskOldFixed = $env:VTCODE_HEAP_FIXED
$taskOldTokens = $env:VTCODE_EFFICIENCY_TOKENS
try {
    $env:CARGO_MANIFEST_DIR = $taskRoot
    & rustc @taskRustcArgs *> (Join-Path $taskOutput 'build.log')
    if ($LASTEXITCODE -ne 0) { Get-Content (Join-Path $taskOutput 'build.log') -Tail 30; throw 'Heap harness failed to compile' }
    $env:VTCODE_CAPACITY_SOURCE = $taskCorpus
    $env:VTCODE_CAPACITY_REPORT = Join-Path $taskOutput 'heap.json'
    $env:VTCODE_HEAP_EXPORT = if ($ExportRecordsDirectory) { [System.IO.Path]::GetFullPath($ExportRecordsDirectory) } else { $null }
    $env:VTCODE_HEAP_FIXED = if ($FixedRecordsDirectory) { (Resolve-Path -LiteralPath $FixedRecordsDirectory).Path } else { $null }
    if ($FixedRecordsDirectory) { $env:VTCODE_EFFICIENCY_TOKENS = '2048' }
    & (Join-Path $taskOutput 'heap.exe') benchmark_vtcode_index_capacity --ignored --nocapture --test-threads=1 *> (Join-Path $taskOutput 'run.log')
    if ($LASTEXITCODE -ne 0) { Get-Content (Join-Path $taskOutput 'run.log') -Tail 25; throw 'Heap workload failed' }
    Write-Output $env:VTCODE_CAPACITY_REPORT
} finally {
    $env:CARGO_MANIFEST_DIR = $taskOldManifest
    $env:VTCODE_CAPACITY_SOURCE = $taskOldCorpus
    $env:VTCODE_CAPACITY_REPORT = $taskOldReport
    $env:VTCODE_HEAP_EXPORT = $taskOldExport
    $env:VTCODE_HEAP_FIXED = $taskOldFixed
    $env:VTCODE_EFFICIENCY_TOKENS = $taskOldTokens
}
