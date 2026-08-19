//! The vocabulary every other crate shares.
//!
//! Deliberately tiny and dependency-free: `serde` and `serde_json`, no async
//! runtime, no HTTP client, no UI. Everything upstream — the provider adapters,
//! the agent loop, the tools, the desktop app — speaks in these types, which is
//! what lets any one of them be replaced without touching the others.

pub mod cache;
pub mod change;
pub mod message;
pub mod plan;
pub mod pricing;
pub mod usage;

pub use cache::CachePolicy;
pub use change::{ChangeRecorder, FileChange, FileChangeEntry, NoRecorder};
pub use message::{
    ContentBlock, Message, Role, StopReason, ToolCall, ToolResult, ToolResultImage, ToolSchema,
};
pub use plan::{NoPlanRecorder, Plan, PlanRecorder, PlanStep, PlanSubstep, StepStatus};
pub use pricing::{Cost, Pricing};
pub use usage::Usage;
