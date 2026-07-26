use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;

/// Tool failure classification (codex `FunctionCallError` design).
#[derive(Debug, Clone)]
pub enum ToolError {
    /// Fed back to the model as the tool output so it can self-correct.
    /// The vast majority of tool failures should use this.
    RespondToModel(String),
    /// Aborts the whole task.
    Fatal(String),
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Text fed back to the model (typically serialized JSON).
    pub content: String,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self { content: content.into() }
    }

    pub fn json(value: &Value) -> Self {
        Self { content: value.to_string() }
    }
}

/// A tool callable by the model. Implemented by the host (nipaserver's
/// nipa-providers crate) for search_tmdb / search_bangumi / etc.
///
/// `call` receives parsed JSON arguments — the runtime already handled
/// malformed-JSON recovery. Field-level validation stays in the tool and
/// should fail with `ToolError::RespondToModel`.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema, passed through verbatim as `tools[].function.parameters`.
    fn parameters(&self) -> Value;
    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>>;
}

/// Name-keyed tool registry. Duplicate names panic (codex behavior): a dup is
/// always a programming error and must fail loudly at startup.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    /// Order preserved for the API `tools` array (stable prefix → prompt cache).
    order: Vec<String>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        let mut map = HashMap::new();
        let mut order = Vec::new();
        for tool in tools {
            let name = tool.name().to_string();
            assert!(
                map.insert(name.clone(), tool).is_none(),
                "duplicate tool name registered: {name}"
            );
            order.push(name);
        }
        Self { tools: map, order }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.order.iter().map(String::as_str)
    }

    /// Chat Completions `tools` array WITHOUT a terminal tool — used by
    /// conversation mode, where a plain text reply is the natural end.
    pub fn to_api_tools_plain(&self) -> Vec<Value> {
        self.order
            .iter()
            .map(|name| {
                let t = &self.tools[name];
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters(),
                    }
                })
            })
            .collect()
    }

    /// Chat Completions `tools` array, including the given terminal tool spec.
    pub fn to_api_tools(&self, submit_result_schema: &Value) -> Vec<Value> {
        let mut out: Vec<Value> = self
            .order
            .iter()
            .map(|name| {
                let t = &self.tools[name];
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters(),
                    }
                })
            })
            .collect();
        out.push(serde_json::json!({
            "type": "function",
            "function": {
                "name": crate::SUBMIT_RESULT_TOOL,
                "description": "Submit your final identification result. \
                    Call this exactly once when you have reached a conclusion. \
                    This terminates the task.",
                "parameters": submit_result_schema,
            }
        }));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy(&'static str);
    impl Tool for Dummy {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "dummy"
        }
        fn parameters(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn call(&self, _arguments: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
            Box::pin(async { Ok(ToolOutput::text("ok")) })
        }
    }

    #[test]
    #[should_panic(expected = "duplicate tool name")]
    fn duplicate_names_panic() {
        ToolRegistry::new(vec![Arc::new(Dummy("a")), Arc::new(Dummy("a"))]);
    }

    #[test]
    fn api_tools_appends_submit_result_last() {
        let reg = ToolRegistry::new(vec![Arc::new(Dummy("a")), Arc::new(Dummy("b"))]);
        let schema = serde_json::json!({"type": "object"});
        let tools = reg.to_api_tools(&schema);
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0]["function"]["name"], "a");
        assert_eq!(tools[2]["function"]["name"], crate::SUBMIT_RESULT_TOOL);
    }
}
