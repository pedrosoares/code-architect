//! `slack_list_channels` and `slack_read_channel_history`, pinned against a
//! mock server — real auth header, real `ok: false` error mapping, real
//! cursor pagination, no network.

use std::sync::Arc;

use architect_slack::{
    SlackClient,
    channel::{ListChannels, ReadChannelHistory},
};
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
async fn lists_channels() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .and(header("Authorization", "Bearer xoxb-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [
                {"id": "C1", "name": "general", "is_private": false, "is_member": true, "topic": {"value": ""}},
                {"id": "C2", "name": "incidents", "is_private": true, "is_member": false, "topic": {"value": "prod fires only"}},
            ],
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ListChannels {
        client: client(&server),
    };

    let result = tool.call(json!({}), &ctx(dir.path())).await.unwrap().text;

    assert!(result.contains("#general  id=C1  public  member=yes"));
    assert!(result.contains("#incidents  id=C2  private  member=no"));
    assert!(result.contains("topic: prod fires only"));
}

#[tokio::test]
async fn channel_listing_follows_cursor_pagination() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [{"id": "C1", "name": "a", "is_private": false, "is_member": true, "topic": {"value": ""}}],
            "response_metadata": {"next_cursor": "cursor-2"},
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .and(query_param("cursor", "cursor-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [{"id": "C2", "name": "b", "is_private": false, "is_member": true, "topic": {"value": ""}}],
            "response_metadata": {"next_cursor": ""},
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ListChannels {
        client: client(&server),
    };

    let result = tool.call(json!({}), &ctx(dir.path())).await.unwrap().text;

    assert!(result.contains("#a  id=C1"));
    assert!(result.contains("#b  id=C2"));
}

#[tokio::test]
async fn no_channels_is_reported_plainly() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [],
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ListChannels {
        client: client(&server),
    };

    let result = tool.call(json!({}), &ctx(dir.path())).await.unwrap().text;
    assert_eq!(result, "No channels found.");
}

#[tokio::test]
async fn an_ok_false_body_is_reported_as_the_slack_error_code() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": false,
            "error": "missing_scope",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ListChannels {
        client: client(&server),
    };

    let error = tool.call(json!({}), &ctx(dir.path())).await.unwrap_err();
    assert!(error.contains("missing_scope"), "got: {error}");
}

#[tokio::test]
async fn reads_channel_history_in_chronological_order_with_thread_markers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/conversations.history"))
        .and(header("Authorization", "Bearer xoxb-test"))
        .and(query_param("channel", "C1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            // Slack returns newest-first.
            "messages": [
                {"user": "U2", "ts": "1700000001.000000", "text": "second"},
                {"user": "U1", "ts": "1700000000.000000", "text": "first", "reply_count": 2},
            ],
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadChannelHistory {
        client: client(&server),
    };

    let result = tool
        .call(json!({"channel": "C1"}), &ctx(dir.path()))
        .await
        .unwrap()
        .text;

    let first = result.find("first").unwrap();
    let second = result.find("second").unwrap();
    assert!(
        first < second,
        "expected chronological order, got:\n{result}"
    );
    assert!(result.contains("(2 replies, thread_ts=1700000000.000000)"));
}

#[tokio::test]
async fn no_messages_is_reported_plainly() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/conversations.history"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "messages": [],
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadChannelHistory {
        client: client(&server),
    };

    let result = tool
        .call(json!({"channel": "C1"}), &ctx(dir.path()))
        .await
        .unwrap()
        .text;

    assert_eq!(result, "No messages found in this channel.");
}
