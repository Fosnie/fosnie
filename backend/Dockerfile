# Fosnie backend — PRODUCTION image. Build context = the `fosnie-core/` repo root.
#
#   docker build -f backend/Dockerfile -t fosnie-backend:latest .
#
# Multi-stage: build the React SPA (Core edition), build the Rust binary with sqlx
# OFFLINE (no DB needed at build time — uses backend/.sqlx), ship a slim runtime
# that serves BOTH the JSON/WebSocket API and the SPA bundle from one port. There
# is no separate frontend container (07-DEPLOYMENT.md §2). Migrations are embedded
# at compile time (sqlx::migrate!) and run on boot, so there is no migrate step.

# ── 1. Frontend (React 19 + Vite) — Core edition ─────────────────────────────
# No EDITION_ENTRY ⇒ the `@edition` alias resolves to the Core stub
# (src/ext/edition-empty.ts): a pure Core bundle with no Enterprise sections.
FROM node:22-bookworm-slim AS frontend
WORKDIR /app/frontend
COPY frontend/package.json frontend/package-lock.json ./
# `npm install`, not `npm ci`: the committed lockfile is generated on Windows and
# omits linux-only optional deps (the tailwind v4 oxide / rollup native binaries
# and their @emnapi wasm-runtime fallback), so a strict `npm ci` fails on linux
# with "Missing @emnapi/... from lock file". `npm install` reconciles the missing
# platform deps at build time.
RUN npm install --no-audit --no-fund
COPY frontend/ ./
RUN npm run build          # → /app/frontend/dist

# ── 2. Rust backend ──────────────────────────────────────────────────────────
FROM rust:1-bookworm AS build
WORKDIR /app/backend
ENV SQLX_OFFLINE=true
# Cache deps: copy manifests + the committed query cache first, then sources.
COPY backend/Cargo.toml backend/Cargo.lock ./
COPY backend/.sqlx ./.sqlx
COPY backend/ ./
RUN cargo build --release --bin fosnie-backend

# ── 3. Runtime (slim; rustls TLS uses the OS trust store) ────────────────────
FROM debian:bookworm-slim AS runtime
# ca-certificates: rustls verifies outbound TLS (to configured providers) against
# the OS trust store — REQUIRED even though the image itself makes no calls by
# default. curl: the container HEALTHCHECK.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /var/lib/pai pai

COPY --from=build    /app/backend/target/release/fosnie-backend /usr/local/bin/fosnie-backend
COPY --from=frontend /app/frontend/dist                          /opt/pai/frontend/dist
# Default-skill library (read-only source the boot seeder copies into skills_dir).
COPY skills/ /opt/pai/skills/

# Image-baked config so the container runs with only DATABASE_URL / REDIS_URL /
# ML base+secret + secrets supplied by compose. Everything here is a sensible
# container default; override any of it with PAI__* env if needed.
ENV PAI__SERVER__HOST=0.0.0.0 \
    PAI__SERVER__PORT=8080 \
    PAI__SERVER__STATIC_DIR=/opt/pai/frontend/dist \
    PAI__STORAGE__SKILLS_LIBRARY_DIR=/opt/pai/skills

# WORKDIR under the persisted volume: the default relative storage paths
# (./data/*) and the runtime skills dir land here and survive restarts.
WORKDIR /var/lib/pai
RUN mkdir -p /var/lib/pai/data && chown -R pai:pai /var/lib/pai
USER pai

EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=5s --start-period=40s --retries=5 \
    CMD curl -fsS http://localhost:8080/health || exit 1

LABEL org.opencontainers.image.source="https://github.com/Fosnie/fosnie" \
      org.opencontainers.image.title="fosnie-backend" \
      org.opencontainers.image.description="Fosnie backend (API + WebSocket + SPA)" \
      org.opencontainers.image.licenses="Apache-2.0"

ENTRYPOINT ["/usr/local/bin/fosnie-backend"]
