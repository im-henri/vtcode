if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') { return }
$global:__VTCodeCommand = @{
    Prompt = $function:prompt
    ReadLine = $function:PSConsoleHostReadLine
    Running = $false
    Ready = $false
    History = $null
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
