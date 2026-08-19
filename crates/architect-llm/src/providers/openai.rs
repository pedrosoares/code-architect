//! OpenAI-compatible chat completions.
//!
//! Covers OpenAI itself and the large family that copies its dialect —
//! LM Studio, DeepSeek, OpenRouter, vLLM, llama.cpp, Ollama. Only `base_url`
//! changes between them.
//!
//! The dialect's two sharp edges, both handled here:
//!
//! * streamed tool calls arrive as fragments keyed by an **array index**, and
//!   must be accumulated per index — the `id` and `name` come once, the
//!   arguments in pieces;
//! * tool results go back as **one message per result** (`role: "tool"`), the
//!   opposite of Anthropic's single batched message.

use std::collections::BTreeMap;

use architect_core::{ContentBlock, Message, Role, StopReason, ToolCall, Usage};
use async_trait::async_trait;
use futures_util::{StreamExt, stream::BoxStream};
use reqwest::header::HeaderMap;
use serde::Deserialize;
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

const PROVIDER: &str = "openai";

pub struct OpenAiProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    headers: HeaderMap,
}

impl OpenAiProvider {
    /// `base_url` includes the version segment, e.g. `https://api.openai.com/v1`
    /// or `http://localhost:1234/v1`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: None,
            headers: HeaderMap::new(),
        }
    }

    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    fn body(&self, request: &ChatRequest) -> Value {
        let mut body = json!({
            "model": request.model,
            "messages": serialize_messages(&request.system, &request.messages),
            "stream": true,
            // Without this, usage never arrives on a streamed response.
            "stream_options": {"include_usage": true},
        });

        let map = body.as_object_mut().expect("object");

        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema,
                        },
                    })
                })
                .collect();
            map.insert("tools".into(), Value::Array(tools));
        }

        // `max_tokens` rather than `max_completion_tokens`: the newer name is
        // not understood by much of the compatible ecosystem, and omitting the
        // field entirely lets small local models use their own cap.
        if let Some(max_tokens) = request.max_tokens {
            map.insert("max_tokens".into(), json!(max_tokens));
        }
        if let Some(temperature) = request.params.temperature {
            map.insert("temperature".into(), json!(temperature));
        }
        if let Some(top_p) = request.params.top_p {
            map.insert("top_p".into(), json!(top_p));
        }
        if let Some(top_k) = request.params.top_k {
            map.insert("top_k".into(), json!(top_k));
        }
        if let Reasoning::Adaptive { effort, .. } = request.reasoning {
            map.insert("reasoning_effort".into(), json!(effort.as_str()));
        }

        body
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> &'static str {
        PROVIDER
    }

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, LlmError> {
        let mut builder = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .headers(self.headers.clone())
            .json(&self.body(&request));

        if let Some(api_key) = &self.api_key {
            builder = builder.bearer_auth(api_key);
        }

        let response = send_retrying(PROVIDER, builder, MAX_RETRIES).await?;
        let events = decode(sse::decode(response.bytes_stream()).boxed(), request.model);

        Ok(cancellable(events, cancel))
    }
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// What an OpenAI-compatible server (LM Studio, vLLM, Ollama, ...) reports
/// as currently loaded/available, via `GET {base_url}/models` — the same
/// dialect `pricing_for`'s doc comment already says needs no new code per
/// provider, just a different `base_url`.
///
/// A bare function rather than a `Provider`/`OpenAiProvider` method: the
/// only caller (the desktop app's settings form) has a candidate
/// `base_url`/`api_key` typed into a form, not a constructed provider —
/// this runs *before* a `Profile` is even saved, let alone activated.
pub async fn list_models(base_url: &str, api_key: Option<&str>) -> Result<Vec<String>, LlmError> {
    let client = reqwest::Client::new();
    let mut builder = client.get(format!("{}/models", base_url.trim_end_matches('/')));
    if let Some(api_key) = api_key {
        builder = builder.bearer_auth(api_key);
    }

    let response = send_retrying(PROVIDER, builder, MAX_RETRIES).await?;
    let body = response.text().await.map_err(LlmError::Transport)?;
    let parsed: ModelsResponse = serde_json::from_str(&body)
        .map_err(|error| LlmError::decode("models list", error, &body))?;

    Ok(parsed.data.into_iter().map(|entry| entry.id).collect())
}

