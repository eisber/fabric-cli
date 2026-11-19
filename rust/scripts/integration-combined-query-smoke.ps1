# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Offline hosted checks for the combined native query, XLSX and Dataflow CLI candidate.
# Each phase runs a fixed list of locked cargo commands, keeps every log and fails on
# the first nonzero exit or on any source-tree change.

param(
    [Parameter(Mandatory)][ValidateSet('sdk', 'hosts', 'binary')][string]$Phase,
    [Parameter(Mandatory)][string]$Receipts
)

$ErrorActionPreference = 'Stop'
$rust = Join-Path $PSScriptRoot '..'
$phaseReceipts = Join-Path $Receipts $Phase
New-Item -ItemType Directory -Force -Path $phaseReceipts | Out-Null

$test = @('test', '--offline', '--locked')
$cli = @('-p', 'fabric-cli', '--no-default-features', '--features', 'headless,v2-query')
$plans = @{
    sdk    = [ordered]@{
        'fetch'             = @('fetch', '--locked', '--target', 'x86_64-pc-windows-msvc')
        'tabular-sdk'       = $test + @('-p', 'fabric-sdk-tabular', '-p', 'fabric-sdk-query')
        'query-tabular-sdk' = $test + @('-p', 'fabric-sdk-query-tabular', '--features', 'aihub')
        'aihub-sdk'         = $test + @('-p', 'fabric-sdk-aihub-query')
        'query-commands'    = $test + @('-p', 'fabric-cli-query-commands')
        'identity'          = $test + @('-p', 'fabric-identity', '--features', 'headless,interactive,windows-broker')
        'dataflow-sdk'      = $test + @('-p', 'fabric-sdk-dataflow', '--test', 'diagnostics', '--test', 'execute_query')
        'discovery-sdk'     = $test + @('-p', 'fabric-sdk', '--test', 'dataflow_queries')
    }
    hosts  = [ordered]@{
        'query-host'          = $test + $cli + @('--lib', 'query_host')
        'query-binary'        = $test + $cli + @('--test', 'query_binary')
        'dataflow-with-query' = $test + $cli + @('--test', 'execute_host', '--test', 'execute_arguments', '--test', 'execute_cli_contract')
        'raw-m-lexer'         = $test + $cli + @('--lib', 'execute::m::tests::')
    }
    binary = [ordered]@{
        'combined-check'  = @('check', '--offline', '--locked', '-p', 'fabric-cli', '--all-targets', '--no-default-features', '--features', 'native-read,v2-query')
        'combined-binary' = @('build', '--offline', '--locked', '-p', 'fabric-cli', '--bin', 'fab-rs', '--no-default-features', '--features', 'native-read,v2-query')
        'format'          = @('fmt', '--check', '-p', 'fabric-sdk-tabular', '-p', 'fabric-sdk-query', '-p', 'fabric-sdk-query-tabular',
            '-p', 'fabric-sdk-aihub-query', '-p', 'fabric-cli-query-commands', '-p', 'fabric-identity', '-p', 'fabric-cli')
    }
}

Push-Location $rust
try {
    $before = @(git status --porcelain=v1 --untracked-files=all)
    $results = [ordered]@{}
    $failed = $null
    foreach ($entry in $plans[$Phase].GetEnumerator()) {
        $log = Join-Path $phaseReceipts "$($entry.Key).log"
        $started = Get-Date
        & cargo @($entry.Value) *> $log
        $exit = $LASTEXITCODE
        $results[$entry.Key] = [ordered]@{ args = $entry.Value; exit = $exit; seconds = ((Get-Date) - $started).TotalSeconds }
        if ($exit -ne 0) {
            $failed = $entry.Key
            Get-Content $log -Tail 150
            break
        }
    }
    if (-not $failed -and $Phase -eq 'binary') {
        $fab = Join-Path $env:CARGO_TARGET_DIR 'debug/fab-rs.exe'
        foreach ($root in 'execute', 'query', 'get') {
            & $fab $root --help *> (Join-Path $phaseReceipts "help-$root.txt")
            $results["help-$root"] = [ordered]@{ exit = $LASTEXITCODE }
            if ($LASTEXITCODE -ne 0) { $failed = "help-$root"; break }
        }
    }
    $after = @(git status --porcelain=v1 --untracked-files=all)
    if (Compare-Object $before $after) { $failed = $failed ?? 'source-changed' }
    [ordered]@{ phase = $Phase; source = $env:GITHUB_SHA; passed = -not $failed; failed = $failed; results = $results } |
        ConvertTo-Json -Depth 6 | Set-Content (Join-Path $phaseReceipts 'summary.json')
    if ($failed) {
        Write-Error "Combined query CLI phase '$Phase' failed: $failed"
        exit 1
    }
}
finally {
    Pop-Location
}
