//! Errors from provider adapters.

use std::time::Duration;

/// Everything a provider call can fail with.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// Non-2xx response. The body is kept verbatim — provider error payloads
    /// carry the actual reason and are the first thing anyone needs.
    #[error("{provider} returned HTTP {status}: {body}")]
    Http {
        provider: &'static str,
        status: u16,
        body: String,
    },

    /// HTTP 429. Carries `Retry-After` when the provider sent one.
    #[error("{provider} rate limited (retry after {retry_after:?}): {body}")]
    RateLimited {
        provider: &'static str,
        retry_after: Option<Duration>,
        body: String,
    },

    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The response parsed as JSON but not into the shape we expect, or did not
    /// parse at all. `raw` is truncated for logging.
    #[error("could not decode {context}: {message} (in: {raw})")]
    Decode {
        context: &'static str,
        message: String,
        raw: String,
    },

    /// An error the provider reported inside an otherwise-successful stream.
    #[error("{provider} error: {message}")]
    Api {
        provider: &'static str,
        message: String,
    },

    #[error("request cancelled")]
    Cancelled,

    #[error("unknown provider kind {0:?}")]
    UnknownProvider(String),

    #[error("invalid provider configuration: {0}")]
    Config(String),
}

impl LlmError {
    /// Whether retrying the same request could plausibly succeed.
    ///
    /// 4xx other than 429 never is: a malformed request stays malformed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } => true,
            Self::Http { status, .. } => *status >= 500,
            Self::Transport(error) => error.is_timeout() || error.is_connect(),
            _ => false,
        }
    }

    /// How long the provider asked us to wait, if it said.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    pub(crate) fn decode(context: &'static str, message: impl ToString, raw: &str) -> Self {
        const MAX_RAW: usize = 400;

        let mut raw: String = raw.chars().take(MAX_RAW).collect();
        if raw.len() < raw.capacity() {
            raw.push_str("...");
        }

        Self::Decode {
            context,
            message: message.to_string(),
            raw,
        }
    }
}
