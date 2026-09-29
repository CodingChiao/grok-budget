# Shared by the installer and task-definition / recovery tests.
function Stop-GrokBudgetLegacyMonitor {
    param([Parameter(Mandatory)][string]$GrokHome)
    # Releases through 0.3.0 used a PowerShell task whose native child can
    # survive Stop-ScheduledTask. Limit migration cleanup to this home's runtime.
    $runtimeRoot = [IO.Path]::GetFullPath((Join-Path $GrokHome 'grok-budget\runtime')).TrimEnd('\') + '\'
    Get-CimInstance Win32_Process -Filter "Name = 'grok-budget.exe'" | Where-Object {
        $_.ExecutablePath -and $_.ExecutablePath.StartsWith($runtimeRoot, [StringComparison]::OrdinalIgnoreCase) -and
        $_.CommandLine -match '(?:^|\s)--daemon(?:\s|$)'
    } | ForEach-Object {
        Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
    }
}

function New-GrokBudgetMonitorTask {
    param(
        [Parameter(Mandatory)][string]$RuntimeExe,
        [Parameter(Mandatory)][string]$GrokHome
    )
    $taskUser = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    # The GUI-subsystem executable has no console window. Launch it directly:
    # stopping the task must stop the monitor, not leave a shell's child behind.
    if ($GrokHome.Contains('"')) { throw 'Grok home cannot contain a double quote.' }
    $quotedHome = $GrokHome -replace '(\\+)$', '$1$1'
    $action = New-ScheduledTaskAction -Execute $RuntimeExe -Argument ('--grok-home "' + $quotedHome + '"')
    $logon = New-ScheduledTaskTrigger -AtLogOn -User $taskUser
    # No duration means repeat indefinitely, including after a successful exit
    # or a manually stopped task. IgnoreNew makes this a watchdog, not a second daemon.
    $watchdog = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Minutes 1)
    $principal = New-ScheduledTaskPrincipal -UserId $taskUser -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) `
        -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
    New-ScheduledTask -Action $action -Trigger @($logon, $watchdog) -Principal $principal -Settings $settings `
        -Description 'Grok Budget account quota monitor (managed by grok-budget installer)'
}

function Wait-GrokBudgetMonitor {
    param([Parameter(Mandatory)][string]$TaskName, [int]$TimeoutSeconds = 15)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        if ((Get-ScheduledTask -TaskName $TaskName -ErrorAction Stop).State -eq 'Running') {
            # Catch immediate startup failures instead of accepting a transient Running state.
            Start-Sleep -Seconds 1
            if ((Get-ScheduledTask -TaskName $TaskName).State -eq 'Running') { return }
        }
        Start-Sleep -Milliseconds 250
    } while ((Get-Date) -lt $deadline)
    $info = Get-ScheduledTaskInfo -TaskName $TaskName
    throw "Quota monitor did not stay running (task result: $($info.LastTaskResult)). Backups have been preserved."
}
