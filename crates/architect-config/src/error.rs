//! What can go wrong talking to `profiles.json`.

use std::path::PathBuf;

use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("could not (de)serialize saved configurations: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("could not determine a config directory: $HOME is not set")]
    NoConfigDir,

    /// A `spawn_blocking` task panicked — should only fire on a genuine bug.
    #[error("config store task panicked: {0}")]
    TaskPanicked(String),

    #[error("unknown configuration {0}")]
    UnknownProfile(Uuid),
}
