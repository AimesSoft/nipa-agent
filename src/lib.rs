//! # nipa-agent
//!
//! Minimal tool-calling agent runtime for AI media scraping.
//!
//! Speaks OpenAI-compatible Chat Completions (non-streaming) against any
//! provider (Gemini / DeepSeek / Qwen / GLM / Groq / OpenRouter / Ollama / …),
//! runs a bounded tool-calling loop, and terminates when the model calls the
//! reserved `submit_result` tool.
//!
//! Design borrowed from openai/codex (event protocol, ToolExecutor split,
//! provider config, retry/backoff) and badlogic/pi (terminate semantics,
//! compat flags) — designs only, no code.
//!
//! The host (nipaserver, or NipaPlay via flutter_rust_bridge) implements
//! [`Tool`] for its business tools, assembles the evidence prompt, and
//! consumes [`AgentEventEnvelope`] for SSE + transcript persistence.
//! See `docs/03-agent接口契约.md` in the nipaserver repo for the contract.

mod agent;
mod client;
mod conversation;
mod error;
mod events;
mod provider;
mod pseudo;
mod tool;
mod wire;

/// Reserved name of the terminal tool. Registered automatically by the
/// runtime; hosts must not register a tool with this name.
pub const SUBMIT_RESULT_TOOL: &str = "submit_result";

pub use agent::{Agent, AgentConfig, TaskInput, TaskOutcome, TokenTotals};
pub use client::{ChatClient, RetryNotice};
pub use conversation::{Conversation, ConversationConfig, ConversationOutcome, ToolInteraction};
pub use error::AgentError;
pub use events::{AbortReason, AgentEvent, AgentEventEnvelope, FailReason};
pub use provider::{CompatFlags, ModelProviderInfo};
pub use tool::{Tool, ToolError, ToolOutput, ToolRegistry};
pub use wire::{ChatMessage, FunctionCall, ToolCall};

// Re-exports the host needs to implement Tool / drive the agent without
// adding its own direct dependency.
pub use futures::future::BoxFuture;
pub use tokio_util::sync::CancellationToken;
