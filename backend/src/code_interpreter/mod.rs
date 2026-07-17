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

//! Code interpreter. The model writes Python;
//! the backend runs it in a **zero-egress Firecracker microVM** (restore a
//! stateless snapshot → execute → destroy) and turns any output files into
//! chat-scoped generated artefacts. A model tool, not a user IDE.
//!
//! This module is the **cross-platform seam**: the executor trait, the
//! request/result types, and `run_and_store` (execute → persist artefacts →
//! audit → format) all compile and test everywhere. The real sandbox backends
//! are `#[cfg(target_os = "linux")]`: [`firecracker`] (KVM microVM, strongest
//! tier) and [`gvisor`] (`runsc`, systrap — no KVM, for locked/VM-guest hosts).
//! [`select_executor`] tiers between them; on any non-Linux host or a host with
//! neither, it returns [`UnavailableExecutor`]. There is no host-subprocess
//! fallback — running model code on the host is exactly the threat this design
//! exists to prevent.

#[cfg(target_os = "linux")]
mod firecracker;
#[cfg(target_os = "linux")]
mod gvisor;

use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, AuditEvent, AuditOutcome};
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::state::AppState;

const CODE_AUDIT_CAP: usize = 16 * 1024; // bytes of code recorded in the audit payload
const STREAM_CAP: usize = 32 * 1024; // bytes of stdout/stderr returned to the model

/// A file handed into the sandbox (host → guest). Empty for v1 (model-authored
/// code), but the contract carries inputs for when document injection lands.
#[derive(Debug, Clone)]
pub struct InputFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// One execution request.
#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub language: String, // "python"
    pub code: String,
    pub inputs: Vec<InputFile>,
}

/// A file produced by the run (guest → host) — becomes a generated artefact.
#[derive(Debug, Clone)]
pub struct OutputFile {
    pub name: String,
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// The result of one execution.
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: i64,
    pub files: Vec<OutputFile>,
}

/// Resource ceilings for one execution (from `CodeInterpreterConfig`).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub vcpus: u32,
    pub mem_mb: u32,
    pub wall_secs: u64,
    pub max_output_bytes: u64,
}

/// Runs model-authored code in an isolated sandbox.
#[async_trait]
pub trait CodeExecutor: Send + Sync {
    async fn execute(&self, req: ExecRequest, limits: &Limits) -> Result<ExecResult>;
}

/// The executor used wherever the sandbox cannot run (feature off, or a non-Linux
/// host). Fails cleanly so the tool surfaces a clear message rather than a hang.
pub struct UnavailableExecutor {
    pub reason: String,
}

#[async_trait]
impl CodeExecutor for UnavailableExecutor {
    async fn execute(&self, _req: ExecRequest, _limits: &Limits) -> Result<ExecResult> {
        Err(AppError::Unavailable(self.reason.clone()))
    }
}

/// The sandbox backend chosen for a host, or why none is available.
#[derive(Debug, PartialEq, Eq)]
enum Backend {
    Firecracker,
    Gvisor,
    Unavailable(String),
}

/// Pure tier decision — no host probing, so it is unit-testable across the matrix.
/// `backend` is the configured selector (`auto`/`firecracker`/`gvisor`/`off`);
/// `feature_on` is `features.code_interpreter`; `kvm` and `runsc` are the probe
/// results. Firecracker wins ties in `auto` (strongest isolation).
fn decide_backend(backend: &str, feature_on: bool, kvm: bool, runsc: bool) -> Backend {
    if !feature_on || backend == "off" {
        return Backend::Unavailable("code-interpreter is disabled on this host".into());
    }
    match backend {
        "firecracker" => {
            if kvm {
                Backend::Firecracker
            } else {
                Backend::Unavailable(
                    "Firecracker backend selected but this host has no KVM (/dev/kvm absent)".into(),
                )
            }
        }
        "gvisor" => {
            if runsc {
                Backend::Gvisor
            } else {
                Backend::Unavailable(
                    "gVisor backend selected but runsc is not usable (install gVisor or set code_interpreter.runsc_bin)".into(),
                )
            }
        }
        // "auto" (and any unrecognised value → treat as auto).
        _ => {
            if kvm {
                Backend::Firecracker
            } else if runsc {
                Backend::Gvisor
            } else {
                Backend::Unavailable(
                    "code-interpreter needs a host with KVM (Firecracker) or gVisor/runsc; this looks like a locked container".into(),
                )
            }
        }
    }
}

