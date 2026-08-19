//! Read-only Linear tools — a ticket, its comments, and its blocking
//! relations.
//!
//! Depends on `architect-tools` to implement its `Tool` trait, the same
//! relationship `architect-mcp` has with it. Does not depend on
//! `architect-config` or `apps/desktop`: `apps/desktop` is the one place
//! that knows a saved API key exists at all — this crate only knows how to
//! use one once it's handed one.
//!
//! Read-only. Nothing here creates, updates, or comments on a ticket.

mod client;
pub mod ticket;

use std::sync::Arc;

use architect_tools::Tool;

pub use client::LinearClient;

/// Every tool this crate offers, authenticated with `api_key` — the
/// engine's extension point, the same role `architect_mcp::connect`'s
/// `McpConnection.tools` plays for an MCP server.
pub fn tools(api_key: impl Into<String>) -> Vec<Arc<dyn Tool>> {
    let client = Arc::new(LinearClient::new(api_key.into()));

    vec![Arc::new(ticket::ReadTicket { client })]
}
