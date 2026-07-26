use std::sync::Arc;
use std::time::Instant;

use futures::stream::{FuturesOrdered, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::client::{truncate, ChatClient};
use crate::error::AgentError;
use crate::events::{AbortReason, AgentEvent, AgentEventEnvelope, FailReason};
use crate::provider::ModelProviderInfo;
use crate::pseudo::detect_pseudo_tool_call;
use crate::tool::{Tool, ToolError, ToolRegistry};
use crate::wire::{ChatMessage, ToolCall};
use crate::SUBMIT_RESULT_TOOL;

const OUTPUT_PREVIEW_LEN: usize = 400;
/// Consecutive assistant-text-only rounds tolerated before failing the task.
const MAX_TEXT_ONLY_STRIKES: u32 = 3;
/// Cap on a single tool output as fed to the model. Protects the context
/// window from a tool returning an unexpectedly huge payload; the full output
/// still reaches the host via events/logs.
const DEFAULT_MAX_TOOL_OUTPUT_BYTES: usize = 16 * 1024;

pub struct AgentConfig {
    pub provider: ModelProviderInfo,
    pub model: String,
    /// Business tools; submit_result is registered automatically.
    pub tools: Vec<Arc<dyn Tool>>,
    /// JSON Schema for submit_result's parameters.
    pub result_schema: Value,
    pub max_rounds: u32,
    /// Total-token budget guard (input+output, cumulative across rounds).
    pub max_total_tokens: Option<u64>,
    /// Cap on a single tool output as fed to the model (bytes).
    pub max_tool_output_bytes: usize,
    pub event_tx: Option<UnboundedSender<AgentEventEnvelope>>,
    pub cancel: CancellationToken,
    pub task_id: String,
}

impl AgentConfig {
    pub fn new(provider: ModelProviderInfo, model: impl Into<String>, result_schema: Value) -> Self {
        Self {
            provider,
            model: model.into(),
            tools: Vec::new(),
            result_schema,
            max_rounds: 16,
            max_total_tokens: None,
            max_tool_output_bytes: DEFAULT_MAX_TOOL_OUTPUT_BYTES,
            event_tx: None,
            cancel: CancellationToken::new(),
            task_id: String::new(),
        }
    }
}

pub struct TaskInput {
    pub system_prompt: String,
    /// Evidence bundle assembled by the host.
    pub user_message: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug)]
pub enum TaskOutcome {
    Completed { result: Value, rounds_used: u32, usage: TokenTotals },
    Failed { reason: FailReason, message: String, rounds_used: u32, usage: TokenTotals },
    Aborted { reason: AbortReason },
}

pub struct Agent {
    client: ChatClient,
    registry: ToolRegistry,
    cfg: AgentConfigInner,
}

/// AgentConfig minus the parts consumed at construction.
struct AgentConfigInner {
    model: String,
    result_schema: Value,
    max_rounds: u32,
    max_total_tokens: Option<u64>,
    max_tool_output_bytes: usize,
    event_tx: Option<UnboundedSender<AgentEventEnvelope>>,
    cancel: CancellationToken,
    task_id: String,
}

struct EventSink {
    tx: Option<UnboundedSender<AgentEventEnvelope>>,
    task_id: String,
    seq: u64,
}

