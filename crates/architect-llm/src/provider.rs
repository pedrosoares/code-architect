//! The provider abstraction.

use std::pin::Pin;

use architect_core::{ContentBlock, Message, Role, StopReason, Usage};
use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::{error::LlmError, event::StreamEvent, request::ChatRequest};

/// A stream of normalized events from one completion.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>;

/// Anything that can run a chat completion.
///
/// Streaming is the only method an implementation must provide — non-streaming
/// is derived from it, so there is exactly one code path per provider to get
/// right and to test.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable identifier for logs and configuration, e.g. `"openai"`.
    fn id(&self) -> &'static str;

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, LlmError>;

    /// Run the request and collect the whole reply.
    async fn complete(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<ChatResponse, LlmError> {
        let model = request.model.clone();
        let stream = self.stream(request, cancel).await?;
        collect(stream, model).await
    }
}

/// A complete assistant reply.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub model: String,
    pub message: Message,
    pub stop_reason: StopReason,
    pub usage: Usage,
}

/// Drain an [`EventStream`] into a [`ChatResponse`].
///
/// Blocks are assembled in the order providers expect them echoed back:
/// reasoning first, then text, then tool calls.
pub async fn collect(stream: EventStream, model: String) -> Result<ChatResponse, LlmError> {
    collect_forwarding(stream, model, |_| {}).await
}

/// [`collect`], handing every event to `on_event` on the way past.
///
/// This is what a UI-facing loop uses: it needs the assembled message *and*
/// the deltas as they arrive, without draining the stream twice or
/// reimplementing the assembly.
pub async fn collect_forwarding<F>(
    mut stream: EventStream,
    model: String,
    mut on_event: F,
) -> Result<ChatResponse, LlmError>
where
    F: FnMut(&StreamEvent),
{
    let mut reasoning = String::new();
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut stop_reason = None;
    let mut usage = Usage::default();

    while let Some(event) = stream.next().await {
        let event = event?;
        on_event(&event);

        match event {
            StreamEvent::TextDelta { text: delta } => text.push_str(&delta),
            StreamEvent::ReasoningDelta { text: delta } => reasoning.push_str(&delta),
            StreamEvent::ToolCallEnd { call, .. } => calls.push(call),
            StreamEvent::Finished {
                stop_reason: stop,
                usage: totals,
            } => {
                stop_reason = Some(stop);
                usage = totals;
            }
            StreamEvent::MessageStart { .. }
            | StreamEvent::ToolCallStart { .. }
            | StreamEvent::ToolCallInputDelta { .. } => {}
        }
    }

    let mut content = Vec::new();
    if !reasoning.is_empty() {
        content.push(ContentBlock::reasoning(reasoning));
    }
    if !text.is_empty() {
        content.push(ContentBlock::text(text));
    }
    content.extend(calls.into_iter().map(ContentBlock::ToolUse));

    let stop_reason = stop_reason.unwrap_or_else(|| {
        // A stream that ends without a terminal event was cut short. Treating it
        // as end-of-turn keeps the conversation usable, but it is worth knowing.
        tracing::warn!(model, "stream ended without a stop reason");
        StopReason::EndTurn
    });

    Ok(ChatResponse {
        model,
        message: Message::new(Role::Assistant, content),
        stop_reason,
        usage,
    })
}

/// Wrap a stream so cancellation ends it with [`LlmError::Cancelled`].
///
/// Dropping the stream would be enough to abort the HTTP request, but callers
/// need to tell "the model finished" apart from "the user hit stop".
pub(crate) fn cancellable<S>(stream: S, cancel: CancellationToken) -> EventStream
where
    S: Stream<Item = Result<StreamEvent, LlmError>> + Send + 'static,
{
    futures_util::stream::unfold(
        (stream.boxed(), cancel, false),
        |(mut stream, cancel, done)| async move {
            if done {
                return None;
            }

            tokio::select! {
                biased;
                () = cancel.cancelled() => Some((Err(LlmError::Cancelled), (stream, cancel, true))),
                item = stream.next() => item.map(|item| (item, (stream, cancel, false))),
            }
        },
    )
    .boxed()
}
