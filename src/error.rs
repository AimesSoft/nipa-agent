use std::time::Duration;
use thiserror::Error;

/// Unified error type for the agent runtime.
///
/// Retryability is decided in exactly one place ([`AgentError::is_retryable`]),
/// and server-provided delays (Retry-After) take precedence over local backoff
/// via [`AgentError::retry_delay`].
#[derive(Debug, Error)]
pub enum AgentError {
    #[error("connection failed: {0}")]
    Connection(String),

    #[error("request timed out")]
    Timeout,

    #[error("server error {0}: {1}")]
    Server(u16, String),

    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },

    /// 4xx that is not auth/rate-limit: the request itself is wrong
    /// (bad model name, malformed schema, ...). Retrying cannot help.
    #[error("bad request {0}: {1}")]
    BadRequest(u16, String),

    #[error("authentication failed ({0}): {1}")]
    Auth(u16, String),

    /// 200 response carrying neither text content nor tool calls.
    #[error("model returned an empty response")]
    EmptyResponse,

    /// A 200 response whose body we could not parse.
    #[error("failed to parse response: {0}")]
    Parse(String),

    #[error("context window exceeded")]
    ContextWindowExceeded,

    #[error("cancelled")]
    Cancelled,

    #[error("retry limit exhausted, last error: {0}")]
    RetryLimit(#[source] Box<AgentError>),

    /// A tool reported an unrecoverable failure ([`crate::tool::ToolError::Fatal`]).
    #[error("fatal tool error: {0}")]
    ToolFatal(String),

    #[error("configuration error: {0}")]
    Config(String),
}

impl AgentError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AgentError::Connection(_)
                | AgentError::Timeout
                | AgentError::Server(..)
                | AgentError::RateLimited { .. }
                | AgentError::EmptyResponse
                | AgentError::Parse(_)
        )
    }

    /// Server-mandated delay (`Retry-After`). Takes precedence over the
    /// runtime's exponential backoff.
    pub fn retry_delay(&self) -> Option<Duration> {
        match self {
            AgentError::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }
}

/// Exponential backoff with ±10% jitter: 200ms, 400ms, 800ms, 1.6s, ...
/// `attempt` starts at 1.
pub(crate) fn backoff(attempt: u32) -> Duration {
    const INITIAL_DELAY_MS: f64 = 200.0;
    const FACTOR: f64 = 2.0;
    let base = INITIAL_DELAY_MS * FACTOR.powi(attempt.saturating_sub(1) as i32);
    let jitter = 0.9 + fastrand::f64() * 0.2;
    Duration::from_millis((base * jitter) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially_within_jitter_bounds() {
        for attempt in 1..=5u32 {
            let d = backoff(attempt).as_millis() as f64;
            let base = 200.0 * 2f64.powi(attempt as i32 - 1);
            assert!(
                d >= base * 0.9 - 1.0,
                "attempt {attempt}: {d} < {}",
                base * 0.9
            );
            assert!(
                d <= base * 1.1 + 1.0,
                "attempt {attempt}: {d} > {}",
                base * 1.1
            );
        }
    }

    #[test]
    fn retryability_classification() {
        assert!(AgentError::Timeout.is_retryable());
        assert!(AgentError::Server(500, String::new()).is_retryable());
        assert!(AgentError::RateLimited { retry_after: None }.is_retryable());
        assert!(AgentError::EmptyResponse.is_retryable());
        assert!(!AgentError::BadRequest(400, String::new()).is_retryable());
        assert!(!AgentError::Auth(401, String::new()).is_retryable());
        assert!(!AgentError::ContextWindowExceeded.is_retryable());
        assert!(!AgentError::Cancelled.is_retryable());
    }
}
