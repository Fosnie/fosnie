param([switch]$RunIntegration)
$ErrorActionPreference = 'Stop'
$taskCore = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$taskPreviousDb = $env:DATABASE_URL
$taskPreviousRoutingDb = $env:FOSNIE_ROUTING_TEST_DATABASE_URL
$taskTestDb = 'postgres://routing_test:routing_test_only@127.0.0.1:15439/routing_test'
try {
    & docker compose -f (Join-Path $PSScriptRoot 'compose.yml') up -d --wait
    if ($LASTEXITCODE -ne 0) { throw 'Isolated routing services could not start; start Docker Desktop first.' }
    $env:DATABASE_URL = $taskTestDb
    Push-Location (Join-Path $taskCore 'backend')
    try {
        & cargo sqlx migrate run --source migrations
        if ($LASTEXITCODE -ne 0) { throw 'Routing test database migration failed.' }
    } finally { Pop-Location }
    if ($RunIntegration) {
        $env:FOSNIE_ROUTING_TEST_DATABASE_URL = $taskTestDb
        & (Join-Path $PSScriptRoot 'test.ps1') -Lane integration
    }
    Write-Host 'Isolated routing services and schema are ready (PostgreSQL 15439, Redis 16389).'
} finally {
    $env:DATABASE_URL = $taskPreviousDb
    $env:FOSNIE_ROUTING_TEST_DATABASE_URL = $taskPreviousRoutingDb
}
