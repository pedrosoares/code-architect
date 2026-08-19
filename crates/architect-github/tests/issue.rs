//! A plain GitHub issue and its comments, pinned against a mock server —
//! real request headers, real pagination, real error mapping, no network.

use std::sync::Arc;

use architect_github::{
    GitHubClient,
    issue::{ReadIssue, ReadIssueComments},
};
use architect_tools::{Tool, ToolContext};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

fn ctx(dir: &std::path::Path) -> ToolContext {
    ToolContext::new(dir).unwrap()
}

fn client(server: &MockServer) -> Arc<GitHubClient> {
    Arc::new(GitHubClient::with_base_url(
        "test-token".to_owned(),
        server.uri(),
    ))
}

#[tokio::test]
async fn reads_an_issue() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues/1"))
        .and(header("Authorization", "Bearer test-token"))
        .and(header("X-GitHub-Api-Version", "2022-11-28"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "title": "Rollback fails on renamed files",
            "user": {"login": "pedro"},
            "state": "open",
            "labels": [{"name": "bug"}],
            "assignees": [],
            "body": "Steps to reproduce...",
            "html_url": "https://github.com/o/r/issues/1",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadIssue {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "issue_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert!(result.contains("Rollback fails on renamed files"));
    assert!(result.contains("Labels: bug"));
}

#[tokio::test]
async fn an_unauthorized_response_is_a_clear_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues/1"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Bad credentials"))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadIssue {
        client: client(&server),
    };

    let error = tool
        .call(
            json!({"owner": "o", "repo": "r", "issue_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap_err();

    assert!(error.contains("401"), "got: {error}");
}

#[tokio::test]
async fn reads_an_issues_comments_in_order() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues/1/comments"))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "user": {"login": "alice"},
                "created_at": "2026-01-01T00:00:00Z",
                "body": "First.",
            },
            {
                "user": {"login": "bob"},
                "created_at": "2026-01-01T00:05:00Z",
                "body": "Second.",
            },
        ])))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadIssueComments {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "issue_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    let first = result.find("First.").unwrap();
    let second = result.find("Second.").unwrap();
    assert!(
        first < second,
        "expected chronological order, got:\n{result}"
    );
}

#[tokio::test]
async fn the_comments_endpoint_paginates_across_multiple_pages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues/1/comments"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{
                    "user": {"login": "alice"},
                    "created_at": "2026-01-01T00:00:00Z",
                    "body": "page one",
                }]))
                .append_header(
                    "Link",
                    format!(
                        "<{}/repos/o/r/issues/1/comments?page=2>; rel=\"next\"",
                        server.uri()
                    ),
                ),
        )
        // Any mock for a paginated endpoint must be bounded — an
        // unconstrained mock re-serves the same `Link: rel="next"` header
        // forever. See the equivalent comment in tests/pull_request.rs.
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues/1/comments"))
        .and(wiremock::matchers::query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "user": {"login": "alice"},
            "created_at": "2026-01-01T00:01:00Z",
            "body": "page two",
        }])))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadIssueComments {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "issue_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert!(result.contains("page one"));
    assert!(result.contains("page two"));
}

#[tokio::test]
async fn no_comments_is_reported_plainly_not_as_an_empty_string() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues/1/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadIssueComments {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "issue_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert_eq!(result, "No comments on this issue.");
}
