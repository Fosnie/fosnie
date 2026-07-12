//! Real gVisor (`runsc`) sandbox smoke — **Linux, KVM-less**, and only when
//! `runsc` + the OCI rootfs (deploy/firecracker/build-rootfs.sh -> gvisor_rootfs)
//! are deployed. Gated on PAI_GVISOR=1 and `#[cfg(target_os = "linux")]`; it never
//! runs (or compiles a body) on the Windows/macOS dev boxes. This is the deploy-box
//! check for the KVM-less backend that cannot be exercised in dev.
//!
//! Point the sandbox at the built artefacts via env, e.g.:
//!   PAI__CODE_INTERPRETER__GVISOR_ROOTFS=/opt/pai/firecracker/rootfs
//!   PAI__CODE_INTERPRETER__RUNSC_BIN=/usr/local/bin/runsc
//!   PAI_GVISOR=1 cargo test --test gvisor -- --nocapture

#![cfg(target_os = "linux")]

use std::sync::Arc;

use fosnie_backend::code_interpreter::{self, ExecRequest};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{cache, db};

fn enabled() -> bool {
    std::env::var("PAI_GVISOR").as_deref() == Ok("1")
}

async fn state() -> AppState {
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 2).await.unwrap();
    let redis = cache::create_pool(&redis_url).unwrap();
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.features.code_interpreter = true;
    boot.code_interpreter.backend = "gvisor".into(); // force gVisor (box has no KVM anyway)
    boot.code_interpreter_vm.wall_secs = 5; // keep the timeout test snappy
    // gvisor_rootfs / runsc_bin / gvisor_state_dir come from PAI__CODE_INTERPRETER__*.
    AppState::new(pg, redis, Arc::new(boot))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_arithmetic_runs_in_sandbox() {
    if !enabled() {
        return;
    }
    let st = state().await;
    let req = ExecRequest { language: "python".into(), code: "print(2+2)".into(), inputs: vec![] };
    let r = code_interpreter::execute(&st, req).await.expect("sandbox execution");
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
    let r = code_interpreter::execute(&st, req).await.expect("sandbox execution");
    assert_eq!(r.exit_code, 0);
    assert!(r.files.iter().any(|f| f.name == "plot.png" && f.mime == "image/png"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_network_is_blocked() {
    if !enabled() {
        return;
    }
    let st = state().await;
    // No try/except: a reachable network would connect and exit 0; --network=none
    // makes connect() raise, so the process exits non-zero (zero-egress proof).
    let code = "import socket; socket.create_connection(('1.1.1.1', 80), timeout=3)";
    let req = ExecRequest { language: "python".into(), code: code.into(), inputs: vec![] };
    let r = code_interpreter::execute(&st, req).await.expect("sandbox execution");
    assert_ne!(r.exit_code, 0, "outbound socket must fail in a network-less sandbox");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runaway_loop_is_killed_at_wall_secs() {
    if !enabled() {
        return;
    }
    let st = state().await; // wall_secs = 5
    let req =
        ExecRequest { language: "python".into(), code: "while True: pass".into(), inputs: vec![] };
    let r = code_interpreter::execute(&st, req).await.expect("sandbox execution");
    // The guest agent's per-job timeout returns exit_code 124 (TimeoutExpired).
    assert_eq!(r.exit_code, 124, "runaway code must be killed at the wall-clock limit");
}