/// Pure decision for whether gVisor should pass `runsc --ignore-cgroups`, given
/// the `[code_interpreter] ignore_cgroups` knob and the two probe results. Kept
/// probe-injected so it is unit-testable (mirrors [`decide_backend`]).
///
/// `auto` (default) ignores cgroups when the process cannot manage them — either
/// it is rootless, or the host cgroup hierarchy is not writable (e.g. root inside
/// a container without `--cgroupns=host`). `always`/`never` are explicit overrides.
/// (`--rootless` itself is decided separately and always tracks `is_rootless`.)
fn should_ignore_cgroups(knob: &str, is_rootless: bool, cgroups_writable: bool) -> bool {
    match knob {
        "always" => true,
        "never" => false,
        _ /* auto */ => is_rootless || !cgroups_writable,
    }
}

/// Pick the executor for this host by tiering Firecracker (KVM) and gVisor
/// (runsc, KVM-less) per `[code_interpreter] backend`. Both backends are
/// Linux-only; every other host gets an [`UnavailableExecutor`].
pub fn select_executor(state: &AppState) -> Box<dyn CodeExecutor> {
    #[cfg(target_os = "linux")]
    {
        let ci = &state.boot.code_interpreter;
        let kvm = std::path::Path::new("/dev/kvm").exists();
        // Probe runsc only when it could matter (gvisor/auto without KVM), so the
        // common KVM+Firecracker path pays nothing.
        let runsc = (ci.backend == "gvisor" || (ci.backend == "auto" && !kvm))
            && gvisor::runsc_usable(&ci.runsc_bin);
        match decide_backend(&ci.backend, state.boot.features.code_interpreter, kvm, runsc) {
            Backend::Firecracker => {
                Box::new(firecracker::FirecrackerExecutor::new(state.boot.code_interpreter_vm.clone()))
            }
            Backend::Gvisor => Box::new(gvisor::GvisorExecutor::new(ci.clone())),
            Backend::Unavailable(reason) => Box::new(UnavailableExecutor { reason }),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let ci = &state.boot.code_interpreter;
        let reason = if !state.boot.features.code_interpreter || ci.backend == "off" {
            "code-interpreter is disabled on this host".to_string()
        } else {
            "code-interpreter requires a Linux host (KVM/Firecracker or gVisor/runsc)".to_string()
        };
        Box::new(UnavailableExecutor { reason })
    }
}

/// Select the host executor and run one request (no persistence) — the thin
/// entry the Linux integration test uses to smoke the real VMM backend.
pub async fn execute(state: &AppState, req: ExecRequest) -> Result<ExecResult> {
    let executor = select_executor(state);
    let limits = limits_from(state);
    executor.execute(req, &limits).await
}

fn limits_from(state: &AppState) -> Limits {
    let c = &state.boot.code_interpreter_vm;
    Limits {
        vcpus: c.vcpus,
        mem_mb: c.mem_mb,
        wall_secs: c.wall_secs,
        max_output_bytes: c.max_output_bytes,
    }
}

fn truncate(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        s.to_string()
    } else {
        format!("{}…[truncated]", &s[..cap])
    }
}

/// Resolve the artefacts root (absolute, or relative to the cwd) — same rule as
/// the `generate_artefact` tool.
fn artefacts_root(state: &AppState) -> std::path::PathBuf {
    crate::storage::resolve_dir(&state.boot.storage.artefacts_dir)
}

