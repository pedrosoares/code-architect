//! Shared HTTP behavior: retries and error mapping.

use std::time::Duration;

use reqwest::{RequestBuilder, Response, StatusCode};

use crate::error::LlmError;

/// How many times a retryable failure is retried before giving up.
pub const MAX_RETRIES: u32 = 3;

const BASE_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Send a request, retrying rate limits and server errors.
///
/// 4xx responses other than 429 are returned immediately — a malformed request
/// will stay malformed, and retrying only delays the error the caller needs.
pub async fn send_retrying(
    provider: &'static str,
    request: RequestBuilder,
    max_retries: u32,
) -> Result<Response, LlmError> {
    let mut attempt = 0;

    loop {
        // Cloning fails only for streaming bodies, which we never send.
        let attempt_request = request
            .try_clone()
            .ok_or_else(|| LlmError::Config("request body is not retryable".into()))?;

        let outcome = match attempt_request.send().await {
            Ok(response) => classify(provider, response).await,
            Err(error) => Err(LlmError::Transport(error)),
        };

        let error = match outcome {
            Ok(response) => return Ok(response),
            Err(error) => error,
        };

        if attempt >= max_retries || !error.is_retryable() {
            return Err(error);
        }

        let wait = error.retry_after().unwrap_or_else(|| backoff(attempt));
        tracing::warn!(
            provider,
            attempt = attempt + 1,
            wait_ms = wait.as_millis() as u64,
            "retrying after {error}"
        );
        tokio::time::sleep(wait).await;
        attempt += 1;
    }
}

/// Turn a non-success response into the right error variant.
async fn classify(provider: &'static str, response: Response) -> Result<Response, LlmError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let retry_after = parse_retry_after(&response);
    let body = response.text().await.unwrap_or_default();

    Err(if status == StatusCode::TOO_MANY_REQUESTS {
        LlmError::RateLimited {
            provider,
            retry_after,
            body,
        }
    } else {
        LlmError::Http {
            provider,
            status: status.as_u16(),
            body,
        }
    })
}

/// `Retry-After` in delay-seconds form. The HTTP-date form is not used by
/// either provider and is not worth the dependency.
fn parse_retry_after(response: &Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn backoff(attempt: u32) -> Duration {
    MAX_BACKOFF.min(BASE_BACKOFF * 2u32.saturating_pow(attempt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_then_caps() {
        assert_eq!(backoff(0), Duration::from_millis(500));
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(20), MAX_BACKOFF);
    }
}
