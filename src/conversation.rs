//! Conversation mode: the tool-calling loop for a steward/assistant persona.
//!
//! Differs from the task loop (`agent.rs`) in one structural way: there is no
//! terminal tool — a plain-text assistant reply IS the natural end of a turn.
//! Tools still execute mid-turn; the same guardrails (round cap, duplicate
//! detection, output truncation) apply.

use std::sync::Arc;
use std::time::Instant;

use futures::stream::{FuturesOrdered, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::client::{truncate, ChatClient};
use crate::error::AgentError;
use crate::events::{AgentEvent, AgentEventEnvelope};
use crate::provider::ModelProviderInfo;
use crate::tool::{Tool, ToolError, ToolRegistry};
use crate::wire::{ChatMessage, ToolCall};

const OUTPUT_PREVIEW_LEN: usize = 400;

pub struct ConversationConfig {
    pub provider: ModelProviderInfo,
    pub model: String,
    pub tools: Vec<Arc<dyn Tool>>,
    /// Cap on model-call rounds within ONE user turn (tool loops). Default 12.
    pub max_rounds: u32,
    pub max_tool_output_bytes: usize,
    pub event_tx: Option<UnboundedSender<AgentEventEnvelope>>,
    pub cancel: CancellationToken,
    /// Correlation id for events (chat session id).
    pub task_id: String,
}

impl ConversationConfig {
    pub fn new(provider: ModelProviderInfo, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            tools: Vec::new(),
            max_rounds: 12,
            max_tool_output_bytes: 16 * 1024,
            event_tx: None,
            cancel: CancellationToken::new(),
            task_id: String::new(),
        }
    }
}

/// Everything the host needs to persist after one user turn.
pub struct ConversationOutcome {
    /// Final assistant text (empty if aborted/failed before any reply).
    pub reply: String,
    /// Tool interactions that happened this turn, in order (for persistence).
    pub tool_events: Vec<ToolInteraction>,
    pub rounds_used: u32,
    pub usage: crate::TokenTotals,
    pub error: Option<AgentError>,
}

#[derive(Debug, Clone)]
pub struct ToolInteraction {
    pub tool: String,
    pub arguments: Value,
    pub output_preview: String,
    pub success: bool,
}

pub struct Conversation {
    client: ChatClient,
    registry: ToolRegistry,
    cfg: ConversationConfigInner,
}

struct ConversationConfigInner {
    model: String,
    max_rounds: u32,
    max_tool_output_bytes: usize,
    event_tx: Option<UnboundedSender<AgentEventEnvelope>>,
    cancel: CancellationToken,
    task_id: String,
}

impl Conversation {
    pub fn new(cfg: ConversationConfig) -> Result<Self, AgentError> {
        let client = ChatClient::new(cfg.provider)?;
        let registry = ToolRegistry::new(cfg.tools);
        Ok(Self {
            client,
            registry,
            cfg: ConversationConfigInner {
                model: cfg.model,
                max_rounds: cfg.max_rounds.max(1),
                max_tool_output_bytes: cfg.max_tool_output_bytes.max(1024),
                event_tx: cfg.event_tx,
                cancel: cfg.cancel,
                task_id: cfg.task_id,
            },
        })
    }

    /// Run one user turn: `messages` is the full assembled context (system +
    /// summary + window + the new user message). Returns the reply and the
    /// tool interactions; the host owns persistence and context management.
    pub async fn run_turn(&self, mut messages: Vec<ChatMessage>) -> ConversationOutcome {
        let api_tools = self.registry.to_api_tools_plain();
        let mut usage = crate::TokenTotals::default();
        let mut tool_events: Vec<ToolInteraction> = Vec::new();
        let mut seq: u64 = 0;
        let mut emit = |event: AgentEvent, tx: &Option<UnboundedSender<AgentEventEnvelope>>| {
            if let Some(tx) = tx {
                let _ = tx.send(AgentEventEnvelope {
                    task_id: self.cfg.task_id.clone(),
                    seq,
                    ts_ms: now_ms(),
                    event,
                });
            }
            seq += 1;
        };

        for round in 1..=self.cfg.max_rounds {
            if self.cfg.cancel.is_cancelled() {
                return ConversationOutcome {
                    reply: String::new(),
                    tool_events,
                    rounds_used: round,
                    usage,
                    error: Some(AgentError::Cancelled),
                };
            }

            let resp = self
                .client
                .chat(
                    &self.cfg.model,
                    &messages,
                    &api_tools,
                    &self.cfg.cancel,
                    |n| {
                        emit(
                            AgentEvent::Retrying {
                                attempt: n.attempt,
                                max_attempts: n.max_attempts,
                                message: n.message,
                            },
                            &self.cfg.event_tx,
                        );
                    },
                )
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    return ConversationOutcome {
                        reply: String::new(),
                        tool_events,
                        rounds_used: round,
                        usage,
                        error: Some(e),
                    }
                }
            };
            if let Some(u) = resp.usage {
                usage.input += u.prompt_tokens;
                usage.output += u.completion_tokens;
            }

