//! Provider construction and extension.
//!
//! A [`ProviderRegistry`] maps a configuration `kind` to a factory. Built-ins
//! cover `"openai"` and `"anthropic"`; anything else is registered by the host
//! application, which is how a custom API is added without changing this crate.
//!
//! Most "other" providers need no code: LM Studio, DeepSeek, OpenRouter, vLLM
//! and Ollama all speak the OpenAI dialect, so they are `kind: "openai"` with a
//! different `base_url`.

use std::{collections::HashMap, sync::Arc};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::{
    error::LlmError,
    provider::Provider,
    providers::{AnthropicProvider, OpenAiProvider},
};

/// How to reach one provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Which adapter to build, e.g. `"openai"` or `"anthropic"`.
    pub kind: String,
    /// Endpoint root. Optional for providers with a canonical URL.
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    /// Extra headers — gateway routing, org ids, beta flags.
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    /// Default model for this provider, used when a request does not name one.
    pub model: Option<String>,
}

impl ProviderConfig {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            ..Self::default()
        }
    }

    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    fn headers(&self) -> Result<HeaderMap, LlmError> {
        let mut headers = HeaderMap::new();

        for (name, value) in &self.extra_headers {
            let name = HeaderName::try_from(name.as_str())
                .map_err(|_| LlmError::Config(format!("invalid header name {name:?}")))?;
            let value = HeaderValue::from_str(value)
                .map_err(|_| LlmError::Config(format!("invalid value for header {name:?}")))?;
            headers.insert(name, value);
        }

        Ok(headers)
    }
}

/// Whether `config` points at a server running on this machine rather than
/// a real hosted API. Local inference servers (LM Studio, Ollama, vLLM,
/// ...) generally can't usefully serve more than one request at a time —
/// callers that would otherwise fan several requests out at once (e.g.
/// `spawn_subagents`) should queue against a local server instead of racing
/// it. A hosted API has no such constraint.
///
/// Only recognizes loopback hosts (`localhost`, `127.0.0.1`, `[::1]`,
/// `0.0.0.0`) — a local server reachable over the LAN by a different
/// hostname/IP won't be detected. A reasonable v1 line to draw: the common
/// case (LM Studio's default `http://localhost:1234/v1`) is covered, and
/// nothing here is safety-critical if it under-detects — worst case, a
/// harder-to-reach local server just gets treated as a hosted API and its
/// sub-agents run concurrently instead of queued.
pub fn is_local(config: &ProviderConfig) -> bool {
    let Some(base_url) = &config.base_url else {
        return false;
    };
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };

    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("[::1]") | Some("0.0.0.0")
    )
}

/// Builds a provider from configuration.
pub trait ProviderFactory: Send + Sync {
    fn build(&self, config: &ProviderConfig) -> Result<Arc<dyn Provider>, LlmError>;
}

impl<F> ProviderFactory for F
where
    F: Fn(&ProviderConfig) -> Result<Arc<dyn Provider>, LlmError> + Send + Sync,
{
    fn build(&self, config: &ProviderConfig) -> Result<Arc<dyn Provider>, LlmError> {
        self(config)
    }
}

/// Known provider kinds.
pub struct ProviderRegistry {
    factories: HashMap<String, Box<dyn ProviderFactory>>,
}

impl ProviderRegistry {
    /// An empty registry, with no built-ins.
    pub fn empty() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register a factory, replacing any existing one for that kind.
    ///
    /// This is the extension point for a custom API.
    pub fn register(&mut self, kind: impl Into<String>, factory: impl ProviderFactory + 'static) {
        self.factories.insert(kind.into(), Box::new(factory));
    }

    pub fn kinds(&self) -> impl Iterator<Item = &str> {
        self.factories.keys().map(String::as_str)
    }

    pub fn build(&self, config: &ProviderConfig) -> Result<Arc<dyn Provider>, LlmError> {
        self.factories
            .get(&config.kind)
            .ok_or_else(|| LlmError::UnknownProvider(config.kind.clone()))?
            .build(config)
    }
}

impl Default for ProviderRegistry {
    /// A registry with the built-in `openai` and `anthropic` kinds.
    fn default() -> Self {
        let mut registry = Self::empty();

        registry.register("openai", |config: &ProviderConfig| {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_owned());

            let mut provider = OpenAiProvider::new(base_url).headers(config.headers()?);
            if let Some(api_key) = &config.api_key {
                provider = provider.api_key(api_key);
            }

            Ok(Arc::new(provider) as Arc<dyn Provider>)
        });

        registry.register("anthropic", |config: &ProviderConfig| {
            let api_key = config
                .api_key
                .clone()
                .ok_or_else(|| LlmError::Config("anthropic requires an api_key".into()))?;

            let mut provider = AnthropicProvider::new(api_key).headers(config.headers()?);
            if let Some(base_url) = &config.base_url {
                provider = provider.base_url(base_url);
            }

            Ok(Arc::new(provider) as Arc<dyn Provider>)
        });

        registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_built_in_kinds() {
        let registry = ProviderRegistry::default();

        let openai = registry.build(&ProviderConfig::new("openai")).unwrap();
        assert_eq!(openai.id(), "openai");

        let anthropic = registry
            .build(&ProviderConfig::new("anthropic").api_key("sk-test"))
            .unwrap();
        assert_eq!(anthropic.id(), "anthropic");
    }

    #[test]
    fn a_local_server_is_just_a_different_base_url() {
        let registry = ProviderRegistry::default();
        let config = ProviderConfig::new("openai")
            .base_url("http://localhost:1234/v1")
            .model("deepseek-v4-flash");

        assert!(registry.build(&config).is_ok());
    }

    #[test]
    fn anthropic_without_a_key_is_a_config_error() {
        let registry = ProviderRegistry::default();
        let Err(error) = registry.build(&ProviderConfig::new("anthropic")) else {
            panic!("a missing api key must be rejected");
        };

        assert!(matches!(error, LlmError::Config(_)), "got {error:?}");
    }

    #[test]
    fn loopback_base_urls_are_local() {
        for base_url in [
            "http://localhost:1234/v1",
            "http://127.0.0.1:1234/v1",
            "http://[::1]:1234/v1",
        ] {
            let config = ProviderConfig::new("openai").base_url(base_url);
            assert!(is_local(&config), "{base_url} should be local");
        }
    }

    #[test]
    fn a_real_hosted_api_is_not_local() {
        let config = ProviderConfig::new("openai").base_url("https://api.openai.com/v1");
        assert!(!is_local(&config));
    }

    #[test]
    fn no_base_url_is_not_local() {
        let config = ProviderConfig::new("anthropic").api_key("sk-test");
        assert!(!is_local(&config));
    }

    #[test]
    fn unknown_kinds_are_named_in_the_error() {
        let registry = ProviderRegistry::default();
        let Err(error) = registry.build(&ProviderConfig::new("bedrock")) else {
            panic!("an unregistered kind must be rejected");
        };

        assert!(matches!(error, LlmError::UnknownProvider(kind) if kind == "bedrock"));
    }
}
