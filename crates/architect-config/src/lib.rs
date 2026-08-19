//! Saved, machine-global app configuration: API provider profiles and MCP
//! server connections.
//!
//! One [`ConfigStore`] (`~/.config/code-architect/profiles.json`), not
//! per-workspace like `architect-session`: an API key or an MCP server is set
//! up once and reused across every project, not scoped to a single `.coder/`
//! directory. Deliberately holds no `architect_llm` or `rmcp` dependency — a
//! [`Profile`]'s fields mirror `architect_llm::ProviderConfig`'s shape and an
//! [`McpServerConfig`] is plain data, but the desktop engine is the one place
//! that knows those crates exist and bridges between them, the same shape as
//! its `SessionStore`/`ToolRegistry` bridge.

mod error;
mod store;
mod types;

pub use error::ConfigError;
pub use store::ConfigStore;
pub use types::{DocsConfig, IntegrationsConfig, McpServerConfig, McpTransport, Profile};
