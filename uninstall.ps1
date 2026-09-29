[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$grokHome = if ($env:GROK_HOME) { $env:GROK_HOME } else { Join-Path $env:USERPROFILE '.grok' }
$task = Get-ScheduledTask -TaskName 'GrokBudgetMonitor' -ErrorAction SilentlyContinue
if ($task) {
    if ($task.Description -ne 'Grok Budget account quota monitor (managed by grok-budget installer)') { throw 'The task belongs to another application.' }
    Stop-ScheduledTask -TaskName $task.TaskName
    Unregister-ScheduledTask -TaskName $task.TaskName -Confirm:$false
}
$configPath = Join-Path $grokHome 'config.toml'
if (Test-Path -LiteralPath $configPath) {
    $config = [IO.File]::ReadAllText($configPath)
    $pattern = '(?ms)^\[ui\.status_line\][^\n]*\n.*?(?=^\[|\z)'
    $match = [regex]::Match($config, $pattern)
    if ($match.Success -and $match.Value.Contains('grok-budget')) {
        $backupDir = Join-Path $grokHome ('grok-budget\backups\uninstall-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fffffff'))
        $null = New-Item -ItemType Directory -Path $backupDir -Force
        Copy-Item -LiteralPath $configPath -Destination $backupDir
        $updated = $config.Remove($match.Index, $match.Length)
        [IO.File]::WriteAllText($configPath, $updated, (New-Object System.Text.UTF8Encoding($false)))
    }
}
& grok plugin uninstall grok-budget --confirm --keep-data
if ($LASTEXITCODE -ne 0) { throw 'Grok plugin uninstall failed.' }
Write-Host 'Uninstalled. History, runtime files and backups are preserved. Restart Grok to reload its configuration.'
