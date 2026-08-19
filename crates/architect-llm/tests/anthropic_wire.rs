//! The Anthropic Messages wire format, pinned against a mock server.

use architect_core::{
    CachePolicy, ContentBlock, Message, Role, StopReason, ToolResult, ToolResultImage, ToolSchema,
};
use architect_llm::{
    ChatRequest, LlmError, Provider, Reasoning, SamplingParams, StreamEvent,
    providers::AnthropicProvider,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// Anthropic names every event on the SSE `event:` line as well as in the
/// payload, and ends the stream with `message_stop` rather than a sentinel.
fn sse(events: &[(&str, &str)]) -> String {
    events
        .iter()
        .map(|(name, data)| format!("event: {name}\ndata: {data}\n\n"))
        .collect()
}

async fn serve(body: String) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;

    server
}

fn provider(server: &MockServer) -> AnthropicProvider {
    AnthropicProvider::new("sk-test").base_url(server.uri())
}

async fn events(server: &MockServer, request: ChatRequest) -> Vec<Result<StreamEvent, LlmError>> {
    let stream = provider(server)
        .stream(request, CancellationToken::new())
        .await
        .expect("stream");
    stream.collect().await
}

async fn ok_events(server: &MockServer, request: ChatRequest) -> Vec<StreamEvent> {
    events(server, request)
        .await
        .into_iter()
        .map(|event| event.expect("event"))
        .collect()
}

fn request() -> ChatRequest {
    ChatRequest::new("claude-opus-5", vec![Message::user("hi")])
}

