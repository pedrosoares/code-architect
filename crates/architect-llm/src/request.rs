//! What we ask a provider for.

use architect_core::{CachePolicy, Message, ToolSchema};
use serde::{Deserialize, Serialize};

/// A chat request, provider-neutral.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    /// System prompt. Kept out of `messages` because Anthropic takes it as a
    /// separate top-level field, and because it is the natural cache prefix.
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    /// `None` lets the provider decide. Anthropic requires a value, so its
    /// adapter substitutes a default; OpenAI-compatible endpoints omit the
    /// field entirely, which matters for local models with small output caps.
    pub max_tokens: Option<u32>,
    pub params: SamplingParams,
    pub cache: CachePolicy,
    pub reasoning: Reasoning,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            system: None,
            messages,
            tools: Vec::new(),
            max_tokens: None,
            params: SamplingParams::default(),
            cache: CachePolicy::default(),
            reasoning: Reasoning::default(),
        }
    }

    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn tools(mut self, tools: Vec<ToolSchema>) -> Self {
        self.tools = tools;
        self
    }

    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn params(mut self, params: SamplingParams) -> Self {
        self.params = params;
        self
    }

    pub fn cache(mut self, cache: CachePolicy) -> Self {
        self.cache = cache;
        self
    }

    pub fn reasoning(mut self, reasoning: Reasoning) -> Self {
        self.reasoning = reasoning;
        self
    }
}

/// Sampling knobs. Every field is optional; unset fields are not sent at all,
/// so provider defaults apply.
///
/// Note that current Claude models reject `temperature`/`top_p`/`top_k`
/// outright — the Anthropic adapter drops them rather than sending a request
/// that would 400.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SamplingParams {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
}

/// How much the model should reason before answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reasoning {
    /// Send nothing and let the provider's own default apply.
    ///
    /// The default, because a reasoning parameter aimed at one provider is
    /// frequently rejected by another — local OpenAI-compatible servers in
    /// particular.
    #[default]
    Auto,
    /// Explicitly off. Note this is not always honored: Claude Opus 5 rejects
    /// disabled thinking above `high` effort.
    Off,
    /// Adaptive thinking — the model decides how much to think.
    ///
    /// This is the only supported on-mode for current Claude models; the fixed
    /// `budget_tokens` ceiling is gone and returns a 400.
    Adaptive {
        effort: Effort,
        display: ReasoningDisplay,
    },
}

impl Reasoning {
    /// Adaptive thinking with reasoning summaries visible — what a UI that
    /// renders a "reasoning" section wants.
    pub const VISIBLE: Self = Self::Adaptive {
        effort: Effort::High,
        display: ReasoningDisplay::Summarized,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    #[default]
    High,
    XHigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Whether reasoning text comes back or is withheld.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningDisplay {
    /// A readable summary of the reasoning.
    Summarized,
    /// Reasoning happens and is billed, but the text arrives empty. This is the
    /// provider default on current Claude models.
    #[default]
    Omitted,
}

impl ReasoningDisplay {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summarized => "summarized",
            Self::Omitted => "omitted",
        }
    }
}
