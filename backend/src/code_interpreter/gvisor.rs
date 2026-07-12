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

//! gVisor (`runsc`) backend for the code interpreter — **Linux, KVM-less**.
//! Runs each execution in a fresh, network-less gVisor sandbox
//! (build OCI bundle → `runsc run` the guest agent in oneshot mode → destroy).
//! gVisor's default `systrap` platform needs no KVM, so this covers Verda-class
//! VM guests and ordinary Docker/VM boxes where Firecracker cannot run.
//!
//! **Integration is runsc-direct**: we spawn the runtime ourselves and hand it a
//! hand-built OCI bundle — no Docker/containerd dependency. Zero egress is
//! enforced two ways: the OCI spec declares no network namespace/NIC, and we
//! invoke `runsc --network=none` so the sandbox has no network stack at all. The
//! rootfs is mounted read-only and the guest runs non-root with empty
//! capabilities and `no_new_privileges`. Same job/result contract as the
//! Firecracker guest agent (`deploy/firecracker/guest_agent.py`).

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::{CodeExecutor, ExecRequest, ExecResult, Limits, OutputFile};
use crate::config::CodeInterpreterBackendConfig;
use crate::error::{AppError, Result};

/// Path of the guest agent inside the rootfs (installed by `build-rootfs.sh`).
const GUEST_AGENT: &str = "/opt/pai-agent/guest_agent.py";

pub struct GvisorExecutor {
    cfg: CodeInterpreterBackendConfig,
}

impl GvisorExecutor {
    pub fn new(cfg: CodeInterpreterBackendConfig) -> Self {
        Self { cfg }
    }
}

/// Cheap probe used by `select_executor`: the runtime binary resolves and
/// `runsc --version` exits successfully. Never boots a sandbox.
pub fn runsc_usable(runsc_bin: &str) -> bool {
    std::process::Command::new(runsc_bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Effective uid != 0 → we must run runsc rootless. Read from `/proc/self/status`
/// so we need no libc dependency. Unknown → assume privileged (do not add
/// `--rootless`). (Whether to ignore cgroups is decided separately — being root
/// is necessary but not sufficient to manage cgroups; see [`cgroups_writable`].)
fn is_rootless() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                // "Uid:\t<real>\t<effective>\t<saved>\t<fs>"
                .and_then(|l| l.split_whitespace().nth(2).map(|e| e != "0"))
        })
        .unwrap_or(false)
}

/// Best-effort probe: can this process manage cgroups? `runsc` (as root) needs to
/// create a child cgroup under the host hierarchy; that fails for a root process
/// inside a container without `--cgroupns=host` (the classic "gvisor cgroups"
/// error). We test exactly that — create then remove a throwaway cgroup dir under
/// the unified `/sys/fs/cgroup`. Any error (missing mount, read-only, EPERM) →
/// treat cgroups as unavailable so the caller adds `--ignore-cgroups`.
fn cgroups_writable() -> bool {
    let probe = std::path::Path::new("/sys/fs/cgroup/pai-ci-cgprobe");
    match std::fs::create_dir(probe) {
        Ok(()) => {
            let _ = std::fs::remove_dir(probe);
            true
        }
        // EEXIST (a leftover probe from a crashed run) still proves writability.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_dir(probe);
            true
        }
        Err(_) => false,
    }
}

// --- job/result contract (mirrors deploy/firecracker/guest_agent.py) ---
#[derive(Serialize)]
struct WireInput {
    name: String,
    b64: String,
}

#[derive(Serialize)]
struct Job {
    language: String,
    code: String,
    inputs: Vec<WireInput>,
    wall_secs: u64,
    max_output_bytes: u64,
}

#[derive(Deserialize)]
struct WireOutput {
    name: String,
    b64: String,
    mime: String,
}

#[derive(Deserialize)]
struct JobResult {
    stdout: String,
    stderr: String,
    exit_code: i32,
    #[serde(default)]
    files: Vec<WireOutput>,
}

