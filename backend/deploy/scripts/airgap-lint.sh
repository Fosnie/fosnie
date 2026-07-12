#!/usr/bin/env sh
# airgap-lint — static zero-egress audit of a deployment.
#
# Verifies, WITHOUT sending any traffic, that every egress point is dormant/local:
#   * connectors (web search, DMS, mail, MCP) are disabled — the guard_egress gate;
#   * no remote MCP servers are registered;
#   * provider base_urls (DB overrides) resolve to private/loopback hosts — the ONE
#     egress class not behind guard_egress;
#   * (optional) keycloak.url is internal, and the ML env forces HF_HUB_OFFLINE.
#
# Exit 0 = PASS (air-gap-clean); non-zero = one or more FAILs (count in the summary).
#
# Usage:
#   DATABASE_URL=postgres://…  airgap-lint.sh [--config config.linux.toml] [--ml-env ml.env]
#
# Requires: psql on PATH, DATABASE_URL set.

set -eu

CONFIG_FILE=""
ML_ENV_FILE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --config)  CONFIG_FILE="$2"; shift 2 ;;
    --ml-env)  ML_ENV_FILE="$2"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

FAILS=0
pass() { printf '  PASS  %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; FAILS=$((FAILS + 1)); }
info() { printf '  ..    %s\n' "$1"; }

if [ -z "${DATABASE_URL:-}" ]; then
  echo "airgap-lint: DATABASE_URL is required" >&2
  exit 2
fi
command -v psql >/dev/null 2>&1 || { echo "airgap-lint: psql not found on PATH" >&2; exit 2; }

q() { psql "$DATABASE_URL" -tAqc "$1" 2>/dev/null || echo "ERR"; }

# A host is "private" if loopback, RFC1918/CGNAT, .internal/.local, or a bare hostname
# (no dots — a service name on the compose/K8s network). Anything else is public.
is_private_host() {
  h=$(printf '%s' "$1" | sed -E 's#^[a-zA-Z]+://##; s#[:/].*$##')
  case "$h" in
    localhost|127.*|::1|10.*|192.168.*|172.1[6-9].*|172.2[0-9].*|172.3[0-1].*) return 0 ;;
    100.6[4-9].*|100.[7-9][0-9].*|100.1[0-1][0-9].*|100.12[0-7].*)             return 0 ;;
    *.internal|*.local|*.svc|*.svc.cluster.local)                              return 0 ;;
    *.*) return 1 ;;   # has a dot and matched nothing above → public FQDN/IP
    *)   return 0 ;;   # no dot → compose/K8s service name
  esac
}

echo "airgap-lint — zero-egress static audit"
echo

# ---------------------------------------------------------------------------
echo "[1] Connector egress gates (guard_egress; dormant by default)"
ENABLED=$(q "SELECT key FROM config_settings WHERE key LIKE 'integration.%.enabled' AND value = 'true';")
if [ "$ENABLED" = "ERR" ]; then
  fail "could not query config_settings (check DATABASE_URL / migrations)"
elif [ -z "$ENABLED" ]; then
  pass "no integration.<kind>.enabled is true (all connectors dormant)"
else
  for k in $ENABLED; do fail "connector enabled: $k"; done
fi

# ---------------------------------------------------------------------------
echo "[2] Remote MCP servers"
MCP=$(q "SELECT count(*) FROM mcp_servers WHERE COALESCE(requires_egress, false) = true;")
if [ "$MCP" = "ERR" ]; then
  info "mcp_servers table absent or unreadable — skipped"
elif [ "$MCP" = "0" ]; then
  pass "no egress-requiring MCP servers registered"
else
  fail "$MCP MCP server(s) require egress"
fi

# ---------------------------------------------------------------------------
echo "[3] Provider base_urls (NOT behind guard_egress — verify each is private)"
BASE_URLS=$(q "
  SELECT base_url FROM provider_configs WHERE base_url IS NOT NULL AND base_url <> ''
  UNION ALL
  SELECT embed_base_url  FROM embedding_index WHERE embed_base_url  IS NOT NULL AND embed_base_url  <> ''
  UNION ALL
  SELECT desired_base_url FROM embedding_index WHERE desired_base_url IS NOT NULL AND desired_base_url <> '';
")
if [ "$BASE_URLS" = "ERR" ]; then
  info "provider_configs/embedding_index not readable — skipped"
elif [ -z "$BASE_URLS" ]; then
  pass "no provider base_url overrides in the database (ML .env defaults apply)"
else
  ANY_PUBLIC=0
  for u in $BASE_URLS; do
    if is_private_host "$u"; then
      info "private provider base_url: $u"
    else
      fail "PUBLIC provider base_url: $u (egress bypasses guard_egress)"
      ANY_PUBLIC=1
    fi
  done
  [ "$ANY_PUBLIC" = "0" ] && pass "all provider base_urls resolve to private/loopback hosts"
fi

# ---------------------------------------------------------------------------
echo "[4] Backend config (optional --config)"
if [ -n "$CONFIG_FILE" ] && [ -f "$CONFIG_FILE" ]; then
  KC=$(grep -iE '^\s*url\s*=' "$CONFIG_FILE" | grep -i keycloak -A0 || true)
  KCURL=$(grep -iEA10 '^\s*\[keycloak\]' "$CONFIG_FILE" | grep -iE '^\s*url\s*=' | head -1 | sed -E 's/.*=\s*"?([^"]*)"?.*/\1/' || true)
  if [ -n "$KCURL" ]; then
    if is_private_host "$KCURL"; then pass "keycloak.url is internal: $KCURL"
    else fail "keycloak.url is public: $KCURL"; fi
  else
    info "no keycloak.url in $CONFIG_FILE (local auth?)"
  fi
else
  info "no --config supplied — skipped keycloak.url check"
fi

# ---------------------------------------------------------------------------
echo "[5] ML offline guard (optional --ml-env)"
if [ -n "$ML_ENV_FILE" ] && [ -f "$ML_ENV_FILE" ]; then
  if grep -qE '^\s*HF_HUB_OFFLINE\s*=\s*1' "$ML_ENV_FILE"; then
    pass "HF_HUB_OFFLINE=1 set (stray model pulls fail loudly)"
  else
    fail "HF_HUB_OFFLINE=1 not set in $ML_ENV_FILE (fastembed Qdrant/bm25 could fetch)"
  fi
else
  info "no --ml-env supplied — skipped HF_HUB_OFFLINE check"
fi

echo
if [ "$FAILS" -eq 0 ]; then
  echo "RESULT: PASS — air-gap-clean (0 failures)"
  exit 0
else
  echo "RESULT: FAIL — $FAILS check(s) failed; the deployment is NOT air-gap-clean"
  exit 1
fi
