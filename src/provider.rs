use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::AgentError;

/// Per-provider compatibility switches for OpenAI-compatible endpoints.
///
/// The flash-tier providers we target (Gemini / DeepSeek / Qwen / GLM / Groq /
/// OpenRouter / Ollama) all speak Chat Completions, but differ in the corners.
/// Design borrowed from pi-ai's compat flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompatFlags {
    /// Send `tool_choice: "auto"`. A few gateways reject the field entirely.
    pub send_tool_choice: bool,
    /// Scan plain-text assistant content for tool calls the model failed to
    /// emit structurally (DeepSeek does this ~20% of the time).
    pub detect_pseudo_tool_calls: bool,
    /// Assistant-message fields stripped before the message re-enters history.
    /// DeepSeek returns 400 if `reasoning_content` is sent back.
    pub strip_from_history: Vec<String>,
}

impl Default for CompatFlags {
    fn default() -> Self {
        Self {
            send_tool_choice: true,
            detect_pseudo_tool_calls: true,
            strip_from_history: vec![
                "reasoning_content".to_string(),
                "reasoning".to_string(),
                "thinking".to_string(),
            ],
        }
    }
}

/// Provider configuration: everything needed to reach one OpenAI-compatible
/// Chat Completions endpoint. Field design follows codex's `ModelProviderInfo`
/// (all-optional serde struct; defaults and caps live in the getters).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProviderInfo {
    /// Display name, used in error messages ("deepseek", "gemini", ...).
    pub name: String,
    /// Base URL up to and excluding `/chat/completions`,
    /// e.g. `https://api.deepseek.com/v1`.
    pub base_url: String,
    /// Inline API key. Takes precedence over `api_key_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Environment variable to read the key from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Extra HTTP headers attached to every request.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub http_headers: BTreeMap<String, String>,
    /// Max retries per model request (transport level). Default 4, capped at 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_max_retries: Option<u32>,
    /// Per-request timeout in milliseconds. Default 120_000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_ms: Option<u64>,
    #[serde(default)]
    pub compat: CompatFlags,
    /// Provider-specific request-body fields, flattened into every request,
    /// e.g. `{"enable_thinking": false}` for Qwen.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra_body: Map<String, Value>,
}

impl ModelProviderInfo {
    pub const MAX_RETRIES_CAP: u32 = 10;
    pub const DEFAULT_RETRIES: u32 = 4;
    pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;

    pub fn new(name: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            api_key: None,
            api_key_env: None,
            http_headers: BTreeMap::new(),
            request_max_retries: None,
            request_timeout_ms: None,
            compat: CompatFlags::default(),
            extra_body: Map::new(),
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    pub fn max_retries(&self) -> u32 {
        self.request_max_retries
            .unwrap_or(Self::DEFAULT_RETRIES)
            .min(Self::MAX_RETRIES_CAP)
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms.unwrap_or(Self::DEFAULT_TIMEOUT_MS))
    }

    pub fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Resolve the API key. A whitespace-only inline key is treated as unset;
    /// a configured-but-missing env var is a hard config error (codex behavior).
    /// `Ok(None)` means "no key required" (local Ollama etc.).
    pub fn resolve_api_key(&self) -> Result<Option<String>, AgentError> {
        if let Some(k) = &self.api_key {
            let k = k.trim();
            if !k.is_empty() {
                return Ok(Some(k.to_string()));
            }
        }
        if let Some(env) = &self.api_key_env {
            return match std::env::var(env) {
                Ok(v) if !v.trim().is_empty() => Ok(Some(v.trim().to_string())),
                _ => Err(AgentError::Config(format!(
                    "environment variable {env} is not set (API key for provider '{}')",
                    self.name
                ))),
            };
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_join_handles_trailing_slash() {
        let p = ModelProviderInfo::new("x", "https://api.example.com/v1/");
        assert_eq!(p.chat_completions_url(), "https://api.example.com/v1/chat/completions");
        let p = ModelProviderInfo::new("x", "https://api.example.com/v1");
        assert_eq!(p.chat_completions_url(), "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn retries_are_capped() {
        let mut p = ModelProviderInfo::new("x", "http://localhost");
        p.request_max_retries = Some(99);
        assert_eq!(p.max_retries(), ModelProviderInfo::MAX_RETRIES_CAP);
    }

    #[test]
    fn blank_inline_key_is_unset() {
        let p = ModelProviderInfo::new("x", "http://localhost").with_api_key("   ");
        assert!(p.resolve_api_key().unwrap().is_none());
    }
}
