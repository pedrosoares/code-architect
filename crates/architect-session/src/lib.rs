//! Durable storage for a workspace's conversations, under `.coder/`.
//!
//! One [`SessionStore`] per workspace, backed by blocking `rusqlite` behind
//! [`tokio::task::spawn_blocking`] — unlike V1, which called blocking SQLite
//! directly from async handlers. Nothing here knows about tools or the agent
//! loop; the desktop engine is the only place that wires a
//! `architect_core::ChangeRecorder` channel into calls on this store, the same
//! shape as its `AgentEvent` bridge.

mod error;
mod schema;
mod store;
mod types;

pub use error::SessionError;
pub use store::SessionStore;
pub use types::{ReverseOutcome, SessionId, SessionSummary};
