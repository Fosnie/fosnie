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

//! Unix platforms other than Linux (macOS): the boundary is not built here yet.
//!
//! This is the placeholder the real filesystem-and-network boundary will replace
//! on these platforms. Because that boundary does not exist, the platform must not
//! pretend it does: it declares the lifecycle tier, which fails closed to the
//! instance still asking about every command, and the command is run by the
//! system shell with no confinement. The existing process-group teardown in the
//! caller is unchanged and remains the way a command's tree is stopped here.

use super::{EnforcementTier, Prepared, SandboxSpec};
use anyhow::Result;

/// No operating-system boundary is in place on this platform, so it declares the
/// weaker tier and the instance keeps asking before it runs a command.
pub fn enforcement_tier() -> EnforcementTier {
    EnforcementTier::Lifecycle
}

/// Nothing is held here yet; the caller's own process-group teardown stands in.
pub struct SandboxedSpawn;

/// Run the command under the system shell, unconfined, until a real backend is
/// built for this platform.
pub fn wrap(command: &str, _spec: &SandboxSpec) -> Result<Prepared> {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(command);
    Ok(Prepared { command: cmd, guard: SandboxedSpawn })
}

impl SandboxedSpawn {
    /// Nothing to bind: the process group set up by the caller is the tree.
    pub fn adopt(&self, _process_handle: isize) {}

    /// Nothing to tear down here; the caller stops the process group directly.
    pub fn terminate(&mut self) {}
}
