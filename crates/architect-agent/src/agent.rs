//! The tool-call loop.

use std::sync::Arc;

use architect_core::{CachePolicy, Message, StopReason, ToolCall, ToolResult, Usage, pricing};
use architect_llm::{
    ChatRequest, LlmError, Provider, Reasoning, SamplingParams, StreamEvent, collect_forwarding,
};
use futures_util::future::join_all;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::{
    event::{AgentEvent, TurnOutcome},
    executor::ToolExecutor,
};

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error("turn cancelled")]
    Cancelled,
}

/// How the agent runs a turn.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentConfig {
    pub model: String,
    pub system: Option<String>,
    /// Requests per turn before the loop gives up. Defaults to effectively
    /// unlimited (`usize::MAX`) — a turn runs until the model stops calling
    /// tools on its own, or the user cancels it. Still overridable via
    /// [`Self::max_iterations`] for anything that wants a real ceiling
    /// (e.g. a test asserting the loop actually stops).
    pub max_iterations: usize,
    pub max_tokens: Option<u32>,
    pub cache: CachePolicy,
    pub reasoning: Reasoning,
    pub params: SamplingParams,
}

impl AgentConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: None,
            max_iterations: usize::MAX,
            max_tokens: None,
            cache: CachePolicy::default(),
            reasoning: Reasoning::default(),
            params: SamplingParams::default(),
        }
    }

    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    pub fn reasoning(mut self, reasoning: Reasoning) -> Self {
        self.reasoning = reasoning;
        self
    }
}

/// Drives a provider and a tool executor until the model is done.
pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: Arc<dyn ToolExecutor>,
    config: AgentConfig,
}

impl Agent {
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Arc<dyn ToolExecutor>,
        config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            tools,
            config,
        }
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Run one turn to completion.
    ///
    /// `history` is appended to as the turn progresses — the assistant's reply
    /// and each batch of tool results — so a cancelled or failed turn still
    /// leaves a conversation that can be resumed or inspected.
    /// `events` may be a channel of any type an [`AgentEvent`] converts into,
    /// so a host can carry its own extra variants — a failure, a cancellation —
    /// on the *same* channel. That ordering matters: a separate error channel
    /// would let a failure overtake deltas that were emitted before it.
    pub async fn run_turn<E>(
        &self,
        history: &mut Vec<Message>,
        events: &UnboundedSender<E>,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, AgentError>
    where
        E: From<AgentEvent>,
    {
        let mut usage = Usage::default();
        let mut latest_usage = Usage::default();
        let mut iterations = 0;
        let mut stop_reason = StopReason::EndTurn;
        let mut hit_iteration_limit = false;

        while iterations < self.config.max_iterations {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }

            let _ = events.send(
                AgentEvent::IterationStarted {
                    iteration: iterations,
                }
                .into(),
            );
            iterations += 1;

            let request = self.request(history);
            let stream = self.provider.stream(request, cancel.clone()).await?;
            let response = collect_forwarding(stream, self.config.model.clone(), |event| {
                let _ = events.send(AgentEvent::Stream(event.clone()).into());
            })
            .await?;

            usage += response.usage;
            // Replaced, not accumulated: each request's `usage.input_tokens`
            // is that request's whole prompt size, so the *last* one is the
            // current context size — summing them the way `usage` above
            // does would overcount by every earlier iteration's prompt.
            latest_usage = response.usage;
            stop_reason = response.stop_reason;
            history.push(response.message.clone());

            if !stop_reason.wants_continuation() {
                break;
            }

            // A paused server-side loop resumes on a bare re-send: adding a
            // "continue" message here would corrupt the resumption.
            if stop_reason == StopReason::PauseTurn {
                continue;
            }

            let calls: Vec<ToolCall> = response.message.tool_calls().cloned().collect();
            if calls.is_empty() {
                // The model asked for tools and then named none. Continuing
                // would send an empty result batch and loop forever.
                tracing::warn!("provider reported tool use with no tool calls");
                break;
            }

            let results = self.run_tools(calls, events, &cancel).await;
            history.push(Message::tool_results(results));

            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
        }

        if stop_reason.wants_continuation() {
            hit_iteration_limit = true;
            tracing::warn!(
                max_iterations = self.config.max_iterations,
                "turn stopped at the iteration limit"
            );
        }

        let outcome = TurnOutcome {
            stop_reason,
            iterations,
            usage,
            cost: pricing::cost_for(&self.config.model, &usage),
            context_tokens: latest_usage.total(),
            context_window: pricing::context_window_for(&self.config.model),
            hit_iteration_limit,
        };
        let _ = events.send(
            AgentEvent::TurnCompleted {
                outcome: outcome.clone(),
            }
            .into(),
        );

        Ok(outcome)
    }

    fn request(&self, history: &[Message]) -> ChatRequest {
        let mut request = ChatRequest::new(self.config.model.clone(), history.to_vec())
            .tools(self.tools.schemas())
            .cache(self.config.cache)
            .reasoning(self.config.reasoning)
            .params(self.config.params);

        if let Some(system) = &self.config.system {
            request = request.system(system.clone());
        }
        if let Some(max_tokens) = self.config.max_tokens {
            request = request.max_tokens(max_tokens);
        }

        request
    }

    /// Run every call of a turn concurrently.
    ///
    /// Results come back in call order regardless of which finished first, and
    /// each result is forced to carry its call's id — a mismatched or missing
    /// id leaves the model waiting on a result that never arrives, and the next
    /// request is rejected.
    async fn run_tools<E>(
        &self,
        calls: Vec<ToolCall>,
        events: &UnboundedSender<E>,
        cancel: &CancellationToken,
    ) -> Vec<ToolResult>
    where
        E: From<AgentEvent>,
    {
        let pending = calls.into_iter().map(|call| {
            let _ = events.send(AgentEvent::ToolStarted { call: call.clone() }.into());

            async move {
                let id = call.id.clone();
                let mut result = self.tools.execute(call, cancel.clone()).await;
                result.tool_use_id = id;
                result
            }
        });

        let results = join_all(pending).await;

        for result in &results {
            let _ = events.send(
                AgentEvent::ToolFinished {
                    result: result.clone(),
                }
                .into(),
            );
        }

        results
    }
}

/// Convenience for callers that only want the deltas as text.
pub fn text_of(event: &AgentEvent) -> Option<&str> {
    match event {
        AgentEvent::Stream(StreamEvent::TextDelta { text }) => Some(text),
        _ => None,
    }
}
