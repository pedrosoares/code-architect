//! Types shared by the store's public methods.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One saved API configuration: which provider, model and credentials to use.
///
/// Field names deliberately mirror `architect_llm::ProviderConfig` so the
/// engine's translation from one to the other is a straight field copy — see
/// this crate's top-level docs for why they aren't the same type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    /// What the sidebar shows — not sent to the provider.
    pub name: String,
    /// `"openai"` or `"anthropic"`; anything else is rejected when a turn
    /// tries to use it, the same as an unregistered `ProviderConfig::kind`.
    pub kind: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: String,
}

impl Profile {
    pub fn new(name: impl Into<String>, kind: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            kind: kind.into(),
            base_url: None,
            api_key: None,
            model: model.into(),
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
}

/// How to reach an MCP server — a locally spawned process talking stdio, or
/// a remote server over streamable HTTP (optionally with a static Bearer
/// token; no OAuth flow).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        /// The command to spawn, e.g. `"npx"` or `"uvx"`.
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        bearer_token: Option<String>,
    },
}

/// One saved MCP server to connect to for extra tools — global to the
/// machine, same as [`Profile`]. Unlike a provider profile, more than one
/// can be `enabled` at once: MCP servers each contribute their own tools to
/// the same tool set, they don't compete for "the" active one.
///
/// Deliberately plain data with no `rmcp` dependency — `apps/desktop` is
/// what turns this into a real connection, the same separation `Profile`
/// already keeps from `architect_llm::ProviderConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: Uuid,
    /// What the settings panel shows.
    pub name: String,
    pub enabled: bool,
    pub transport: McpTransport,
}

impl McpServerConfig {
    pub fn stdio(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            enabled: true,
            transport: McpTransport::Stdio {
                command: command.into(),
                args: Vec::new(),
                env: HashMap::new(),
            },
        }
    }

    pub fn http(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            enabled: true,
            transport: McpTransport::Http {
                url: url.into(),
                bearer_token: None,
            },
        }
    }

    /// Only meaningful when `transport` is already `Stdio` — a no-op
    /// otherwise, since there is no argument list to set on an `Http`
    /// transport.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        if let McpTransport::Stdio { args: existing, .. } = &mut self.transport {
            *existing = args.into_iter().map(Into::into).collect();
        }
        self
    }

    /// Only meaningful when `transport` is already `Http` — a no-op
    /// otherwise.
    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        if let McpTransport::Http { bearer_token, .. } = &mut self.transport {
            *bearer_token = Some(token.into());
        }
        self
    }
}

/// Saved credentials for the read-only GitHub/Slack/Linear tool crates —
/// global to the machine, same as [`Profile`]/[`McpServerConfig`]. Unlike
/// either of those, there is exactly one of each: no id, no enabled flag,
/// no list — an absent or empty credential just means that integration's
/// tools aren't registered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsConfig {
    #[serde(default)]
    pub github_token: Option<String>,
    /// When set, GitHub tools resolve a token by running `gh auth token`
    /// instead of using `github_token` — see `apps/desktop/src/engine.rs`'s
    /// `resolve_github_token`.
    #[serde(default)]
    pub github_use_gh_cli: bool,
    #[serde(default)]
    pub slack_token: Option<String>,
    #[serde(default)]
    pub linear_api_key: Option<String>,
}

/// Which documentation driver backs the `write_doc`/`edit_doc`/`read_doc`/
/// `search_docs`/`list_docs`/`scaffold_docs` tools, global to the machine
/// same as [`IntegrationsConfig`]. Unlike GitHub/Slack/Linear, Obsidian
/// needs no credential — `enabled` alone decides whether the doc tools are
/// registered, and `vault_path` is only needed to point at a vault that
/// lives somewhere other than `<workspace_root>/docs`, which is otherwise
/// already the right answer per-workspace without any saved override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocsConfig {
    #[serde(default = "default_docs_enabled")]
    pub enabled: bool,
    /// `"obsidian"` for v1; `"notion"`/`"jira"` later.
    #[serde(default = "default_docs_driver")]
    pub driver: String,
    /// `None` resolves to `<workspace_root>/docs`.
    #[serde(default)]
    pub vault_path: Option<String>,
}

fn default_docs_enabled() -> bool {
    true
}

fn default_docs_driver() -> String {
    "obsidian".to_owned()
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            enabled: default_docs_enabled(),
            driver: default_docs_driver(),
            vault_path: None,
        }
    }
}
