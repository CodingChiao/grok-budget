[CmdletBinding()]
param([switch]$Build)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$grokHome = if ($env:GROK_HOME) { $env:GROK_HOME } else { Join-Path $env:USERPROFILE '.grok' }
$pluginSource = Join-Path $root 'grok-budget'
$binary = Join-Path $pluginSource 'grok-budget.exe'
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Invoke-Grok {
    param([string[]]$Arguments)
    & grok @Arguments
    if ($LASTEXITCODE -ne 0) { throw "grok failed: $($Arguments -join ' ')" }
}

if ($Build -or -not (Test-Path -LiteralPath $binary)) {
    Push-Location $root
    try {
        & cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'Rust build failed.' }
        Copy-Item -LiteralPath (Join-Path $root 'target\release\grok-budget.exe') -Destination $binary -Force
    } finally { Pop-Location }
}

$null = Get-Command grok -ErrorAction Stop
Invoke-Grok @('plugin', 'validate', $pluginSource)
$plugins = (Invoke-Grok @('plugin', 'list', '--json')) | ConvertFrom-Json
$existing = $plugins | Where-Object name -eq 'grok-budget' | Select-Object -First 1
$expectedVersion = (Get-Content -LiteralPath (Join-Path $pluginSource 'plugin.json') -Raw | ConvertFrom-Json).version
$binaryVersion = & $binary --version
if ($LASTEXITCODE -ne 0 -or $binaryVersion -ne "grok-budget $expectedVersion") { throw 'Manifest and executable versions differ.' }
if ($existing) {
    $oldSource = [IO.Path]::GetFullPath($existing.source).TrimEnd('\','/')
    if ($oldSource -ne [IO.Path]::GetFullPath($pluginSource).TrimEnd('\','/') -and (Test-Path -LiteralPath $oldSource)) {
        throw "Existing plugin comes from another directory: $oldSource"
    }
}
$taskName = 'GrokBudgetMonitor'
$taskDescription = 'Grok Budget account quota monitor (managed by grok-budget installer)'
$null = Get-Command Register-ScheduledTask -ErrorAction Stop
$oldTask = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($oldTask -and $oldTask.Description -ne $taskDescription) { throw "Task $taskName belongs to another application." }

# Back up BEFORE Grok mutates the installation or registry.
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss-fffffff'
$backup = Join-Path $grokHome "grok-budget\backups\$stamp"
$null = New-Item -ItemType Directory -Path $backup -Force
$configPath = Join-Path $grokHome 'config.toml'
$launcher = Join-Path $grokHome 'grok-budget\statusline.cmd'
foreach ($file in @($configPath, $launcher, (Join-Path $grokHome 'installed-plugins\registry.json'))) {
    if (Test-Path -LiteralPath $file) { Copy-Item -LiteralPath $file -Destination $backup -Force }
}
if ($existing -and (Test-Path -LiteralPath $existing.path)) {
    Copy-Item -LiteralPath $existing.path -Destination (Join-Path $backup 'previous-plugin') -Recurse
}
if ($oldTask) {
    Export-ScheduledTask -TaskName $taskName | Set-Content -LiteralPath (Join-Path $backup 'monitor-task.xml') -Encoding Unicode
}

if ($existing) {
    Invoke-Grok @('plugin', 'update', 'grok-budget')
    $listed = (Invoke-Grok @('plugin', 'list', '--json')) | ConvertFrom-Json
    $registered = $listed | Where-Object name -eq 'grok-budget' | Select-Object -First 1
    if ($registered.version -ne $expectedVersion -or $registered.source -ne $pluginSource) {
        # Local update is a no-op in Grok 1.0.41. Re-register via the official CLI.
        # Keep already-open status lines usable while the copied plugin is replaced.
        $rollbackExe = Join-Path $backup 'previous-plugin\grok-budget.exe'
        if (Test-Path -LiteralPath $rollbackExe) {
            [IO.File]::WriteAllText($launcher, '@echo off' + "`r`n" + '"' + $rollbackExe + '" --statusline' + "`r`n", $utf8)
        }
        Invoke-Grok @('plugin', 'uninstall', 'grok-budget', '--confirm', '--keep-data')
        Invoke-Grok @('plugin', 'install', $pluginSource, '--trust')
    }
} else { Invoke-Grok @('plugin', 'install', $pluginSource, '--trust') }
Invoke-Grok @('plugin', 'enable', 'grok-budget')
$plugins = (Invoke-Grok @('plugin', 'list', '--json')) | ConvertFrom-Json
$registered = $plugins | Where-Object name -eq 'grok-budget' | Select-Object -First 1
$installed = $registered.path
if (-not $installed -or $registered.version -ne $expectedVersion) { throw 'Plugin registration/version validation failed.' }

# Grok's local-plugin update can leave a copied installation stale: publish explicitly.
if ([IO.Path]::GetFullPath($installed) -ne [IO.Path]::GetFullPath($pluginSource)) {
    foreach ($relative in @('grok-budget.exe','plugin.json','hooks\hooks.json','commands\budget.md','report.html')) {
        $destination = Join-Path $installed $relative
        $null = New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force
        $published = $false
        for ($attempt = 0; $attempt -lt 40; $attempt++) {
            try { Copy-Item -LiteralPath (Join-Path $pluginSource $relative) -Destination $destination -Force; $published = $true; break }
            catch { if ($attempt -eq 39) { throw }; Start-Sleep -Milliseconds 250 }
        }
        if (-not $published) { throw "Cannot publish $relative" }
    }
}
$installedExe = Join-Path $installed 'grok-budget.exe'
& $installedExe --version
if ($LASTEXITCODE -ne 0) { throw 'Installed Rust binary failed validation.' }

# A versioned independent runtime avoids Grok's child-process cleanup and update locks.
$runtimeDir = Join-Path $grokHome "grok-budget\runtime\$expectedVersion"
$null = New-Item -ItemType Directory -Path $runtimeDir -Force
$runtimeExe = Join-Path $runtimeDir 'grok-budget.exe'
if ($oldTask) { Stop-ScheduledTask -TaskName $taskName }
if (-not (Test-Path -LiteralPath $runtimeExe) -or (Get-FileHash -LiteralPath $runtimeExe).Hash -ne (Get-FileHash -LiteralPath $binary).Hash) {
    Copy-Item -LiteralPath $binary -Destination $runtimeExe -Force
}
$taskUser = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$daemonCommand = "& '" + $runtimeExe.Replace("'", "''") + "' --daemon --grok-home '" + $grokHome.Replace("'", "''") + "'"
$action = New-ScheduledTaskAction -Execute (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe') -Argument ('-NoProfile -NonInteractive -WindowStyle Hidden -Command "' + $daemonCommand + '"')
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $taskUser
$principal = New-ScheduledTaskPrincipal -UserId $taskUser -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
$null = Register-ScheduledTask -TaskName $taskName -Description $taskDescription -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force
Start-ScheduledTask -TaskName $taskName
Start-Sleep -Milliseconds 800
if ((Get-ScheduledTask -TaskName $taskName).State -ne 'Running') { throw 'Quota monitor did not start. Backups have been preserved.' }

# Keep the existing launcher path, so already-open sessions pick up Rust immediately.
$launcherText = '@echo off' + "`r`n" + '"' + $runtimeExe + '" --statusline' + "`r`n"
[IO.File]::WriteAllText($launcher, $launcherText, $utf8)
$existingConfig = if (Test-Path -LiteralPath $configPath) { [IO.File]::ReadAllText($configPath) } else { '' }
$commandValue = ConvertTo-Json -InputObject $launcher -Compress
$block = '[ui.status_line]' + "`n" + 'type = "command"' + "`n" + 'command = ' + $commandValue + "`n" + 'refresh_interval = 1' + "`n"
$pattern = '(?ms)^\[ui\.status_line\][^\n]*\n.*?(?=^\[|\z)'
if ([regex]::IsMatch($existingConfig, $pattern)) {
    $updated = [regex]::Replace($existingConfig, $pattern, [System.Text.RegularExpressions.MatchEvaluator]{ param($m) $block + "`n" })
} else { $updated = $existingConfig.TrimEnd() + "`n`n" + $block }
if ($updated -ne $existingConfig) { [IO.File]::WriteAllText($configPath, $updated, $utf8) }
Write-Host "Installed native runtime: $runtimeExe"
Write-Host "Monitor task: $taskName; restart Grok to apply the 1-second display timer and new hooks."
Write-Host "Rollback files: $backup"
& $installedExe
if ($LASTEXITCODE -ne 0) { Write-Warning "Installed successfully; quota query unavailable. Run: & '$installedExe' --refresh" }
