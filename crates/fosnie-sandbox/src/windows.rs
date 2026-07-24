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

//! Windows: manage the command's process tree, and nothing more.
//!
//! A job object with kill-on-close is put around the command, and the command is
//! placed in it. Closing the last handle to the job — which happens when the
//! command finishes, or the moment the boundary is told to tear it down — takes
//! every process in the job with it, so a command that started a shell that
//! started a program does not leave the program running behind it. This replaces
//! the reach that a "kill this process id" call cannot have on its own, and it
//! fires even if this client is the thing that crashed.
//!
//! It is not a filesystem or network boundary. A command run this way can still
//! write outside the folder and open the network, so it is declared as the
//! lifecycle tier and stays something the person is asked about. Fencing the
//! filesystem and the network off is a separate piece of work with its own
//! per-platform design, so this file consults neither the read-only carve nor the
//! network policy on the spec.

use super::{EnforcementTier, Prepared, SandboxSpec};
use anyhow::Result;
use win32job::Job;

/// Windows keeps the process-lifetime line but not the filesystem or network one,
/// so it declares the lifecycle tier. A stronger tier is only ever reported once
/// an operating-system boundary is actually in place to keep it.
pub fn enforcement_tier() -> EnforcementTier {
    EnforcementTier::Lifecycle
}

/// A command's job object, held for as long as the command runs.
///
/// Dropping it (or [`terminate`](SandboxedSpawn::terminate)) closes the last
/// handle to the job, which — with kill-on-close set — ends every process still
/// in it. `None` means the job could not be created; the command runs anyway,
/// unmanaged, because the process lifetime is a convenience and not a security
/// boundary, and the older tree-kill path is still there as a fallback.
pub struct SandboxedSpawn {
    job: Option<Job>,
}

/// Build the command run by the system shell, wrapped in the job that will end
/// its tree. This never fails the command: if the job cannot be set up, that is
/// logged and an unmanaged guard is returned. The process is bound to the job
/// after it is spawned, in [`SandboxedSpawn::adopt`], because that is when its
/// handle exists.
pub fn wrap(command: &str, _spec: &SandboxSpec) -> Result<Prepared> {
    let mut cmd = tokio::process::Command::new("cmd");
    cmd.arg("/C").arg(command);
    let guard = match build_job() {
        Ok(job) => SandboxedSpawn { job: Some(job) },
        Err(e) => {
            tracing::warn!(error = %e, "could not set up grouped teardown for the command; it will run unmanaged");
            SandboxedSpawn { job: None }
        }
    };
    Ok(Prepared { command: cmd, guard })
}

fn build_job() -> Result<Job> {
    let job = Job::create()?;
    let mut info = job.query_extended_limit_info()?;
    info.limit_kill_on_job_close();
    job.set_extended_limit_info(&info)?;
    Ok(job)
}

impl SandboxedSpawn {
    /// Place an already-spawned process in the job, so the tree it goes on to
    /// build is torn down with it. A failure here is logged, not fatal: the
    /// command is already running, and the fallback tree-kill still applies.
    pub fn adopt(&self, process_handle: isize) {
        if let Some(job) = &self.job {
            if let Err(e) = job.assign_process(process_handle) {
                tracing::warn!(error = %e, "could not place the command under its job object");
            }
        }
    }

    /// Tear the tree down now, by releasing the job. With kill-on-close set,
    /// closing the last handle ends every process still in it.
    pub fn terminate(&mut self) {
        self.job = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::process::Command as StdCommand;
    use std::time::{Duration, Instant};

    fn spec() -> SandboxSpec {
        SandboxSpec {
            workspace_rw: std::env::temp_dir(),
            ro_carve: vec![],
            net: super::super::NetPolicy::Denied,
        }
    }

    #[test]
    fn terminate_kills_the_adopted_process() {
        // A command placed under its job is ended when the job is released, with
        // no per-process-id call: this is the reach a "kill this pid" cannot have
        // and the reason the job object is used at all.
        let prepared = wrap("cmd", &spec()).expect("wrap");
        let mut sandbox = prepared.guard;
        assert!(sandbox.job.is_some(), "a job object should be created on Windows");

        // A process that would otherwise run for a very long time.
        let mut child = StdCommand::new("cmd")
            .args(["/C", "ping", "-n", "999", "127.0.0.1"])
            .spawn()
            .expect("spawn");
        sandbox.adopt(child.as_raw_handle() as isize);

        // Releasing the job ends it.
        sandbox.terminate();

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if child.try_wait().expect("try_wait").is_some() {
                break; // gone, as intended
            }
            assert!(Instant::now() < deadline, "the process outlived its released job");
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
    }

    #[test]
    fn a_command_without_a_job_still_runs() {
        // The job is a convenience, not a gate: if one cannot be created the guard
        // is empty and adopt/terminate are harmless no-ops rather than failures.
        let mut sandbox = SandboxedSpawn { job: None };
        sandbox.adopt(0);
        sandbox.terminate();
    }

    #[test]
    fn this_platform_declares_the_lifecycle_tier() {
        assert_eq!(enforcement_tier(), EnforcementTier::Lifecycle);
    }
}
