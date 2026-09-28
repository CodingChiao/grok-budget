[CmdletBinding()]
param([switch]$Build)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$grokHome = Join-Path $env:USERPROFILE '.grok'
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
if ($existing) {
    $oldSource = [IO.Path]::GetFullPath($existing.source).TrimEnd('\','/')
    if ($oldSource -ne [IO.Path]::GetFullPath($pluginSource).TrimEnd('\','/')) {
        if (Test-Path -LiteralPath $oldSource) { throw "Existing plugin comes from another directory: $oldSource" }
        Invoke-Grok @('plugin', 'uninstall', 'grok-budget', '--confirm', '--keep-data')
        Invoke-Grok @('plugin', 'install', $pluginSource, '--trust')
    } else { Invoke-Grok @('plugin', 'update', 'grok-budget') }
} else { Invoke-Grok @('plugin', 'install', $pluginSource, '--trust') }
Invoke-Grok @('plugin', 'enable', 'grok-budget')
$plugins = (Invoke-Grok @('plugin', 'list', '--json')) | ConvertFrom-Json
$installed = ($plugins | Where-Object name -eq 'grok-budget' | Select-Object -First 1).path
if (-not $installed) { throw 'Cannot locate installed plugin.' }

# Retain the previous runtime and configuration together for explicit rollback.
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss-fffffff'
$backup = Join-Path $grokHome "grok-budget\backups\$stamp"
$null = New-Item -ItemType Directory -Path $backup -Force
$configPath = Join-Path $grokHome 'config.toml'
$launcher = Join-Path $grokHome 'grok-budget\statusline.cmd'
foreach ($file in @($configPath, $launcher, (Join-Path $installed 'grok-budget.exe'), (Join-Path $installed 'budget.py'))) {
    if (Test-Path -LiteralPath $file) { Copy-Item -LiteralPath $file -Destination $backup -Force }
}
$oldHooks = Join-Path $installed 'hooks\hooks.json'
if (Test-Path -LiteralPath $oldHooks) { Copy-Item -LiteralPath $oldHooks -Destination (Join-Path $backup 'hooks.json') }
foreach ($stale in @((Join-Path $installed 'budget.py'), (Join-Path $installed '__pycache__'))) {
    if (Test-Path -LiteralPath $stale) { Remove-Item -LiteralPath $stale -Recurse -Force }
}

# Grok's local-plugin update can leave a copied installation stale: publish explicitly.
if ([IO.Path]::GetFullPath($installed) -ne [IO.Path]::GetFullPath($pluginSource)) {
    foreach ($relative in @('grok-budget.exe','plugin.json','hooks\hooks.json','commands\budget.md','report.html')) {
        $destination = Join-Path $installed $relative
        $null = New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force
        $published = $false
        for ($attempt = 0; $attempt -lt 10; $attempt++) {
            try { Copy-Item -LiteralPath (Join-Path $pluginSource $relative) -Destination $destination -Force; $published = $true; break }
            catch { if ($attempt -eq 9) { throw }; Start-Sleep -Milliseconds 250 }
        }
        if (-not $published) { throw "Cannot publish $relative" }
    }
}
$installedExe = Join-Path $installed 'grok-budget.exe'
& $installedExe --version
if ($LASTEXITCODE -ne 0) { throw 'Installed Rust binary failed validation.' }

# Keep the existing launcher path, so already-open sessions pick up Rust immediately.
$launcherText = '@echo off' + "`r`n" + '"' + $installedExe + '" --statusline' + "`r`n"
[IO.File]::WriteAllText($launcher, $launcherText, $utf8)
$existingConfig = if (Test-Path -LiteralPath $configPath) { [IO.File]::ReadAllText($configPath) } else { '' }
$commandValue = ConvertTo-Json -InputObject $launcher -Compress
$block = '[ui.status_line]' + "`n" + 'type = "command"' + "`n" + 'command = ' + $commandValue + "`n" + 'refresh_interval = 60' + "`n"
$pattern = '(?ms)^\[ui\.status_line\][^\n]*\n.*?(?=^\[|\z)'
if ([regex]::IsMatch($existingConfig, $pattern)) {
    $updated = [regex]::Replace($existingConfig, $pattern, [System.Text.RegularExpressions.MatchEvaluator]{ param($m) $block + "`n" })
} else { $updated = $existingConfig.TrimEnd() + "`n`n" + $block }
if ($updated -ne $existingConfig) { [IO.File]::WriteAllText($configPath, $updated, $utf8) }
Write-Host "Installed native runtime: $installedExe"
Write-Host "Rollback files: $backup"
& $installedExe
if ($LASTEXITCODE -ne 0) { Write-Warning 'Installed successfully; quota query unavailable. Run grok-budget.cmd later.' }
