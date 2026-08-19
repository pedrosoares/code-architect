//! The normalized stream.

use architect_core::{StopReason, ToolCall, Usage};
use serde::{Deserialize, Serialize};

/// One event from a streaming completion, identical in shape across providers.
///
/// Tool calls arrive in three parts because that is how both providers stream
/// them: a start with the name, then the arguments as JSON fragments, then a
/// completed call once the fragments parse. A UI can render the name
/// immediately and fill the arguments in as they arrive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        id: String,
        model: String,
    },
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolCallInputDelta {
        index: usize,
        partial_json: String,
    },
    ToolCallEnd {
        index: usize,
        call: ToolCall,
    },
    Finished {
        stop_reason: StopReason,
        usage: Usage,
    },
}
