//! A thin wrapper over Slack's Web API — just enough to read a thread.
//!
//! Slack's Web API is RPC-style (`POST/GET https://slack.com/api/{method}`),
//! not REST — every call returns HTTP 200 even on failure, reporting
//! success via a body-level `ok: bool` and, on failure, an `error` code.
//! This is the one place the usual "check the status code" instinct is
//! wrong for this crate.

use serde_json::Value;

const API_ROOT: &str = "https://slack.com/api";

pub struct SlackClient {
    http: reqwest::Client,
    token: String,
    /// `https://slack.com/api` in production; a `wiremock` server's URI in
    /// tests.
    base_url: String,
}

impl SlackClient {
    pub fn new(token: String) -> Self {
        Self::with_base_url(token, API_ROOT.to_owned())
    }

    /// For tests: point requests at a local mock server instead of Slack.
    pub fn with_base_url(token: String, base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            token,
            base_url,
        }
    }

    /// One Slack Web API method call, e.g. `call("conversations.replies",
    /// &[("channel", channel), ("ts", ts)])`.
    pub async fn call(&self, method: &str, params: &[(&str, &str)]) -> Result<Value, String> {
        let response = self
            .http
            .get(format!("{}/{method}", self.base_url))
            .header("Authorization", format!("Bearer {}", self.token))
            .query(params)
            .send()
            .await
            .map_err(|error| format!("Slack request failed: {error}"))?;

        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("could not parse Slack's response: {error}"))?;

        if body["ok"].as_bool() == Some(true) {
            Ok(body)
        } else {
            let error = body["error"].as_str().unwrap_or("unknown_error");
            Err(format!("Slack API error: {error}"))
        }
    }
}
