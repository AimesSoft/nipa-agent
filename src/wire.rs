//! Chat Completions wire types.
//!
//! Deliberately loose: unknown provider fields are preserved via
//! `#[serde(flatten)] extra` maps so that provider quirks
//! (`reasoning_content`, `thought_signature`, ...) survive a round-trip
//! without the runtime knowing about each one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    /// `None` for assistant messages that only carry tool_calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Set on role="tool" messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Provider-specific fields (reasoning_content, ...). Kept on receive,
    /// filtered by CompatFlags::strip_from_history before re-entering history.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::plain("system", content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::plain("user", content)
    }
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            extra: Map::new(),
        }
    }
    fn plain(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Some gateways omit ids; the runtime backfills a synthetic one so that
    /// tool results can always be correlated.
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default = "default_tool_call_type")]
    pub call_type: String,
    pub function: FunctionCall,
}

fn default_tool_call_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Raw string, exactly as the model produced it. Parsing happens at
    /// dispatch time; a parse failure is fed back to the model (codex design).
    #[serde(default)]
    pub arguments: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: ChatMessage,
    /// Advisory only. DeepSeek/sglang may say "stop" while tool_calls are
    /// present — the loop must check tool_calls itself. Deserialized for
    /// diagnostics; never used for control flow.
    #[serde(default)]
    #[allow(dead_code)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
}

/// Request body. `extra_body` from provider config is merged at the top level.
pub(crate) fn build_request_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[Value],
    send_tool_choice: bool,
    extra_body: &Map<String, Value>,
) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), Value::String(model.into()));
    body.insert("messages".into(), serde_json::to_value(messages).expect("messages serialize"));
    body.insert("tools".into(), Value::Array(tools.to_vec()));
    if send_tool_choice {
        body.insert("tool_choice".into(), Value::String("auto".into()));
    }
    body.insert("stream".into(), Value::Bool(false));
    for (k, v) in extra_body {
        body.insert(k.clone(), v.clone());
    }
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_deepseek_style_response_with_extra_fields() {
        // reasoning_content is a DeepSeek extension; must survive into `extra`.
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": "thinking...",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "search_tmdb", "arguments": "{\"query\":\"Frieren\"}"}
                    }]
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20}
        });
        let resp: ChatResponse = serde_json::from_value(json).unwrap();
        let msg = &resp.choices[0].message;
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].function.name, "search_tmdb");
        assert!(msg.extra.contains_key("reasoning_content"));
        // finish_reason says stop even though tool_calls are present — the
        // exact trap the loop guards against.
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn tool_result_message_shape() {
        let m = ChatMessage::tool_result("call_1", "{\"ok\":true}");
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["tool_call_id"], "call_1");
        assert!(v.get("tool_calls").is_none());
    }

    #[test]
    fn request_body_merges_extra_body() {
        let mut extra = Map::new();
        extra.insert("enable_thinking".into(), Value::Bool(false));
        let body = build_request_body("m", &[], &[], true, &extra);
        assert_eq!(body["enable_thinking"], false);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["stream"], false);
    }
}
