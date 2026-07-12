//! Real Firecracker microVM smoke — **Linux + KVM only**, and only when the
//! rootfs/kernel/guest-agent are deployed (deploy/firecracker/README.md). Gated
//! on PAI_FIRECRACKER=1 and `#[cfg(target_os = "linux")]`; it never runs (or even
//! compiles a body) on the Windows/macOS dev boxes. This is the deploy-box check
//! for the VMM path that cannot be exercised in dev.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use fosnie_backend::code_interpreter::{self, ExecRequest};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{cache, db};

fn enabled() -> bool {
    std::env::var("PAI_FIRECRACKER").as_deref() == Ok("1")
}

async fn state() -> AppState {
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 2).await.unwrap();
    let redis = cache::create_pool(&redis_url).unwrap();
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.features.code_interpreter = true; // VM paths come from PAI__CODE_INTERPRETER_VM__* / config file
    AppState::new(pg, redis, Arc::new(boot))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_arithmetic_runs_in_microvm() {
    if !enabled() {
        return;
    }
    let st = state().await;
    let req = ExecRequest { language: "python".into(), code: "print(2+2)".into(), inputs: vec![] };
    let r = code_interpreter::execute(&st, req).await.expect("vm execution");
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.stdout.trim(), "4");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matplotlib_produces_png_artefact() {
    if !enabled() {
        return;
    }
    let st = state().await;
    let code = "import matplotlib; matplotlib.use('Agg'); import matplotlib.pyplot as plt; \
                plt.plot([1,2,3]); plt.savefig('plot.png')";
    let req = ExecRequest { language: "python".into(), code: code.into(), inputs: vec![] };
    let r = code_interpreter::execute(&st, req).await.expect("vm execution");
    assert_eq!(r.exit_code, 0);
    assert!(r.files.iter().any(|f| f.name == "plot.png" && f.mime == "image/png"));
}
