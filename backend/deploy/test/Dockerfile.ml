# PAI Platform ML/RAG service — TEST image. Build context = repo root.
# This service is a pure HTTP client of the inference stack (no torch / no vLLM
# in the image) → small and fast to rebuild. The native tools below let the
# "beautiful documents" paths actually work on a test box (pandoc + LibreOffice
# + WeasyPrint's native libs); all are optional/graceful, but we want a test
# deploy where everything works.

FROM python:3.12-slim-bookworm

# uv for the pinned (uv.lock) install.
COPY --from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/

# Document toolchain: pandoc (primary DOCX), LibreOffice (DOCX→PDF), the
# Pango/cairo/GDK-PixBuf libs WeasyPrint needs (primary PDF route), and ffmpeg
# (STT transcodes browser Opus/WebM → 16 kHz WAV before the ASR engine).
RUN apt-get update && apt-get install -y --no-install-recommends \
        pandoc \
        libreoffice-writer libreoffice-calc libreoffice-impress \
        libpango-1.0-0 libpangoft2-1.0-0 libcairo2 libgdk-pixbuf-2.0-0 libffi8 \
        fonts-dejavu \
        ffmpeg \
        curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/pai/ml
ENV UV_PROJECT_ENVIRONMENT=/opt/pai/ml/.venv \
    UV_COMPILE_BYTECODE=1 \
    PATH=/opt/pai/ml/.venv/bin:$PATH

# Install deps first (cache layer), then app code.
COPY ml/pyproject.toml ml/uv.lock ./
RUN uv sync --frozen --no-dev --no-install-project
COPY ml/ ./
RUN uv sync --frozen --no-dev
# Chromium for the web-search render fallback (WEB_RENDER_ENABLED=true). The
# SearXNG/DuckDuckGo paths work without it; this covers JS-heavy result pages.
RUN playwright install --with-deps chromium

RUN mkdir -p /var/lib/pai
EXPOSE 8090
# Host networking → bind loopback (matches PAI__ML__BASE_URL=127.0.0.1:8090).
CMD ["uvicorn", "app.main:app", "--host", "127.0.0.1", "--port", "8090"]
