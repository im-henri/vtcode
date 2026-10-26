# Use the same arguments as build.py; for example:
# .\scripts\build.ps1 --target all --glibc both --output-dir D:\VTCode-builds
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$buildScript = Join-Path $PSScriptRoot 'build.py'
$versionCheck = 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)'
foreach ($candidate in @('python', 'py')) {
    if (-not (Get-Command $candidate -ErrorAction SilentlyContinue)) {
        continue
    }
    $pythonArguments = @()
    if ($candidate -eq 'py') {
        $pythonArguments += '-3'
    }
    & $candidate @pythonArguments -c $versionCheck 2>$null
    if ($LASTEXITCODE -ne 0) {
        continue
    }
    & $candidate @pythonArguments $buildScript @args
    exit $LASTEXITCODE
}
throw 'Install Python 3.11 or newer, then run this script again.'
