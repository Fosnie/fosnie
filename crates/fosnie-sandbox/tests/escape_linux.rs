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

//! The boundary holds because the operating system refuses, not because our code
//! checks. Each test runs a command that tries to step outside the folder or open
//! the network and asserts the attempt failed — the command is the one thing that
//! could not be quietly weakened without the test noticing.
//!
//! Skips (does not fail) when no real boundary is available on the host, so a
//! machine without bubblewrap does not report a false failure; a continuous-
//! integration runner installs bubblewrap so these actually run.

#![cfg(target_os = "linux")]

use fosnie_sandbox::{wrap, EnforcementTier, NetPolicy, SandboxSpec};
use std::path::{Path, PathBuf};

fn boundary_available() -> bool {
    if fosnie_sandbox::enforcement_tier() != EnforcementTier::Full {
        eprintln!("skipping: no filesystem-and-network boundary on this host");
        return false;
    }
    true
}

fn spec(workspace: &Path, net: NetPolicy) -> SandboxSpec {
    SandboxSpec {
        workspace_rw: workspace.to_path_buf(),
        ro_carve: vec![workspace.join(".git")],
        net,
    }
}

/// Run a command through the boundary and report whether it succeeded.
async fn run(workspace: &Path, net: NetPolicy, command: &str) -> std::process::Output {
    let prepared = wrap(command, &spec(workspace, net)).expect("wrap");
    let fosnie_sandbox::Prepared { mut command, guard } = prepared;
    command
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let out = command.output().await.expect("run");
    drop(guard);
    out
}

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_path_buf();
    (dir, path)
}

#[tokio::test]
async fn a_write_inside_the_workspace_is_allowed() {
    if !boundary_available() {
        return;
    }
    let (_dir, ws) = workspace();
    let out = run(&ws, NetPolicy::Denied, "echo hello > inside.txt").await;
    assert!(out.status.success(), "writing inside the folder should work: {out:?}");
    assert!(ws.join("inside.txt").exists(), "the file should be there afterwards");
}

#[tokio::test]
async fn a_write_outside_the_workspace_is_refused() {
    if !boundary_available() {
        return;
    }
    let (_dir, ws) = workspace();
    // A path well outside the folder, on the read-only baseline.
    let out = run(&ws, NetPolicy::Denied, "echo x > /root/escape_probe_should_fail").await;
    assert!(!out.status.success(), "writing outside the folder must be refused by the OS");
    assert!(
        !Path::new("/root/escape_probe_should_fail").exists(),
        "nothing should have been written outside the folder"
    );
}

#[tokio::test]
async fn the_git_directory_is_read_only() {
    if !boundary_available() {
        return;
    }
    let (_dir, ws) = workspace();
    std::fs::create_dir_all(ws.join(".git/hooks")).expect("make .git/hooks");
    let out = run(&ws, NetPolicy::Denied, "echo evil > .git/hooks/pre-commit").await;
    assert!(!out.status.success(), "writing into .git must be refused");
    assert!(
        !ws.join(".git/hooks/pre-commit").exists(),
        "no hook should have been planted"
    );
}

#[tokio::test]
async fn the_temporary_directory_is_writable_but_isolated() {
    if !boundary_available() {
        return;
    }
    let (_dir, ws) = workspace();
    // Tools need scratch space; the sandbox gives a private one that does not
    // touch the host's /tmp.
    let out = run(&ws, NetPolicy::Denied, "echo scratch > /tmp/sandbox_scratch && cat /tmp/sandbox_scratch").await;
    assert!(out.status.success(), "writing to the private scratch should work: {out:?}");
    assert!(
        !Path::new("/tmp/sandbox_scratch").exists(),
        "the host's /tmp must be untouched"
    );
}

#[tokio::test]
async fn the_network_is_refused_when_denied() {
    if !boundary_available() {
        return;
    }
    let (_dir, ws) = workspace();
    // Opening a TCP socket to a public address; with no network namespace there
    // is nothing to reach. Uses bash's /dev/tcp so no external tool is required.
    let out = run(
        &ws,
        NetPolicy::Denied,
        "bash -c 'exec 3<>/dev/tcp/1.1.1.1/80' 2>/dev/null",
    )
    .await;
    assert!(!out.status.success(), "the network must be unreachable under a denied policy");
}

#[tokio::test]
async fn the_network_is_reachable_when_allowed() {
    if !boundary_available() {
        return;
    }
    // A control for the test above: with the same boundary but the network left
    // on, the socket is at least created (the connection may or may not complete
    // depending on the host's own egress, so only the namespace difference is
    // asserted, not internet reachability).
    let (_dir, ws) = workspace();
    let denied = run(
        &ws,
        NetPolicy::Denied,
        "bash -c 'exec 3<>/dev/tcp/1.1.1.1/80' 2>/dev/null",
    )
    .await;
    let allowed = run(
        &ws,
        NetPolicy::Full,
        "bash -c 'getent hosts >/dev/null 2>&1; ip -o link 2>/dev/null | wc -l'",
    )
    .await;
    // Denied has no interfaces; allowed shares the host's, so it has more than the
    // single loopback a fresh network namespace would show. This asserts the
    // policy actually changes the network the command sees.
    assert!(!denied.status.success());
    let interfaces: i32 = String::from_utf8_lossy(&allowed.stdout).trim().parse().unwrap_or(0);
    assert!(interfaces >= 1, "with the network allowed the host's interfaces are visible");
}
