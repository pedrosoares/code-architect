//! The Anthropic Messages API.
//!
//! Differs from the OpenAI dialect in ways that matter to a tool-calling loop:
//!
//! * a turn's tool results go back in **one** user message. Splitting them
//!   across messages teaches the model to stop calling tools in parallel.
//! * the stream is typed lifecycle events, and tool arguments arrive as
//!   `input_json_delta` fragments of partial JSON.
//! * thinking blocks are signed and must be echoed back unchanged to continue a
//!   reasoning conversation.
//! * `refusal` is a *stop reason* on an HTTP 200, not an error.
//! * current models reject `temperature`/`top_p`/`top_k` and `budget_tokens`
//!   outright, so those are dropped rather than forwarded.

use std::collections::BTreeMap;

use architect_core::{ContentBlock, Message, Role, StopReason, ToolCall, Usage};
use async_trait::async_trait;
use futures_util::{StreamExt, stream::BoxStream};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    error::LlmError,
    event::StreamEvent,
    http::{MAX_RETRIES, send_retrying},
    provider::{EventStream, Provider, cancellable},
    request::{ChatRequest, Reasoning},
    sse,
};

const PROVIDER: &str = "anthropic";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// Required by the API. Streaming makes a large value safe.
const DEFAULT_MAX_TOKENS: u32 = 64_000;

pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    headers: HeaderMap,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            api_key: api_key.into(),
            headers: HeaderMap::new(),
        }
    }

    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_owned();
        self
    }

    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    fn body(&self, request: &ChatRequest) -> Value {
        let (system, messages) = split_system(&request.system, &request.messages);

        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": messages,
            "stream": true,
        });
        let map = body.as_object_mut().expect("object");

        // Order matters for prompt caching: tools, then system, then messages.
        // A cache breakpoint only matches if everything before it is unchanged.
        if !request.tools.is_empty() {
            let last = request.tools.len() - 1;
            let tools: Vec<Value> = request
                .tools
                .iter()
                .enumerate()
                .map(|(index, tool)| {
                    let mut value = json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.input_schema,
                    });
                    if request.cache.tools && index == last {
                        with_cache_control(&mut value);
                    }
                    value
                })
                .collect();
            map.insert("tools".into(), Value::Array(tools));
        }

        if !system.is_empty() {
            let mut block = json!({"type": "text", "text": system});
            if request.cache.system {
                with_cache_control(&mut block);
            }
            map.insert("system".into(), json!([block]));
        }

        match request.reasoning {
            // `budget_tokens` is gone on current models — sending it is a 400.
            Reasoning::Adaptive { effort, display } => {
                map.insert(
                    "thinking".into(),
                    json!({"type": "adaptive", "display": display.as_str()}),
                );
                map.insert("output_config".into(), json!({"effort": effort.as_str()}));
            }
            Reasoning::Off => {
                map.insert("thinking".into(), json!({"type": "disabled"}));
            }
            // Omitted entirely: current models run adaptive thinking by default.
            Reasoning::Auto => {}
        }

        body
    }
}

/// Attach a cache breakpoint to a request element.
fn with_cache_control(value: &mut Value) {
    if let Some(map) = value.as_object_mut() {
        map.insert("cache_control".into(), json!({"type": "ephemeral"}));
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &'static str {
        PROVIDER
    }

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, LlmError> {
        debug_assert!(
            request.cache.breakpoints() <= architect_core::cache::MAX_BREAKPOINTS,
            "at most {} cache breakpoints are allowed",
            architect_core::cache::MAX_BREAKPOINTS
        );

        let api_key = HeaderValue::from_str(&self.api_key)
            .map_err(|_| LlmError::Config("API key is not a valid header value".into()))?;

        let builder = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .headers(self.headers.clone())
            .header(HeaderName::from_static("x-api-key"), api_key)
            .header(HeaderName::from_static("anthropic-version"), API_VERSION)
            .json(&self.body(&request));

        let response = send_retrying(PROVIDER, builder, MAX_RETRIES).await?;
        let events = decode(sse::decode(response.bytes_stream()).boxed(), request.model);

        Ok(cancellable(events, cancel))
    }
}

