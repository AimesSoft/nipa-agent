//! Detection of "pseudo tool calls": tool invocations the model emitted as
//! plain text content instead of the structured `tool_calls` field.
//! DeepSeek does this ~20% of the time; other flash models occasionally too.
//!
//! Strategy: only accept high-confidence shapes — a JSON object (possibly
//! inside a ```json fence) carrying a known tool name plus arguments. False
//! positives are worse than misses (a miss just costs one reminder round).

use serde_json::Value;

use crate::wire::{FunctionCall, ToolCall};

/// Scan assistant text content for a tool call. `known_tools` must contain
/// every registered tool name including submit_result.
pub(crate) fn detect_pseudo_tool_call(content: &str, known_tools: &[&str]) -> Option<ToolCall> {
    let candidate = extract_json_object(content)?;
    let obj = candidate.as_object()?;

    // Shape 1: {"name": "search_tmdb", "arguments": {...}}
    // Shape 2: {"tool": "search_tmdb", "parameters"|"arguments": {...}}
    // Shape 3: {"function": {"name": ..., "arguments": ...}} (echo of wire format)
    let (name, args) = if let Some(func) = obj.get("function").and_then(Value::as_object) {
        (
            func.get("name").and_then(Value::as_str)?,
            func.get("arguments")
                .cloned()
                .unwrap_or(Value::Object(Default::default())),
        )
    } else {
        let name = obj
            .get("name")
            .or_else(|| obj.get("tool"))
            .or_else(|| obj.get("tool_name"))
            .and_then(Value::as_str)?;
        let args = obj
            .get("arguments")
            .or_else(|| obj.get("parameters"))
            .or_else(|| obj.get("args"))
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        (name, args)
    };

    if !known_tools.contains(&name) {
        return None;
    }
    let arguments = match args {
        // Some models double-encode arguments as a string.
        Value::String(s) => s,
        other => other.to_string(),
    };
    Some(ToolCall {
        id: format!("pseudo_{}", fastrand::u64(..)),
        call_type: "function".into(),
        function: FunctionCall {
            name: name.to_string(),
            arguments,
        },
    })
}

/// Pull the first plausible JSON object out of the text: either the whole
/// trimmed content, or the body of a ``` fence, or a braces-balanced span.
fn extract_json_object(content: &str) -> Option<Value> {
    let trimmed = content.trim();
    if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(trimmed) {
        return Some(v);
    }
    // ```json ... ``` fence
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        let after = after.strip_prefix("json").unwrap_or(after);
        if let Some(end) = after.find("```") {
            if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(after[..end].trim()) {
                return Some(v);
            }
        }
    }
    // First balanced {...} span
    let bytes = trimmed.as_bytes();
    let open = trimmed.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        if escape {
            escape = false;
            continue;
        }
        match b {
            b'\\' if in_str => escape = true,
            b'"' => in_str = !in_str,
            b'{' if !in_str => depth += 1,
            b'}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    if let Ok(v @ Value::Object(_)) =
                        serde_json::from_str::<Value>(&trimmed[open..=i])
                    {
                        return Some(v);
                    }
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: &[&str] = &["search_tmdb", "submit_result"];

    #[test]
    fn detects_bare_json_call() {
        let c = r#"{"name": "search_tmdb", "arguments": {"query": "葬送的芙莉莲"}}"#;
        let call = detect_pseudo_tool_call(c, TOOLS).unwrap();
        assert_eq!(call.function.name, "search_tmdb");
        assert!(call.function.arguments.contains("芙莉莲"));
    }

    #[test]
    fn detects_fenced_call_with_prose() {
        let c = "I'll search for this title.\n```json\n{\"tool\": \"search_tmdb\", \"parameters\": {\"query\": \"Frieren\"}}\n```";
        let call = detect_pseudo_tool_call(c, TOOLS).unwrap();
        assert_eq!(call.function.name, "search_tmdb");
    }

    #[test]
    fn detects_wire_format_echo() {
        let c = r#"{"function": {"name": "submit_result", "arguments": "{\"title\":\"x\"}"}}"#;
        let call = detect_pseudo_tool_call(c, TOOLS).unwrap();
        assert_eq!(call.function.name, "submit_result");
        assert_eq!(call.function.arguments, r#"{"title":"x"}"#);
    }

    #[test]
    fn ignores_unknown_tool_and_plain_prose() {
        assert!(detect_pseudo_tool_call(r#"{"name": "rm_rf", "arguments": {}}"#, TOOLS).is_none());
        assert!(detect_pseudo_tool_call("根据字幕内容这是《芙莉莲》第 3 集。", TOOLS).is_none());
        // JSON that mentions a tool name but isn't a call shape
        assert!(
            detect_pseudo_tool_call(r#"{"note": "search_tmdb found nothing"}"#, TOOLS).is_none()
        );
    }

    #[test]
    fn extracts_embedded_object_with_nested_braces() {
        let c = r#"Sure: {"name": "search_tmdb", "arguments": {"query": "a {b} c"}} done"#;
        let call = detect_pseudo_tool_call(c, TOOLS).unwrap();
        assert_eq!(call.function.name, "search_tmdb");
    }
}
