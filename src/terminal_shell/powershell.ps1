if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') { return }
$global:__VTCodeCommand = @{
    Prompt = $function:prompt
    ReadLine = $function:PSConsoleHostReadLine
    Running = $false
    Ready = $false
    History = $null
    Directory = $null
    Rich = [bool](Get-Module PSReadLine)
}
function global:prompt {
    $__vtcodeSucceeded = $global:?
    Set-StrictMode -Off
    $__vtcodeHistory = Get-History -Count 1
    $__vtcodeState = $global:__VTCodeCommand
    $__vtcodeSignals = ''
    if (!$__vtcodeState.Rich -and $__vtcodeState.Ready -and $__vtcodeHistory.Id -ne $__vtcodeState.History) {
        $__vtcodeSignals = "$([char]27)]633;C`a"
        $__vtcodeState.Running = $true
    }
    if ($__vtcodeState.Running) {
        $__vtcodeSignals += "$([char]27)]633;D;$([int]!$__vtcodeSucceeded)`a"
        $__vtcodeState.Running = $false
    }
    $__vtcodeState.History = $__vtcodeHistory.Id
    $__vtcodeState.Ready = $true
    if ($PWD.Provider.Name -eq 'FileSystem' -and $PWD.ProviderPath -cne $__vtcodeState.Directory) {
        $__vtcodeNative = $PWD.ProviderPath
        if ($__vtcodeNative.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) {
            $__vtcodeNative = '\\' + $__vtcodeNative.Substring(8)
        } elseif ($__vtcodeNative.StartsWith('\\?\')) {
            $__vtcodeNative = $__vtcodeNative.Substring(4)
        }
        $__vtcodeDirectory = $null
        if ([uri]::TryCreate($__vtcodeNative, [UriKind]::Absolute, [ref]$__vtcodeDirectory) -and $__vtcodeDirectory.Scheme -eq 'file') {
            $__vtcodeSignals += "$([char]27)]7;$($__vtcodeDirectory.AbsoluteUri)`a"
            $__vtcodeState.Directory = $PWD.ProviderPath
        }
    }
    # Restore $? before invoking a custom prompt that displays command status.
    if (!$__vtcodeSucceeded) { Write-Error -Message 'Command failed' -ErrorAction Ignore }
    $__vtcodeText = $__vtcodeState.Prompt.Invoke()
    $__vtcodeSignals + ($__vtcodeText -join '')
}
if ($global:__VTCodeCommand.Rich -and $global:__VTCodeCommand.ReadLine) {
    function global:PSConsoleHostReadLine {
        $__vtcodeLine = $global:__VTCodeCommand.ReadLine.Invoke()
        if (![string]::IsNullOrWhiteSpace($__vtcodeLine)) {
            $global:__VTCodeCommand.Running = $true
            [Console]::Write("$([char]27)]633;C`a")
        }
        $__vtcodeLine
    }
}
