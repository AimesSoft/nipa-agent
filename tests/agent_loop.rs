//! End-to-end loop tests against a wiremock OpenAI-compatible endpoint.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use nipa_agent::{
    Agent, AgentConfig, AgentEvent, BoxFuture, CancellationToken, FailReason, ModelProviderInfo,
    TaskInput, TaskOutcome, Tool, ToolError, ToolOutput,
};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

// ---------- helpers ----------

struct SearchTool {
    calls: Arc<AtomicU32>,
}

impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search_tmdb"
    }
    fn description(&self) -> &str {
        "Search TMDB for a title"
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]})
    }
    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let q = arguments["query"].as_str().unwrap_or_default();
            if q.is_empty() {
                return Err(ToolError::RespondToModel("query must not be empty".into()));
            }
            Ok(ToolOutput::json(
                &json!({"results": [{"id": 209867, "name": "葬送的芙莉莲"}]}),
            ))
        })
    }
}

fn result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string"},
            "confidence": {"type": "string", "enum": ["high", "medium", "low"]}
        },
        "required": ["title", "confidence"]
    })
}

fn assistant_tool_call_body(name: &str, args: &str, finish: &str) -> Value {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": name, "arguments": args}
                }]
            },
            "finish_reason": finish
        }],
        "usage": {"prompt_tokens": 50, "completion_tokens": 10}
    })
}

async fn run_agent(
    server: &MockServer,
    tools: Vec<Arc<dyn Tool>>,
) -> (TaskOutcome, Vec<AgentEvent>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut cfg = AgentConfig::new(
        ModelProviderInfo::new("mock", format!("{}/v1", server.uri())).with_api_key("test-key"),
        "mock-model",
        result_schema(),
    );
    cfg.tools = tools;
    cfg.event_tx = Some(tx);
    cfg.task_id = "task-1".into();
    let agent = Agent::new(cfg).unwrap();
    let outcome = agent
        .run(TaskInput {
            system_prompt: "You identify media files.".into(),
            user_message: "File: [Sub] Sousou no Frieren - 03.mkv".into(),
        })
        .await;
    let mut events = Vec::new();
    while let Ok(env) = rx.try_recv() {
        events.push(env.event);
    }
    (outcome, events)
}

// ---------- tests ----------

/// Happy path: tool call round → submit_result round.
#[tokio::test]
async fn two_round_happy_path() {
    let server = MockServer::start().await;
    let counter = Arc::new(AtomicU32::new(0));

    struct Sequenced {
        n: AtomicU32,
    }
    impl Respond for Sequenced {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            let body = if n == 0 {
                assistant_tool_call_body(
                    "search_tmdb",
                    r#"{"query":"Sousou no Frieren"}"#,
                    "tool_calls",
                )
            } else {
                assistant_tool_call_body(
                    "submit_result",
                    r#"{"title":"葬送的芙莉莲","confidence":"high"}"#,
                    "tool_calls",
                )
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequenced {
            n: AtomicU32::new(0),
        })
        .mount(&server)
        .await;

    let tool = Arc::new(SearchTool {
        calls: counter.clone(),
    });
    let (outcome, events) = run_agent(&server, vec![tool]).await;

    match outcome {
        TaskOutcome::Completed {
            result,
            rounds_used,
            usage,
        } => {
            assert_eq!(result["title"], "葬送的芙莉莲");
            assert_eq!(rounds_used, 2);
            assert_eq!(usage.input, 100);
            assert_eq!(usage.output, 20);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    // Event stream invariants.
    let last = events.last().unwrap();
    assert!(matches!(last, AgentEvent::TaskCompleted { .. }));
    assert_eq!(events.iter().filter(|e| e.is_terminal()).count(), 1);
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallBegin { tool, .. } if tool == "search_tmdb")));
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallEnd { success: true, .. })));
}

