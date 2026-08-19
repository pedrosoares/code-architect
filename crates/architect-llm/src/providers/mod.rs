//! Built-in provider adapters.

pub mod anthropic;
pub mod openai;

pub use anthropic::AnthropicProvider;
pub use openai::{OpenAiProvider, list_models};
