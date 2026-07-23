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

//! The per-turn reasoning request, which rides on a chat frame and is therefore
//! part of the wire contract. What a *model* can do with reasoning — the
//! capability descriptor and its detection — stays on the server, where the
//! provider configuration lives.

use serde::{Deserialize, Serialize};

/// The per-turn reasoning request from the client.
/// `enabled` is kept distinct from `level` so an explicit "off" is unambiguous;
/// `level` is `low|medium|high|max|auto` (None ⇒ provider default effort).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningSpec {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub level: Option<String>,
    /// Whether to stream/return the reasoning trace for display (paid for either
    /// way). Defaults true so a new model never silently loses its trace.
    #[serde(default = "default_true")]
    pub return_trace: bool,
}

fn default_true() -> bool {
    true
}

impl ReasoningSpec {
    /// A reduced copy for internal scaffolding — the agentic tool loop, which only
    /// decides which tool to call, not the final answer. Keeps
    /// `enabled`/`return_trace` but clamps the effort DOWN to at most `low`, so
    /// tool-deciding steps stay fast (a heavy reasoning model otherwise burns minutes
    /// per non-streaming step → timeout). The user's full effort still applies to the
    /// streamed final answer. A no-op on non-reasoning/local models (the ML side omits
    /// the field there anyway).
    pub fn capped_for_scaffolding(&self) -> ReasoningSpec {
        let level = match self.level.as_deref().map(str::trim) {
            None => None,
            Some(l) if l.eq_ignore_ascii_case("minimal") || l.eq_ignore_ascii_case("low") => {
                self.level.clone()
            }
            _ => Some("low".to_string()),
        };
        ReasoningSpec { enabled: self.enabled, level, return_trace: self.return_trace }
    }

    /// Whether `capped_for_scaffolding` would actually reduce this spec (reasoning is
    /// enabled and its level is above the `low` cap) — the terminal tool-loop content
    /// is then low-reasoning and should not be reused verbatim as the answer.
    pub fn is_capped_for_scaffolding(&self) -> bool {
        if !self.enabled {
            return false;
        }
        match self.level.as_deref().map(str::trim) {
            None => false,
            Some(l) => !(l.eq_ignore_ascii_case("minimal") || l.eq_ignore_ascii_case("low")),
        }
    }

    /// Clamp the effort DOWN to `cap` (never up), used for the RAG final answer
    /// the sub-answers already did the local work, so a lower effort
    /// on the synthesis pass is ~as good and cuts minutes. A per-turn choice already at
    /// or below the cap is respected; `auto`/None (provider default, potentially heavy)
    /// is pulled to the cap. No-op when reasoning is disabled or `cap` is unrecognised.
    pub fn clamped_to(&self, cap: &str) -> ReasoningSpec {
        let rank = |l: &str| match l.trim().to_ascii_lowercase().as_str() {
            "minimal" => Some(0),
            "low" => Some(1),
            "medium" => Some(2),
            "high" => Some(3),
            "xhigh" => Some(4),
            "max" => Some(5),
            _ => None, // auto / unknown → not a comparable level
        };
        let Some(cap_rank) = rank(cap) else { return self.clone() };
        if !self.enabled {
            return self.clone();
        }
        let level = match self.level.as_deref().and_then(rank) {
            Some(cur) if cur <= cap_rank => self.level.clone(), // already at/below the cap
            _ => Some(cap.trim().to_ascii_lowercase()),          // above cap, or auto/None
        };
        ReasoningSpec { enabled: self.enabled, level, return_trace: self.return_trace }
    }

    /// Derive a spec from the legacy `thinking:"adaptive:<level>"` / `"off"` string
    /// (wire back-compat) so an old client keeps working.
    pub fn from_legacy(thinking: Option<&str>) -> Option<Self> {
        let s = thinking?.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("off") {
            return Some(Self { enabled: false, level: None, return_trace: true });
        }
        let level = s.split_once(':').map(|(_, l)| l.trim().to_string()).filter(|l| !l.is_empty());
        Some(Self { enabled: true, level, return_trace: true })
    }
}
