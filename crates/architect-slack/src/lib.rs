//! Read-only Slack tools — listing channels, browsing a channel's recent
//! history, and reading a thread's parent message and its replies.
//!
//! Depends on `architect-tools` to implement its `Tool` trait, the same
//! relationship `architect-mcp` has with it. Does not depend on
//! `architect-config` or `apps/desktop`: `apps/desktop` is the one place
//! that knows a saved token exists at all — this crate only knows how to
//! use one once it's handed one.
//!
//! Read-only. Nothing here posts a message or reacts to one.

pub mod channel;
mod client;
mod format;
pub mod thread;

use std::sync::Arc;

use architect_tools::Tool;

pub use client::SlackClient;

/// Every tool this crate offers, authenticated with `token` — the engine's
/// extension point, the same role `architect_mcp::connect`'s `McpConnection.
/// tools` plays for an MCP server.
pub fn tools(token: impl Into<String>) -> Vec<Arc<dyn Tool>> {
    let client = Arc::new(SlackClient::new(token.into()));

    vec![
        Arc::new(thread::ReadThread {
            client: client.clone(),
        }),
        Arc::new(channel::ListChannels {
            client: client.clone(),
        }),
        Arc::new(channel::ReadChannelHistory { client }),
    ]
}