/// Execute `req` with `executor`, persist any output files as chat-scoped
/// artefacts, audit invoked/completed, and format the result for the model.
/// This is the testable core — inject a mock executor in tests.
pub async fn run_and_store(
    state: &AppState,
    ctx: &AuthContext,
    chat_id: Uuid,
    turn_id: Uuid,
    executor: &dyn CodeExecutor,
    req: ExecRequest,
) -> Result<String> {
    let exec_id = Uuid::now_v7();

    // Audit the submission (the security-relevant record: who ran what code).
    let mut inv = AuditEvent::action("code_interpreter.invoked", ctx.role.as_str());
    inv.actor_user_id = ctx.user_id;
    inv.resource_type = Some("code_execution".into());
    inv.resource_id = Some(exec_id);
    inv.payload = Some(json!({
        "chat_id": chat_id,
        "language": req.language,
        "code": truncate(&req.code, CODE_AUDIT_CAP),
        "inputs": req.inputs.iter().map(|f| &f.name).collect::<Vec<_>>(),
    }));
    let _ = audit::append(&state.pg, &inv).await;

    let submitted_code = req.code.clone(); // kept for the user-facing frame (req is moved below)
    let limits = limits_from(state);
    let outcome = executor.execute(req, &limits).await;

    let result = match outcome {
        Ok(r) => r,
        Err(e) => {
            // Surface the failure as a tool error, never silently.
            let mut ev = AuditEvent::action("code_interpreter.completed", ctx.role.as_str());
            ev.actor_user_id = ctx.user_id;
            ev.resource_type = Some("code_execution".into());
            ev.resource_id = Some(exec_id);
            ev.outcome = AuditOutcome::Failure;
            ev.outcome_reason = Some(e.to_string());
            let _ = audit::append(&state.pg, &ev).await;
            return Err(e);
        }
    };

    // Persist each output file as a chat-scoped generated artefact.
    let root = artefacts_root(state);
    let dir = root.join(chat_id.to_string());
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("create artefact dir: {e}")))?;

    let mut links: Vec<String> = Vec::new();
    for f in &result.files {
        let artefact_id = Uuid::now_v7();
        let safe_name = f.name.replace(['/', '\\'], "_");
        // Store the RELATIVE suffix; write to the absolute path.
        let rel = format!("{chat_id}/{artefact_id}_{safe_name}");
        let path = dir.join(format!("{artefact_id}_{safe_name}"));
        tokio::fs::write(&path, &f.bytes)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("write output file: {e}")))?;
        // Record `turn_id` so run_turn's post-turn back-fill links `message_id` to
        // the assistant message (like the generate_artefact tool) — that is what
        // lets the UI render the file under the answer rather than orphaning it.
        sqlx::query!(
            "INSERT INTO generated_artefacts (id, chat_id, turn_id, kind, title, disk_path, mime, created_by) \
             VALUES ($1, $2, $3, 'file', $4, $5, $6, $7)",
            artefact_id, chat_id, turn_id, f.name, rel, f.mime, ctx.user_id,
        )
        .execute(&state.pg)
        .await?;
        links.push(format!("{} → /api/artefacts/{artefact_id}/download", f.name));
    }

    let mut done = AuditEvent::action("code_interpreter.completed", ctx.role.as_str());
    done.actor_user_id = ctx.user_id;
    done.resource_type = Some("code_execution".into());
    done.resource_id = Some(exec_id);
    done.payload = Some(json!({
        "chat_id": chat_id,
        "exit_code": result.exit_code,
        "duration_ms": result.duration_ms,
        "files": result.files.iter().map(|f| &f.name).collect::<Vec<_>>(),
    }));
    let _ = audit::append(&state.pg, &done).await;

    // Surface the run to the user's sockets (code + output), best-effort.
    if let Some(uid) = ctx.user_id {
        state.hub.send_to_user(
            uid,
            crate::ws::protocol::ServerFrame::CodeResult {
                chat_id,
                code: truncate(&submitted_code, STREAM_CAP),
                stdout: truncate(&result.stdout, STREAM_CAP),
                stderr: truncate(&result.stderr, STREAM_CAP),
                exit_code: result.exit_code,
            },
        );
    }

    // Compose the model-facing result.
    let mut out = format!("exit_code: {}\n", result.exit_code);
    if !result.stdout.is_empty() {
        out.push_str(&format!("stdout:\n{}\n", truncate(&result.stdout, STREAM_CAP)));
    }
    if !result.stderr.is_empty() {
        out.push_str(&format!("stderr:\n{}\n", truncate(&result.stderr, STREAM_CAP)));
    }
    if !links.is_empty() {
        out.push_str("artefacts:\n");
        for l in &links {
            out.push_str(&format!("  {l}\n"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unavailable_executor_errors() {
        let ex = UnavailableExecutor { reason: "off".into() };
        let req = ExecRequest { language: "python".into(), code: "x=1".into(), inputs: vec![] };
        let limits = Limits { vcpus: 1, mem_mb: 128, wall_secs: 5, max_output_bytes: 1024 };
        let r = ex.execute(req, &limits).await;
        assert!(matches!(r, Err(AppError::Unavailable(_))));
    }

    #[test]
    fn truncate_caps_long_strings() {
        assert_eq!(truncate("abc", 10), "abc");
        assert!(truncate(&"x".repeat(100), 10).ends_with("[truncated]"));
    }

    // Tier matrix for the pure selector (no host probing).
    #[test]
    fn decide_backend_feature_off_or_off_is_unavailable() {
        assert!(matches!(decide_backend("auto", false, true, true), Backend::Unavailable(_)));
        assert!(matches!(decide_backend("off", true, true, true), Backend::Unavailable(_)));
    }

    #[test]
    fn decide_backend_auto_prefers_firecracker_then_gvisor() {
        // KVM present → Firecracker (strongest), regardless of runsc.
        assert_eq!(decide_backend("auto", true, true, true), Backend::Firecracker);
        assert_eq!(decide_backend("auto", true, true, false), Backend::Firecracker);
        // No KVM but runsc usable → gVisor.
        assert_eq!(decide_backend("auto", true, false, true), Backend::Gvisor);
        // Neither → clear Unavailable.
        assert!(matches!(decide_backend("auto", true, false, false), Backend::Unavailable(_)));
    }

    #[test]
    fn decide_backend_explicit_firecracker_needs_kvm() {
        assert_eq!(decide_backend("firecracker", true, true, true), Backend::Firecracker);
        // Explicit firecracker without KVM does NOT silently fall back to gVisor.
        assert!(matches!(
            decide_backend("firecracker", true, false, true),
            Backend::Unavailable(_)
        ));
    }

    #[test]
    fn decide_backend_explicit_gvisor_needs_runsc() {
        assert_eq!(decide_backend("gvisor", true, false, true), Backend::Gvisor);
        // Explicit gVisor never uses Firecracker even if KVM is present.
        assert_eq!(decide_backend("gvisor", true, true, true), Backend::Gvisor);
        assert!(matches!(decide_backend("gvisor", true, true, false), Backend::Unavailable(_)));
    }

    #[test]
    fn decide_backend_unknown_value_treated_as_auto() {
        assert_eq!(decide_backend("wat", true, true, false), Backend::Firecracker);
        assert_eq!(decide_backend("wat", true, false, true), Backend::Gvisor);
    }

    // gVisor cgroups matrix: (knob, is_rootless, cgroups_writable) -> ignore?
    #[test]
    fn should_ignore_cgroups_explicit_overrides() {
        // "always"/"never" ignore the probes entirely.
        assert!(should_ignore_cgroups("always", false, true));
        assert!(should_ignore_cgroups("always", false, false));
        assert!(!should_ignore_cgroups("never", true, false));
        assert!(!should_ignore_cgroups("never", false, false));
    }

    #[test]
    fn should_ignore_cgroups_auto() {
        // Root on a host with writable cgroups → keep cgroups (real enforcement).
        assert!(!should_ignore_cgroups("auto", false, true));
        // Root inside a container (cgroups not writable) → ignore.
        assert!(should_ignore_cgroups("auto", false, false));
        // Rootless → ignore regardless of writability.
        assert!(should_ignore_cgroups("auto", true, true));
        assert!(should_ignore_cgroups("auto", true, false));
        // Unknown knob value falls through to auto.
        assert!(should_ignore_cgroups("wat", false, false));
        assert!(!should_ignore_cgroups("wat", false, true));
    }
}
