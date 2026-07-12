//! Extension-surface proof for the scheduler job-registry seam.
//!
//! A `FakeJobRegistrar` injected via `AppStateBuilder::with_jobs` is consumed by
//! the scheduler boot path (`state.jobs.register(&mut reg)`), and its task handler
//! runs — proving the registry slot is wired. An unknown key resolves to `None`
//! (the worker dead-letters such tasks instead of crashing). A second test checks
//! the Core default registrar registers the host set. No DB: lazy pool, no I/O.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;

use fosnie_backend::config::BootConfig;
use fosnie_backend::ext::JobRegistrar;
use fosnie_backend::scheduler::registry::{BoxFut, PeriodicFn};
use fosnie_backend::scheduler::{CoreJobs, JobRegistry, TaskHandler};
use fosnie_backend::state::{AppState, AppStateBuilder};

struct FakeTaskHandler {
    calls: Arc<AtomicU32>,
}

#[async_trait]
impl TaskHandler for FakeTaskHandler {
    async fn handle(&self, _state: &AppState, _payload: &serde_json::Value) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct FakeJobRegistrar {
    calls: Arc<AtomicU32>,
}

impl JobRegistrar for FakeJobRegistrar {
    fn register(&self, reg: &mut JobRegistry) {
        let job: PeriodicFn = Arc::new(|_s: AppState| -> BoxFut { Box::pin(async {}) });
        reg.register_periodic_job("0 0 3 * * *", job);
        reg.register_task_handler("fake_task", Arc::new(FakeTaskHandler { calls: self.calls.clone() }));
    }
}

fn lazy_state(jobs: Arc<dyn JobRegistrar>) -> AppState {
    let pg = PgPoolOptions::new().connect_lazy("postgres://localhost/pai_test").expect("lazy pg pool");
    let redis = fosnie_backend::cache::create_pool("redis://localhost:6379").expect("redis pool");
    AppStateBuilder::new(pg, redis, Arc::new(BootConfig::default()))
        .with_jobs(jobs)
        .build()
}

#[tokio::test]
async fn injected_registrar_is_consumed() {
    let calls = Arc::new(AtomicU32::new(0));
    let state = lazy_state(Arc::new(FakeJobRegistrar { calls: calls.clone() }));

    let mut reg = JobRegistry::default();
    state.jobs.register(&mut reg);

    assert!(!reg.periodic.is_empty(), "the boot path consumed the injected registrar's periodic job");
    let handler = reg.task_handler("fake_task").expect("fake_task handler registered");
    handler.handle(&state, &serde_json::json!({})).await.expect("handler runs");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the registered task handler was invoked");
    assert!(reg.task_handler("nope").is_none(), "unknown task_type → None (worker dead-letters, no panic)");
}

#[tokio::test]
async fn core_jobs_register_host_set() {
    let mut reg = JobRegistry::default();
    CoreJobs.register(&mut reg);
    assert_eq!(reg.periodic.len(), 3, "Core host periodic jobs (audit-retention, mcp-health, artefact-cleanup); checkpoint + moderation-retention are Enterprise");
    assert!(reg.task_handler("audit_checkpoint").is_none(), "checkpoint is an Enterprise-only task handler — Core registers none");
}
