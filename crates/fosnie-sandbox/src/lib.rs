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

//! The boundary a command runs inside.
//!
//! A command the agent runs acts for the person at this keyboard, in the folder
//! they chose, and it should be able to reach no further than that: it may write
//! inside the folder and nowhere else, and the programs it starts should not be
//! able to open the network unless the command was allowed to. Where the
//! operating system can be made to hold that line itself, the line is real and
//! the person no longer has to vet every command by hand. Where it cannot yet,
//! the platform says so plainly and keeps asking.
//!
//! Two things are separate on purpose:
//!
//! - **What is enforced.** [`wrap`] turns a command line into the process that
//!   runs it, configured for the boundary its platform can keep, and returns a
//!   guard that tears down the whole process tree afterwards. The strength of
//!   that enforcement differs by platform, and the platform is the authority.
//! - **What is claimed.** [`enforcement_tier`] is the single source of the tier a
//!   platform declares. It is declared, never implied: a platform whose real
//!   boundary is unavailable reports the weaker tier and fails closed, so the tier
//!   a caller reads is always one the operating system will actually keep.
//!
//! Kept out of the desktop application so the same code can be exercised without
//! any of the application's user-interface stack — a boundary is a thing to test
//! on its own, on the operating system it targets.

use std::path::PathBuf;

/// Whether the programs a command starts may reach the network.
///
/// Binary for now: a loopback pinhole arrives with the egress proxy, and is not
/// expressible here until it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetPolicy {
    /// The command and anything it starts cannot open the network.
    Denied,
    /// No network restriction is imposed.
    Full,
}

/// The shape of the boundary one command should run inside: the one folder it may
/// write in, the paths within that folder that stay read-only even so (a repo's
/// `.git`, to keep a checked-out hook or config from being rewritten under the
/// person), and whether it may reach the network.
///
/// A backend consumes as much of this as its platform can enforce; a backend that
/// only manages the process lifetime records the rest without acting on it, so
/// the caller builds one description and every platform reads the same thing.
#[derive(Debug, Clone)]
pub struct SandboxSpec {
    pub workspace_rw: PathBuf,
    pub ro_carve: Vec<PathBuf>,
    pub net: NetPolicy,
}

/// How strong a boundary a platform will actually keep for a command.
///
/// `Full` means the operating system holds the filesystem and network line
/// itself; `Lifecycle` means only that the process tree is managed, and a command
/// is still asked about before it runs. A platform reports the tier it can keep,
/// never the one it would like to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementTier {
    Full,
    Lifecycle,
}

impl EnforcementTier {
    /// The token a client puts in its opening handshake so the instance learns,
    /// from the machine itself, how much it may safely stop asking about.
    pub fn as_capability(self) -> &'static str {
        match self {
            EnforcementTier::Full => "sandbox:full",
            EnforcementTier::Lifecycle => "sandbox:lifecycle",
        }
    }
}

/// A command built ready to run inside its boundary, with the guard that ends its
/// process tree. The caller sets the parts it owns — working directory, streams,
/// which variables the child may see, process-group placement — then spawns the
/// command and, on platforms that need it, hands the running process to the guard.
pub struct Prepared {
    pub command: tokio::process::Command,
    pub guard: SandboxedSpawn,
}

#[cfg(windows)]
#[path = "windows.rs"]
mod platform;
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;
#[cfg(all(unix, not(target_os = "linux")))]
#[path = "other_unix.rs"]
mod platform;

pub use platform::{enforcement_tier, wrap, SandboxedSpawn};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_strings_are_stable() {
        // The instance keys behaviour off these exact strings; they are protocol.
        assert_eq!(EnforcementTier::Full.as_capability(), "sandbox:full");
        assert_eq!(EnforcementTier::Lifecycle.as_capability(), "sandbox:lifecycle");
    }
}
