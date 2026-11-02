# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#requires -Version 7.4
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ValidatorRoot,
    [Parameter(Mandatory)][string]$RequestPath,
    [Parameter(Mandatory)][string]$LogDirectory,
    [string]$Distribution = 'Ubuntu',
    [ValidateRange(1, 1800)][int]$MutexWaitSeconds = 1800
)

$ErrorActionPreference = 'Stop'
if ($ValidatorRoot -notmatch '^/home/[a-z_][a-z0-9_-]*/fabric-wsl-validator-[a-zA-Z0-9_-]+$') {
    throw 'ValidatorRoot must be a dedicated fabric-wsl-validator-* directory in the WSL user home.'
}
$request = Get-Content -LiteralPath $RequestPath -Raw | ConvertFrom-Json
if ($request.schema_version -ne 1 -or $request.timeout_seconds -lt 1 -or $request.timeout_seconds -gt 600) {
    throw 'Expected schema_version 1 and timeout_seconds in 1..600.'
}
$runId = [guid]::NewGuid().ToString('N')
$unit = "fabric-validator-$runId.service"
$null = New-Item -ItemType Directory -Force -Path $LogDirectory
$logPrefix = Join-Path $LogDirectory $runId
$mutex = [Threading.Mutex]::new($false, 'Local\FabricRustParityValidation')
$held = $false
$process = $null
$started = [DateTime]::UtcNow
$result = [ordered]@{
    schema_version = 1; run_id = $runId; unit = $unit
    validator_root = $ValidatorRoot; started_utc = $started.ToString('o')
    request_sha256 = (Get-FileHash -LiteralPath $RequestPath -Algorithm SHA256).Hash.ToLowerInvariant()
    mutex = 'Local\FabricRustParityValidation'; mutex_probe = $false
    cpu_quota_percent = 200; memory_max_bytes = 6442450944; tasks_max = 128
    runtime_max_seconds = [int]$request.timeout_seconds; exit_code = $null
}

function Invoke-Wsl {
    param([string[]]$Arguments)
    & wsl.exe --distribution $Distribution --exec @Arguments
    if ($LASTEXITCODE -ne 0) { throw "WSL command failed ($LASTEXITCODE): $($Arguments[0])" }
}

