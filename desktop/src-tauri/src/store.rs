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

//! Where the pairing lives between runs.
//!
//! The device token is a bearer credential with the owner's rights, so the only
//! place it is written is the operating system's own credential store — the
//! Windows Credential Manager, the macOS Keychain, the Secret Service on Linux.
//! Nothing this process writes to disk contains it, and it is never logged: the
//! `Debug` impl below exists to make that impossible to do by accident.

use std::fmt;

use anyhow::{Context, Result};

/// The credential-store service name. Stable across releases: changing it strands
/// every already-paired machine on a pairing screen.
const SERVICE: &str = "fosnie-desktop";
const ENTRY_INSTANCE: &str = "instance-url";
const ENTRY_TOKEN: &str = "device-token";
const ENTRY_DEVICE_ID: &str = "device-id";

/// A paired instance: where it is, and the credential that speaks for the owner.
///
/// Deliberately not serialisable. What the window is given is assembled by hand
/// from the two fields it needs; a token-carrying struct that can be turned into
/// JSON by a single call is one careless line away from being written to a log,
/// an error body or a file.
#[derive(Clone)]
pub struct Pairing {
    pub base_url: String,
    pub token: String,
    /// This machine's device id, so the client can withdraw itself on sign-out.
    /// Absent for a pairing made before the id was recorded.
    pub device_id: Option<String>,
}

// Formatting a pairing prints where it points and says nothing about the token.
// A credential that reaches a log line has left the credential store.
impl fmt::Debug for Pairing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pairing").field("base_url", &self.base_url).finish_non_exhaustive()
    }
}

fn entry(name: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, name).context("the credential store is unavailable")
}

/// Read an entry, treating "no such entry" as absence rather than failure.
fn read(name: &str) -> Result<Option<String>> {
    match entry(name)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {name} from the credential store")),
    }
}

fn write(name: &str, value: &str) -> Result<()> {
    entry(name)?
        .set_password(value)
        .with_context(|| format!("writing {name} to the credential store"))
}

fn clear(name: &str) -> Result<()> {
    match entry(name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("clearing {name} from the credential store")),
    }
}

/// The current pairing, or `None` when this machine has not been paired (or has
/// been signed out). A half-written pairing counts as none: without both halves
/// there is nothing to connect with.
pub fn load() -> Result<Option<Pairing>> {
    let (Some(base_url), Some(token)) = (read(ENTRY_INSTANCE)?, read(ENTRY_TOKEN)?) else {
        return Ok(None);
    };
    Ok(Some(Pairing { base_url, token, device_id: read(ENTRY_DEVICE_ID)? }))
}

pub fn save(pairing: &Pairing) -> Result<()> {
    write(ENTRY_INSTANCE, &pairing.base_url)?;
    write(ENTRY_TOKEN, &pairing.token)?;
    if let Some(id) = &pairing.device_id {
        write(ENTRY_DEVICE_ID, id)?;
    }
    Ok(())
}

/// Forget the pairing. Deliberately tolerant: signing out has to succeed even if
/// one entry has already gone, or a client whose store is half-empty could never
/// get back to a clean pairing screen.
pub fn forget() -> Result<()> {
    let results = [clear(ENTRY_TOKEN), clear(ENTRY_INSTANCE), clear(ENTRY_DEVICE_ID)];
    results.into_iter().collect::<Result<Vec<_>>>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_carries_the_token() {
        let p = Pairing {
            base_url: "https://ai.example.com".into(),
            token: "sk-fosnie-supersecret".into(),
            device_id: None,
        };
        let rendered = format!("{p:?}");
        assert!(rendered.contains("ai.example.com"));
        assert!(!rendered.contains("supersecret"), "the token reached a log line: {rendered}");
    }
}
