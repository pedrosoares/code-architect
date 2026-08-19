//! The OpenAI-compatible wire format, pinned against a mock server.

use architect_core::{ContentBlock, Message, StopReason, ToolResult, ToolResultImage, ToolSchema};
use architect_llm::{
    ChatRequest, LlmError, Provider, StreamEvent, list_models, providers::OpenAiProvider,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// Build an SSE body from raw `data:` payloads, terminated the way the dialect
/// terminates: an explicit `[DONE]`.
fn sse(chunks: &[&str]) -> String {
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    body
}

async fn serve(body: String) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;

    server
}

async fn events(server: &MockServer, request: ChatRequest) -> Vec<StreamEvent> {
    let provider = OpenAiProvider::new(format!("{}/v1", server.uri()));
    let stream = provider
        .stream(request, CancellationToken::new())
        .await
        .expect("stream");

    stream.map(|event| event.expect("event")).collect().await
}

fn request() -> ChatRequest {
    ChatRequest::new("test-model", vec![Message::user("hi")])
}

#[tokio::test]
async fn streams_text_and_reports_usage() {
    let server = serve(sse(&[
        r#"{"id":"chatcmpl-1","choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
        r#"{"id":"chatcmpl-1","choices":[{"delta":{"content":"lo"}}]}"#,
        r#"{"id":"chatcmpl-1","choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        // Usage arrives in its own trailing chunk with no choices.
        r#"{"id":"chatcmpl-1","choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3}}"#,
    ]))
    .await;

    let events = events(&server, request()).await;

    assert_eq!(
        events[0],
        StreamEvent::MessageStart {
            id: "chatcmpl-1".into(),
            model: "test-model".into()
        }
    );
    assert_eq!(events[1], StreamEvent::TextDelta { text: "Hel".into() });
    assert_eq!(events[2], StreamEvent::TextDelta { text: "lo".into() });

    match events.last().expect("a terminal event") {
        StreamEvent::Finished { stop_reason, usage } => {
            assert_eq!(*stop_reason, StopReason::EndTurn);
            assert_eq!(usage.input_tokens, 12);
            assert_eq!(usage.output_tokens, 3);
        }
        other => panic!("expected Finished, got {other:?}"),
    }
}

#[tokio::test]
async fn accumulates_parallel_tool_calls_by_index() {
    // The name arrives once, the arguments in fragments, and the two calls
    // interleave — which is exactly why they are keyed by index.
    let server = serve(sse(&[
        r#"{"id":"c","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":""}}]}}]}"#,
        r#"{"id":"c","choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"list_dir","arguments":""}}]}}]}"#,
        r#"{"id":"c","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
        r#"{"id":"c","choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"path\":\".\"}"}}]}}]}"#,
        r#"{"id":"c","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.rs\"}"}}]}}]}"#,
        r#"{"id":"c","choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ]))
    .await;

    let events = events(&server, request()).await;

    let starts: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolCallStart { index, name, .. } => Some((*index, name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(starts, [(0, "read_file"), (1, "list_dir")]);

    let calls: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolCallEnd { call, .. } => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "call_a");
    assert_eq!(calls[0].input, json!({"path": "a.rs"}));
    assert_eq!(calls[1].input, json!({"path": "."}));

    assert!(matches!(
        events.last(),
        Some(StreamEvent::Finished {
            stop_reason: StopReason::ToolUse,
            ..
        })
    ));
}

#[tokio::test]
async fn surfaces_reasoning_content() {
    let server = serve(sse(&[
        r#"{"id":"c","choices":[{"delta":{"reasoning_content":"let me think"}}]}"#,
        r#"{"id":"c","choices":[{"delta":{"content":"answer"},"finish_reason":"stop"}]}"#,
    ]))
    .await;

    let events = events(&server, request()).await;

    assert!(events.contains(&StreamEvent::ReasoningDelta {
        text: "let me think".into()
    }));
    assert!(events.contains(&StreamEvent::TextDelta {
        text: "answer".into()
    }));
}

#[tokio::test]
async fn malformed_tool_arguments_do_not_kill_the_turn() {
    let server = serve(sse(&[
        r#"{"id":"c","choices":[{"delta":{"tool_calls":[{"index":0,"id":"x","function":{"name":"f","arguments":"{not json"}}]}}]}"#,
        r#"{"id":"c","choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ]))
    .await;

    let events = events(&server, request()).await;

    // The call still comes through so the loop can answer with an error result;
    // a non-object input is the signal that the model produced junk.
    match events
        .iter()
        .find(|e| matches!(e, StreamEvent::ToolCallEnd { .. }))
    {
        Some(StreamEvent::ToolCallEnd { call, .. }) => {
            assert_eq!(call.input, Value::String("{not json".into()));
        }
        other => panic!("expected a completed tool call, got {other:?}"),
    }
}

#[tokio::test]
async fn subtracts_cached_tokens_from_the_prompt_total() {
    let server = serve(sse(&[
        r#"{"id":"c","choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}]}"#,
        r#"{"id":"c","choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":10,"prompt_tokens_details":{"cached_tokens":800}}}"#,
    ]))
    .await;

    let events = events(&server, request()).await;

    match events.last() {
        Some(StreamEvent::Finished { usage, .. }) => {
            // Cached tokens are reported inside prompt_tokens; counting both
            // would double-bill the prompt.
            assert_eq!(usage.input_tokens, 200);
            assert_eq!(usage.cache_read_tokens, 800);
        }
        other => panic!("expected Finished, got {other:?}"),
    }
}

#[tokio::test]
async fn sends_tools_and_one_message_per_tool_result() {
    let server = serve(sse(&[
        r#"{"id":"c","choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
    ]))
    .await;

    let history = vec![
        Message::user("read it"),
        Message::tool_results([
            ToolResult::ok("call_a", "file contents"),
            ToolResult::error("call_b", "not found"),
        ]),
    ];
    let request = ChatRequest::new("test-model", history)
        .system("be brief")
        .tools(vec![ToolSchema {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object"}),
        }]);

    events(&server, request).await;

    let requests = server.received_requests().await.expect("recorded requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");

    assert_eq!(body["stream"], json!(true));
    // Without this, a streamed response never reports usage.
    assert_eq!(body["stream_options"]["include_usage"], json!(true));
    assert_eq!(body["tools"][0]["type"], json!("function"));
    assert_eq!(body["tools"][0]["function"]["name"], json!("read_file"));

    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages[0]["role"], json!("system"));
    assert_eq!(messages[1]["role"], json!("user"));
    // Each result is its own `tool` message here — the inverse of Anthropic.
    assert_eq!(messages[2]["role"], json!("tool"));
    assert_eq!(messages[2]["tool_call_id"], json!("call_a"));
    assert_eq!(messages[3]["tool_call_id"], json!("call_b"));
    // The dialect has no error flag, so failure has to be visible in the text.
    assert_eq!(messages[3]["content"], json!("ERROR: not found"));
    assert_eq!(messages.len(), 4);
}

#[tokio::test]
async fn retries_a_rate_limit_then_succeeds() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(&[r#"{"id":"c","choices":[{"delta":{"content":"after retry"},"finish_reason":"stop"}]}"#]),
            "text/event-stream",
        ))
        .with_priority(2)
        .mount(&server)
        .await;

    let events = events(&server, request()).await;

    assert!(events.contains(&StreamEvent::TextDelta {
        text: "after retry".into()
    }));
    assert_eq!(server.received_requests().await.expect("requests").len(), 2);
}

#[tokio::test]
async fn lists_the_models_a_server_reports() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "qwen/qwen3.8-27b", "object": "model"},
                {"id": "gpt-oss-20b", "object": "model"},
            ]
        })))
        .mount(&server)
        .await;

    let models = list_models(&format!("{}/v1", server.uri()), None)
        .await
        .expect("models");

    assert_eq!(models, ["qwen/qwen3.8-27b", "gpt-oss-20b"]);
}