try {
    try { $held = $mutex.WaitOne([TimeSpan]::FromSeconds($MutexWaitSeconds)) }
    catch [Threading.AbandonedMutexException] {
        $held = $true
        throw 'Abandoned shared mutex: confirm the previous heavy process tree ended before retrying.'
    }
    if (-not $held) { throw 'Timed out waiting for the shared Rust validation mutex.' }
    $hostFreeBytes = [long](Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory * 1024
    $linuxFreeBytes = [long](Invoke-Wsl @('/usr/bin/awk', '/^MemAvailable:/{print $2}', '/proc/meminfo')).Trim() * 1024
    $availableBytes = [Math]::Min($hostFreeBytes, $linuxFreeBytes)
    $memoryGiB = [Math]::Min(6, [Math]::Floor(($availableBytes - 2GB) / 1GB))
    if ($memoryGiB -lt 1) { throw 'Insufficient free host/WSL memory after preserving a 2GiB margin.' }
    $result.host_free_memory_bytes = $hostFreeBytes
    $result.wsl_available_memory_bytes = $linuxFreeBytes
    $result.memory_max_bytes = [long]$memoryGiB * 1GB
    # A separate Windows process must observe the mutex as unavailable.
    $probe = '$ErrorActionPreference="Stop"; try{$m=[Threading.Mutex]::new($false,"Local\FabricRustParityValidation"); $h=$m.WaitOne(0); if($h){$m.ReleaseMutex()}; $m.Dispose(); if($h){exit 7}else{exit 0}}catch{exit 8}'
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($probe))
    & ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -NoProfile -NonInteractive -EncodedCommand $encoded
    if ($LASTEXITCODE -ne 0) { throw 'Cross-process mutex exclusion proof failed.' }
    $result.mutex_probe = $true

    $identity = (Invoke-Wsl @('/usr/bin/id', '-un')).Trim()
    if ($ValidatorRoot -notlike "/home/$identity/*") { throw 'Validator root does not match the default WSL identity.' }
    Invoke-Wsl @('/usr/bin/mkdir', '-p', '--', "$ValidatorRoot/requests", "$ValidatorRoot/logs", "$ValidatorRoot/runtime-home")
    Invoke-Wsl @('/usr/bin/chmod', '700', '--', $ValidatorRoot)
    $linuxRequest = "$ValidatorRoot/requests/$runId.json"
    $linuxRunner = "$ValidatorRoot/validator_runner.py"
    $runner = Join-Path $PSScriptRoot 'validator_runner.py'
    $sourceRunner = (Invoke-Wsl @('/usr/bin/wslpath', '-a', '-u', $runner)).Trim()
    $sourceRequest = (Invoke-Wsl @('/usr/bin/wslpath', '-a', '-u', (Resolve-Path -LiteralPath $RequestPath).Path)).Trim()
    Invoke-Wsl @('/usr/bin/cp', '--', $sourceRunner, $linuxRunner)
    Invoke-Wsl @('/usr/bin/cp', '--', $sourceRequest, $linuxRequest)
    $arguments = @(
        '--distribution', $Distribution, '--exec', '/usr/bin/sudo', '-n', '/usr/bin/systemd-run',
        "--unit=$unit", '--wait', '--pipe', '--collect', '--service-type=exec',
        "--uid=$identity", "--working-directory=$ValidatorRoot",
        '--property=CPUQuota=200%', "--property=MemoryMax=${memoryGiB}G", '--property=MemorySwapMax=0',
        '--property=TasksMax=128', "--property=RuntimeMaxSec=$($request.timeout_seconds)",
        '--property=KillMode=control-group', '--property=TimeoutStopSec=5',
        '/usr/bin/env', '-i', "HOME=$ValidatorRoot/runtime-home", "USER=$identity", "LOGNAME=$identity",
        'LANG=C.UTF-8', "PATH=$ValidatorRoot/cargo/bin:$ValidatorRoot/tools:/usr/bin:/bin",
        "CARGO_HOME=$ValidatorRoot/cargo", "RUSTUP_HOME=$ValidatorRoot/rustup",
        "UV_PYTHON_INSTALL_DIR=$ValidatorRoot/python", "UV_CACHE_DIR=$ValidatorRoot/uv-cache",
        'CARGO_BUILD_JOBS=1', 'RUST_TEST_THREADS=1', 'PYTHONDONTWRITEBYTECODE=1',
        "VALIDATOR_ROOT=$ValidatorRoot", '/usr/bin/python3', $linuxRunner, $linuxRequest, $runId
    )
    $info = [Diagnostics.ProcessStartInfo]::new('wsl.exe')
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $arguments) { $info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    if (-not $process.Start()) { throw 'Could not start the owned WSL child.' }
    $result.windows_wsl_pid = $process.Id
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(([int]$request.timeout_seconds + 20) * 1000)) {
        throw 'Owned WSL invocation exceeded its systemd runtime plus cleanup allowance.'
    }
    $stdoutText = $stdout.GetAwaiter().GetResult()
    $stderrText = $stderr.GetAwaiter().GetResult()
    [IO.File]::WriteAllText("$logPrefix.stdout.log", $stdoutText)
    [IO.File]::WriteAllText("$logPrefix.stderr.log", $stderrText)
    Write-Output $stdoutText
    if ($stderrText) { Write-Output $stderrText }
    $result.exit_code = $process.ExitCode
    if ($process.ExitCode -ne 0) { throw "Bounded validation failed with exit $($process.ExitCode); logs: $logPrefix" }
}
catch {
    $result.error = $_.Exception.Message
    throw
}
finally {
    if ($held) {
        # Stop only this uniquely named transient unit, including any descendants.
        & wsl.exe --distribution $Distribution --exec /usr/bin/sudo -n /usr/bin/systemctl stop $unit 2>&1 |
            Out-File -LiteralPath "$logPrefix.cleanup.log"
        $state = & wsl.exe --distribution $Distribution --exec /usr/bin/sudo -n /usr/bin/systemctl show $unit --property=ActiveState --value 2>&1
        $stateExit = $LASTEXITCODE
        $result.cleanup_state = "$state".Trim()
        $result.cleanup_verified = ($stateExit -eq 0 -and $result.cleanup_state -in @('inactive', 'failed')) -or
            ($stateExit -ne 0 -and "$state" -match 'not found|could not be found')
        if ($process -and -not $process.HasExited) {
            $process.Kill()
            $process.WaitForExit()
        }
        if ($process -and $stdout -and -not (Test-Path -LiteralPath "$logPrefix.stdout.log")) {
            [IO.File]::WriteAllText("$logPrefix.stdout.log", $stdout.GetAwaiter().GetResult())
            [IO.File]::WriteAllText("$logPrefix.stderr.log", $stderr.GetAwaiter().GetResult())
        }
        $result.finished_utc = [DateTime]::UtcNow.ToString('o')
        $result.elapsed_seconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 3)
        $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath "$logPrefix.receipt.json"
        $mutex.ReleaseMutex()
        if (-not $result.cleanup_verified) { Write-Error "Could not verify unit cleanup: $unit" }
    }
    if ($process) { $process.Dispose() }
    $mutex.Dispose()
}
