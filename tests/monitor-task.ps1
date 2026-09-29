[CmdletBinding()]
param([switch]$Integration, [string]$Binary)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $repoRoot 'scripts\monitor-task.ps1')
function Assert-Task($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

$fixtureExe = "C:\test space\O'Brien\monitor.exe"
$fixturePath = "C:\test space\O'Brien\.grok\"
$definition = New-GrokBudgetMonitorTask -RuntimeExe $fixtureExe -GrokHome $fixturePath
Assert-Task ($definition.Triggers.Count -eq 2) 'Expected logon and watchdog triggers.'
$repeat = $definition.Triggers | Where-Object { $_.Repetition.Interval -eq 'PT1M' }
Assert-Task ($null -ne $repeat) 'Watchdog must run every minute.'
Assert-Task ([string]::IsNullOrEmpty($repeat.Repetition.Duration)) 'Watchdog must repeat indefinitely.'
Assert-Task ($definition.Settings.MultipleInstances -eq 2) 'IgnoreNew must prevent duplicate daemons.'
Assert-Task ($definition.Settings.ExecutionTimeLimit -eq 'PT0S') 'Daemon must have no execution deadline.'
Assert-Task ($definition.Settings.RestartCount -eq 3) 'Failure retries must remain enabled.'
Assert-Task ($definition.Principal.RunLevel -eq 0) 'Monitor must not request elevation.'
Assert-Task ($definition.Actions.Execute -eq $fixtureExe) 'Scheduler must own the native executable directly.'
Assert-Task ($definition.Actions.Arguments -eq ('--grok-home "' + $fixturePath + '\"')) 'Windows path quoting must preserve spaces, apostrophes and trailing backslashes.'
if (-not $Binary) { $Binary = Join-Path $repoRoot 'target\release\grok-budget-monitor.exe' }
$Binary = (Resolve-Path -LiteralPath $Binary).Path
# Check the PE subsystem; Task Scheduler's Hidden setting alone does not hide a console.
$bytes = [IO.File]::ReadAllBytes($Binary)
$peOffset = [BitConverter]::ToInt32($bytes, 0x3c)
$subsystem = [BitConverter]::ToUInt16($bytes, $peOffset + 24 + 68)
Assert-Task ($subsystem -eq 2) 'Monitor must use the Windows GUI subsystem to run without a console.'
$badArgs = Start-Process -FilePath $Binary -ArgumentList '--invalid-option' -WindowStyle Hidden -Wait -PassThru
Assert-Task ($badArgs.ExitCode -ne 0) 'Native argument failure must produce a nonzero exit code.'
Write-Host 'Monitor definition, windowless binary and native exit-code tests passed.'

if (-not $Integration) { exit 0 }
$fixtureId = [Guid]::NewGuid().ToString('N')
$fixtureHome = Join-Path $repoRoot "target\monitor-tests\$fixtureId"
$null = New-Item -ItemType Directory -Path $fixtureHome -Force
$taskName = 'GrokBudgetTest-' + $fixtureId
$registered = $false
function Get-FixtureProcess {
    @(Get-CimInstance Win32_Process -Filter "Name = 'grok-budget-monitor.exe'" | Where-Object {
        $_.ExecutablePath -eq $Binary -and $_.CommandLine.Contains($fixtureHome)
    })
}
try {
    $definition = New-GrokBudgetMonitorTask -RuntimeExe $Binary -GrokHome $fixtureHome
    $null = Register-ScheduledTask -TaskName $taskName -InputObject $definition
    $registered = $true
    Start-ScheduledTask -TaskName $taskName
    Wait-GrokBudgetMonitor -TaskName $taskName
    Assert-Task (@(Get-FixtureProcess).Count -eq 1) 'Task must launch exactly one native monitor.'
    1..3 | ForEach-Object { Start-ScheduledTask -TaskName $taskName }
    Start-Sleep -Seconds 2
    Assert-Task (@(Get-FixtureProcess).Count -eq 1) 'Repeated starts created duplicate monitors.'
    Stop-ScheduledTask -TaskName $taskName
    $deadline = (Get-Date).AddSeconds(15)
    while (@(Get-FixtureProcess).Count -gt 0 -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 250 }
    Assert-Task (@(Get-FixtureProcess).Count -eq 0) 'Stopping the task did not stop its native child.'
    Write-Host 'Stopped the isolated monitor; waiting for its automatic one-minute restart.'
    Wait-GrokBudgetMonitor -TaskName $taskName -TimeoutSeconds 80
    Assert-Task (@(Get-FixtureProcess).Count -eq 1) 'Watchdog failed to restore one monitor.'
    Write-Host 'Real Task Scheduler recovery and single-instance tests passed.'
} finally {
    if ($registered) {
        $null = Disable-ScheduledTask -TaskName $taskName
        Stop-ScheduledTask -TaskName $taskName
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
    }
}
