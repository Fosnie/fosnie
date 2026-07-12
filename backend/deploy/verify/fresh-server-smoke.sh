#!/usr/bin/env bash
# Fosnie fresh-server smoke test — the automatable half of fresh-server-checklist.md.
# Run it from the install directory (where docker-compose.yml + .env live), after
# install.sh has brought the stack up.
#
#   ./fresh-server-smoke.sh            # external-inference deploy
#   ./fresh-server-smoke.sh --local    # fully-local deploy (asserts the local profile)
#
# Mechanical checks only (health, port surface, secret perms, container health, log
# hygiene, restart survival, upgrade idempotence). The interactive UX checks (chat
# answers, ingest+citation, 2FA) stay manual — see the checklist.
set -u

LOCAL=0
[ "${1:-}" = "--local" ] && LOCAL=1
HOST_PORT="$(grep -E '^HOST_PORT=' .env 2>/dev/null | cut -d= -f2)"; HOST_PORT="${HOST_PORT:-8080}"
COMPOSE="docker compose"
[ "$LOCAL" = "1" ] && COMPOSE="docker compose --profile local"

pass=0; fail=0
ok()   { printf '\033[1;32mPASS\033[0m %s\n' "$1"; pass=$((pass+1)); }
no()   { printf '\033[1;31mFAIL\033[0m %s\n' "$1"; fail=$((fail+1)); }
info() { printf '\033[1;36m==>\033[0m %s\n' "$1"; }

wait_health() {
  i=0; while [ "$i" -lt 90 ]; do
    curl -fsS "http://localhost:$HOST_PORT/health" >/dev/null 2>&1 && return 0
    i=$((i+1)); sleep 2
  done
  return 1
}

# 1. compose file is valid
if $COMPOSE config >/dev/null 2>&1; then ok "docker compose config is valid"; else no "docker compose config is invalid"; fi

# 2. backend healthy
if wait_health; then ok "backend /health returns 200"; else no "backend never became healthy"; fi

# 3. all containers healthy/running (no exited/unhealthy)
bad="$($COMPOSE ps --format '{{.Name}} {{.State}} {{.Status}}' 2>/dev/null | grep -Ei 'exited|unhealthy|restarting' || true)"
if [ -z "$bad" ]; then ok "all containers running/healthy"; else no "unhealthy containers:"; printf '     %s\n' "$bad"; fi

# 4. only the frontend/backend port is published to the host
published="$(docker compose ps --format '{{.Publishers}}' 2>/dev/null | tr ',' '\n' | grep -Eo '0\.0\.0\.0:[0-9]+|:::[0-9]+|127\.0\.0\.1:[0-9]+' | grep -Eo '[0-9]+$' | sort -u | grep -v '^$' || true)"
extra="$(printf '%s\n' "$published" | grep -v "^${HOST_PORT}$" || true)"
if [ -z "$extra" ]; then ok "only host port $HOST_PORT is published"; else no "unexpected published host ports: $(echo "$extra" | tr '\n' ' ')"; fi

# 5. .env secret perms
perm="$(stat -c '%a' .env 2>/dev/null || stat -f '%Lp' .env 2>/dev/null || echo '?')"
if [ "$perm" = "600" ]; then ok ".env is mode 600"; else no ".env perms are $perm (expected 600)"; fi

# 6. no panics / tracebacks in the logs
if $COMPOSE logs --no-color 2>/dev/null | grep -Eiq "panicked at|thread 'main' panicked|Traceback \(most recent call last\)"; then
  no "panics/tracebacks found in logs (grep 'panicked'/'Traceback')"
else ok "no panics/tracebacks in logs"; fi

# 7. local profile: the seeded providers exist
if [ "$LOCAL" = "1" ]; then
  n="$(docker compose exec -T postgres psql -U pai -d pai -tAc "SELECT count(*) FROM provider_configs WHERE scope='deployment' AND role IN ('llm','embed','rerank')" 2>/dev/null | tr -d '[:space:]')"
  if [ "${n:-0}" = "3" ]; then ok "LOCAL_STACK seeded 3 provider rows"; else no "expected 3 seeded provider rows, found ${n:-0}"; fi
fi

# 8. airgap-lint if present (only meaningful with the repo checked out)
if [ -x ../scripts/airgap-lint.sh ]; then
  if ../scripts/airgap-lint.sh >/dev/null 2>&1; then ok "airgap-lint PASS"; else no "airgap-lint reported egress"; fi
else info "airgap-lint.sh not present (repo not checked out) — skipping"; fi

# 9. restart survival (proxy for a host reboot)
info "restart survival: down && up -d …"
$COMPOSE down >/dev/null 2>&1
$COMPOSE up -d >/dev/null 2>&1
if wait_health; then ok "stack healthy again after restart"; else no "stack did not recover after restart"; fi

# 10. upgrade idempotence
info "docker compose pull (idempotence check)…"
if $COMPOSE pull >/dev/null 2>&1; then ok "docker compose pull succeeded"; else no "docker compose pull failed"; fi

echo
info "smoke result: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
