# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#requires -Version 7.0

<#
.SYNOPSIS
Builds the owned Windows Notebook Run preview without an expired agent launcher.
.DESCRIPTION
Requires exact committed Rust source, an exclusive caller-owned output root,
Rust 1.99.1/MSVC and restored dependencies. It never authenticates or runs a
Notebook. FAB_RUN_AGENT_DEADLINE, when present, only shortens the 90-minute bound.
#>
param(
    [Parameter(Mandatory)][ValidatePattern('^[a-f0-9]{40}$')][string]$ExpectedHead,
    [Parameter(Mandatory)][string]$OutputRoot
)
$ErrorActionPreference = 'Stop'
foreach ($override in @('RUSTC','RUSTDOC','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','CARGO_BUILD_TARGET','CARGO_BUILD_RUSTC','CARGO_BUILD_RUSTC_WRAPPER','CARGO_BUILD_RUSTFLAGS','CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS','CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER')) {
    if ([Environment]::GetEnvironmentVariable($override)) {
        throw "Custom compiler/target override $override is incompatible with this pinned build"
    }
}
$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
if ($OutputRoot -notmatch '^[A-Za-z]:\\' -or $OutputRoot -eq [IO.Path]::GetPathRoot($OutputRoot)) {
    throw 'Select an absolute caller-owned local output directory, not a drive root'
}
if ($OutputRoot -eq $repository -or $OutputRoot.StartsWith($repository + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Keep build output outside the source checkout'
}
if ((git -C $repository rev-parse HEAD).Trim() -ne $ExpectedHead) { throw 'Source HEAD differs from ExpectedHead' }
$dirty = @(git -C $repository status --porcelain --untracked-files=normal -- rust)
if ($LASTEXITCODE -ne 0) { throw 'Could not verify Rust source cleanliness' }
if ($dirty.Count) { throw 'Commit or preserve Rust source changes before building an exact artifact' }
$toolchain = Join-Path $env:USERPROFILE '.rustup\toolchains\1.99.1-x86_64-pc-windows-msvc\bin'
$cargo = Join-Path $toolchain 'cargo.exe'
if (-not (Test-Path -LiteralPath $cargo)) { throw 'Pinned Rust 1.99.1 MSVC toolchain is unavailable' }
$rustVersion = & (Join-Path $toolchain 'rustc.exe') -Vv
if ($LASTEXITCODE -ne 0 -or $rustVersion[0] -notmatch '^rustc 1\.99\.1 ') { throw 'Actual compiler does not match the pinned toolchain' }
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw 'MSVC discovery is unavailable' }
$visualStudio = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $visualStudio) { throw 'MSVC x64 build tools are unavailable' }
$vcvars = Join-Path $visualStudio 'VC\Auxiliary\Build\vcvars64.bat'
$deadline = [DateTimeOffset]::UtcNow.AddMinutes(90)
if ($env:FAB_RUN_AGENT_DEADLINE) {
    $supervisorEnd = [DateTimeOffset]::Parse($env:FAB_RUN_AGENT_DEADLINE)
    if ($supervisorEnd -lt $deadline) { $deadline = $supervisorEnd }
}
$mutex = [Threading.Mutex]::new($false, 'Local\FabricRustParityValidation')
$held = $false
$process = $null
try {
    $wait = [Math]::Min(300, ($deadline - [DateTimeOffset]::UtcNow).TotalSeconds)
    if ($wait -le 0) { throw 'The enclosing supervision deadline has expired' }
    try { $held = $mutex.WaitOne([TimeSpan]::FromSeconds($wait)) }
    catch [Threading.AbandonedMutexException] { $held = $true }
    if (-not $held) { throw 'Another validator owns the physical-host allocation' }
    if (@(Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('cargo.exe','rustc.exe','cl.exe','link.exe') }).Count) {
        throw 'A heavy build process is active; no peer process was interrupted'
    }
    $marker = Join-Path $OutputRoot 'run-notebook-owner.json'
    if (Test-Path -LiteralPath $marker) {
        $owner = Get-Content -Raw -LiteralPath $marker | ConvertFrom-Json
        if ($owner.repository -ne $repository -or $owner.purpose -ne 'RunNotebookStudio') { throw 'Output-root ownership does not match' }
    } else {
        if ((Test-Path -LiteralPath $OutputRoot) -and @(Get-ChildItem -Force -LiteralPath $OutputRoot).Count) {
            throw 'An existing nonempty directory needs its original ownership marker; do not adopt a peer cache'
        }
        New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
        @{ repository = $repository; purpose = 'RunNotebookStudio' } | ConvertTo-Json | Set-Content -LiteralPath $marker
    }
    foreach ($directory in @('cargo','target','tmp','logs','bin')) {
        New-Item -ItemType Directory -Force -Path (Join-Path $OutputRoot $directory) | Out-Null
    }
    $drive = [IO.Path]::GetPathRoot($OutputRoot).Substring(0,1)
    $env:CARGO_HOME = Join-Path $OutputRoot 'cargo'
    $env:CARGO_TARGET_DIR = Join-Path $OutputRoot 'target'
    $env:TEMP = Join-Path $OutputRoot 'tmp'
    $env:TMP = $env:TEMP
    $env:CARGO_BUILD_JOBS = '1'
    $env:RUST_TEST_THREADS = '1'
    $env:CARGO_PROFILE_DEV_DEBUG = '0'
    $env:CARGO_INCREMENTAL = '0'
    $env:PATH = "$toolchain;$(Split-Path $vswhere);$env:PATH"
    if ((Get-Volume -DriveLetter C).SizeRemaining -lt 512MB -or
        (Get-Volume -DriveLetter $drive).SizeRemaining -lt 20GB) { throw 'Free-space floor failed (C512MiB/output20GiB)' }
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $env:ComSpec
    $info.Arguments = '/d /c call "' + $vcvars + '" >nul && cargo build --offline --locked --jobs 1 -p fabric-cli --features native-read,v2-run --bin fab-rs'
    $info.WorkingDirectory = Join-Path $repository 'rust'
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    if (-not $process.Start()) { throw 'The owned build process could not start' }
    $created = $process.StartTime.ToUniversalTime().ToString('o')
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $stop = $null
    while (-not $process.WaitForExit(1000)) {
        if ([DateTimeOffset]::UtcNow -ge $deadline) { $stop = 'deadline'; break }
        if ((Get-Volume -DriveLetter C).SizeRemaining -lt 512MB -or
            (Get-Volume -DriveLetter $drive).SizeRemaining -lt 20GB) { $stop = 'disk-floor'; break }
    }
    if ($stop) { $process.Kill($true); $process.WaitForExit() }
    $stdout.GetAwaiter().GetResult() | Set-Content -LiteralPath (Join-Path $OutputRoot 'logs\owner-build.stdout.txt')
    $stderr.GetAwaiter().GetResult() | Set-Content -LiteralPath (Join-Path $OutputRoot 'logs\owner-build.stderr.txt')
    if ($stop) { throw "Owned build stopped at $stop; no peer process was interrupted" }
    if ($process.ExitCode -ne 0) { throw "Build failed ($($process.ExitCode)); inspect logs. Missing dependencies require an explicit normal-TLS restore, not an automatic build retry." }
    if ((git -C $repository rev-parse HEAD).Trim() -ne $ExpectedHead) { throw 'Source HEAD changed during the build' }
    $changed = @(git -C $repository status --porcelain --untracked-files=normal -- rust)
    if ($LASTEXITCODE -ne 0 -or $changed.Count) { throw 'Rust source changed during the build; no artifact was published' }
    $destination = Join-Path $OutputRoot 'bin\fab-rs.exe'
    Copy-Item -LiteralPath (Join-Path $OutputRoot 'target\debug\fab-rs.exe') -Destination $destination -Force
    $binary = Get-Item -LiteralPath $destination
    $receipt = @{
        host = [Environment]::MachineName; repository = $repository; head = $ExpectedHead
        tree = (git -C $repository rev-parse 'HEAD^{tree}').Trim()
        features = @('native-read','v2-run'); profile = 'dev debug0 incremental0'; path = $destination
        bytes = $binary.Length; sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash.ToLowerInvariant()
        owned_pid = $process.Id; created = $created; deadline = $deadline.ToString('o')
        rustc = $rustVersion
        finished = [DateTimeOffset]::UtcNow.ToString('o')
    }
    $receipt | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputRoot 'bin\fab-rs.provenance.json')
    $receipt | ConvertTo-Json
} finally {
    if ($null -ne $process) {
        if (-not $process.HasExited) { $process.Kill($true); $process.WaitForExit() }
        $process.Dispose()
    }
    if ($held) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
