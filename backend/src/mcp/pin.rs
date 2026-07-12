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

//! Tool-poisoning / "rug-pull" defence (FEATURE B1, acceptance #6).
//!
//! On approval we pin a fingerprint of each tool's `name + description + schema`.
//! On every reconnect/health-sweep we diff the live catalog against the pinned set;
//! any changed, removed, or newly-appeared tool means the server's advertised
//! capabilities drifted from what the admin reviewed → auto-quarantine + alert.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::mcp::ToolCatalogEntry;

/// Stable fingerprint of one tool definition (domain-tagged, field-delimited).
pub fn tool_fingerprint(name: &str, description: &str, schema: &Value) -> String {
    let mut h = Sha256::new();
    h.update(b"pai.mcp.tool.v1");
    h.update(name.as_bytes());
    h.update([0]);
    h.update(description.as_bytes());
    h.update([0]);
    h.update(serde_json::to_vec(schema).unwrap_or_default());
    hex::encode(h.finalize())
}

/// `toolName -> fingerprint` for a catalog — the pinned form stored at approval.
pub fn fingerprints(catalog: &[ToolCatalogEntry]) -> BTreeMap<String, String> {
    catalog
        .iter()
        .map(|t| (t.name.clone(), tool_fingerprint(&t.name, &t.description, &t.schema)))
        .collect()
}

/// Compare the live catalog against the pinned fingerprints. Returns the first drift
/// reason (changed / disappeared / newly-appeared tool), or `None` if they match.
pub fn diff(pinned: &Map<String, Value>, live: &[ToolCatalogEntry]) -> Option<String> {
    let live_fp = fingerprints(live);
    for (name, fp) in pinned {
        match live_fp.get(name) {
            None => return Some(format!("pinned tool '{name}' disappeared")),
            Some(lf) if Some(lf.as_str()) != fp.as_str() => {
                return Some(format!("tool '{name}' definition changed since approval"));
            }
            _ => {}
        }
    }
    for t in live {
        if !pinned.contains_key(&t.name) {
            return Some(format!("new tool '{}' appeared since approval", t.name));
        }
    }
    None
}
