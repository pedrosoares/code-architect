//! What can go wrong talking to `sessions.db`.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),

    #[error("could not (de)serialize message content: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("could not restore {path}: {source}")]
    Restore {
        path: PathBuf,
        source: std::io::Error,
    },

    /// A `spawn_blocking` task panicked. Every query the store runs is
    /// infallible Rust apart from the SQL itself, so this should only ever
    /// fire on a genuine bug — it is kept distinct from [`SessionError::Sql`]
    /// so that distinction is visible in logs.
    #[error("session store task panicked: {0}")]
    TaskPanicked(String),
}
