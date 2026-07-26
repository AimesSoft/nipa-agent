use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::error::{backoff, AgentError};
use crate::provider::ModelProviderInfo;
use crate::wire::{build_request_body, ChatMessage, ChatResponse};

/// Non-streaming Chat Completions client with transport-level retry.
///
/// Two-layer retry design (codex): this client owns the per-request loop
/// (connection errors, 5xx, 429); the agent loop above owns nothing extra in
/// v1 because non-streaming means a request either fully succeeds or fails.
pub struct ChatClient {
    http: reqwest::Client,
    provider: ModelProviderInfo,
    api_key: Option<String>,
}

pub struct RetryNotice {
    pub attempt: u32,
    pub max_attempts: u32,
    pub message: String,
}

impl ChatClient {
    pub fn new(provider: ModelProviderInfo) -> Result<Self, AgentError> {
        let api_key = provider.resolve_api_key()?;
        let http = reqwest::Client::builder()
            .timeout(provider.request_timeout())
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| AgentError::Config(format!("failed to build http client: {e}")))?;
        Ok(Self {
            http,
            provider,
            api_key,
        })
    }

    pub fn provider(&self) -> &ModelProviderInfo {
        &self.provider
    }

    /// One logical model call: retries transient failures with exponential
    /// backoff (server Retry-After takes precedence), reports each retry via
    /// `on_retry`, and honors cancellation between attempts.
    pub async fn chat(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        cancel: &CancellationToken,
        mut on_retry: impl FnMut(RetryNotice),
    ) -> Result<ChatResponse, AgentError> {
        let max_retries = self.provider.max_retries();
        let mut attempt: u32 = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let err = match self.chat_once(model, messages, tools, cancel).await {
                Ok(resp) => return Ok(resp),
                Err(e) if !e.is_retryable() => return Err(e),
                Err(e) => e,
            };
            if attempt >= max_retries {
                return Err(AgentError::RetryLimit(Box::new(err)));
            }
            attempt += 1;
            // Server Retry-After wins over local backoff, but is capped: a
            // hostile/buggy header must not park a task for an hour.
            const RETRY_AFTER_CAP: Duration = Duration::from_secs(120);
            let delay = err
                .retry_delay()
                .map(|d| d.min(RETRY_AFTER_CAP))
                .unwrap_or_else(|| backoff(attempt));
            warn!(provider = %self.provider.name, attempt, ?delay, error = %err, "retrying model request");
            on_retry(RetryNotice {
                attempt,
                max_attempts: max_retries,
                message: err.to_string(),
            });
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = cancel.cancelled() => return Err(AgentError::Cancelled),
            }
        }
    }

    async fn chat_once(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        cancel: &CancellationToken,
    ) -> Result<ChatResponse, AgentError> {
        let body = build_request_body(
            model,
            messages,
            tools,
            self.provider.compat.send_tool_choice,
            &self.provider.extra_body,
        );
        let mut req = self
            .http
            .post(self.provider.chat_completions_url())
            .json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        for (k, v) in &self.provider.http_headers {
            req = req.header(k, v);
        }

        let resp = tokio::select! {
            r = req.send() => r,
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
        };
        let resp = resp.map_err(|e| {
            if e.is_timeout() {
                AgentError::Timeout
            } else {
                AgentError::Connection(e.to_string())
            }
        })?;

        let status = resp.status();
        if !status.is_success() {
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs);
            let text = resp.text().await.unwrap_or_default();
            let text = truncate(&text, 2000);
            return Err(classify_http_error(status.as_u16(), text, retry_after));
        }

        let text = tokio::select! {
            r = resp.text() => r.map_err(|e| AgentError::Connection(format!("reading body: {e}")))?,
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
        };
        let parsed: ChatResponse = serde_json::from_str(&text).map_err(|e| {
            debug!(body = %truncate(&text, 500), "unparseable response body");
            AgentError::Parse(format!("{e}"))
        })?;
        if parsed.choices.is_empty() {
            return Err(AgentError::EmptyResponse);
        }
        Ok(parsed)
    }
}

fn classify_http_error(status: u16, body: String, retry_after: Option<Duration>) -> AgentError {
    match status {
        401 | 403 => AgentError::Auth(status, body),
        429 => AgentError::RateLimited { retry_after },
        408 => AgentError::Timeout,
        400..=499 => {
            // Context-window errors come back as 400 with provider-specific
            // wording; catch the common phrasings.
            let lower = body.to_lowercase();
            if lower.contains("context length")
                || lower.contains("context_length")
                || lower.contains("maximum context")
                || lower.contains("token limit")
            {
                AgentError::ContextWindowExceeded
            } else {
                AgentError::BadRequest(status, body)
            }
        }
        _ => AgentError::Server(status, body),
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_error_classification() {
        assert!(matches!(
            classify_http_error(401, String::new(), None),
            AgentError::Auth(..)
        ));
        assert!(matches!(
            classify_http_error(429, String::new(), Some(Duration::from_secs(2))),
            AgentError::RateLimited {
                retry_after: Some(_)
            }
        ));
        assert!(matches!(
            classify_http_error(400, "bad".into(), None),
            AgentError::BadRequest(..)
        ));
        assert!(matches!(
            classify_http_error(
                400,
                "This model's maximum context length is 65536".into(),
                None
            ),
            AgentError::ContextWindowExceeded
        ));
        assert!(matches!(
            classify_http_error(500, String::new(), None),
            AgentError::Server(..)
        ));
        // 4xx must NOT be retryable (a wrong model name would burn retries).
        assert!(!classify_http_error(404, String::new(), None).is_retryable());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "字幕组测试";
        let t = truncate(s, 4);
        assert!(t.ends_with('…'));
    }
}
