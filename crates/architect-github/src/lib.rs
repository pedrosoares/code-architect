//! Read-only GitHub tools — pull requests (summary, comments, diff,
//! commits), plain issues (and their comments), and reading a repo's files
//! and directories at any ref.
//!
//! Depends on `architect-tools` to implement its `Tool` trait, the same
//! relationship `architect-mcp` has with it. Does not depend on
//! `architect-config` or `apps/desktop`: `apps/desktop` is the one place
//! that knows a saved token exists at all — this crate only knows how to
//! use one once it's handed one.
//!
//! Every tool here is read-only. Nothing in this crate writes a comment,
//! merges a PR, or otherwise mutates anything on GitHub.

mod client;
pub mod contents;
mod format;
pub mod issue;
pub mod pull_request;

use std::sync::Arc;

use architect_tools::Tool;

pub use client::GitHubClient;

/// Every tool this crate offers, authenticated with `token` — the engine's
/// extension point, the same role `architect_mcp::connect`'s `McpConnection.
/// tools` plays for an MCP server.
pub fn tools(token: impl Into<String>) -> Vec<Arc<dyn Tool>> {
    let client = Arc::new(GitHubClient::new(token.into()));

    vec![
        Arc::new(pull_request::ReadPullRequest {
            client: client.clone(),
        }),
        Arc::new(pull_request::ReadPullRequestComments {
            client: client.clone(),
        }),
        Arc::new(pull_request::ReadPullRequestDiff {
            client: client.clone(),
        }),
        Arc::new(pull_request::ReadPullRequestCommits {
            client: client.clone(),
        }),
        Arc::new(issue::ReadIssue {
            client: client.clone(),
        }),
        Arc::new(issue::ReadIssueComments {
            client: client.clone(),
        }),
        Arc::new(contents::ReadFile {
            client: client.clone(),
        }),
        Arc::new(contents::ListDirectory { client }),
    ]
}
