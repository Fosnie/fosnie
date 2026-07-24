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

//! Linux: a real filesystem and network boundary.
//!
//! The command is run inside a bubblewrap sandbox. The whole filesystem is bound
//! read-only, one folder (the workspace) is bound writable, a fresh empty scratch
//! is mounted at the usual temporary path, and any read-only carve within the
//! workspace (a repo's `.git`) is bound back read-only over it. When the boundary
//! denies the network, the sandbox is given no network namespace at all, so there
//! is nothing for a program to reach. On top of the namespace, a small seccomp
//! filter refuses the syscall classes a namespace does not cover — the io_uring
//! family (which can otherwise reach the kernel's I/O engine sideways) and the
//! process-inspection calls (`ptrace`, `process_vm_*`).
//!
//! If a boundary cannot be established — bubblewrap is absent, or the host forbids
//! the unprivileged user namespaces it needs — the platform declares the weaker
//! lifecycle tier and runs the command unconfined, so the instance keeps asking
//! about it. It never suppresses the question while quietly running without a
//! boundary.

use super::{EnforcementTier, NetPolicy, Prepared, SandboxSpec};
use anyhow::{anyhow, Context, Result};
use std::os::unix::io::{FromRawFd, OwnedFd, RawFd};
use std::process::Stdio;
use std::sync::OnceLock;

/// Holds, for the command's lifetime, the read end of the pipe the syscall filter
/// was handed to bubblewrap on — kept open so the number given on the command line
/// stays valid, and closed when this drops. `None` when no filter was installed.
/// The process tree itself is stopped by the caller's process group, so there is
/// nothing to bind or kill here.
pub struct SandboxedSpawn {
    _seccomp_fd: Option<OwnedFd>,
}

impl SandboxedSpawn {
    /// Nothing to bind: the process group set up by the caller is the tree.
    pub fn adopt(&self, _process_handle: isize) {}

    /// Nothing to tear down here; the caller stops the process group directly, and
    /// bubblewrap's die-with-parent ends the sandbox if this process goes.
    pub fn terminate(&mut self) {}
}

/// Whether a real boundary is available on this host, decided once. A stronger
/// tier is only ever reported when an actual attempt to build a sandbox succeeded.
pub fn enforcement_tier() -> EnforcementTier {
    static TIER: OnceLock<EnforcementTier> = OnceLock::new();
    *TIER.get_or_init(probe)
}