#[async_trait::async_trait]
impl CodeExecutor for GvisorExecutor {
    async fn execute(&self, req: ExecRequest, limits: &Limits) -> Result<ExecResult> {
        let started = std::time::Instant::now();
        let id = Uuid::now_v7();
        let state_dir = self.cfg.gvisor_state_dir.trim_end_matches('/');
        let runsc_root = format!("{state_dir}/root");
        let bundle = PathBuf::from(format!("{state_dir}/exec-{id}"));
        let work = bundle.join("work");

        tokio::fs::create_dir_all(&work)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("create gvisor bundle: {e}")))?;
        tokio::fs::create_dir_all(&runsc_root)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("create runsc root: {e}")))?;

        // Run the whole lifecycle under the wall-clock limit; always tear down.
        let outcome = tokio::time::timeout(
            Duration::from_secs(limits.wall_secs + 10),
            self.boot_and_run(&runsc_root, &bundle, &work, &id, &req, limits),
        )
        .await;

        // Best-effort teardown: kill (if still up), delete container state, remove
        // the scratch bundle. Runs on every path (success, error, timeout).
        let _ = tokio::process::Command::new(&self.cfg.runsc_bin)
            .args(["--root", &runsc_root, "kill", &id.to_string(), "KILL"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        let _ = tokio::process::Command::new(&self.cfg.runsc_bin)
            .args(["--root", &runsc_root, "delete", "--force", &id.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        let _ = tokio::fs::remove_dir_all(&bundle).await;

        let result = match outcome {
            Ok(r) => r?,
            Err(_) => return Err(AppError::Unavailable("code execution timed out".into())),
        };

        Ok(ExecResult {
            stdout: result.stdout,
            stderr: result.stderr,
            exit_code: result.exit_code,
            duration_ms: started.elapsed().as_millis() as i64,
            files: result
                .files
                .into_iter()
                .filter_map(|f| {
                    base64::engine::general_purpose::STANDARD
                        .decode(f.b64.as_bytes())
                        .ok()
                        .map(|bytes| OutputFile { name: f.name, bytes, mime: f.mime })
                })
                .collect(),
        })
    }
}

impl GvisorExecutor {
    async fn boot_and_run(
        &self,
        runsc_root: &str,
        bundle: &std::path::Path,
        work: &std::path::Path,
        id: &Uuid,
        req: &ExecRequest,
        limits: &Limits,
    ) -> Result<JobResult> {
        // 1. Write the job into the writable /work mount (same shape as FC vsock).
        let job = Job {
            language: req.language.clone(),
            code: req.code.clone(),
            inputs: req
                .inputs
                .iter()
                .map(|f| WireInput {
                    name: f.name.clone(),
                    b64: base64::engine::general_purpose::STANDARD.encode(&f.bytes),
                })
                .collect(),
            wall_secs: limits.wall_secs,
            max_output_bytes: limits.max_output_bytes,
        };
        let job_bytes = serde_json::to_vec(&job)
            .map_err(|e| AppError::Other(anyhow::anyhow!("encode job: {e}")))?;
        tokio::fs::write(work.join("job.json"), &job_bytes)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("write job.json: {e}")))?;
        // The guest runs as a non-root uid; make /work writable so it can drop
        // result.json (the bundle is a per-exec throwaway).
        std::fs::set_permissions(work, std::fs::Permissions::from_mode(0o777))
            .map_err(|e| AppError::Other(anyhow::anyhow!("chmod work: {e}")))?;

        // 2. Write the OCI bundle spec (bind source must be an absolute host path).
        let work_abs = work
            .to_str()
            .ok_or_else(|| AppError::Other(anyhow::anyhow!("non-utf8 work path")))?;
        let spec = self.oci_spec(limits, work_abs);
        tokio::fs::write(
            bundle.join("config.json"),
            serde_json::to_vec_pretty(&spec)
                .map_err(|e| AppError::Other(anyhow::anyhow!("encode oci spec: {e}")))?,
        )
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("write config.json: {e}")))?;

        // 3. runsc [global] run --bundle <bundle> <id>. Global flags BEFORE the
        //    subcommand: network-less stack, our state root, rootless when unpriv,
        //    and skip cgroup setup when we cannot manage cgroups (e.g. root inside
        //    a container without --cgroupns=host) — driven by the config knob.
        let rootless = is_rootless();
        let ignore_cgroups = super::should_ignore_cgroups(
            &self.cfg.ignore_cgroups,
            rootless,
            cgroups_writable(),
        );
        let mut cmd = tokio::process::Command::new(&self.cfg.runsc_bin);
        cmd.arg("--root").arg(runsc_root).arg("--network=none");
        if rootless {
            cmd.arg("--rootless");
        }
        if ignore_cgroups {
            cmd.arg("--ignore-cgroups");
        }
        cmd.arg("run")
            .arg("--bundle")
            .arg(bundle)
            .arg(id.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let out = cmd
            .output()
            .await
            .map_err(|e| AppError::Unavailable(format!("spawn runsc: {e}")))?;

        // 4. The guest agent writes the real result to /work/result.json (stdout of
        //    the model code is captured there, not on runsc's stdout). If it is
        //    missing, the sandbox itself failed — surface runsc's stderr.
        let result_path = work.join("result.json");
        match tokio::fs::read(&result_path).await {
            Ok(bytes) => serde_json::from_slice::<JobResult>(&bytes)
                .map_err(|e| AppError::Other(anyhow::anyhow!("decode result.json: {e}"))),
            Err(_) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let first = stderr.lines().next().unwrap_or("no result and no runsc error");
                Err(AppError::Unavailable(format!(
                    "gvisor sandbox produced no result (exit {:?}): {first}",
                    out.status.code()
                )))
            }
        }
    }

    /// Build the OCI runtime spec: read-only rootfs, non-root, no capabilities,
    /// no_new_privileges, no network namespace (paired with `--network=none`),
    /// resource caps from `Limits`, writable `/work` (nosuid,nodev) + tmpfs
    /// `/tmp`, masked sensitive `/proc` and `/sys`.
    fn oci_spec(&self, limits: &Limits, work_abs: &str) -> serde_json::Value {
        let mem_bytes = i64::from(limits.mem_mb) * 1024 * 1024;
        let cpu_quota = i64::from(limits.vcpus.max(1)) * 100_000;
        let cpu_wall = limits.wall_secs;
        json!({
            "ociVersion": "1.0.2",
            "process": {
                "terminal": false,
                "user": { "uid": 65534, "gid": 65534 },
                "args": ["python3", GUEST_AGENT, "--oneshot", "/work/job.json", "/work/result.json"],
                "env": [
                    "PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin",
                    "PYTHONDONTWRITEBYTECODE=1",
                    "HOME=/tmp",
                    "MPLBACKEND=Agg",
                    "MPLCONFIGDIR=/tmp/mpl",
                    "PAI_AGENT_PORT=5005"
                ],
                "cwd": "/work",
                "noNewPrivileges": true,
                "capabilities": {
                    "bounding": [], "effective": [], "permitted": [],
                    "inheritable": [], "ambient": []
                },
                "rlimits": [
                    { "type": "RLIMIT_NOFILE", "hard": 1024, "soft": 1024 },
                    { "type": "RLIMIT_NPROC",  "hard": 256,  "soft": 256 },
                    { "type": "RLIMIT_CPU",    "hard": cpu_wall + 5, "soft": cpu_wall }
                ]
            },
            "root": { "path": self.cfg.gvisor_rootfs.clone(), "readonly": true },
            "hostname": "sandbox",
            "mounts": [
                { "destination": "/proc", "type": "proc", "source": "proc" },
                {
                    "destination": "/work", "type": "bind", "source": work_abs,
                    "options": ["rbind", "rw", "nosuid", "nodev"]
                },
                {
                    "destination": "/tmp", "type": "tmpfs", "source": "tmpfs",
                    "options": ["nosuid", "nodev", "size=256m", "mode=1777"]
                },
                {
                    "destination": "/dev", "type": "tmpfs", "source": "tmpfs",
                    "options": ["nosuid", "mode=0755", "size=65536k"]
                },
                {
                    "destination": "/dev/shm", "type": "tmpfs", "source": "shm",
                    "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"]
                }
            ],
            "linux": {
                // No "network" namespace: paired with `runsc --network=none`, the
                // sandbox has no NIC and no route out (zero egress).
                "namespaces": [
                    { "type": "pid" },
                    { "type": "ipc" },
                    { "type": "uts" },
                    { "type": "mount" }
                ],
                "resources": {
                    "memory": { "limit": mem_bytes },
                    "cpu": { "quota": cpu_quota, "period": 100_000 }
                },
                "maskedPaths": [
                    "/proc/kcore", "/proc/keys", "/proc/latency_stats",
                    "/proc/timer_list", "/proc/sched_debug", "/sys/firmware"
                ],
                "readonlyPaths": [
                    "/proc/asound", "/proc/bus", "/proc/fs", "/proc/irq",
                    "/proc/sys", "/proc/sysrq-trigger"
                ]
            }
        })
    }
}
