//! Real tools for the agent: files, search, and a shell.
//!
//! [`Tool`] is the interface; [`ToolRegistry`] composes any number of them into
//! one [`architect_agent::ToolExecutor`]. Nothing here depends on where a turn
//! or a session is stored — that's `architect-session`'s job, bridged only by
//! `architect_core::ChangeRecorder`.

mod blocklist;
mod context;
mod process;
mod registry;
mod sub_agent;
mod tool;
mod tools;

pub use context::{ChannelPlanRecorder, ChannelRecorder, ToolContext};
pub use process::{ProcessEvent, ProcessRegistry, ProcessStatus};
pub use registry::ToolRegistry;
pub use sub_agent::{NoSubAgentSpawner, SubAgentSpawner};
pub use tool::{Tool, ToolOutput};
pub use tools::plan::{ReadPlan, WritePlan};
pub use tools::process::{GetProcessLogs, StartProcess, StopProcess};
pub use tools::spawn_subagents::SpawnSubAgents;
