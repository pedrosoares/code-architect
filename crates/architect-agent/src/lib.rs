//! The agent loop: model, tools, repeat.
//!
//! Given a [`Provider`] and a [`ToolExecutor`], [`Agent::run_turn`] sends the
//! conversation, streams the reply, runs whatever tools the model asks for, and
//! goes around again until it stops asking. Progress is reported as
//! [`AgentEvent`]s on an unbounded channel, so a slow consumer — a UI — can
//! never stall the model stream.
//!
//! Nothing here knows what a tool does or where the conversation is stored.
//!
//! [`Provider`]: architect_llm::Provider

pub mod agent;
pub mod event;
pub mod executor;

pub use agent::{Agent, AgentConfig, AgentError};
pub use event::{AgentEvent, TurnOutcome};
pub use executor::{NoTools, ToolExecutor};
