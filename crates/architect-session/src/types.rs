//! Types shared by the store's public methods.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
    /// Mints a fresh id. `pub` rather than `pub(crate)` — the desktop
    /// engine mints ids client-side (for "New Chat") so both sides of the
    /// UI/worker channel agree on a session's identity before either does
    /// any work for it, which per-session concurrent state depends on.
    ///
    /// No `Default` impl: unlike a real default value, every call here must
    /// mint a genuinely distinct id, so a caller reaching for `default()`
    /// out of habit would be a bug worth a compile error, not a silent
    /// collision.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for SessionId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

/// A row from [`crate::SessionStore::list_sessions`].
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub title: String,
    pub provider_kind: String,
    pub model: String,
    /// `Some` for a sub-agent's session, spawned by a `spawn_subagents` tool
    /// call in another session's turn — `None` for an ordinary chat.
    pub parent_id: Option<SessionId>,
    pub created_at: String,
    pub updated_at: String,
}

/// What [`crate::SessionStore::reverse_to_point`] actually did, newest-first —
/// the order the changes were undone in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReverseOutcome {
    /// Files put back to their content before the change.
    pub restored: Vec<PathBuf>,
    /// Files removed because the change had created them.
    pub deleted: Vec<PathBuf>,
}
