//! A thin wrapper over Linear's GraphQL API.

use serde_json::Value;

const API_ROOT: &str = "https://api.linear.app/graphql";

pub struct LinearClient {
    http: reqwest::Client,
    api_key: String,
    /// `https://api.linear.app/graphql` in production; a `wiremock`
    /// server's URI in tests.
    url: String,
}

impl LinearClient {
    pub fn new(api_key: String) -> Self {
        Self::with_url(api_key, API_ROOT.to_owned())
    }

    /// For tests: point requests at a local mock server instead of Linear.
    pub fn with_url(api_key: String, url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            url,
        }
    }

    /// Run one GraphQL query. Linear's own docs specify the raw API key as
    /// the `Authorization` header value — no `Bearer` prefix, unlike almost
    /// every other API this app talks to.
    pub async fn query(&self, query: &str, variables: Value) -> Result<Value, String> {
        let response = self
            .http
            .post(&self.url)
            .header("Authorization", &self.api_key)
            .json(&serde_json::json!({"query": query, "variables": variables}))
            .send()
            .await
            .map_err(|error| format!("Linear request failed: {error}"))?;

        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("could not parse Linear's response: {error}"))?;

        if !status.is_success() {
            return Err(format!("Linear API error {status}: {body}"));
        }
        if let Some(errors) = body["errors"]
            .as_array()
            .filter(|errors| !errors.is_empty())
        {
            let messages: Vec<&str> = errors
                .iter()
                .filter_map(|error| error["message"].as_str())
                .collect();
            return Err(format!("Linear API error: {}", messages.join("; ")));
        }

        Ok(body["data"].clone())
    }
}
