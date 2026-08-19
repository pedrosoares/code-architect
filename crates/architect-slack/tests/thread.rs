//! `slack_read_thread`, pinned against a mock server — real auth header,
//! real `ok: false` error mapping, real cursor pagination, no network.

use std::sync::Arc;

use architect_slack::{SlackClient, thread::ReadThread};
use architect_tools::{Tool, ToolContext};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

fn ctx(dir: &std::path::Path) -> ToolContext {
    ToolContext::new(dir).unwrap()
}

fn client(server: &MockServer) -> Arc<SlackClient> {
    Arc::new(SlackClient::with_base_url(
        "xoxb-test".to_owned(),
        server.uri(),
    ))
}

#[tokio::test]
async fn reads_a_thread_in_order() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/conversations.replies"))
        .and(header("Authorization", "Bearer xoxb-test"))
        .and(query_param("channel", "C1"))
        .and(query_param("ts", "1700000000.000000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "messages": [
                {"user": "U1", "ts": "1700000000.000000", "text": "anyone seen this fail?"},
                {"user": "U2", "ts": "1700000001.000000", "text": "yep, looking now"},
            ],
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadThread {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"channel": "C1", "thread_ts": "1700000000.000000"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    let first = result.find("anyone seen this fail?").unwrap();
    let second = result.find("yep, looking now").unwrap();
    assert!(
        first < second,
        "expected parent before reply, got:\n{result}"
    );
}

#[tokio::test]
async fn an_ok_false_body_is_reported_as_the_slack_error_code_not_swallowed() {
    let server = MockServer::start().await;
    // Slack always answers HTTP 200 — the failure is in the body.
    Mock::given(method("GET"))
        .and(path("/conversations.replies"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": false,
            "error": "channel_not_found",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadThread {
        client: client(&server),
    };

    let error = tool
        .call(
            json!({"channel": "bad", "thread_ts": "1"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap_err();

    assert!(error.contains("channel_not_found"), "got: {error}");
}

#[tokio::test]
async fn follows_cursor_pagination_across_pages() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/conversations.replies"))
        .and(query_param("channel", "C1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "messages": [{"user": "U1", "ts": "1", "text": "page one"}],
            "response_metadata": {"next_cursor": "cursor-2"},
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/conversations.replies"))
        .and(query_param("cursor", "cursor-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "messages": [{"user": "U1", "ts": "2", "text": "page two"}],
            "response_metadata": {"next_cursor": ""},
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadThread {
        client: client(&server),
    };

    let result = tool
        .call(json!({"channel": "C1", "thread_ts": "1"}), &ctx(dir.path()))
        .await
        .unwrap()
        .text;

    assert!(result.contains("page one"));
    assert!(result.contains("page two"));
}

#[tokio::test]
async fn a_pasted_link_resolves_to_the_right_channel_and_thread_ts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/conversations.replies"))
        .and(query_param("channel", "C0123456789"))
        .and(query_param("ts", "1700000000.000100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "messages": [{"user": "U1", "ts": "1700000000.000100", "text": "shipping soon"}],
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadThread {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"link": "https://workspace.slack.com/archives/C0123456789/p1700000000000100"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert!(result.contains("shipping soon"));
}