/// Split system prompts out of the history — Anthropic takes them top-level —
/// and serialize the rest.
fn split_system(system: &Option<String>, messages: &[Message]) -> (String, Vec<Value>) {
    let mut system_parts: Vec<String> = system.iter().cloned().collect();
    let mut out = Vec::with_capacity(messages.len());

    for message in messages {
        if message.role == Role::System {
            system_parts.push(message.text());
            continue;
        }

        let content = serialize_content(message);
        if content.is_empty() {
            continue;
        }

        let role = match message.role {
            Role::Assistant => "assistant",
            // Tool results are user turns in this API.
            _ => "user",
        };
        out.push(json!({"role": role, "content": content}));
    }

    (system_parts.join("\n\n"), out)
}

fn serialize_content(message: &Message) -> Vec<Value> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } if text.is_empty() => None,
            ContentBlock::Text { text } => Some(json!({"type": "text", "text": text})),
            // A thinking block without its signature is rejected. That happens
            // when the reasoning came from a different provider, so it is
            // dropped rather than sent and 400'd.
            ContentBlock::Reasoning { text, signature } => signature.as_ref().map(
                |signature| json!({"type": "thinking", "thinking": text, "signature": signature}),
            ),
            ContentBlock::ToolUse(call) => Some(json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.name,
                "input": call.input,
            })),
            ContentBlock::ToolResult(result) => {
                // A tool that also produced an image (`view_image`,
                // `screenshot`) nests it inside this specific result's own
                // content array — the only unambiguous place for it when
                // several tool calls run in parallel in one turn and only
                // one of them returns an image. Anthropic's own
                // computer-use tool returns screenshots the same way.
                let content = match &result.image {
                    Some(image) => json!([
                        {"type": "text", "text": result.content},
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": image.media_type,
                                "data": image.data,
                            },
                        },
                    ]),
                    None => json!(result.content),
                };
                Some(json!({
                    "type": "tool_result",
                    "tool_use_id": result.tool_use_id,
                    "content": content,
                    "is_error": result.is_error,
                }))
            }
            ContentBlock::Image { media_type, data } => Some(json!({
                "type": "image",
                "source": {"type": "base64", "media_type": media_type, "data": data},
            })),
        })
        .collect()
}

/// Tracks the open content blocks of a streamed message.
#[derive(Default)]
struct Accumulator {
    tools: BTreeMap<usize, PartialCall>,
    stop_reason: Option<StopReason>,
    usage: Usage,
}

struct PartialCall {
    id: String,
    name: String,
    input: String,
}

impl Accumulator {
    fn event(&mut self, kind: &str, data: &Value, out: &mut Vec<StreamEvent>) {
        match kind {
            "message_start" => {
                let message = data.get("message");
                let id = message
                    .and_then(|m| m.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let model = message
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if let Some(usage) = message.and_then(|m| m.get("usage")) {
                    self.usage = parse_usage(usage);
                }
                out.push(StreamEvent::MessageStart { id, model });
            }
            "content_block_start" => {
                let index = block_index(data);
                let block = data.get("content_block");
                if block.and_then(|b| b.get("type")).and_then(Value::as_str) == Some("tool_use") {
                    let id = block
                        .and_then(|b| b.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let name = block
                        .and_then(|b| b.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    out.push(StreamEvent::ToolCallStart {
                        index,
                        id: id.clone(),
                        name: name.clone(),
                    });
                    self.tools.insert(
                        index,
                        PartialCall {
                            id,
                            name,
                            input: String::new(),
                        },
                    );
                }
            }
            "content_block_delta" => {
                let index = block_index(data);
                let Some(delta) = data.get("delta") else {
                    return;
                };

                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            out.push(StreamEvent::TextDelta {
                                text: text.to_owned(),
                            });
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                            out.push(StreamEvent::ReasoningDelta {
                                text: text.to_owned(),
                            });
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                            if let Some(call) = self.tools.get_mut(&index) {
                                call.input.push_str(partial);
                            }
                            out.push(StreamEvent::ToolCallInputDelta {
                                index,
                                partial_json: partial.to_owned(),
                            });
                        }
                    }
                    // `signature_delta` carries the thinking signature. It is
                    // not surfaced as an event; only a full round-trip of the
                    // block needs it, which `complete()` does not reconstruct.
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = block_index(data);
                if let Some(call) = self.tools.remove(&index) {
                    out.push(StreamEvent::ToolCallEnd {
                        index,
                        call: ToolCall {
                            id: call.id,
                            name: call.name,
                            input: parse_input(&call.input),
                        },
                    });
                }
            }
            "message_delta" => {
                if let Some(reason) = data
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(parse_stop_reason(reason));
                }
                if let Some(output) = data
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_u64)
                {
                    self.usage.output_tokens = output;
                }
            }
            "message_stop" => out.push(StreamEvent::Finished {
                stop_reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
                usage: self.usage,
            }),
            // `ping` and anything added later.
            _ => {}
        }
    }
}

