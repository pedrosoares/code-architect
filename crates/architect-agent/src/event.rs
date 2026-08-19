//! What the loop reports as it runs.

use architect_core::{Cost, StopReason, ToolCall, ToolResult, Usage};
use architect_llm::StreamEvent;

/// Everything a caller needs to render a turn as it happens.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// A request is about to be sent. `iteration` is 0 for the model's first
    /// reply, 1 after the first round of tool results, and so on.
    IterationStarted {
        iteration: usize,
    },
    /// A pass-through of the provider's normalized stream.
    Stream(StreamEvent),
    ToolStarted {
        call: ToolCall,
    },
    ToolFinished {
        result: ToolResult,
    },
    TurnCompleted {
        outcome: TurnOutcome,
    },
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnOutcome {
    pub stop_reason: StopReason,
    /// Requests sent during this turn.
    pub iterations: usize,
    /// Totals across every request in the turn.
    pub usage: Usage,
    /// `None` for models with no published pricing — every local model.
    pub cost: Option<Cost>,
    /// Size of the context as of the turn's *last* request — that one
    /// call's usage total (input + cache write/read + output), not summed
    /// across this turn's tool round-trips the way `usage` above is. Each
    /// request reports the size of the whole prompt it was sent, so the
    /// last one is what the next request will start from — what a "context
    /// used" figure should be computed from.
    pub context_tokens: u64,
    /// The model's context window, if known — `None` for a local/
    /// self-hosted model, same as `cost` being `None` for one. Looked up
    /// here, the only place a `TurnOutcome` is built, rather than carrying
    /// the model name any further downstream.
    pub context_window: Option<u64>,
    /// The loop stopped at `max_iterations` while the model still wanted to
    /// continue. The conversation is intact and can be resumed.
    pub hit_iteration_limit: bool,
}
