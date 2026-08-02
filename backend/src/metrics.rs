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

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Where each timing metric's buckets fall.
///
/// **A metric absent from this table cannot be plotted as a percentile.** Without
/// declared boundaries a histogram is rendered as a rolling quantile summary, which has
/// no `_bucket` series at all, and a dashboard panel or an alert built on
/// `histogram_quantile` over one returns nothing: not an error, not a zero, nothing. So a
/// panel simply stays blank and an alert simply never fires, which is the worst way for
/// monitoring to fail.
///
/// Two rules about the numbers themselves. Every value a target or an alert threshold is
/// stated in **is an exact boundary**, because a quantile read between boundaries is
/// interpolated, and alerting on an interpolated number means alerting on an estimate of
/// something that was measured exactly. And boundaries ascend, since the exporter takes
/// them as upper bounds in order.
const BUCKETS: &[(&str, MatchKind, &[f64])] = &[
    // Every voice stage shares one ladder, so a stage added later is plottable without
    // touching this table. 0.5 and 0.8 are the documented turn-latency targets.
    (
        "voice_",
        MatchKind::Prefix,
        &[0.05, 0.1, 0.15, 0.2, 0.3, 0.4, 0.5, 0.65, 0.8, 1.0, 1.5, 2.0, 3.0, 5.0],
    ),
    // Loudness, not time: the two values that are the speech and talk-over gates are
    // boundaries, because reading the distribution against them is the whole purpose.
    // A full-name match beats the prefix above, which the exporter documents.
    (
        "voice_frame_rms",
        MatchKind::Full,
        &[0.002, 0.004, 0.008, 0.012, 0.02, 0.035, 0.05, 0.08, 0.12, 0.2, 0.35],
    ),
    // 2.0 is the request-latency alert threshold.
    (
        "http_request_duration_seconds",
        MatchKind::Full,
        &[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 2.5, 5.0, 10.0],
    ),
    // 5.0 is the slow-first-token alert threshold.
    ("llm_ttft_seconds", MatchKind::Full, &[0.1, 0.25, 0.5, 1.0, 2.0, 3.0, 5.0, 8.0, 13.0, 21.0, 34.0]),
    // A reasoning model generates for minutes, so the tail is long on purpose.
    (
        "llm_generation_seconds",
        MatchKind::Full,
        &[0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 30.0, 60.0, 120.0, 300.0, 600.0],
    ),
    // One series spans an embedding call and a whole research run.
    (
        "ml_request_duration_seconds",
        MatchKind::Full,
        &[0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0],
    ),
    // Telephone calls are measured in minutes, not fractions of a second.
    ("telephony_call_seconds", MatchKind::Full, &[5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0]),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchKind {
    Full,
    Prefix,
}

impl MatchKind {
    fn matcher(self, name: &str) -> Matcher {
        match self {
            MatchKind::Full => Matcher::Full(name.to_string()),
            MatchKind::Prefix => Matcher::Prefix(name.to_string()),
        }
    }
}

/// Install the global Prometheus recorder. Idempotent-ish: a second call is a
/// no-op (the first handle wins). Call once at startup, after telemetry init.
pub fn init() {
    let mut builder = PrometheusBuilder::new();
    // Per-metric only. The exporter documents that setting one global bucket set makes
    // every per-metric override inert, so calling the global form here would silently
    // undo this whole table.
    for (name, kind, bounds) in BUCKETS {
        // The only way the call below can fail is an empty boundary list, and taking it
        // out here rather than catching it after keeps the builder, which the call
        // consumes. Warn and carry on: losing one metric's percentiles is not a reason to
        // start with no metrics at all.
        if bounds.is_empty() {
            tracing::warn!(metric = %name, "no bucket boundaries declared; percentiles will be unavailable");
            continue;
        }
        builder = builder
            .set_buckets_for_metric(kind.matcher(name), bounds)
            .expect("the boundary list was just checked to be non-empty");
    }
    match builder.install_recorder() {
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

#[cfg(test)]
mod bucket_tests {
    use super::*;

    fn bounds(name: &str) -> &'static [f64] {
        BUCKETS
            .iter()
            .find(|(n, kind, _)| *n == name && *kind == MatchKind::Full)
            .map(|(_, _, b)| *b)
            .or_else(|| {
                BUCKETS
                    .iter()
                    .find(|(n, kind, _)| *kind == MatchKind::Prefix && name.starts_with(*n))
                    .map(|(_, _, b)| *b)
            })
            .unwrap_or_else(|| panic!("{name} has no declared buckets, so it cannot be plotted"))
    }

    /// A quantile read between two boundaries is interpolated. So a figure anybody
    /// alerts on, or states as a target, has to be a boundary itself: otherwise the
    /// number being compared with the threshold is an estimate of the number that was
    /// measured exactly.
    ///
    /// Each pair here is a value written down somewhere outside this file: in an alert
    /// rule, in a dashboard threshold, or in the deployment notes.
    #[test]
    fn every_stated_threshold_is_an_exact_boundary() {
        for (metric, threshold) in [
            // The documented turn-latency targets.
            ("voice_turn_latency_seconds", 0.5),
            ("voice_turn_latency_seconds", 0.8),
            ("voice_reply_heard_seconds", 0.8),
            // The two loudness gates, which are read off the distribution.
            ("voice_frame_rms", 0.012),
            ("voice_frame_rms", 0.035),
            // The shipped alert thresholds.
            ("http_request_duration_seconds", 2.0),
            ("llm_ttft_seconds", 5.0),
        ] {
            assert!(
                bounds(metric).contains(&threshold),
                "{threshold} is not a boundary of {metric}, so a percentile compared with it is interpolated"
            );
        }
    }

    /// The exporter takes boundaries as ordered upper bounds.
    #[test]
    fn boundaries_ascend_and_are_usable() {
        for (name, _, bounds) in BUCKETS {
            assert!(!bounds.is_empty(), "{name} declares no boundaries");
            for pair in bounds.windows(2) {
                assert!(pair[0] < pair[1], "{name} boundaries are out of order at {pair:?}");
            }
            assert!(bounds.iter().all(|b| b.is_finite() && *b > 0.0), "{name} has an unusable bound");
        }
    }

    /// A metric matched by name must not also be matched by a second name-match, and the
    /// one prefix is only allowed to overlap a full match, which the exporter resolves in
    /// the full match's favour.
    #[test]
    fn no_two_entries_claim_the_same_metric_ambiguously() {
        let full: Vec<&str> =
            BUCKETS.iter().filter(|(_, k, _)| *k == MatchKind::Full).map(|(n, _, _)| *n).collect();
        let mut sorted = full.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), full.len(), "a metric is named twice");

        let prefixes: Vec<&str> =
            BUCKETS.iter().filter(|(_, k, _)| *k == MatchKind::Prefix).map(|(n, _, _)| *n).collect();
        for a in &prefixes {
            for b in &prefixes {
                if a != b {
                    assert!(!a.starts_with(b), "prefix {a} is also matched by {b}");
                }
            }
        }
    }

    /// Every voice stage inherits the shared ladder, so a stage added later is plottable
    /// without anybody remembering to come back here.
    #[test]
    fn a_new_voice_stage_is_plottable_without_a_new_entry() {
        let ladder = bounds("voice_turn_latency_seconds");
        assert_eq!(bounds("voice_some_stage_added_later_seconds"), ladder);
        // ...except the loudness one, which is not a duration and says so by name.
        assert_ne!(bounds("voice_frame_rms"), ladder);
    }
}
