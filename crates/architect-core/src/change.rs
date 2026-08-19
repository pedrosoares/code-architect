//! What a tool changed on disk, and where that gets reported.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One file mutation, as reported by the tool that made it.
///
/// `old_content: None` means the file did not exist before — the same
/// created-vs-edited sentinel a rollback needs: recreate on undo if it existed,
/// delete on undo if it didn't.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileChange {
    pub file_path: PathBuf,
    pub old_content: Option<String>,
    pub new_content: String,
    pub tool_name: &'static str,
}

/// A [`FileChange`] together with the point in the conversation it happened
/// at — what a caller needs to offer "roll back to before this" without
/// reaching back into the database itself. Never itself persisted or sent
/// over the wire (unlike `FileChange`), so it doesn't need `Serialize`/
/// `Deserialize` — and `FileChange::tool_name` being `&'static str` makes
/// deriving `Deserialize` for anything that nests it a real lifetime
/// problem, not just unnecessary.
#[derive(Debug, Clone, PartialEq)]
pub struct FileChangeEntry {
    pub message_seq: i64,
    pub change: FileChange,
}

/// Where a tool reports the changes it makes.
///
/// Kept separate from [`crate::ToolResult`]: the result is what the model
/// sees, this is what persistence sees. A tool that mutates files calls both.
pub trait ChangeRecorder: Send + Sync {
    fn record(&self, change: FileChange);
}

/// Discards every change. The default for tools used outside a session — tests,
/// a future CLI, anywhere nothing is listening.
pub struct NoRecorder;

impl ChangeRecorder for NoRecorder {
    fn record(&self, _change: FileChange) {}
}