            let mut assistant = resp
                .choices
                .into_iter()
                .next()
                .expect("non-empty choices")
                .message;
            let mut tool_calls: Vec<ToolCall> = std::mem::take(&mut assistant.tool_calls);
            for (i, call) in tool_calls.iter_mut().enumerate() {
                if call.id.is_empty() {
                    call.id = format!("backfill_{i}_{}", fastrand::u32(..));
                }
            }
            let content_text = assistant.content.clone().unwrap_or_default();
            // Pseudo tool-call recovery (same trap as the task loop).
            if tool_calls.is_empty()
                && self.client.provider().compat.detect_pseudo_tool_calls
                && !content_text.is_empty()
            {
                let known: Vec<&str> = self.registry.names().collect();
                if let Some(call) = crate::pseudo::detect_pseudo_tool_call(&content_text, &known) {
                    assistant.content = None;
                    tool_calls.push(call);
                }
            }

            for field in &self.client.provider().compat.strip_from_history {
                assistant.extra.remove(field);
            }
            assistant.tool_calls = tool_calls.clone();
            messages.push(assistant);

            if tool_calls.is_empty() {
                // Plain text reply = the turn is complete. This is the normal exit.
                return ConversationOutcome {
                    reply: content_text,
                    tool_events,
                    rounds_used: round,
                    usage,
                    error: None,
                };
            }

            // Execute tools concurrently, refill all results before next round.
            for call in &tool_calls {
                emit(
                    AgentEvent::ToolCallBegin {
                        call_id: call.id.clone(),
                        tool: call.function.name.clone(),
                        arguments: serde_json::from_str(&call.function.arguments)
                            .unwrap_or(Value::String(call.function.arguments.clone())),
                    },
                    &self.cfg.event_tx,
                );
            }
            let mut futs = FuturesOrdered::new();
            for call in tool_calls {
                futs.push_back(self.execute(call));
            }
            while let Some(done) = futs.next().await {
                let mut content = done.content;
                if content.len() > self.cfg.max_tool_output_bytes {
                    content = format!(
                        "{}\n…[truncated]",
                        truncate(&content, self.cfg.max_tool_output_bytes)
                    );
                }
                emit(
                    AgentEvent::ToolCallEnd {
                        call_id: done.call_id.clone(),
                        tool: done.tool.clone(),
                        success: done.success,
                        output_preview: truncate(&content, OUTPUT_PREVIEW_LEN),
                        error: done.error.clone(),
                        duration_ms: done.duration_ms,
                    },
                    &self.cfg.event_tx,
                );
                tool_events.push(ToolInteraction {
                    tool: done.tool,
                    arguments: done.arguments,
                    output_preview: truncate(&content, OUTPUT_PREVIEW_LEN),
                    success: done.success,
                });
                messages.push(ChatMessage::tool_result(done.call_id, content));
            }
        }

        // Round budget exhausted mid-tool-loop: surface what we have.
        ConversationOutcome {
            reply: "（这轮我查了太多次还没得出结论——请把问题拆小一点再问我。）".to_string(),
            tool_events,
            rounds_used: self.cfg.max_rounds,
            usage,
            error: None,
        }
    }

    async fn execute(&self, call: ToolCall) -> ExecutedConvCall {
        let started = Instant::now();
        let tool_name = call.function.name.clone();
        let arguments: Value = serde_json::from_str(&call.function.arguments)
            .unwrap_or(Value::String(call.function.arguments.clone()));
        let mut out = ExecutedConvCall {
            call_id: call.id,
            tool: tool_name.clone(),
            arguments,
            success: false,
            content: String::new(),
            error: None,
            duration_ms: 0,
        };
        let Some(tool) = self.registry.get(&tool_name) else {
            out.content = format!(
                "Error: unknown tool '{tool_name}'. Available: {}.",
                self.registry.names().collect::<Vec<_>>().join(", ")
            );
            out.error = Some("unknown tool".into());
            return out;
        };
        let args: Value = match serde_json::from_str(&call.function.arguments) {
            Ok(v) => v,
            Err(e) => {
                out.content =
                    format!("Error: could not parse arguments as JSON ({e}). Re-emit the call.");
                out.error = Some(format!("argument parse error: {e}"));
                out.duration_ms = started.elapsed().as_millis() as u64;
                return out;
            }
        };
        match tool.call(args).await {
            Ok(o) => {
                out.success = true;
                out.content = o.content;
            }
            Err(ToolError::RespondToModel(msg)) | Err(ToolError::Fatal(msg)) => {
                // Conversation mode has no fatal path: every tool failure is
                // surfaced to the model, which explains it to the user.
                out.content = format!("Error: {msg}");
                out.error = Some(msg);
            }
        }
        out.duration_ms = started.elapsed().as_millis() as u64;
        out
    }
}

struct ExecutedConvCall {
    call_id: String,
    tool: String,
    arguments: Value,
    success: bool,
    content: String,
    error: Option<String>,
    duration_ms: u64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
