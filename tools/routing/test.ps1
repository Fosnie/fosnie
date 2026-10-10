param(
    [ValidateSet('fast','integration','ml','classifier','eval-offline','fallback','quality','e2e','all','native','native-release','enterprise','enterprise-routing','enterprise-acceptance','acceptance')][string]$Lane='fast',
    [switch]$WithEnterprise,
    [string]$Only,
    [string]$StartAt,
    [string]$Output,
    [switch]$List
)
$ErrorActionPreference='Stop'
$taskCore=(Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$taskPython=if ($env:FOSNIE_ROUTING_PYTHON) { $env:FOSNIE_ROUTING_PYTHON } else { Join-Path $taskCore 'ml/.venv/Scripts/python.exe' }
if (-not (Test-Path -LiteralPath $taskPython)) { throw 'Existing ML Python environment required, or set FOSNIE_ROUTING_PYTHON' }
$taskArgs=@((Join-Path $PSScriptRoot 'checks.py'),'--lane',$Lane)
if ($WithEnterprise) { $taskArgs+='--with-enterprise' }
if ($Only) { $taskArgs+=@('--only',$Only) }
if ($StartAt) { $taskArgs+=@('--start-at',$StartAt) }
if ($Output) { $taskArgs+=@('--output',$Output) }
if ($List) { $taskArgs+='--list' }
& $taskPython @taskArgs
if ($LASTEXITCODE -ne 0) { throw "Routing lane $Lane failed (exit $LASTEXITCODE)" }
