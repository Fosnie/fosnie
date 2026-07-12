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

//! Firecracker microVM backend for the code interpreter — **Linux + KVM only**.
//! Boots a fresh, network-less microVM per execution
//! (stateless: boot → run the model's code via the in-guest agent over vsock →
//! destroy). Zero egress: no network interface is configured, so injected code
//! cannot reach out even in principle.
//!
//! **Deploy-verified.** This file compiles only on Linux and runs only on a
//! KVM host with the rootfs/kernel/guest-agent deployment artefacts in place
//! (see `backend/deploy/firecracker/README.md`). It cannot be exercised on the
//! Windows/macOS dev boxes. The snapshot warm-pool optimisation is Pass-2; v1
//! boots per execution, which is equally stateless.

use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use uuid::Uuid;

use super::{CodeExecutor, ExecRequest, ExecResult, Limits, OutputFile};
use crate::config::CodeInterpreterConfig;
use crate::error::{AppError, Result};

pub struct FirecrackerExecutor {
    cfg: CodeInterpreterConfig,
}

impl FirecrackerExecutor {
    pub fn new(cfg: CodeInterpreterConfig) -> Self {
        Self { cfg }
    }
}

// --- guest-agent wire protocol (vsock; see deploy/firecracker/guest_agent.py) ---
// Framing: each side writes a 4-byte little-endian length prefix then the JSON.
// File bodies are base64. The guest writes inputs to a scratch dir, runs the
// code, captures stdout/stderr/exit, and returns any newly-created files.

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
impl CodeExecutor for FirecrackerExecutor {
    async fn execute(&self, req: ExecRequest, limits: &Limits) -> Result<ExecResult> {
        let started = std::time::Instant::now();
        let id = Uuid::now_v7();
        let api_sock = format!("{}/fc-{id}.sock", self.cfg.socket_dir.trim_end_matches('/'));
        let vsock_uds = format!("{}/vsock-{id}.sock", self.cfg.socket_dir.trim_end_matches('/'));

        tokio::fs::create_dir_all(&self.cfg.socket_dir)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("create socket dir: {e}")))?;

        // Spawn the VMM; it creates the API socket.
        let mut child = tokio::process::Command::new(&self.cfg.firecracker_bin)
            .arg("--api-sock")
            .arg(&api_sock)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| AppError::Unavailable(format!("spawn firecracker: {e}")))?;

        // Run the whole lifecycle under the wall-clock limit; always clean up.
        let outcome = tokio::time::timeout(
            Duration::from_secs(limits.wall_secs + 10),
            self.boot_and_run(&api_sock, &vsock_uds, &req, limits),
        )
        .await;

        let _ = child.start_kill();
        let _ = tokio::fs::remove_file(&api_sock).await;
        let _ = tokio::fs::remove_file(&vsock_uds).await;

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

