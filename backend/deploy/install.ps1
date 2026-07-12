# Fosnie one-line installer for Windows (Docker Desktop). PowerShell 5.1+.
#
#   irm https://github.com/Fosnie/fosnie/releases/latest/download/install.ps1 | iex
#   # fully local:  & ([scriptblock]::Create((irm .../install.ps1))) -Local
#
# Note: the Firecracker code-interpreter is Linux/KVM only and is unavailable on
# Windows; everything else works.
[CmdletBinding()]
param(
  [switch]$Local,
  [string]$Version = $(if ($env:FOSNIE_VERSION) { $env:FOSNIE_VERSION } else { "latest" }),
  [string]$Dir = "fosnie",
  [string]$OllamaLlm = "qwen3:4b",
  [string]$OllamaEmbed = "hf.co/ggml-org/bge-m3-Q8_0-GGUF:Q8_0"
)
$ErrorActionPreference = "Stop"
$Repo = "Fosnie/fosnie"   # ← swapped at repo-publish time

function AssetUrl($name) {
  if ($Version -eq "latest") { "https://github.com/$Repo/releases/latest/download/$name" }
  else { "https://github.com/$Repo/releases/download/$Version/$name" }
}
function RandHex($bytes) {
  $b = New-Object byte[] $bytes
  [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($b)
  ($b | ForEach-Object { $_.ToString("x2") }) -join ""
}
function RandB64($bytes) {
  $b = New-Object byte[] $bytes
  [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($b)
  [Convert]::ToBase64String($b)
}
function SetKv($path, $key, $val) {
  $lines = Get-Content $path
  if ($lines -match "^$key=") { $lines = $lines -replace "^$key=.*", "$key=$val" }
  else { $lines += "$key=$val" }
  Set-Content -Path $path -Value $lines -Encoding ascii
}

# 1. Preflight
docker compose version *> $null; if ($LASTEXITCODE -ne 0) { throw "Docker Compose v2 not found (need 'docker compose', >= 2.24)" }
docker info *> $null; if ($LASTEXITCODE -ne 0) { throw "the Docker daemon is not running — start Docker Desktop" }

# 2. Download pinned assets
New-Item -ItemType Directory -Force -Path $Dir | Out-Null
Set-Location $Dir
Write-Host "==> downloading pinned compose + env ($Version)..." -ForegroundColor Cyan
Invoke-WebRequest -Uri (AssetUrl "docker-compose.yml") -OutFile "docker-compose.yml"
$keepEnv = Test-Path ".env"
if (-not $keepEnv) { Invoke-WebRequest -Uri (AssetUrl "example.env") -OutFile ".env" }
else { Write-Host "!! .env already exists — keeping it" -ForegroundColor Yellow }

# 3. Secrets
if (-not $keepEnv) {
  Write-Host "==> generating secrets..." -ForegroundColor Cyan
  SetKv ".env" "POSTGRES_PASSWORD" (RandHex 24)
  SetKv ".env" "MESSAGE_ENCRYPTION_KEY" (RandB64 32)
  SetKv ".env" "ML_SHARED_SECRET" (RandHex 32)
  # Strip a single leading "v": image tags are SemVer-normalised (v1.2.0 -> 1.2.0),
  # while the asset-download URLs keep the raw v-prefixed release tag.
  SetKv ".env" "APP_VERSION" ($Version -replace '^v','')
}

# 4. Up
if ($Local) {
  SetKv ".env" "LOCAL_STACK" "1"
  Write-Host "==> starting stack (local profile: Ollama + reranker)..." -ForegroundColor Cyan
  docker compose --profile local pull
  docker compose --profile local up -d
  Write-Host "==> waiting for Ollama, then pulling models (first run only)..." -ForegroundColor Cyan
  for ($i = 0; $i -lt 60; $i++) { docker compose exec -T ollama ollama list *> $null; if ($LASTEXITCODE -eq 0) { break }; Start-Sleep 2 }
  docker compose exec -T ollama ollama pull $OllamaLlm
  docker compose exec -T ollama ollama pull $OllamaEmbed
} else {
  Write-Host "==> starting stack (external inference)..." -ForegroundColor Cyan
  docker compose pull
  docker compose up -d
}

# 5. Wait for health
$port = ((Get-Content ".env" | Where-Object { $_ -match "^HOST_PORT=" }) -replace "^HOST_PORT=", "")
if (-not $port) { $port = "8080" }
Write-Host "==> waiting for the backend on http://localhost:$port ..." -ForegroundColor Cyan
$ready = $false
for ($i = 0; $i -lt 90; $i++) {
  try { Invoke-WebRequest -UseBasicParsing "http://localhost:$port/health" *> $null; $ready = $true; break } catch { Start-Sleep 2 }
}
if ($ready) {
  Write-Host "==> Fosnie is up. Open http://localhost:$port and create the first account (it becomes the admin)." -ForegroundColor Green
  if (-not $Local) { Write-Host "    Then add a model provider under Settings -> Providers to enable chat." }
} else {
  Write-Host "!! backend did not report healthy in time. Check: docker compose logs -f backend" -ForegroundColor Yellow
}