/// DeepSeek trap: finish_reason "stop" but tool_calls present → must still dispatch.
#[tokio::test]
async fn dispatches_tool_calls_despite_stop_finish_reason() {
    let server = MockServer::start().await;

    struct Sequenced {
        n: AtomicU32,
    }
    impl Respond for Sequenced {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            let body = if n == 0 {
                // finish_reason lies
                assistant_tool_call_body("search_tmdb", r#"{"query":"Frieren"}"#, "stop")
            } else {
                assistant_tool_call_body(
                    "submit_result",
                    r#"{"title":"Frieren","confidence":"high"}"#,
                    "stop",
                )
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequenced {
            n: AtomicU32::new(0),
        })
        .mount(&server)
        .await;

    let calls = Arc::new(AtomicU32::new(0));
    let (outcome, _) = run_agent(
        &server,
        vec![Arc::new(SearchTool {
            calls: calls.clone(),
        })],
    )
    .await;
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Pseudo tool call in text content (DeepSeek ~20% case) → recovered.
#[tokio::test]
async fn recovers_pseudo_tool_call_from_text() {
    let server = MockServer::start().await;

    struct Sequenced {
        n: AtomicU32,
    }
    impl Respond for Sequenced {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            let body = if n == 0 {
                json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": "```json\n{\"name\": \"search_tmdb\", \"arguments\": {\"query\": \"Frieren\"}}\n```"
                        },
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 40, "completion_tokens": 30}
                })
            } else {
                assistant_tool_call_body(
                    "submit_result",
                    r#"{"title":"Frieren","confidence":"medium"}"#,
                    "tool_calls",
                )
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequenced {
            n: AtomicU32::new(0),
        })
        .mount(&server)
        .await;

    let calls = Arc::new(AtomicU32::new(0));
    let (outcome, events) = run_agent(
        &server,
        vec![Arc::new(SearchTool {
            calls: calls.clone(),
        })],
    )
    .await;
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "pseudo call must be executed"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::Warning { .. })));
}

/// Malformed tool arguments → error fed back → model corrects itself.
#[tokio::test]
async fn feeds_back_malformed_arguments_for_self_correction() {
    let server = MockServer::start().await;

    struct Sequenced {
        n: AtomicU32,
    }
    impl Respond for Sequenced {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            let body = match n {
                0 => assistant_tool_call_body("search_tmdb", r#"{"query": "Frieren"#, "tool_calls"), // broken JSON
                1 => {
                    // The runtime must have fed an error back as a tool message.
                    let body: Value = serde_json::from_slice(&req.body).unwrap();
                    let msgs = body["messages"].as_array().unwrap();
                    let last = msgs.last().unwrap();
                    assert_eq!(last["role"], "tool");
                    assert!(last["content"]
                        .as_str()
                        .unwrap()
                        .contains("could not parse"));
                    assistant_tool_call_body("search_tmdb", r#"{"query":"Frieren"}"#, "tool_calls")
                }
                _ => assistant_tool_call_body(
                    "submit_result",
                    r#"{"title":"Frieren","confidence":"high"}"#,
                    "tool_calls",
                ),
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequenced {
            n: AtomicU32::new(0),
        })
        .mount(&server)
        .await;

    let calls = Arc::new(AtomicU32::new(0));
    let (outcome, _) = run_agent(
        &server,
        vec![Arc::new(SearchTool {
            calls: calls.clone(),
        })],
    )
    .await;
    match outcome {
        TaskOutcome::Completed { rounds_used, .. } => assert_eq!(rounds_used, 3),
        other => panic!("expected Completed, got {other:?}"),
    }
}

/// Text-only answers → reminder injected; three strikes → InvalidToolCall.
#[tokio::test]
async fn text_only_rounds_fail_after_strikes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "这是《芙莉莲》第 3 集。"},
                "finish_reason": "stop"
            }]
        })))
        .mount(&server)
        .await;

    let (outcome, _) = run_agent(&server, vec![]).await;
    match outcome {
        TaskOutcome::Failed { reason, .. } => assert_eq!(reason, FailReason::InvalidToolCall),
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// 500 twice then success → retried transparently with Retrying events.
#[tokio::test]
async fn retries_transient_server_errors() {
    let server = MockServer::start().await;

    struct FlakyThenSubmit {
        n: AtomicU32,
    }
    impl Respond for FlakyThenSubmit {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                ResponseTemplate::new(500).set_body_string("internal error")
            } else {
                ResponseTemplate::new(200).set_body_json(assistant_tool_call_body(
                    "submit_result",
                    r#"{"title":"Frieren","confidence":"high"}"#,
                    "tool_calls",
                ))
            }
        }
    }
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(FlakyThenSubmit {
            n: AtomicU32::new(0),
        })
        .mount(&server)
        .await;

    let (outcome, events) = run_agent(&server, vec![]).await;
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Retrying { .. }))
            .count(),
        2
    );
}