impl FirecrackerExecutor {
    async fn boot_and_run(
        &self,
        api_sock: &str,
        vsock_uds: &str,
        req: &ExecRequest,
        limits: &Limits,
    ) -> Result<JobResult> {
        // Wait briefly for the API socket to appear.
        for _ in 0..100 {
            if tokio::fs::metadata(api_sock).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Configure: kernel, read-only rootfs, machine, vsock — and crucially NO
        // network interface (zero egress, §A.6.4).
        fc_put(
            api_sock,
            "/boot-source",
            json!({
                "kernel_image_path": self.cfg.kernel_image,
                "boot_args": "console=ttyS0 reboot=k panic=1 pci=off i8042.noaux i8042.nomux i8042.nopnp i8042.dumbkbd"
            }),
        )
        .await?;
        fc_put(
            api_sock,
            "/drives/rootfs",
            json!({
                "drive_id": "rootfs",
                "path_on_host": self.cfg.rootfs_image,
                "is_root_device": true,
                "is_read_only": true
            }),
        )
        .await?;
        fc_put(
            api_sock,
            "/machine-config",
            json!({ "vcpu_count": limits.vcpus, "mem_size_mib": limits.mem_mb }),
        )
        .await?;
        fc_put(
            api_sock,
            "/vsock",
            json!({ "guest_cid": self.cfg.vsock_cid, "uds_path": vsock_uds }),
        )
        .await?;
        fc_put(api_sock, "/actions", json!({ "action_type": "InstanceStart" })).await?;

        // The guest agent listens on vsock; Firecracker bridges it at `vsock_uds`
        // via a `CONNECT <port>` handshake.
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
        self.run_job_over_vsock(vsock_uds, &job).await
    }

    async fn run_job_over_vsock(&self, vsock_uds: &str, job: &Job) -> Result<JobResult> {
        // Wait for the guest agent's bridge socket, then connect + handshake.
        let mut stream = None;
        for _ in 0..200 {
            if let Ok(s) = UnixStream::connect(vsock_uds).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let mut stream =
            stream.ok_or_else(|| AppError::Unavailable("guest vsock not reachable".into()))?;

        // Firecracker vsock multiplexer handshake.
        stream
            .write_all(format!("CONNECT {}\n", self.cfg.vsock_port).as_bytes())
            .await
            .map_err(|e| AppError::Unavailable(format!("vsock connect: {e}")))?;
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = stream
                .read(&mut byte)
                .await
                .map_err(|e| AppError::Unavailable(format!("vsock handshake: {e}")))?;
            if n == 0 || byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
        }
        if !line.starts_with(b"OK") {
            return Err(AppError::Unavailable("vsock handshake rejected".into()));
        }

        // Length-prefixed JSON request → length-prefixed JSON response.
        let body = serde_json::to_vec(job)
            .map_err(|e| AppError::Other(anyhow::anyhow!("encode job: {e}")))?;
        stream
            .write_all(&(body.len() as u32).to_le_bytes())
            .await
            .map_err(|e| AppError::Unavailable(format!("vsock write len: {e}")))?;
        stream
            .write_all(&body)
            .await
            .map_err(|e| AppError::Unavailable(format!("vsock write job: {e}")))?;

        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| AppError::Unavailable(format!("vsock read len: {e}")))?;
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut resp = vec![0u8; len];
        stream
            .read_exact(&mut resp)
            .await
            .map_err(|e| AppError::Unavailable(format!("vsock read result: {e}")))?;
        serde_json::from_slice::<JobResult>(&resp)
            .map_err(|e| AppError::Other(anyhow::anyhow!("decode job result: {e}")))
    }
}

/// Minimal HTTP/1.1 PUT-with-JSON to the Firecracker API over its Unix socket.
/// Firecracker replies `204 No Content` on success.
async fn fc_put(api_sock: &str, path: &str, body: serde_json::Value) -> Result<()> {
    let mut stream = UnixStream::connect(api_sock)
        .await
        .map_err(|e| AppError::Unavailable(format!("fc api connect: {e}")))?;
    let body = serde_json::to_vec(&body).unwrap_or_default();
    let req = format!(
        "PUT {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| AppError::Unavailable(format!("fc api write: {e}")))?;
    stream
        .write_all(&body)
        .await
        .map_err(|e| AppError::Unavailable(format!("fc api body: {e}")))?;

    let mut resp = Vec::new();
    stream
        .read_to_end(&mut resp)
        .await
        .map_err(|e| AppError::Unavailable(format!("fc api read: {e}")))?;
    let head = String::from_utf8_lossy(&resp);
    let status_ok = head
        .lines()
        .next()
        .map(|l| l.contains(" 204") || l.contains(" 200"))
        .unwrap_or(false);
    if status_ok {
        Ok(())
    } else {
        let first = head.lines().next().unwrap_or("").to_string();
        Err(AppError::Unavailable(format!("firecracker {path} failed: {first}")))
    }
}
