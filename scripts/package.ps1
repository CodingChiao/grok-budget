[CmdletBinding()]
param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) { $OutputDirectory = $repoRoot }
if (-not [IO.Path]::IsPathRooted($OutputDirectory)) { $OutputDirectory = Join-Path $repoRoot $OutputDirectory }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$manifest = Get-Content -LiteralPath (Join-Path $repoRoot 'grok-budget\plugin.json') -Raw | ConvertFrom-Json
$version = $manifest.version
if ($version -notmatch '^\d+\.\d+\.\d+$') { throw 'Expected a numeric release version.' }
$cargoText = [IO.File]::ReadAllText((Join-Path $repoRoot 'Cargo.toml'))
if ($cargoText -notmatch ('(?m)^version = "' + [regex]::Escape($version) + '"\r?$')) { throw 'Cargo and plugin versions differ.' }
$binary = Join-Path $repoRoot 'target\release\grok-budget.exe'
$binaryVersion = & $binary --version
if ($LASTEXITCODE -ne 0 -or $binaryVersion -ne "grok-budget $version") { throw 'Release binary and manifest versions differ.' }
Copy-Item -LiteralPath $binary -Destination (Join-Path $repoRoot 'grok-budget\grok-budget.exe') -Force
$monitor = Join-Path $repoRoot 'target\release\grok-budget-monitor.exe'
Copy-Item -LiteralPath $monitor -Destination (Join-Path $repoRoot 'grok-budget\grok-budget-monitor.exe') -Force
$files = @('Cargo.toml','Cargo.lock','rust-toolchain.toml','README.md','LICENSE','CHANGELOG.md','install.ps1','uninstall.ps1')
foreach ($directory in @('src','tests','scripts','grok-budget')) {
    $files += Get-ChildItem -LiteralPath (Join-Path $repoRoot $directory) -File -Recurse | ForEach-Object {
        $_.FullName.Substring($repoRoot.Length + 1).Replace('\','/')
    }
}
$files = @($files | Sort-Object -Unique)
Add-Type -AssemblyName System.IO.Compression.FileSystem
$staging = Join-Path $repoRoot ('target\package\' + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $staging,$OutputDirectory -Force
$name = "GrokBudget-$version-windows-x64.zip"
$tempZip = Join-Path $staging $name
$zip = [IO.Compression.ZipFile]::Open($tempZip, [IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($relative in $files) {
        $null = [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, (Join-Path $repoRoot $relative), $relative, [IO.Compression.CompressionLevel]::Optimal)
    }
} finally { $zip.Dispose() }
# Verify all packaged bytes, then smoke-test the extracted binary without any user data.
$extracted = Join-Path $staging 'extracted'
[IO.Compression.ZipFile]::ExtractToDirectory($tempZip, $extracted)
foreach ($relative in $files) {
    if ((Get-FileHash -LiteralPath (Join-Path $repoRoot $relative)).Hash -ne (Get-FileHash -LiteralPath (Join-Path $extracted $relative)).Hash) {
        throw "Packaged file differs: $relative"
    }
}
$extractedVersion = & (Join-Path $extracted 'grok-budget\grok-budget.exe') --version
if ($LASTEXITCODE -ne 0 -or $extractedVersion -ne $binaryVersion) { throw 'Extracted binary smoke test failed.' }
$output = Join-Path $OutputDirectory $name
Move-Item -LiteralPath $tempZip -Destination $output -Force
$hash = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText($output + '.sha256', "$hash  $name`n", (New-Object Text.UTF8Encoding($false)))
Write-Host "Verified package: $output ($($files.Count) files)"
