#!/usr/bin/env bash
# Build the two Fosnie application images locally — the offline equivalent of the
# GHCR release workflow. The prod docker-compose.yml works with these local tags,
# so nothing depends on a registry for testing.
#
#   release/build-images.sh                 # tags :latest (+ git sha)
#   APP_VERSION=v1.2.0 release/build-images.sh
#   ORG=myorg release/build-images.sh       # tag under ghcr.io/myorg/...
#
# Context is the repo root (this file lives in release/). Build for the host arch;
# pass PLATFORM=linux/amd64,linux/arm64 to cross-build with buildx.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ORG="${ORG:-OWNER}"
APP_VERSION="${APP_VERSION:-latest}"
SHA="$(git rev-parse --short HEAD 2>/dev/null || echo nogit)"
PLATFORM="${PLATFORM:-}"

build() { # build <image> <dockerfile>
  image="ghcr.io/$ORG/$1"
  echo "==> building $image:$APP_VERSION ($2)"
  if [ -n "$PLATFORM" ]; then
    docker buildx build --platform "$PLATFORM" \
      -f "$2" -t "$image:$APP_VERSION" -t "$image:$SHA" --load .
  else
    docker build -f "$2" -t "$image:$APP_VERSION" -t "$image:$SHA" .
  fi
}

build fosnie-backend backend/Dockerfile
build fosnie-ml      ml/Dockerfile

echo
echo "Built:"
echo "  ghcr.io/$ORG/fosnie-backend:$APP_VERSION  (+ :$SHA)"
echo "  ghcr.io/$ORG/fosnie-ml:$APP_VERSION       (+ :$SHA)"
echo
echo "Run with the prod compose (from backend/deploy/):"
echo "  ORG=$ORG APP_VERSION=$APP_VERSION  # then edit the image org in docker-compose.yml or export it"
echo "  docker compose up -d"
