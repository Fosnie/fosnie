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

//! Capability-aware reasoning control.
//!
//! One control that auto-adapts to the connected LLM: a model that doesn't reason
//! hides the control, a levels model shows segments, an always-on model hides
//! "Off". Detection is **provider-kind by host + a best-effort model heuristic**,
//! overridden by the operator's `reasoning_mode` (`auto|none|toggle|levels|budget|
//! always_on`) on the llm `provider_configs` row — the override always wins, since
//! model names churn and local engines are arbitrary (addendum). Core, fully
//! provider-agnostic: it computes a capability descriptor; the per-provider
//! *translation* of a chosen level into the wire parameter lives in the ML service.

use serde::Serialize;

// The per-turn reasoning request rides on a chat frame, so it is part of the
// wire contract and lives with the rest of it — shared verbatim with the clients
// that are compiled separately from the server. What a *model* can do with
// reasoning, below, stays here: it is computed from provider configuration the
// client never sees.
pub use fosnie_protocol::ReasoningSpec;

/// What a model can do with reasoning, used to render the right control and to
/// gate the backend so a reasoning parameter is never sent to a model that
/// rejects it (the non-reasoning-OpenAI 400 footgun).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReasoningCapability {
    pub mode: Mode,
    /// Supported discrete effort levels for `levels`/`budget` modes (e.g.
    /// `["low","medium","high"]`). Empty for `none`/`toggle`.
    pub levels: Vec<String>,
    /// Whether the model can have reasoning turned **off** (drives the "Off"
    /// segment in the UI and whether the backend may send a disable).
    pub can_disable: bool,
    /// Whether the provider can return a reasoning trace for display.
    pub supports_trace: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Model doesn't reason → control hidden, no parameter sent.
    None,
    /// Reasoning on/off only (typical local reasoning models).
    Toggle,
    /// Discrete effort (OpenAI reasoning, Anthropic, Gemini 3) → segments.
    Levels,
    /// Numeric token budget surfaced as Off/Low/Med/High (Anthropic legacy,
    /// Gemini 2.5); the budget mapping is applied downstream.
    Budget,
    /// Always reasons, cannot be disabled (Gemini *-pro, Anthropic Fable/Mythos)
    /// → UI hides "Off", backend never sends a disable.
    AlwaysOn,
}

impl ReasoningCapability {
    fn none() -> Self {
        Self { mode: Mode::None, levels: vec![], can_disable: true, supports_trace: false }
    }
    fn toggle(supports_trace: bool) -> Self {
        Self { mode: Mode::Toggle, levels: vec![], can_disable: true, supports_trace }
    }
    fn levels(can_disable: bool) -> Self {
        Self::levels_with(vec!["low".into(), "medium".into(), "high".into()], can_disable)
    }
    fn levels_with(levels: Vec<String>, can_disable: bool) -> Self {
        Self { mode: Mode::Levels, levels, can_disable, supports_trace: true }
    }
    fn budget(can_disable: bool) -> Self {
        Self {
            mode: Mode::Budget,
            levels: vec!["low".into(), "medium".into(), "high".into()],
            can_disable,
            supports_trace: true,
        }
    }
    fn always_on(mode_levels: bool) -> Self {
        Self {
            mode: Mode::AlwaysOn,
            levels: if mode_levels { vec!["low".into(), "medium".into(), "high".into()] } else { vec![] },
            can_disable: false,
            supports_trace: true,
        }
    }
}

/// Lower-cased host of a base URL (`https://api.openai.com/v1` → `api.openai.com`).
pub(crate) fn host_of(base_url: &str) -> String {
    let s = base_url.trim();
    // Strip scheme.
    let after_scheme = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
    // Authority ends at the first '/', '?' or '#'.
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Drop any userinfo and port.
    let host = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let host = host.split_once(':').map(|(h, _)| h).unwrap_or(host);
    host.to_ascii_lowercase()
}

pub(crate) fn is_anthropic(host: &str) -> bool {
    host == "api.anthropic.com" || host.ends_with(".anthropic.com")
}
pub(crate) fn is_openai(host: &str) -> bool {
    host == "api.openai.com" || host.ends_with(".openai.com")
}
pub(crate) fn is_gemini(host: &str) -> bool {
    host == "generativelanguage.googleapis.com" || host.ends_with(".googleapis.com")
}