impl EventSink {
    fn emit(&mut self, event: AgentEvent) {
        if let Some(tx) = &self.tx {
            let envelope = AgentEventEnvelope {
                task_id: self.task_id.clone(),
                seq: self.seq,
                ts_ms: now_ms(),
                event,
            };
            // A closed receiver must never kill the task.
            let _ = tx.send(envelope);
        }
        self.seq += 1;
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Agent {
    pub fn new(cfg: AgentConfig) -> Result<Self, AgentError> {
        let client = ChatClient::new(cfg.provider)?;
        let registry = ToolRegistry::new(cfg.tools);
        assert!(
            registry.get(SUBMIT_RESULT_TOOL).is_none(),
            "'{SUBMIT_RESULT_TOOL}' is reserved and registered automatically"
        );
        Ok(Self {
            client,
            registry,
            cfg: AgentConfigInner {
                model: cfg.model,
                result_schema: cfg.result_schema,
                max_rounds: cfg.max_rounds.max(1),
                max_total_tokens: cfg.max_total_tokens,
                max_tool_output_bytes: cfg.max_tool_output_bytes.max(1024),
                event_tx: cfg.event_tx,
                cancel: cfg.cancel,
                task_id: cfg.task_id,
            },
        })
    }

    /// Run one task to a terminal state. Exactly one terminal event is
    /// emitted, always last.
    pub async fn run(&self, task: TaskInput) -> TaskOutcome {
        let started = Instant::now();
        let mut sink = EventSink {
            tx: self.cfg.event_tx.clone(),
            task_id: self.cfg.task_id.clone(),
            seq: 0,
        };
        sink.emit(AgentEvent::TaskStarted {
            model: self.cfg.model.clone(),
            max_rounds: self.cfg.max_rounds,
        });

        let outcome = self.run_inner(task, started, &mut sink).await;

        match &outcome {
            TaskOutcome::Completed { result, rounds_used, .. } => {
                info!(task = %self.cfg.task_id, rounds = rounds_used, "task completed");
                sink.emit(AgentEvent::TaskCompleted {
                    result: result.clone(),
                    rounds_used: *rounds_used,
                    duration_ms: started.elapsed().as_millis() as u64,
                });
            }
            TaskOutcome::Failed { reason, message, rounds_used, .. } => {
                warn!(task = %self.cfg.task_id, ?reason, %message, "task failed");
                sink.emit(AgentEvent::TaskFailed {
                    reason: *reason,
                    message: message.clone(),
                    rounds_used: *rounds_used,
                });
            }
            TaskOutcome::Aborted { reason } => {
                sink.emit(AgentEvent::TaskAborted { reason: *reason });
            }
        }
        outcome
    }

    async fn run_inner(
        &self,
        task: TaskInput,
        _started: Instant,
        sink: &mut EventSink,
    ) -> TaskOutcome {
        let api_tools = self.registry.to_api_tools(&self.cfg.result_schema);
        let known_tools: Vec<String> = self
            .registry
            .names()
            .map(str::to_string)
            .chain(std::iter::once(SUBMIT_RESULT_TOOL.to_string()))
            .collect();
        let known_tools_ref: Vec<&str> = known_tools.iter().map(String::as_str).collect();

        let mut messages =
            vec![ChatMessage::system(task.system_prompt), ChatMessage::user(task.user_message)];
        let mut usage = TokenTotals::default();
        let mut text_only_strikes: u32 = 0;
        // (tool, normalized args) → successful output. Duplicate calls are
        // answered from here with a nudge instead of re-executing — observed
        // with gemini-3-flash looping the same search before concluding.
        let mut seen_calls: std::collections::HashMap<(String, String), String> =
            std::collections::HashMap::new();
        let task_started = Instant::now();

        for round in 1..=self.cfg.max_rounds {
            if self.cfg.cancel.is_cancelled() {
                return TaskOutcome::Aborted { reason: AbortReason::UserCancelled };
            }
            sink.emit(AgentEvent::RoundStarted { round, max_rounds: self.cfg.max_rounds });

            let resp = {
                // Borrow-splitting: the retry callback needs the sink.
                let sink_ref = &mut *sink;
                self.client
                    .chat(&self.cfg.model, &messages, &api_tools, &self.cfg.cancel, |notice| {
                        sink_ref.emit(AgentEvent::Retrying {
                            attempt: notice.attempt,
                            max_attempts: notice.max_attempts,
                            message: notice.message,
                        });
                    })
                    .await
            };

            let resp = match resp {
                Ok(r) => r,
                Err(AgentError::Cancelled) => {
                    return TaskOutcome::Aborted { reason: AbortReason::UserCancelled }
                }
                Err(e) => {
                    let reason = match &e {
                        AgentError::ContextWindowExceeded => FailReason::ContextWindowExceeded,
                        _ => FailReason::ApiError,
                    };
                    return TaskOutcome::Failed {
                        reason,
                        message: e.to_string(),
                        rounds_used: round,
                        usage,
                    };
                }
            };

            if let Some(u) = resp.usage {
                usage.input += u.prompt_tokens;
                usage.output += u.completion_tokens;
                sink.emit(AgentEvent::TokenUsage {
                    last_input: u.prompt_tokens,
                    last_output: u.completion_tokens,
                    total_input: usage.input,
                    total_output: usage.output,
                });
            }

            let mut assistant = resp.choices.into_iter().next().expect("non-empty choices").message;

            // --- tool call extraction (structured first, pseudo fallback) ---
            let mut tool_calls: Vec<ToolCall> = std::mem::take(&mut assistant.tool_calls);
            for (i, call) in tool_calls.iter_mut().enumerate() {
                if call.id.is_empty() {
                    call.id = format!("backfill_{i}_{}", fastrand::u32(..));
                }
            }
            let content_text = assistant.content.clone().unwrap_or_default();
            if tool_calls.is_empty()
                && self.client.provider().compat.detect_pseudo_tool_calls
                && !content_text.is_empty()
            {
                if let Some(call) = detect_pseudo_tool_call(&content_text, &known_tools_ref) {
                    sink.emit(AgentEvent::Warning {
                        message: format!(
                            "model emitted tool call '{}' as text; recovered",
                            call.function.name
                        ),
                    });
                    // Rewrite history as if the call had been structured, so
                    // the tool result message stays protocol-correct.
                    assistant.content = None;
                    tool_calls.push(call);
                }
            }
            if !content_text.is_empty() && assistant.content.is_some() {
                sink.emit(AgentEvent::AssistantMessage { text: content_text.clone() });
            }

            // Strip provider-quirk fields before the message re-enters history
            // (DeepSeek 400s if reasoning_content is echoed back).
            for field in &self.client.provider().compat.strip_from_history {
                assistant.extra.remove(field);
            }
            assistant.tool_calls = tool_calls.clone();
            messages.push(assistant);

            if tool_calls.is_empty() {
                // Text-only round: remind the model of the contract.
                text_only_strikes += 1;
                if text_only_strikes >= MAX_TEXT_ONLY_STRIKES {
                    return TaskOutcome::Failed {
                        reason: FailReason::InvalidToolCall,
                        message: format!(
                            "model produced {MAX_TEXT_ONLY_STRIKES} consecutive rounds without tool calls"
                        ),
                        rounds_used: round,
                        usage,
                    };
                }
                messages.push(ChatMessage::user(
                    "You must use the provided tools. When you have reached a conclusion, \
                     call `submit_result` with your final answer. Do not answer in plain text."
                        .to_string(),
                ));
                continue;
            }
            text_only_strikes = 0;

            // --- submit_result check (terminate semantics, pi-style) ---
            if let Some(call) =
                tool_calls.iter().find(|c| c.function.name == SUBMIT_RESULT_TOOL)
            {
                match serde_json::from_str::<Value>(&call.function.arguments) {
                    Ok(result) if result.is_object() => {
                        // Reconcile: end events for every other in-flight call.
                        for other in tool_calls.iter().filter(|c| c.id != call.id) {
                            sink.emit(AgentEvent::ToolCallEnd {
                                call_id: other.id.clone(),
                                tool: other.function.name.clone(),
                                success: false,
                                output_preview: String::new(),
                                error: Some("skipped: task terminated by submit_result".into()),
                                duration_ms: 0,
                            });
                        }
                        return TaskOutcome::Completed {
                            result,
                            rounds_used: round,
                            usage,
                        };
                    }
                    _ => {
                        // Malformed submission: feed the error back (self-correction).
                        messages.push(ChatMessage::tool_result(
                            call.id.clone(),
                            "Error: submit_result arguments must be a valid JSON object \
                             matching the provided schema. Fix the arguments and call \
                             submit_result again.",
                        ));
                        // Other calls in the same round still need responses;
                        // execute them normally below.
                    }
                }
            }

            // --- execute business tool calls concurrently, refill in order ---
            let exec_calls: Vec<ToolCall> = tool_calls
                .iter()
                .filter(|c| c.function.name != SUBMIT_RESULT_TOOL)
                .cloned()
                .collect();
            for call in &exec_calls {
                sink.emit(AgentEvent::ToolCallBegin {
                    call_id: call.id.clone(),
                    tool: call.function.name.clone(),
                    arguments: serde_json::from_str(&call.function.arguments)
                        .unwrap_or(Value::String(call.function.arguments.clone())),
                });
            }
            let mut futs = FuturesOrdered::new();
            for call in exec_calls {
                // Duplicate-call detection: same tool + same normalized args
                // already succeeded → answer from memory with a nudge instead
                // of re-executing (repeat loops observed with flash models).
                let dedup_key = (
                    call.function.name.clone(),
                    normalize_args(&call.function.arguments),
                );
                if let Some(prev) = seen_calls.get(&dedup_key) {
                    sink.emit(AgentEvent::ToolCallEnd {
                        call_id: call.id.clone(),
                        tool: call.function.name.clone(),
                        success: true,
                        output_preview: "(duplicate call, served from earlier result)".into(),
                        error: None,
                        duration_ms: 0,
                    });
                    messages.push(ChatMessage::tool_result(
                        call.id,
                        format!(
                            "Note: you already called this tool with identical arguments. \
                             Previous result repeated below — do not call it again; use the \
                             information you have or try different arguments.\n{prev}"
                        ),
                    ));
                    continue;
                }
                futs.push_back(self.execute_tool_call(call));
            }
            while let Some(mut done) = futs.next().await {
                // Cap what the model sees; hosts get the preview via events
                // and can log full outputs inside the tool itself.
                if done.content.len() > self.cfg.max_tool_output_bytes {
                    done.content = format!(
                        "{}\n…[output truncated: {} of {} bytes shown]",
                        truncate(&done.content, self.cfg.max_tool_output_bytes),
                        self.cfg.max_tool_output_bytes,
                        done.content.len()
                    );
                }
                sink.emit(AgentEvent::ToolCallEnd {
                    call_id: done.call_id.clone(),
                    tool: done.tool.clone(),
                    success: done.success,
                    output_preview: truncate(&done.content, OUTPUT_PREVIEW_LEN),
                    error: done.error.clone(),
                    duration_ms: done.duration_ms,
                });
                if let Some(fatal) = done.fatal {
                    return TaskOutcome::Failed {
                        reason: FailReason::Other,
                        message: format!("fatal tool error in {}: {fatal}", done.tool),
                        rounds_used: round,
                        usage,
                    };
                }
                if done.success {
                    seen_calls.insert(
                        (done.tool.clone(), normalize_args(&done.raw_arguments)),
                        done.content.clone(),
                    );
                }
                // All results are appended before the next model call —
                // required by Gemini (every tool_call_id must be answered).
                messages.push(ChatMessage::tool_result(done.call_id, done.content));
            }

            // Token budget check after the round completes.
            if let Some(budget) = self.cfg.max_total_tokens {
                if usage.input + usage.output >= budget {
                    return TaskOutcome::Failed {
                        reason: FailReason::TokenBudgetExhausted,
                        message: format!(
                            "token budget exhausted: {} used of {budget}",
                            usage.input + usage.output
                        ),
                        rounds_used: round,
                        usage,
                    };
                }
            }
            let _ = task_started; // reserved for future per-task wall-clock budget
        }

        TaskOutcome::Failed {
            reason: FailReason::RoundBudgetExhausted,
            message: format!(
                "no submit_result call within {} rounds",
                self.cfg.max_rounds
            ),
            rounds_used: self.cfg.max_rounds,
            usage,
        }
    }

    async fn execute_tool_call(&self, call: ToolCall) -> ExecutedCall {
        let started = Instant::now();
        let tool_name = call.function.name.clone();
        let mut done = ExecutedCall {
            call_id: call.id,
            tool: tool_name.clone(),
            raw_arguments: call.function.arguments.clone(),
            success: false,
            content: String::new(),
            error: None,
            fatal: None,
            duration_ms: 0,
        };

        let Some(tool) = self.registry.get(&tool_name) else {
            done.content = format!(
                "Error: unknown tool '{tool_name}'. Available tools: {}.",
                self.registry.names().collect::<Vec<_>>().join(", ")
            );
            done.error = Some("unknown tool".into());
            return done;
        };

        let args: Value = match serde_json::from_str(&call.function.arguments) {
            Ok(v) => v,
            Err(e) => {
                // Malformed JSON from the model: feed the parse error back.
                done.content = format!(
                    "Error: could not parse tool arguments as JSON ({e}). \
                     Re-emit the call with valid JSON."
                );
                done.error = Some(format!("argument parse error: {e}"));
                done.duration_ms = started.elapsed().as_millis() as u64;
                return done;
            }
        };

        match tool.call(args).await {
            Ok(output) => {
                done.success = true;
                done.content = output.content;
            }
            Err(ToolError::RespondToModel(msg)) => {
                done.content = format!("Error: {msg}");
                done.error = Some(msg);
            }
            Err(ToolError::Fatal(msg)) => {
                done.content = format!("Error: {msg}");
                done.error = Some(msg.clone());
                done.fatal = Some(msg);
            }
        }
        done.duration_ms = started.elapsed().as_millis() as u64;
        done
    }
}

struct ExecutedCall {
    call_id: String,
    tool: String,
    /// Original argument string, kept for duplicate-call detection.
    raw_arguments: String,
    success: bool,
    /// Fed back to the model as the tool message content.
    content: String,
    error: Option<String>,
    fatal: Option<String>,
    duration_ms: u64,
}

/// Canonical form of a tool-call argument string for duplicate detection:
/// parsed and re-serialized so that key order and whitespace don't matter.
fn normalize_args(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(v) => canonicalize(&v).to_string(),
        Err(_) => raw.trim().to_string(),
    }
}

fn canonicalize(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            // BTreeMap iteration is key-sorted.
            let sorted: std::collections::BTreeMap<_, _> =
                map.iter().map(|(k, val)| (k.clone(), canonicalize(val))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}
