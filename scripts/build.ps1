[CmdletBinding()]
param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) { $OutputDirectory = $repoRoot }
function Invoke-Cargo {
    param([string[]]$CargoArguments)
    & cargo @CargoArguments
    if ($LASTEXITCODE -ne 0) { throw "cargo $($CargoArguments -join ' ') failed." }
}
Push-Location $repoRoot
try {
    Invoke-Cargo @('fmt', '--all', '--', '--check')
    Invoke-Cargo @('test', '--release', '--locked')
    Invoke-Cargo @('clippy', '--release', '--locked', '--all-targets', '--', '-D', 'warnings')
    Invoke-Cargo @('build', '--release', '--locked')
    foreach ($script in @('install.ps1', 'uninstall.ps1') + @(Get-ChildItem scripts,tests -Filter '*.ps1' | ForEach-Object FullName)) {
        $tokens = $null
        $errors = $null
        $null = [Management.Automation.Language.Parser]::ParseFile((Resolve-Path $script).Path, [ref]$tokens, [ref]$errors)
        if ($errors.Count) { throw "PowerShell syntax errors in ${script}: $errors" }
    }
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File tests\monitor-task.ps1
    if ($LASTEXITCODE -ne 0) { throw 'Monitor task tests failed.' }
    & (Join-Path $PSScriptRoot 'package.ps1') -OutputDirectory $OutputDirectory
} finally { Pop-Location }