/// Compute the effective capability for an llm endpoint+model.
///
/// `override_mode` is the operator's `reasoning_mode` from the resolved
/// `provider_configs` row (`None`/`"auto"` ⇒ auto-detect). The override always
/// wins (addendum). `base_url`/`model` empty ⇒ the ML service `.env` default
/// is in force, which we can't introspect here — degrade to `toggle` (a safe,
/// non-committal control) unless an override says otherwise.
pub fn detect(
    base_url: Option<&str>,
    model: Option<&str>,
    override_mode: Option<&str>,
) -> ReasoningCapability {
    // Manual override beats everything.
    match override_mode.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("none") => return ReasoningCapability::none(),
        Some("toggle") => return ReasoningCapability::toggle(true),
        Some("levels") => return ReasoningCapability::levels(true),
        Some("budget") => return ReasoningCapability::budget(true),
        Some("always_on") => return ReasoningCapability::always_on(true),
        // "auto", "", or unknown → fall through to detection.
        _ => {}
    }

    let Some(base_url) = base_url.filter(|s| !s.trim().is_empty()) else {
        // Unknown endpoint (ML .env default) → safe, minimal control.
        return ReasoningCapability::toggle(true);
    };
    let host = host_of(base_url);
    let m = model.unwrap_or("").trim().to_ascii_lowercase();

    if is_anthropic(&host) {
        // Newer Anthropic flagships (Fable/Mythos 5) reason always-on; the rest
        // expose adaptive effort levels and can be turned off.
        if m.contains("fable") || m.contains("mythos") {
            return ReasoningCapability::always_on(true);
        }
        return ReasoningCapability::levels(true);
    }
    if is_openai(&host) {
        // Only reasoning models (gpt-5.x, o-series) accept reasoning_effort; on a
        // plain chat model the parameter is a hard error → hide the control.
        if is_openai_reasoning(&m) {
            // gpt-5.x documents `none` (can disable) + an extra `xhigh` level;
            // o-series floors at `low` and can't be disabled.
            if m.starts_with("gpt-5") {
                return ReasoningCapability::levels_with(
                    vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()],
                    true,
                );
            }
            return ReasoningCapability::levels(false);
        }
        return ReasoningCapability::none();
    }
    if is_gemini(&host) {
        // 2.5/3 Pro think unconditionally; others expose a thinking budget.
        if m.contains("pro") {
            return ReasoningCapability::always_on(true);
        }
        return ReasoningCapability::budget(true);
    }
    // Local / unknown OpenAI-compatible engine (vLLM/Ollama/llama.cpp): reasoning
    // is usually a deploy-time on/off, not graduated — expose a toggle, never fake
    // Low/Med/High (addendum). An operator override promotes it to budget/levels.
    ReasoningCapability::toggle(true)
}

