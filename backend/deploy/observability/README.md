# Observability — operator wiring guide

After deploy, the platform is watched through **metrics + logs + the audit trail**. This
directory holds the Prometheus + Grafana assets; the audit trail is queried via the admin
API (below). Everything stays inside the perimeter — no egress.

## Metrics (Prometheus + Grafana)

- The **backend** exposes `GET /metrics` on `:8080` (Prometheus text), **fail-closed**:
  it is **disabled (404) until `observability.metrics_token` is set**, then requires it
  (Bearer or `?token=`, constant-time checked). The **ML service** exposes `GET /metrics`
  on `:8090`, gated by its shared secret — a scraper presents it as `Authorization: Bearer
  <ml.shared_secret>` (Prometheus can't send the `X-PAI-ML-Key` header the backend uses).
  Both ports are loopback-only; **never proxy `/metrics`** to the public gateway.
- Add [`prometheus-scrape.yml`](./prometheus-scrape.yml) to your `prometheus.yml`
  `scrape_configs:`.
- Add [`alerts.yml`](./alerts.yml) to `rule_files:` (golden-signal + platform alerts).
- Import [`grafana-dashboard.json`](./grafana-dashboard.json) into Grafana (pick your
  Prometheus datasource on import).

### Key series

**A percentile needs declared buckets.** A histogram with no bucket boundaries is exported
as a rolling quantile summary, which has no `_bucket` series, so `histogram_quantile` over
it returns nothing at all: the panel is blank and the alert never fires, with no error
anywhere. The boundaries live in one table in the backend's metrics module, and every value
a target or an alert threshold is stated in is an exact boundary of the metric it applies
to. A new timing metric that wants a percentile needs an entry there.

| Area | Series |
| --- | --- |
| HTTP | `http_requests_total{method,route,status}`, `http_request_duration_seconds{…,status}` |
| LLM | `llm_ttft_seconds`, `llm_generation_seconds`, `llm_tokens_total{kind,model}`, `chat_turns_total` |
| ML upstream | `ml_request_duration_seconds{op}` (backend→ML), `ml_request_seconds{path}` (ML self) |
| Voice | Per stage of a turn, all `{transport="browser"\|"phone"}`: `voice_endpoint_seconds` (speaker stopped → turn ended), `voice_turn_detect_seconds` + `voice_turn_detect_total{outcome}`, `voice_stt_commit_seconds`, `voice_commit_overhead_seconds`, `voice_first_token_seconds`, `voice_first_clause_seconds`, `voice_tts_open_seconds`, `voice_turn_latency_seconds` (→ first reply audio handed to the transport). Plus `voice_barge_in_total`, `voice_turn_forced_total`, and `voice_frame_rms{transport,phase}` — the loudness distribution the two speech gates are set from. Batch speech-to-text on the fallback path is `ml_request_duration_seconds{op="transcribe"}` rather than a series of its own. |
| Telephony | `voice_reply_heard_seconds{transport}` / `voice_reply_spoken_seconds{transport}` — when the **caller** began and finished hearing a reply, which is later than `voice_turn_latency_seconds` by the pacing, the socket and the carrier's own buffer. Also `telephony_calls_total`, `telephony_call_seconds`, `telephony_refused_total{reason}`, `telephony_clear_total`, `telephony_frames_out_total`, `telephony_error_total`, `telephony_mark_timeout_total`. |
| Tasks | `task_queue_depth{status}`, `task_runs_total{type,outcome}` |
| Datastores | `db_pool_connections{state}`, `redis_pool_connections{state}`, `db_ping_seconds`, `redis_ping_seconds` |
| WebSocket | `ws_connections`, `ws_origin_rejected_total` |
| Client (SPA) | `client_errors_total{kind}` — the SPA self-reports browser errors to `POST /api/telemetry` (intra-perimeter; logged + metered, never forwarded outward) |
| Audit | `audit_anomaly_total{action}` |
| Process | `process_resident_memory_bytes`, `process_cpu_usage_percent` |

## Health probes

- `GET /health` — liveness (always 200 if the process is up).
- `GET /health/ready` — readiness; 200 only when Postgres **and** Redis answer (503 otherwise,
  with a per-dependency breakdown). Use it for the load balancer / systemd / a blackbox probe.

## Logs

Set `observability.log_format = "json"` for structured logs to stdout, then ship them
(journald → Loki / your aggregator). The backend never logs message content, tokens, or
secrets. **Retention is the log driver's responsibility** (journald `SystemMaxUse`, Docker
`max-size`, …) — set it to your compliance window.

## Audit trail (compliance — not Prometheus)

- `GET /api/admin/audit?action=&actor_role=&resource_type=&before_seq=&limit=` — filtered search.
- `GET /api/admin/anomalies` — recent risk-flagged events (break-glass use, agent-run starts, …).
- `GET /api/admin/audit/export` — the full SHA-256 hash-chain + an integrity-verification result +
  the Ed25519 public key (a tamper-evidence package). Admin / break-glass gated; every read is
  itself audited.

## Dependency scan

[`../scripts/security-audit.sh`](../scripts/security-audit.sh) (`cargo audit` + `npm audit`) —
run in CI and before a release.