fn probe() -> EnforcementTier {
    // bubblewrap must be present and unprivileged user namespaces must actually
    // work — some distributions restrict them. A cheap real attempt is the only
    // honest test; anything less would risk claiming a boundary the host refuses.
    let ok = std::process::Command::new("bwrap")
        .args([
            "--unshare-user",
            "--unshare-net",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--",
            "/bin/true",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        EnforcementTier::Full
    } else {
        tracing::warn!(
            "a filesystem-and-network sandbox is unavailable on this host; commands will run without one and stay behind approval"
        );
        EnforcementTier::Lifecycle
    }
}

/// Build the command to run, confined if a boundary is available and plainly under
/// the system shell if it is not.
pub fn wrap(command: &str, spec: &SandboxSpec) -> Result<Prepared> {
    match enforcement_tier() {
        EnforcementTier::Full => build_confined(command, spec),
        EnforcementTier::Lifecycle => {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg(command);
            Ok(Prepared {
                command: cmd,
                guard: SandboxedSpawn { _seccomp_fd: None },
            })
        }
    }
}

fn build_confined(command: &str, spec: &SandboxSpec) -> Result<Prepared> {
    let ws = spec
        .workspace_rw
        .to_str()
        .context("the workspace path is not valid text")?;

    // Later options win where paths overlap, so the order matters: read-only
    // baseline first, then the working virtual filesystems and scratch, then the
    // one writable folder, then the read-only carve back over part of it.
    let mut args: Vec<String> = vec![
        "--unshare-user".into(),
        "--unshare-pid".into(),
        "--unshare-ipc".into(),
        "--unshare-uts".into(),
        "--unshare-cgroup-try".into(),
        "--die-with-parent".into(),
        "--ro-bind".into(),
        "/".into(),
        "/".into(),
        "--dev".into(),
        "/dev".into(),
        "--proc".into(),
        "/proc".into(),
        "--tmpfs".into(),
        "/tmp".into(),
        "--bind".into(),
        ws.into(),
        ws.into(),
    ];
    if matches!(spec.net, NetPolicy::Denied) {
        args.push("--unshare-net".into());
    }
    for p in &spec.ro_carve {
        if p.exists() {
            if let Some(s) = p.to_str() {
                args.push("--ro-bind".into());
                args.push(s.into());
                args.push(s.into());
            }
        }
    }
    args.push("--chdir".into());
    args.push(ws.into());

    // A syscall filter for what the namespace does not cover. Best effort: if it
    // cannot be built or handed over, the namespace boundary still stands, so this
    // never fails the command.
    let seccomp_fd = match attach_seccomp(&mut args) {
        Ok(fd) => fd,
        Err(e) => {
            tracing::warn!(error = %e, "could not attach a syscall filter; the command's namespace boundary still applies");
            None
        }
    };

    args.push("--".into());
    args.push("sh".into());
    args.push("-c".into());
    args.push(command.into());

    let mut cmd = tokio::process::Command::new("bwrap");
    cmd.args(&args);
    Ok(Prepared {
        command: cmd,
        guard: SandboxedSpawn {
            _seccomp_fd: seccomp_fd,
        },
    })
}

/// Compile the filter, write it to a pipe, and add `--seccomp <fd>` pointing at
/// the read end. Returns that read end to be held open for the command's life.
fn attach_seccomp(args: &mut Vec<String>) -> Result<Option<OwnedFd>> {
    let Some(bytes) = compile_seccomp()? else {
        return Ok(None);
    };

    // A pipe whose read end is left inheritable (no close-on-exec), so bubblewrap
    // can read the filter from it after the fork.
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), 0) } != 0 {
        return Err(anyhow!(std::io::Error::last_os_error())).context("creating the filter pipe");
    }
    let read_fd: RawFd = fds[0];
    let write_fd: RawFd = fds[1];
    // Own the read end now so it is always closed, whatever happens next.
    let read_owned = unsafe { OwnedFd::from_raw_fd(read_fd) };

    let mut off = 0usize;
    while off < bytes.len() {
        let n = unsafe {
            libc::write(
                write_fd,
                bytes[off..].as_ptr() as *const libc::c_void,
                bytes.len() - off,
            )
        };
        if n <= 0 {
            unsafe { libc::close(write_fd) };
            return Err(anyhow!("could not write the syscall filter to its pipe"));
        }
        off += n as usize;
    }
    // Close the write end so the reader sees the end of the program.
    unsafe { libc::close(write_fd) };

    args.push("--seccomp".into());
    args.push(read_fd.to_string());
    Ok(Some(read_owned))
}

/// The compiled BPF program bytes, or `None` on an architecture the filter does
/// not target (the namespace boundary carries the load there).
fn compile_seccomp() -> Result<Option<Vec<u8>>> {
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};
    use std::collections::BTreeMap;

    let Some(arch) = target_arch() else {
        return Ok(None);
    };

    // The io_uring family can reach the kernel's I/O engine sideways; the
    // process-inspection calls read and write another process's memory. Neither
    // is anything a command legitimately needs here.
    let blocked: [libc::c_long; 6] = [
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
    ];
    let mut rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> = BTreeMap::new();
    for s in blocked {
        rules.insert(s as i64, vec![]); // empty rule set: matched unconditionally
    }
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,                        // everything not listed is allowed
        SeccompAction::Errno(libc::EPERM as u32),    // a listed call is refused
        arch,
    )
    .context("building the syscall filter")?;
    let program: BpfProgram = filter.try_into().context("compiling the syscall filter")?;
    // `sock_filter` is `#[repr(C)]` and exactly eight bytes with no padding, which
    // is the on-the-wire form bubblewrap expects.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            program.as_ptr() as *const u8,
            std::mem::size_of_val(program.as_slice()),
        )
    }
    .to_vec();
    Ok(Some(bytes))
}

fn target_arch() -> Option<seccompiler::TargetArch> {
    #[cfg(target_arch = "x86_64")]
    {
        Some(seccompiler::TargetArch::x86_64)
    }
    #[cfg(target_arch = "aarch64")]
    {
        Some(seccompiler::TargetArch::aarch64)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_syscall_filter_compiles_to_a_program() {
        // On a targeted architecture the filter must produce a non-empty program;
        // on any other it is simply absent, and that is not an error.
        match compile_seccomp().expect("compile") {
            Some(bytes) => assert!(!bytes.is_empty() && bytes.len() % 8 == 0),
            None => {} // architecture without a filter; the namespace carries it
        }
    }
}
