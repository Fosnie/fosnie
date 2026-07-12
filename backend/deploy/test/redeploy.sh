#!/usr/bin/env bash
# Fast redeploy loop for the PAI test stack.
#
#   Deploy → click around → find a bug → Claude Code fixes the code → ./redeploy.sh
#
# Only the PLATFORM images (backend + ml) are rebuilt and restarted. The heavy,
# slow-to-start inference services (vLLM main/OCR/embed, reranker), Keycloak and
# the datastores are LEFT RUNNING — so the model weights stay warm in GPU and a
# redeploy is seconds, not the ~minutes a cold model load costs.
#
# Usage:
#   ./redeploy.sh              # rebuild + restart backend and ml (default)
#   ./redeploy.sh backend      # just the Rust backend (+ SPA)
#   ./redeploy.sh ml           # just the Python ML service
#   ./redeploy.sh all          # rebuild EVERYTHING incl. inference (rare; full reset)
#   ./redeploy.sh logs         # tail backend + ml logs
#   ./redeploy.sh down         # stop the whole stack (models unload)
set -euo pipefail
cd "$(dirname "$0")"

COMPOSE=(docker compose --env-file .env -f docker-compose.test.yml)
TARGET="${1:-platform}"

case "$TARGET" in
  platform) SERVICES=(fosnie-backend pai-ml) ;;
  backend)  SERVICES=(fosnie-backend) ;;
  ml)       SERVICES=(pai-ml) ;;
  all)
    echo "▶ Full rebuild incl. inference (this unloads + reloads the models)…"
    "${COMPOSE[@]}" up -d --build
    exit $? ;;
  logs)
    exec "${COMPOSE[@]}" logs -f --tail=120 fosnie-backend pai-ml ;;
  down)
    exec "${COMPOSE[@]}" down ;;
  *)
    echo "unknown target: $TARGET (use: platform|backend|ml|all|logs|down)"; exit 2 ;;
esac

echo "▶ Rebuilding: ${SERVICES[*]}"
"${COMPOSE[@]}" build "${SERVICES[@]}"

echo "▶ Restarting (inference + datastores stay up, models stay warm)…"
"${COMPOSE[@]}" up -d --no-deps "${SERVICES[@]}"

echo "▶ Waiting for backend readiness…"
for i in $(seq 1 30); do
  if curl -fsS http://localhost:8080/health/ready >/dev/null 2>&1; then
    echo "✓ ready — http://localhost:8080  (tunnel 8080+8081 to your laptop)"
    exit 0
  fi
  sleep 2
done
echo "⚠ backend not ready after 60s — check: ./redeploy.sh logs"
exit 1
