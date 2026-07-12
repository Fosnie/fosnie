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

//! Capability detection for image vision (multimodal input).
//!
//! Decides whether the connected LLM can be sent an image as a content block.
//! When it can, an attached image is forwarded as a vision part (built in
//! `chat::run_turn`); when it can't, the backend falls back to OCR'd text so the
//! image is still useful. Detection mirrors `reasoning::detect`: provider-kind by
//! host + a best-effort model heuristic, with an operator override that always
//! wins. Core, provider-agnostic — the per-provider *translation* of an image
//! part to the wire shape lives in the ML adapters.

/// Whether the given llm endpoint+model can accept image input.
///
/// `override_kind` is an operator dial (`on`/`off`/`auto`); `None`/`"auto"` ⇒
/// auto-detect. An empty/unset endpoint (ML `.env` default) can't be introspected
/// here, so we degrade to **false** (OCR fallback) — safe, since a non-vision
/// model sent an image part typically errors. An override of `on` lets an operator
/// enable vision for a local multimodal model (e.g. Qwen-VL).
pub fn detect(base_url: Option<&str>, model: Option<&str>, override_kind: Option<&str>) -> bool {
    match override_kind.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("on") | Some("true") | Some("yes") => return true,
        Some("off") | Some("false") | Some("no") => return false,
        _ => {}
    }

    let Some(base_url) = base_url.filter(|s| !s.trim().is_empty()) else {
        return false;
    };
    let host = crate::reasoning::host_of(base_url);
    let m = model.unwrap_or("").trim().to_ascii_lowercase();

    if crate::reasoning::is_anthropic(&host) {
        // Every modern Claude (3.x/4.x, Fable/Mythos) is multimodal.
        return true;
    }
    if crate::reasoning::is_openai(&host) {
        // gpt-4o / gpt-4.1 / gpt-5.x / o3+ are multimodal; legacy gpt-3.5 and the
        // original gpt-4 text model are not.
        if m.contains("3.5") || m == "gpt-4" || m.starts_with("gpt-4-") && m.contains("0314") {
            return false;
        }
        return m.contains("4o")
            || m.contains("4.1")
            || m.starts_with("gpt-5")
            || m.starts_with("gpt-4")
            || m.starts_with("o3")
            || m.starts_with("o4")
            || m.contains("vision");
    }
    if crate::reasoning::is_gemini(&host) {
        // Gemini 1.5/2.x/3 are all natively multimodal.
        return true;
    }
    // Local / unknown OpenAI-compatible engine: can't know — default off (OCR),
    // an operator override promotes a multimodal local model to vision.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_and_gemini_are_vision() {
        assert!(detect(Some("https://api.anthropic.com/v1"), Some("claude-opus-4-8"), None));
        assert!(detect(Some("https://generativelanguage.googleapis.com"), Some("gemini-2.5-flash"), None));
    }

    #[test]
    fn openai_vision_vs_text() {
        assert!(detect(Some("https://api.openai.com/v1"), Some("gpt-4o"), None));
        assert!(detect(Some("https://api.openai.com/v1"), Some("gpt-5.5"), None));
        assert!(!detect(Some("https://api.openai.com/v1"), Some("gpt-3.5-turbo"), None));
    }

    #[test]
    fn local_defaults_off_and_override_wins() {
        assert!(!detect(Some("http://localhost:8000/v1"), Some("qwen3-vl"), None));
        assert!(detect(Some("http://localhost:8000/v1"), Some("qwen3-vl"), Some("on")));
        assert!(!detect(Some("https://api.anthropic.com/v1"), Some("claude-opus-4-8"), Some("off")));
        // Unknown endpoint (ML .env default) → OCR fallback.
        assert!(!detect(None, None, None));
    }
}
