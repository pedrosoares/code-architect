//! The tool seam.

use architect_core::{ToolCall, ToolResult, ToolSchema};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// Runs the tools the model asks for.
///
/// The loop talks to tools only through this trait, which is why
/// `architect-agent` does not depend on any tool implementation. The real
/// registry, the test doubles, and a permission-gating wrapper are all just
/// implementations of it.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// What to advertise to the model. Kept stable across a session: the tool
    /// list is part of the cached prompt prefix, so churn here costs cache hits.
    fn schemas(&self) -> Vec<ToolSchema>;

    /// Run one call.
    ///
    /// Returns a [`ToolResult`] rather than a `Result` on purpose — a failure
    /// is something the model should see and recover from, not something that
    /// aborts the turn. Implementations should catch their own errors and
    /// return [`ToolResult::error`].
    async fn execute(&self, call: ToolCall, cancel: CancellationToken) -> ToolResult;
}

/// An executor offering nothing, for plain chat.
pub struct NoTools;

#[async_trait]
impl ToolExecutor for NoTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        Vec::new()
    }

    async fn execute(&self, call: ToolCall, _cancel: CancellationToken) -> ToolResult {
        ToolResult::error(
            call.id,
            format!("no tool named {:?} is available", call.name),
        )
    }
}