fn block_index(data: &Value) -> usize {
    data.get("index").and_then(Value::as_u64).unwrap_or(0) as usize
}

/// Assemble streamed `partial_json` fragments.
///
/// As with the OpenAI adapter, unparseable arguments become a string rather
/// than failing the turn, so the loop can return an error result and continue.
fn parse_input(input: &str) -> Value {
    let input = input.trim();
    if input.is_empty() {
        return json!({});
    }

    serde_json::from_str(input).unwrap_or_else(|error| {
        tracing::warn!(%error, input, "tool input was not valid JSON");
        Value::String(input.to_owned())
    })
}

fn parse_stop_reason(reason: &str) -> StopReason {
    match reason {
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "refusal" => StopReason::Refusal,
        "pause_turn" => StopReason::PauseTurn,
        _ => StopReason::EndTurn,
    }
}

fn parse_usage(usage: &Value) -> Usage {
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);

    Usage {
        input_tokens: field("input_tokens"),
        output_tokens: field("output_tokens"),
        cache_write_tokens: field("cache_creation_input_tokens"),
        cache_read_tokens: field("cache_read_input_tokens"),
    }
}

fn decode(
    stream: BoxStream<'static, Result<sse::SseEvent, LlmError>>,
    model: String,
) -> impl futures_util::Stream<Item = Result<StreamEvent, LlmError>> + Send {
    let _ = model;
    let state = (stream, Accumulator::default(), Vec::new(), false);

    futures_util::stream::unfold(
        state,
        |(mut stream, mut acc, mut queue, mut done)| async move {
            loop {
                if !queue.is_empty() {
                    let event = queue.remove(0);
                    return Some((Ok(event), (stream, acc, queue, done)));
                }
                if done {
                    return None;
                }

                match stream.next().await {
                    Some(Ok(event)) => {
                        let data: Value = match serde_json::from_str(&event.data) {
                            Ok(data) => data,
                            Err(error) => {
                                done = true;
                                let error = LlmError::decode(
                                    "an Anthropic stream event",
                                    error,
                                    &event.data,
                                );
                                return Some((Err(error), (stream, acc, queue, done)));
                            }
                        };

                        // The type is on the payload as well as the SSE `event:`
                        // line; prefer the payload, which is what the docs specify.
                        let kind = data
                            .get("type")
                            .and_then(Value::as_str)
                            .or(event.event.as_deref())
                            .unwrap_or_default()
                            .to_owned();

                        if kind == "error" {
                            done = true;
                            let message = data
                                .get("error")
                                .and_then(|e| e.get("message"))
                                .and_then(Value::as_str)
                                .unwrap_or("unknown error")
                                .to_owned();
                            let error = LlmError::Api {
                                provider: PROVIDER,
                                message,
                            };
                            return Some((Err(error), (stream, acc, queue, done)));
                        }

                        if kind == "message_stop" {
                            done = true;
                        }
                        acc.event(&kind, &data, &mut queue);
                    }
                    Some(Err(error)) => {
                        done = true;
                        return Some((Err(error), (stream, acc, queue, done)));
                    }
                    None => done = true,
                }
            }
        },
    )
}