/// 401 → immediate failure, no retries.
#[tokio::test]
async fn auth_error_fails_fast() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid key"))
        .expect(1) // exactly one request: no retry on auth errors
        .mount(&server)
        .await;

    let (outcome, _) = run_agent(&server, vec![]).await;
    match outcome {
        TaskOutcome::Failed {
            reason,
            rounds_used,
            ..
        } => {
            assert_eq!(reason, FailReason::ApiError);
            assert_eq!(rounds_used, 1);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// Round budget exhausted without submit_result.
#[tokio::test]
async fn round_budget_exhaustion() {
    let server = MockServer::start().await;
    // Model loops on the same tool forever.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(assistant_tool_call_body(
                "search_tmdb",
                r#"{"query":"Frieren"}"#,
                "tool_calls",
            )),
        )
        .mount(&server)
        .await;

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut cfg = AgentConfig::new(
        ModelProviderInfo::new("mock", format!("{}/v1", server.uri())).with_api_key("k"),
        "mock-model",
        result_schema(),
    );
    cfg.tools = vec![Arc::new(SearchTool {
        calls: Arc::new(AtomicU32::new(0)),
    })];
    cfg.event_tx = Some(tx);
    cfg.max_rounds = 3;
    let agent = Agent::new(cfg).unwrap();
    let outcome = agent
        .run(TaskInput {
            system_prompt: "s".into(),
            user_message: "u".into(),
        })
        .await;
    match outcome {
        TaskOutcome::Failed {
            reason,
            rounds_used,
            ..
        } => {
            assert_eq!(reason, FailReason::RoundBudgetExhausted);
            assert_eq!(rounds_used, 3);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// Cancellation mid-task → Aborted with terminal event.
#[tokio::test]
async fn cancellation_aborts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(5))
                .set_body_json(assistant_tool_call_body(
                    "search_tmdb",
                    r#"{"query":"x"}"#,
                    "tool_calls",
                )),
        )
        .mount(&server)
        .await;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let mut cfg = AgentConfig::new(
        ModelProviderInfo::new("mock", format!("{}/v1", server.uri())).with_api_key("k"),
        "mock-model",
        result_schema(),
    );
    cfg.tools = vec![Arc::new(SearchTool {
        calls: Arc::new(AtomicU32::new(0)),
    })];
    cfg.event_tx = Some(tx);
    cfg.cancel = cancel.clone();
    let agent = Agent::new(cfg).unwrap();

    let run = tokio::spawn(async move {
        agent
            .run(TaskInput {
                system_prompt: "s".into(),
                user_message: "u".into(),
            })
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    cancel.cancel();
    let outcome = run.await.unwrap();
    assert!(matches!(outcome, TaskOutcome::Aborted { .. }));

    let mut events = Vec::new();
    while let Ok(env) = rx.try_recv() {
        events.push(env.event);
    }
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TaskAborted { .. }
    ));
}

