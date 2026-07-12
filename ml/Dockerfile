# Fosnie ML/RAG service — PRODUCTION image. Build context = the `fosnie-core/` repo root.
#
#   docker build -f ml/Dockerfile -t fosnie-ml:latest .
#
# This service is a pure HTTP client of the inference stack (no torch / no vLLM in
# the image) → small and fast. The one runtime model download is fastembed's
# `Qdrant/bm25` sparse model (sparse.py); we WARM it at build time into the HF
# cache so the runtime can run fully offline (HF_HUB_OFFLINE=1) with no first-use
# network reach. The document toolchain (pandoc + LibreOffice + WeasyPrint's
# native libs) + ffmpeg make the "beautiful documents" and voice-transcode paths
# work out of the box; all are optional/graceful.

FROM python:3.12-slim-bookworm

# uv for the pinned (uv.lock) install.
COPY --from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/

RUN apt-get update && apt-get install -y --no-install-recommends \
        pandoc \
        libreoffice-writer libreoffice-calc libreoffice-impress \
        libpango-1.0-0 libpangoft2-1.0-0 libcairo2 libgdk-pixbuf-2.0-0 libffi8 \
        fonts-dejavu \
        ffmpeg \
        curl ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /var/lib/pai pai

WORKDIR /opt/pai/ml
ENV UV_PROJECT_ENVIRONMENT=/opt/pai/ml/.venv \
    UV_COMPILE_BYTECODE=1 \
    PATH=/opt/pai/ml/.venv/bin:$PATH \
    HF_HOME=/opt/pai/models \
    FASTEMBED_CACHE_PATH=/opt/pai/models

# Install deps first (cache layer), then app code.
COPY ml/pyproject.toml ml/uv.lock ./
RUN uv sync --frozen --no-dev --no-install-project
COPY ml/ ./
RUN uv sync --frozen --no-dev

# Warm the fastembed BM25 sparse model so the runtime — pinned HF_HUB_OFFLINE=1 by
# the deploy env — never reaches the network on first use. NOTE: offline is NOT set
# here so the download can happen. fastembed ignores HF_HOME; it only respects
# FASTEMBED_CACHE_PATH (default /tmp/fastembed_cache), hence that env above.
RUN python -c "from fastembed import SparseTextEmbedding; SparseTextEmbedding(model_name='Qdrant/bm25').embed(['warm'])" \
    && chown -R pai:pai /opt/pai/models

RUN mkdir -p /var/lib/pai && chown -R pai:pai /var/lib/pai /opt/pai/ml
USER pai

EXPOSE 8090
HEALTHCHECK --interval=15s --timeout=5s --start-period=30s --retries=5 \
    CMD curl -fsS http://localhost:8090/health || exit 1

LABEL org.opencontainers.image.source="https://github.com/Fosnie/fosnie" \
      org.opencontainers.image.title="fosnie-ml" \
      org.opencontainers.image.description="Fosnie ML/RAG service (agentic RAG, extraction, embeddings, generation)" \
      org.opencontainers.image.licenses="Apache-2.0"

# Bind 0.0.0.0 so the backend service can reach it on the compose network.
CMD ["uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8090"]
