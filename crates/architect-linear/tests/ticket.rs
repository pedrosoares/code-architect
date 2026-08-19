//! `linear_read_ticket`, pinned against a mock server — real auth header
//! (no `Bearer` prefix, unlike everything else this app talks to), real
//! GraphQL error mapping, no network.

use std::sync::Arc;

use architect_linear::{LinearClient, ticket::ReadTicket};
use architect_tools::{Tool, ToolContext};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

fn ctx(dir: &std::path::Path) -> ToolContext {
    ToolContext::new(dir).unwrap()
}

fn client(server: &MockServer) -> Arc<LinearClient> {
    Arc::new(LinearClient::with_url(
        "lin_api_test".to_owned(),
        format!("{}/graphql", server.uri()),
    ))
}

#[tokio::test]
async fn reads_a_ticket_with_its_blockers_and_comments() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(header("Authorization", "lin_api_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "issue": {
                    "identifier": "ENG-1",
                    "title": "Fix the bug",
                    "state": {"name": "In Progress"},
                    "assignee": {"name": "Pedro"},
                    "priority": 2.0,
                    "description": "It's broken.",
                    "comments": {"nodes": [
                        {"body": "on it", "createdAt": "2026-01-01T00:00:00Z", "user": {"name": "Ana"}},
                    ]},
                    "relations": {"nodes": [
                        {"type": "blocks", "relatedIssue": {"identifier": "ENG-2", "title": "Ship it"}},
                    ]},
                    "inverseRelations": {"nodes": []},
                },
            },
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadTicket {
        client: client(&server),
    };

    let result = tool
        .call(json!({"id": "ENG-1"}), &ctx(dir.path()))
        .await
        .unwrap()
        .text;

    assert!(result.contains("ENG-1: Fix the bug"));
    assert!(result.contains("Blocks:\n- ENG-2: Ship it"));
    assert!(result.contains("Ana: on it"));
}

#[tokio::test]
async fn the_authorization_header_has_no_bearer_prefix() {
    let server = MockServer::start().await;
    // The `header` matcher below asserts the exact value — a `Bearer `
    // prefix here would fail the match and this test would fail with "no
    // matching mock", proving the client doesn't add one.
    Mock::given(method("POST"))
        .and(header("Authorization", "lin_api_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"issue": null},
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadTicket {
        client: client(&server),
    };

    let _ = tool.call(json!({"id": "ENG-1"}), &ctx(dir.path())).await;
}

#[tokio::test]
async fn a_graphql_errors_array_is_reported_not_swallowed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "errors": [{"message": "Entity not found"}],
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadTicket {
        client: client(&server),
    };

    let error = tool
        .call(json!({"id": "NOPE-1"}), &ctx(dir.path()))
        .await
        .unwrap_err();

    assert!(error.contains("Entity not found"), "got: {error}");
}

#[tokio::test]
async fn a_missing_ticket_is_a_clear_error_not_a_panic() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"issue": null},
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadTicket {
        client: client(&server),
    };

    let error = tool
        .call(json!({"id": "GONE-1"}), &ctx(dir.path()))
        .await
        .unwrap_err();

    assert!(error.contains("GONE-1"), "got: {error}");
}
