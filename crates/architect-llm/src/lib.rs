//! Provider-agnostic LLM access.
//!
//! One [`Provider`] trait, one normalized [`StreamEvent`] stream, and adapters
//! that hide how differently the providers actually behave. Nothing here knows
//! what a tool does or when to call one — that is [`architect-agent`]'s job.
//!
//! Adding a provider means implementing [`Provider`] and registering a
//! [`ProviderFactory`]. Anything speaking the OpenAI chat-completions dialect —
//! LM Studio, DeepSeek, OpenRouter, vLLM, Ollama — needs no new code at all,
//! only a different `base_url`.
//!
//! [`architect-agent`]: https://docs.rs/architect-agent

pub mod error;
pub mod event;
pub(crate) mod http;
pub mod provider;
pub mod providers;
pub mod registry;
pub mod request;
pub mod sse;

pub use error::LlmError;
pub use event::StreamEvent;
pub use provider::{ChatResponse, EventStream, Provider, collect, collect_forwarding};
pub use providers::list_models;
pub use registry::{ProviderConfig, ProviderFactory, ProviderRegistry, is_local};
pub use request::{ChatRequest, Effort, Reasoning, ReasoningDisplay, SamplingParams};
