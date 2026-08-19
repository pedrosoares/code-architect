//! Where a tool reaches out to spawn a sub-agent session.
//!
//! Same shape as [`architect_core::ChangeRecorder`]/[`architect_core::
//! PlanRecorder`] — a trait a tool's [`crate::ToolContext`] holds, whose
//! real implementation lives in `apps/desktop` (the only thing that knows
//! how to start a session), so `architect-tools` never depends on it. Kept
//! here rather than alongside those two in `architect-core` because this
//! trait is unavoidably `async` (spawning a sub-agent means starting a real
//! turn and waiting for it to finish) — `architect-core` is deliberately
//! kept free of any async-runtime dependency, `architect-tools` already
//! isn't (see [`crate::Tool::call`]).

use async_trait::async_trait;

/// Starts sub-agent sessions and waits for their final answers, on behalf
/// of the `spawn_subagents` tool.
#[async_trait]
pub trait SubAgentSpawner: Send + Sync {
    /// True when spawned sub-agents must run one at a time rather than
    /// concurrently — the active model is a local inference server (LM
    /// Studio, Ollama, vLLM, ...) that generally can't usefully serve
    /// overlapping requests. False for a real hosted API.
    fn run_sequentially(&self) -> bool;

    /// Start a new child session seeded with `prompt`, its tools scoped to
    /// `path`, and wait for it to finish. Returns its final answer text.
    async fn spawn(&self, prompt: String, path: String) -> Result<String, String>;
}

/// The default for a [`crate::ToolContext`] nothing has wired a real spawner
/// into — tests, or a turn run outside the desktop engine entirely. Matches
/// `architect_core::NoRecorder`/`NoPlanRecorder`'s shape: a safe no-op
/// rather than a panic, since most tool calls never touch this at all.
pub struct NoSubAgentSpawner;

#[async_trait]
impl SubAgentSpawner for NoSubAgentSpawner {
    fn run_sequentially(&self) -> bool {
        true
    }

    async fn spawn(&self, _prompt: String, _path: String) -> Result<String, String> {
        Err("sub-agents are not available in this context".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_sub_agent_spawner_reports_a_clear_error() {
        let error = NoSubAgentSpawner
            .spawn("investigate".to_owned(), ".".to_owned())
            .await
            .unwrap_err();

        assert!(error.contains("not available"), "got: {error}");
    }
}