/// Best-effort: does this OpenAI model id name a reasoning model? (`gpt-5*`, `o1`+.)
fn is_openai_reasoning(model_lower: &str) -> bool {
    model_lower.starts_with("gpt-5")
        || model_lower
            .strip_prefix('o')
            .and_then(|r| r.chars().next())
            .map(|c| c.is_ascii_digit() && c != '0')
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_for_scaffolding_clamps_high_levels() {
        let spec = |lvl: Option<&str>, en: bool| ReasoningSpec {
            enabled: en,
            level: lvl.map(String::from),
            return_trace: true,
        };
        // High effort → clamped to low; capped flag true.
        let high = spec(Some("high"), true);
        assert_eq!(high.capped_for_scaffolding().level.as_deref(), Some("low"));
        assert!(high.is_capped_for_scaffolding());
        assert_eq!(spec(Some("xhigh"), true).capped_for_scaffolding().level.as_deref(), Some("low"));
        assert_eq!(spec(Some("auto"), true).capped_for_scaffolding().level.as_deref(), Some("low"));
        // Already at/below the cap → unchanged; capped flag false.
        assert_eq!(spec(Some("low"), true).capped_for_scaffolding().level.as_deref(), Some("low"));
        assert!(!spec(Some("low"), true).is_capped_for_scaffolding());
        assert_eq!(spec(Some("minimal"), true).capped_for_scaffolding().level.as_deref(), Some("minimal"));
        assert_eq!(spec(None, true).capped_for_scaffolding().level, None);
        assert!(!spec(None, true).is_capped_for_scaffolding());
        // Disabled reasoning → never capped; `enabled` preserved through the clamp.
        assert!(!spec(Some("high"), false).is_capped_for_scaffolding());
        assert!(high.capped_for_scaffolding().enabled);
    }

    #[test]
    fn clamped_to_caps_effort_down_only() {
        let spec = |lvl: Option<&str>, en: bool| ReasoningSpec {
            enabled: en,
            level: lvl.map(String::from),
            return_trace: true,
        };
        // Above the cap → pulled down to it.
        assert_eq!(spec(Some("high"), true).clamped_to("medium").level.as_deref(), Some("medium"));
        assert_eq!(spec(Some("xhigh"), true).clamped_to("medium").level.as_deref(), Some("medium"));
        // At/below the cap → respected (never raised).
        assert_eq!(spec(Some("low"), true).clamped_to("medium").level.as_deref(), Some("low"));
        assert_eq!(spec(Some("medium"), true).clamped_to("medium").level.as_deref(), Some("medium"));
        // auto / None → pulled to the cap (provider default could be heavy).
        assert_eq!(spec(Some("auto"), true).clamped_to("medium").level.as_deref(), Some("medium"));
        assert_eq!(spec(None, true).clamped_to("medium").level.as_deref(), Some("medium"));
        // Disabled reasoning → untouched.
        assert!(!spec(Some("high"), false).clamped_to("medium").enabled);
        assert_eq!(spec(Some("high"), false).clamped_to("medium").level.as_deref(), Some("high"));
        // Unrecognised cap → no-op (safety).
        assert_eq!(spec(Some("high"), true).clamped_to("bogus").level.as_deref(), Some("high"));
        // return_trace is preserved through the clamp.
        assert!(spec(Some("high"), true).clamped_to("medium").return_trace);
    }

    #[test]
    fn host_parsing() {
        assert_eq!(host_of("https://api.openai.com/v1"), "api.openai.com");
        assert_eq!(host_of("https://user:pw@api.anthropic.com:443/v1"), "api.anthropic.com");
        assert_eq!(host_of("http://localhost:8000/v1"), "localhost");
    }

    #[test]
    fn override_wins_over_detection() {
        // An OpenAI non-reasoning model would auto-detect `none`, but an override forces levels.
        let cap = detect(Some("https://api.openai.com/v1"), Some("gpt-4o"), Some("levels"));
        assert_eq!(cap.mode, Mode::Levels);
    }

    #[test]
    fn openai_reasoning_vs_chat() {
        assert_eq!(detect(Some("https://api.openai.com"), Some("gpt-5.5"), None).mode, Mode::Levels);
        assert!(detect(Some("https://api.openai.com"), Some("gpt-5.5"), None).can_disable);
        // gpt-5.x exposes the extra `xhigh` level.
        assert!(detect(Some("https://api.openai.com"), Some("gpt-5.5"), None).levels.contains(&"xhigh".to_string()));
        assert!(!detect(Some("https://api.openai.com"), Some("o3"), None).levels.contains(&"xhigh".to_string()));
        assert_eq!(detect(Some("https://api.openai.com"), Some("o3"), None).mode, Mode::Levels);
        assert!(!detect(Some("https://api.openai.com"), Some("o3"), None).can_disable);
        // Non-reasoning chat model → control hidden, nothing sent.
        assert_eq!(detect(Some("https://api.openai.com"), Some("gpt-4o"), None).mode, Mode::None);
    }

    #[test]
    fn anthropic_levels_and_always_on() {
        assert_eq!(detect(Some("https://api.anthropic.com/v1"), Some("claude-opus-4-8"), None).mode, Mode::Levels);
        assert_eq!(detect(Some("https://api.anthropic.com/v1"), Some("claude-fable-5"), None).mode, Mode::AlwaysOn);
    }

    #[test]
    fn gemini_budget_and_pro() {
        assert_eq!(detect(Some("https://generativelanguage.googleapis.com"), Some("gemini-2.5-flash"), None).mode, Mode::Budget);
        assert_eq!(detect(Some("https://generativelanguage.googleapis.com"), Some("gemini-3-pro"), None).mode, Mode::AlwaysOn);
    }

    #[test]
    fn local_is_toggle_and_unknown_endpoint_degrades() {
        assert_eq!(detect(Some("http://localhost:8000/v1"), Some("qwen3-32b"), None).mode, Mode::Toggle);
        assert_eq!(detect(None, None, None).mode, Mode::Toggle);
        // Override still applies with no endpoint.
        assert_eq!(detect(None, None, Some("none")).mode, Mode::None);
    }
}