#[tokio::test]
async fn a_server_error_listing_models_is_reported_not_retried_forever() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&server)
        .await;

    let error = list_models(&format!("{}/v1", server.uri()), None)
        .await
        .expect_err("a 500 must not be treated as success");

    assert!(
        matches!(error, LlmError::Http { status: 500, .. }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn an_unparseable_body_is_a_decode_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;

    let error = list_models(&format!("{}/v1", server.uri()), None)
        .await
        .expect_err("an unparseable body must not be treated as an empty list");

    assert!(matches!(error, LlmError::Decode { .. }), "got {error:?}");
}

#[tokio::test]
async fn an_attached_image_switches_content_to_the_array_shape() {
    let server = serve(sse(&[
        r#"{"id":"c","choices":[{"delta":{"content":"a cat"},"finish_reason":"stop"}]}"#,
    ]))
    .await;

    let history = vec![Message::user_with_images(
        "what is this?",
        [ContentBlock::image("image/png", "aGVsbG8=")],
    )];
    let request = ChatRequest::new("test-model", history);

    events(&server, request).await;

    let requests = server.received_requests().await.expect("recorded requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");

    let content = body["messages"][0]["content"]
        .as_array()
        .expect("content must be an array once an image is attached");
    assert_eq!(content[0]["type"], json!("image_url"));
    assert_eq!(
        content[0]["image_url"]["url"],
        json!("data:image/png;base64,aGVsbG8=")
    );
    assert_eq!(content[1], json!({"type": "text", "text": "what is this?"}));
}

#[tokio::test]
async fn an_image_only_message_is_not_dropped() {
    let server = serve(sse(&[
        r#"{"id":"c","choices":[{"delta":{"content":"a cat"},"finish_reason":"stop"}]}"#,
    ]))
    .await;

    let history = vec![Message::user_with_images(
        "",
        [ContentBlock::image("image/png", "aGVsbG8=")],
    )];
    let request = ChatRequest::new("test-model", history);

    events(&server, request).await;

    let requests = server.received_requests().await.expect("recorded requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");

    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 1, "a captionless image must still be sent");
    let content = messages[0]["content"].as_array().expect("content array");
    assert_eq!(content.len(), 1, "no text part when there is no caption");
}

#[tokio::test]
async fn a_tool_results_image_produces_a_trailing_user_message() {
    let server = serve(sse(&[
        r#"{"id":"c","choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
    ]))
    .await;

    // This dialect has no way to attach an image to a `tool`-role message,
    // so the image rides along in a synthetic message right after it.
    let history = vec![
        Message::user("check the screen"),
        Message::tool_results([ToolResult::ok_with_image(
            "call_a",
            "Captured a screenshot of the screen.",
            ToolResultImage {
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            },
        )]),
    ];
    let request = ChatRequest::new("test-model", history);

    events(&server, request).await;

    let requests = server.received_requests().await.expect("recorded requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");

    let messages = body["messages"].as_array().expect("messages");
    // user, then the tool-role text message, then the synthetic image message.
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["role"], json!("tool"));
    assert_eq!(messages[1]["tool_call_id"], json!("call_a"));
    assert_eq!(
        messages[1]["content"],
        json!("Captured a screenshot of the screen.")
    );

    assert_eq!(messages[2]["role"], json!("user"));
    let content = messages[2]["content"].as_array().expect("content array");
    assert_eq!(content[0]["type"], json!("image_url"));
    assert_eq!(
        content[0]["image_url"]["url"],
        json!("data:image/png;base64,aGVsbG8=")
    );
}