/// Sequence numbers are contiguous from 0 and the terminal event is last.
#[tokio::test]
async fn event_seq_contiguous() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(assistant_tool_call_body(
                "submit_result",
                r#"{"title":"T","confidence":"low"}"#,
                "tool_calls",
            )),
        )
        .mount(&server)
        .await;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut cfg = AgentConfig::new(
        ModelProviderInfo::new("mock", format!("{}/v1", server.uri())).with_api_key("k"),
        "mock-model",
        result_schema(),
    );
    cfg.event_tx = Some(tx);
    cfg.task_id = "seq-test".into();
    let agent = Agent::new(cfg).unwrap();
    agent
        .run(TaskInput {
            system_prompt: "s".into(),
            user_message: "u".into(),
        })
        .await;

    let mut seqs = Vec::new();
    while let Ok(env) = rx.try_recv() {
        assert_eq!(env.task_id, "seq-test");
        seqs.push(env.seq);
    }
    let expect: Vec<u64> = (0..seqs.len() as u64).collect();
    assert_eq!(seqs, expect);
}

/// Duplicate tool call (same tool + same args) → served from memory, tool
/// backend hit only once.
#[tokio::test]
async fn duplicate_calls_served_from_memory() {
    let server = MockServer::start().await;

    struct Sequenced {
        n: AtomicU32,
    }
    impl Respond for Sequenced {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            let body = match n {
                // Same call twice (key order differs — must still dedup).
                0 => {
                    assistant_tool_call_body("search_tmdb", r#"{"query":"Frieren"}"#, "tool_calls")
                }
                1 => assistant_tool_call_body(
                    "search_tmdb",
                    r#"{ "query" : "Frieren" }"#,
                    "tool_calls",
                ),
                2 => {
                    // The dedup note must be visible to the model.
                    let body: Value = serde_json::from_slice(&req.body).unwrap();
                    let msgs = body["messages"].as_array().unwrap();
                    let last = msgs.last().unwrap();
                    assert_eq!(last["role"], "tool");
                    assert!(last["content"].as_str().unwrap().contains("already called"));
                    assistant_tool_call_body(
                        "submit_result",
                        r#"{"title":"Frieren","confidence":"high"}"#,
                        "tool_calls",
                    )
                }
                _ => panic!("too many rounds"),
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequenced {
            n: AtomicU32::new(0),
        })
        .mount(&server)
        .await;

    let calls = Arc::new(AtomicU32::new(0));
    let (outcome, _) = run_agent(
        &server,
        vec![Arc::new(SearchTool {
            calls: calls.clone(),
        })],
    )
    .await;
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "backend must be hit exactly once"
    );
}

/// Oversized tool output is truncated before reaching the model.
#[tokio::test]
async fn oversized_tool_output_truncated() {
    struct BigTool;
    impl Tool for BigTool {
        fn name(&self) -> &str {
            "big"
        }
        fn description(&self) -> &str {
            "returns a huge payload"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }
        fn call(&self, _: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
            Box::pin(async { Ok(ToolOutput::text("x".repeat(100_000))) })
        }
    }

    let server = MockServer::start().await;
    struct Sequenced {
        n: AtomicU32,
    }
    impl Respond for Sequenced {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            let body = if n == 0 {
                assistant_tool_call_body("big", "{}", "tool_calls")
            } else {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                let msgs = body["messages"].as_array().unwrap();
                let tool_msg = msgs.last().unwrap();
                let content = tool_msg["content"].as_str().unwrap();
                assert!(
                    content.len() < 20_000,
                    "tool output not truncated: {} bytes",
                    content.len()
                );
                assert!(content.contains("output truncated"));
                assistant_tool_call_body(
                    "submit_result",
                    r#"{"title":"T","confidence":"low"}"#,
                    "tool_calls",
                )
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequenced {
            n: AtomicU32::new(0),
        })
        .mount(&server)
        .await;

    let (outcome, _) = run_agent(&server, vec![Arc::new(BigTool)]).await;
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
}
