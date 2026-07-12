// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Prometheus metrics. A single process-global recorder is installed at boot;
//! instrumentation elsewhere uses the `metrics` macros (`counter!`, `histogram!`,
//! `gauge!`) with no handle threading. `GET /metrics` renders the text format.

use std::sync::OnceLock;

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Install the global Prometheus recorder. Idempotent-ish: a second call is a
/// no-op (the first handle wins). Call once at startup, after telemetry init.
pub fn init() {
    match PrometheusBuilder::new().install_recorder() {
        Ok(handle) => {
            let _ = HANDLE.set(handle);
        }
        Err(e) => tracing::warn!(error = %e, "could not install Prometheus recorder; /metrics will be empty"),
    }
}

/// Render the current metrics in Prometheus text exposition format.
pub fn render() -> String {
    HANDLE.get().map(|h| h.render()).unwrap_or_default()
}

/// Spawn a background task that publishes this process's resource usage
/// (resident/virtual memory, CPU%) as Prometheus gauges every 10s — operational
/// observability for remote servicing, distinct from the compliance audit log.
/// A background tick (not per-scrape) keeps the scrape fast and gives CPU% a
/// delta interval. Cross-platform via `sysinfo` (Linux/macOS/Windows). Refreshes
/// only the current PID, so it is cheap.
pub fn spawn_process_collector() {
    use sysinfo::{get_current_pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let Ok(pid) = get_current_pid() else {
        tracing::warn!("process metrics disabled: cannot resolve current pid");
        return;
    };

    tokio::spawn(async move {
        let mut sys = System::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        let kind = ProcessRefreshKind::nothing().with_cpu().with_memory();
        loop {
            interval.tick().await;
            sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, kind);
            if let Some(proc) = sys.process(pid) {
                metrics::gauge!("process_resident_memory_bytes").set(proc.memory() as f64);
                metrics::gauge!("process_virtual_memory_bytes").set(proc.virtual_memory() as f64);
                metrics::gauge!("process_cpu_usage_percent").set(proc.cpu_usage() as f64);
            }
        }
    });
}

/// Spawn a background task publishing datastore + durable-task-queue health as
/// gauges every 15s: connection-pool saturation, datastore ping latency, and the
/// task-queue depth by status. Post-deploy observability — the operator alerts on
/// pool exhaustion, a slow datastore, or a backing-up / dead-lettering queue.
pub fn spawn_runtime_collector(pg: sqlx::PgPool, redis: deadpool_redis::Pool) {
    // The durable-task statuses (the `task_status` enum) — pre-zeroed each tick so a
    // drained status reads 0 rather than a stale value.
    const TASK_STATUSES: [&str; 5] = ["queued", "running", "succeeded", "failed", "dead_letter"];
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            interval.tick().await;

            // Connection-pool saturation.
            metrics::gauge!("db_pool_connections", "state" => "size").set(pg.size() as f64);
            metrics::gauge!("db_pool_connections", "state" => "idle").set(pg.num_idle() as f64);
            let rs = redis.status();
            metrics::gauge!("redis_pool_connections", "state" => "size").set(rs.size as f64);
            metrics::gauge!("redis_pool_connections", "state" => "available").set(rs.available as f64);

            // Datastore responsiveness (a SELECT 1 / PING round-trip).
            let t = std::time::Instant::now();
            if crate::db::ping(&pg).await {
                metrics::gauge!("db_ping_seconds").set(t.elapsed().as_secs_f64());
            }
            let t = std::time::Instant::now();
            if crate::cache::ping(&redis).await {
                metrics::gauge!("redis_ping_seconds").set(t.elapsed().as_secs_f64());
            }

            // Durable-task queue depth by status (runtime query → no .sqlx churn).
            for s in TASK_STATUSES {
                metrics::gauge!("task_queue_depth", "status" => s).set(0.0);
            }
            if let Ok(rows) = sqlx::query_as::<_, (String, i64)>(
                "SELECT status::text, count(*) FROM tasks GROUP BY status",
            )
            .fetch_all(&pg)
            .await
            {
                for (status, n) in rows {
                    metrics::gauge!("task_queue_depth", "status" => status).set(n as f64);
                }
            }
        }
    });
}
