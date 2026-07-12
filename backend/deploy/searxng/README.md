# SearXNG — dev SERP layer for the web-search connector

Web search is a **dormant opt-in outbound connector**. When enabled, the LLM sees one `web_search` tool;
Rust gates it through `guard_egress` and makes a single `POST /web_search` to the
ML service, which runs the search pipeline server-side: SERP via **SearXNG** →
SSRF-hardened paced fetch → trafilatura extraction → rerank → digest + citations.

SearXNG is the **only** v1 SERP provider (resolved decision 1 — no paid APIs).
Queries reach the upstream engines unauthenticated and unattributed. Running the
container causes no egress by itself; nothing calls it until the connector is
explicitly enabled.

## Start (dev)

```
docker compose -f backend/deploy/docker-compose.dev.yml up -d searxng
```

Verify the JSON API (a 403 here means `search.formats` lacks `json` in
`settings.yml`):

```
curl "http://localhost:8888/search?q=test&format=json"
```

## Contract the platform targets

- `GET {SEARXNG_BASE_URL}/search?q=…&format=json[&time_range=day|week|month|year][&pageno=1]`
  → `{ "results": [ { "url", "title", "content", "publishedDate"?, "engine" }, … ] }`

## Configuration

- `settings.yml` (mounted read-only into the container): enables the JSON format,
  disables the built-in limiter (the ML service is the single client; politeness
  pacing towards upstream engines lives in `ml/app/web/pacing.py`), and enables
  the reliable engine basket — DuckDuckGo, Mojeek, Startpage, Qwant, Wikipedia.
  Google stays best-effort (it TLS-fingerprints and blocks instances).
- ML service: `SEARXNG_BASE_URL=http://localhost:8888` (see `ml/.env.example`,
  `WEB_*` settings in `ml/app/config.py`).
- Enable switch: the runtime config row `integration.web_search.enabled`
  (super-admin `PUT /api/admin/integrations/web_search`). Absent ⇒ dormant ⇒ the
  tool call is refused and audited (`integration.blocked`).

## Fallback search

When SearXNG returns nothing (engines blocked/down), the ML service falls back to
DuckDuckGo HTML parsing through the same SSRF-guarded, paced fetcher — and, only
if `WEB_RENDER_ENABLED=true` and Chromium is installed (`playwright install
chromium`), to a Playwright-driven search. Rendering is **off by default**; the
service boots and tests pass without Playwright/Chromium.

## Production notes

- Generate a fresh `server.secret_key` (the committed value is dev-only).
- Quiet on-prem egress IPs last; hyperscaler IPs get engine-blocked quickly.
- The instance should not be reachable from anywhere except the ML service.