/// Flatten provider-neutral messages into the OpenAI wire shape.
///
/// One input message can become several: a turn's tool results are one message
/// each here, where the neutral form keeps them together.
fn serialize_messages(system: &Option<String>, messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len() + 1);

    if let Some(system) = system {
        out.push(json!({"role": "system", "content": system}));
    }

    for message in messages {
        let results: Vec<&architect_core::ToolResult> = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult(result) => Some(result),
                _ => None,
            })
            .collect();

        for result in &results {
            out.push(json!({
                "role": "tool",
                "tool_call_id": result.tool_use_id,
                // The dialect has no error flag, so it is stated in the text —
                // otherwise a failure reads as a successful result.
                "content": if result.is_error {
                    format!("ERROR: {}", result.content)
                } else {
                    result.content.clone()
                },
            }));
        }

        let text = message.text();
        // This dialect has no way to put an image inside a `tool`-role
        // message at all, so a tool result's image (`view_image`,
        // `screenshot`) rides along here instead, in whatever trailing
        // image message an attached `ContentBlock::Image` would already
        // produce below — the model still sees it, just not attributed to
        // a specific tool_call_id (unlike Anthropic, this dialect has no
        // way to express that attribution either).
        let images: Vec<(&str, &str)> = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Image { media_type, data } => {
                    Some((media_type.as_str(), data.as_str()))
                }
                _ => None,
            })
            .chain(results.iter().filter_map(|result| {
                result
                    .image
                    .as_ref()
                    .map(|image| (image.media_type.as_str(), image.data.as_str()))
            }))
            .collect();
        let calls: Vec<Value> = message
            .tool_calls()
            .map(|call| {
                json!({
                    "id": call.id,
                    "type": "function",
                    "function": {"name": call.name, "arguments": call.input.to_string()},
                })
            })
            .collect();

        if text.is_empty() && calls.is_empty() && images.is_empty() {
            continue;
        }

        let role = match message.role {
            Role::System => "system",
            Role::Assistant => "assistant",
            // A tool-only message was already emitted above.
            Role::User | Role::Tool => "user",
        };

        let mut entry = json!({"role": role});
        let map = entry.as_object_mut().expect("object");
        // Assistant turns that only call tools legitimately have null content.
        // A message with an attached image switches `content` to this
        // dialect's array-of-parts shape instead of a plain string — every
        // other message keeps the plain string it already had.
        map.insert(
            "content".into(),
            if images.is_empty() {
                if text.is_empty() {
                    Value::Null
                } else {
                    json!(text)
                }
            } else {
                let mut parts: Vec<Value> = images
                    .iter()
                    .map(|(media_type, data)| {
                        json!({
                            "type": "image_url",
                            "image_url": {"url": format!("data:{media_type};base64,{data}")},
                        })
                    })
                    .collect();
                if !text.is_empty() {
                    parts.push(json!({"type": "text", "text": text}));
                }
                Value::Array(parts)
            },
        );
        if !calls.is_empty() {
            map.insert("tool_calls".into(), Value::Array(calls));
        }
        // Reasoning is deliberately not echoed back: it is an output-only field
        // in this dialect, and sending it back is rejected by several servers.
        out.push(entry);
    }

    out
}

/// Accumulates the fragments of a streamed response.
#[derive(Default)]
struct Accumulator {
    calls: BTreeMap<usize, PartialCall>,
    stop_reason: Option<StopReason>,
    usage: Usage,
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
    announced: bool,
}

impl Accumulator {
    fn chunk(&mut self, chunk: &Value, out: &mut Vec<StreamEvent>) {
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            self.usage = parse_usage(usage);
        }

