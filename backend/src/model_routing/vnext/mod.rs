// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0

//! Connections, variants and user-configured routing profiles.
//! Routing is functional configuration; it requires no savings certification.
pub mod catalogue;
pub mod connections;
pub mod deployments;
pub mod discovery;
pub mod pool;
pub mod profile;
pub mod profiles;
pub mod execution;
pub mod bindings;
pub mod variants;
pub mod recovery;

pub use fosnie_protocol::routing_vnext::{MAX_ACTIVE_VARIANTS, ROUTING_VNEXT_VERSION};

pub(super) fn digest<T: serde::Serialize>(
    domain: &str,
    value: &T,
) -> Result<String, super::RoutingError> {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(value)
        .map_err(|_| super::RoutingError::Invalid("vNext canonical contract"))?;
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(bytes);
    Ok(hex::encode(hash.finalize()))
}

pub(super) fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

#[cfg(test)]
mod tests;

pub mod cost;


pub mod selection;