#[tokio::test]
async fn streams_thinking_text_and_a_tool_call() {
    let server = serve(sse(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_1","model":"claude-opus-5","usage":{"input_tokens":100,"cache_creation_input_tokens":20,"cache_read_input_tokens":900}}}"#,
        ),
        ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"weighing options"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Reading it."}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
        ("content_block_start", r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file"}}"#),
        // Tool input arrives as fragments of partial JSON.
        ("content_block_delta", r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\""}}"#),
        ("content_block_delta", r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":": \"a.rs\"}"}}"#),
        ("content_block_stop", r#"{"type":"content_block_stop","index":2}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]))
    .await;

    let events = ok_events(&server, request()).await;

    assert_eq!(
        events[0],
        StreamEvent::MessageStart {
            id: "msg_1".into(),
            model: "claude-opus-5".into()
        }
    );
    assert!(events.contains(&StreamEvent::ReasoningDelta {
        text: "weighing options".into()
    }));
    assert!(events.contains(&StreamEvent::TextDelta {
        text: "Reading it.".into()
    }));

    match events
        .iter()
        .find(|e| matches!(e, StreamEvent::ToolCallEnd { .. }))
    {
        Some(StreamEvent::ToolCallEnd { call, .. }) => {
            assert_eq!(call.id, "toolu_1");
            assert_eq!(call.name, "read_file");
            assert_eq!(call.input, json!({"path": "a.rs"}));
        }
        other => panic!("expected a completed tool call, got {other:?}"),
    }

    match events.last() {
        Some(StreamEvent::Finished { stop_reason, usage }) => {
            assert_eq!(*stop_reason, StopReason::ToolUse);
            assert_eq!(usage.input_tokens, 100);
            assert_eq!(usage.output_tokens, 42);
            assert_eq!(usage.cache_write_tokens, 20);
            assert_eq!(usage.cache_read_tokens, 900);
        }
        other => panic!("expected Finished, got {other:?}"),
    }
}

#[tokio::test]
async fn a_refusal_is_a_stop_reason_not_an_error() {
    // A safety decline arrives as HTTP 200. Treating it as text would render an
    // empty reply with no explanation.
    let server = serve(sse(&[
        ("message_start", r#"{"type":"message_start","message":{"id":"msg_2","model":"claude-opus-5","usage":{"input_tokens":5}}}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"refusal"},"usage":{"output_tokens":0}}"#),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]))
    .await;

    let events = ok_events(&server, request()).await;

    assert!(matches!(
        events.last(),
        Some(StreamEvent::Finished {
            stop_reason: StopReason::Refusal,
            ..
        })
    ));
}

#[tokio::test]
async fn a_pause_turn_is_reported_for_resumption() {
    let server = serve(sse(&[
        ("message_start", r#"{"type":"message_start","message":{"id":"msg_3","model":"claude-opus-5","usage":{"input_tokens":5}}}"#),
        ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"pause_turn"},"usage":{"output_tokens":1}}"#),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]))
    .await;

    let events = ok_events(&server, request()).await;

    assert!(matches!(
        events.last(),
        Some(StreamEvent::Finished {
            stop_reason: StopReason::PauseTurn,
            ..
        })
    ));
}

#[tokio::test]
async fn an_error_event_ends_the_stream_with_the_providers_message() {
    let server = serve(sse(&[
        ("message_start", r#"{"type":"message_start","message":{"id":"msg_4","model":"claude-opus-5","usage":{"input_tokens":5}}}"#),
        ("error", r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#),
    ]))
    .await;

    let events = events(&server, request()).await;

    match events.last() {
        Some(Err(LlmError::Api { provider, message })) => {
            assert_eq!(*provider, "anthropic");
            assert_eq!(message, "Overloaded");
        }
        other => panic!("expected an API error, got {other:?}"),
    }
}

#[tokio::test]
async fn builds_a_request_the_api_accepts() {
    let server = serve(sse(&[("message_stop", r#"{"type":"message_stop"}"#)])).await;

    let history = vec![
        Message::user("read them"),
        // Reasoning without a signature cannot be echoed back; with one it must be.
        Message::new(
            Role::Assistant,
            vec![
                ContentBlock::Reasoning {
                    text: "unsigned".into(),
                    signature: None,
                },
                ContentBlock::Reasoning {
                    text: "signed".into(),
                    signature: Some("sig-abc".into()),
                },
            ],
        ),
        Message::tool_results([
            ToolResult::ok("toolu_a", "contents"),
            ToolResult::error("toolu_b", "not found"),
        ]),
    ];

    let tool = |name: &str| ToolSchema {
        name: name.into(),
        description: "a tool".into(),
        input_schema: json!({"type": "object"}),
    };

    let request = ChatRequest::new("claude-opus-5", history)
        .system("be brief")
        .tools(vec![tool("read_file"), tool("list_dir")])
        .reasoning(Reasoning::VISIBLE)
        .cache(CachePolicy::default())
        // Current Claude models reject these outright.
        .params(SamplingParams {
            temperature: Some(0.7),
            top_p: Some(0.9),
            top_k: Some(40),
        });

    let _ = events(&server, request).await;

    let requests = server.received_requests().await.expect("recorded requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");

    assert_eq!(body["stream"], json!(true));
    // Required by the API; supplied even though the caller left it unset.
    assert_eq!(body["max_tokens"], json!(64_000));

    // Adaptive thinking with effort — never `budget_tokens`, which is a 400.
    assert_eq!(
        body["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert_eq!(body["output_config"]["effort"], json!("high"));
    assert!(body.get("budget_tokens").is_none());

    // Sampling knobs are dropped rather than forwarded into a rejected request.
    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert!(body.get("top_k").is_none());

    // Cache breakpoints sit on the last tool and on the system prompt, so the
    // stable prefix is cached and the volatile history is not.
    assert_eq!(
        body["tools"][1]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert!(body["tools"][0].get("cache_control").is_none());
    assert_eq!(body["system"][0]["text"], json!("be brief"));
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );

    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 3);

    // Only the signed thinking block survives the round trip.
    let assistant = messages[1]["content"].as_array().expect("content");
    assert_eq!(assistant.len(), 1);
    assert_eq!(assistant[0]["type"], json!("thinking"));
    assert_eq!(assistant[0]["signature"], json!("sig-abc"));

    // Both results ride in ONE user message — splitting them would train the
    // model out of parallel tool calls.
    assert_eq!(messages[2]["role"], json!("user"));
    let results = messages[2]["content"].as_array().expect("content");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["type"], json!("tool_result"));
    assert_eq!(results[0]["tool_use_id"], json!("toolu_a"));
    assert_eq!(results[1]["is_error"], json!(true));
}

#[tokio::test]
async fn sends_the_required_auth_headers() {
    let server = serve(sse(&[("message_stop", r#"{"type":"message_stop"}"#)])).await;

    let _ = events(&server, request()).await;

    let requests = server.received_requests().await.expect("recorded requests");
    let headers = &requests[0].headers;

    assert_eq!(headers.get("x-api-key").expect("x-api-key"), "sk-test");
    assert_eq!(
        headers.get("anthropic-version").expect("version"),
        "2023-06-01"
    );
}

#[tokio::test]
async fn an_attached_image_becomes_a_base64_image_block() {
    let server = serve(sse(&[("message_stop", r#"{"type":"message_stop"}"#)])).await;

    let history = vec![Message::user_with_images(
        "what is this?",
        [ContentBlock::image("image/png", "aGVsbG8=")],
    )];
    let request = ChatRequest::new("claude-opus-5", history);

    let _ = events(&server, request).await;

    let requests = server.received_requests().await.expect("recorded requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");

    let content = body["messages"][0]["content"].as_array().expect("content");
    assert_eq!(content[0]["type"], json!("image"));
    assert_eq!(content[0]["source"]["type"], json!("base64"));
    assert_eq!(content[0]["source"]["media_type"], json!("image/png"));
    assert_eq!(content[0]["source"]["data"], json!("aGVsbG8="));
    assert_eq!(content[1], json!({"type": "text", "text": "what is this?"}));
}

#[tokio::test]
async fn a_tool_results_image_is_nested_in_its_content_array() {
    let server = serve(sse(&[("message_stop", r#"{"type":"message_stop"}"#)])).await;

    let history = vec![Message::tool_results([ToolResult::ok_with_image(
        "toolu_a",
        "Captured a screenshot of the screen.",
        ToolResultImage {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        },
    )])];
    let request = ChatRequest::new("claude-opus-5", history);

    let _ = events(&server, request).await;

    let requests = server.received_requests().await.expect("recorded requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");

    let result = &body["messages"][0]["content"][0];
    assert_eq!(result["type"], json!("tool_result"));
    assert_eq!(result["tool_use_id"], json!("toolu_a"));

    let content = result["content"].as_array().expect("content array");
    assert_eq!(
        content[0],
        json!({"type": "text", "text": "Captured a screenshot of the screen."})
    );
    assert_eq!(content[1]["type"], json!("image"));
    assert_eq!(content[1]["source"]["type"], json!("base64"));
    assert_eq!(content[1]["source"]["media_type"], json!("image/png"));
    assert_eq!(content[1]["source"]["data"], json!("aGVsbG8="));
}