        let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else {
            // The final usage-only chunk carries no choices.
            return;
        };

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(parse_stop_reason(reason));
        }

        let Some(delta) = choice.get("delta") else {
            return;
        };

        if let Some(text) = delta.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            out.push(StreamEvent::TextDelta {
                text: text.to_owned(),
            });
        }

        // `reasoning_content` is DeepSeek's field name; `reasoning` is used by
        // some gateways for the same thing.
        let reasoning = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str);
        if let Some(text) = reasoning
            && !text.is_empty()
        {
            out.push(StreamEvent::ReasoningDelta {
                text: text.to_owned(),
            });
        }

        for fragment in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.tool_fragment(fragment, out);
        }
    }

    fn tool_fragment(&mut self, fragment: &Value, out: &mut Vec<StreamEvent>) {
        // Index is what ties fragments together; without it a parallel call
        // would interleave into the wrong buffer.
        let index = fragment.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let call = self.calls.entry(index).or_default();

        if let Some(id) = fragment.get("id").and_then(Value::as_str) {
            call.id = id.to_owned();
        }
        if let Some(name) = fragment
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
        {
            call.name.push_str(name);
        }

        if !call.announced && !call.name.is_empty() {
            call.announced = true;
            out.push(StreamEvent::ToolCallStart {
                index,
                id: call.id.clone(),
                name: call.name.clone(),
            });
        }

        if let Some(arguments) = fragment
            .get("function")
            .and_then(|f| f.get("arguments"))
            .and_then(Value::as_str)
            && !arguments.is_empty()
        {
            call.arguments.push_str(arguments);
            out.push(StreamEvent::ToolCallInputDelta {
                index,
                partial_json: arguments.to_owned(),
            });
        }
    }

    /// Emit the completed calls and the terminal event.
    fn finish(&mut self, out: &mut Vec<StreamEvent>) {
        for (index, call) in std::mem::take(&mut self.calls) {
            out.push(StreamEvent::ToolCallEnd {
                index,
                call: ToolCall {
                    id: call.id,
                    name: call.name,
                    input: parse_arguments(&call.arguments),
                },
            });
        }

        out.push(StreamEvent::Finished {
            stop_reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
            usage: self.usage,
        });
    }
}

/// Parse accumulated tool arguments.
///
/// Malformed JSON is passed through as a string rather than failing the turn:
/// the executor then returns an error result the model can recover from, which
/// is far better than aborting a long agent run over one bad call.
fn parse_arguments(arguments: &str) -> Value {
    let arguments = arguments.trim();
    if arguments.is_empty() {
        return json!({});
    }

    serde_json::from_str(arguments).unwrap_or_else(|error| {
        tracing::warn!(%error, arguments, "tool arguments were not valid JSON");
        Value::String(arguments.to_owned())
    })
}

fn parse_stop_reason(reason: &str) -> StopReason {
    match reason {
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        "content_filter" => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

fn parse_usage(usage: &Value) -> Usage {
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    let cached = usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Usage {
        // Cached tokens are reported inside the prompt total, so they are
        // subtracted out to keep the two from being counted twice.
        input_tokens: field("prompt_tokens").saturating_sub(cached),
        output_tokens: field("completion_tokens"),
        cache_write_tokens: 0,
        cache_read_tokens: cached,
    }
}

fn decode(
    stream: BoxStream<'static, Result<sse::SseEvent, LlmError>>,
    model: String,
) -> impl futures_util::Stream<Item = Result<StreamEvent, LlmError>> + Send {
    let state = (
        stream,
        Accumulator::default(),
        Vec::new(),
        Some(model),
        false,
    );

    futures_util::stream::unfold(
        state,
        |(mut stream, mut acc, mut queue, mut model, mut done)| async move {
            loop {
                if !queue.is_empty() {
                    let event = queue.remove(0);
                    return Some((Ok(event), (stream, acc, queue, model, done)));
                }
                if done {
                    return None;
                }

                match stream.next().await {
                    Some(Ok(sse::SseEvent { data, .. })) => {
                        if data.trim() == "[DONE]" {
                            done = true;
                            acc.finish(&mut queue);
                            continue;
                        }

                        let chunk: Value = match serde_json::from_str(&data) {
                            Ok(chunk) => chunk,
                            Err(error) => {
                                done = true;
                                let error =
                                    LlmError::decode("an OpenAI stream chunk", error, &data);
                                return Some((Err(error), (stream, acc, queue, model, done)));
                            }
                        };

                        if let Some(message) = chunk
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(Value::as_str)
                        {
                            done = true;
                            let error = LlmError::Api {
                                provider: PROVIDER,
                                message: message.to_owned(),
                            };
                            return Some((Err(error), (stream, acc, queue, model, done)));
                        }

                        if let Some(model) = model.take() {
                            let id = chunk
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                            queue.push(StreamEvent::MessageStart { id, model });
                        }

                        acc.chunk(&chunk, &mut queue);
                    }
                    Some(Err(error)) => {
                        done = true;
                        return Some((Err(error), (stream, acc, queue, model, done)));
                    }
                    None => {
                        // Stream closed without `[DONE]` — still emit what we have.
                        done = true;
                        acc.finish(&mut queue);
                    }
                }
            }
        },
    )
}
