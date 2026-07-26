use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Event envelope: one SSE push = one transcript line.
/// SSE mapping: `id: seq`, `event: <type>`, `data: <envelope JSON>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEventEnvelope {
    pub task_id: String,
    /// Monotonically increasing within a task, starting at 0.
    pub seq: u64,
    /// Unix milliseconds.
    pub ts_ms: i64,
    #[serde(flatten)]
    pub event: AgentEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TaskStarted {
        model: String,
        max_rounds: u32,
    },
    RoundStarted {
        round: u32,
        max_rounds: u32,
    },
    /// Free text the model produced outside of tool calls (non-streaming).
    AssistantMessage {
        text: String,
    },
    ToolCallBegin {
        call_id: String,
        tool: String,
        arguments: Value,
    },
    ToolCallEnd {
        call_id: String,
        tool: String,
        success: bool,
        /// Truncated preview of the output fed back to the model.
        output_preview: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        duration_ms: u64,
    },
    /// Emitted once per round; total + last double counting (codex TokenUsageInfo).
    TokenUsage {
        last_input: u64,
        last_output: u64,
        total_input: u64,
        total_output: u64,
    },
    /// Non-fatal: request failed, runtime is retrying.
    Retrying {
        attempt: u32,
        max_attempts: u32,
        message: String,
    },
    Warning {
        message: String,
    },
    TaskCompleted {
        result: Value,
        rounds_used: u32,
        duration_ms: u64,
    },
    TaskFailed {
        reason: FailReason,
        message: String,
        rounds_used: u32,
    },
    TaskAborted {
        reason: AbortReason,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailReason {
    /// max_rounds exhausted without a valid submit_result call.
    RoundBudgetExhausted,
    TokenBudgetExhausted,
    /// Upstream API error with retries exhausted.
    ApiError,
    ContextWindowExceeded,
    /// The model persistently produced unusable tool calls.
    InvalidToolCall,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AbortReason {
    UserCancelled,
    Shutdown,
}

impl AgentEvent {
    /// Stable string name: SSE `event:` field / logging.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::TaskStarted { .. } => "task_started",
            Self::RoundStarted { .. } => "round_started",
            Self::AssistantMessage { .. } => "assistant_message",
            Self::ToolCallBegin { .. } => "tool_call_begin",
            Self::ToolCallEnd { .. } => "tool_call_end",
            Self::TokenUsage { .. } => "token_usage",
            Self::Retrying { .. } => "retrying",
            Self::Warning { .. } => "warning",
            Self::TaskCompleted { .. } => "task_completed",
            Self::TaskFailed { .. } => "task_failed",
            Self::TaskAborted { .. } => "task_aborted",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::TaskCompleted { .. } | Self::TaskFailed { .. } | Self::TaskAborted { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_serializes_with_flattened_type_tag() {
        let env = AgentEventEnvelope {
            task_id: "t1".into(),
            seq: 3,
            ts_ms: 1_700_000_000_000,
            event: AgentEvent::RoundStarted { round: 2, max_rounds: 16 },
        };
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["type"], "round_started");
        assert_eq!(v["seq"], 3);
        assert_eq!(v["round"], 2);
        let back: AgentEventEnvelope = serde_json::from_value(v).unwrap();
        assert_eq!(back.event.type_name(), "round_started");
    }

    #[test]
    fn fail_reason_snake_case() {
        let e = AgentEvent::TaskFailed {
            reason: FailReason::RoundBudgetExhausted,
            message: "x".into(),
            rounds_used: 16,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["reason"], "round_budget_exhausted");
    }
}
