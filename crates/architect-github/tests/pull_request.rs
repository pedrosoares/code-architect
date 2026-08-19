//! Both GitHub tools, pinned against a mock server — real request headers,
//! real pagination, real error mapping, no network.

use std::sync::Arc;

use architect_github::{
    GitHubClient,
    pull_request::{
        ReadPullRequest, ReadPullRequestComments, ReadPullRequestCommits, ReadPullRequestDiff,
    },
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
async fn reads_a_pull_requests_summary() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1"))
        .and(header("Authorization", "Bearer test-token"))
        .and(header("X-GitHub-Api-Version", "2022-11-28"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "title": "Add rollback UI",
            "user": {"login": "pedro"},
            "state": "open",
            "merged": false,
            "base": {"ref": "main"},
            "head": {"ref": "feature/rollback"},
            "additions": 120,
            "deletions": 4,
            "changed_files": 6,
            "body": "Wires up the Rollback button.",
            "html_url": "https://github.com/o/r/pull/1",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadPullRequest {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "pull_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert!(result.contains("Add rollback UI"));
    assert!(result.contains("by pedro"));
}

#[tokio::test]
async fn an_unauthorized_response_is_a_clear_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Bad credentials"))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadPullRequest {
        client: client(&server),
    };

    let error = tool
        .call(
            json!({"owner": "o", "repo": "r", "pull_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap_err();

    assert!(error.contains("401"), "got: {error}");
    assert!(error.contains("Bad credentials"), "got: {error}");
}

#[tokio::test]
async fn comments_merge_issue_comments_review_comments_and_reviews_in_order() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues/1/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "user": {"login": "alice"},
            "created_at": "2026-01-01T00:00:00Z",
            "body": "Looks good overall.",
        }])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "user": {"login": "bob"},
            "created_at": "2026-01-01T00:05:00Z",
            "path": "src/lib.rs",
            "line": 10,
            "body": "nit: typo",
        }])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "user": {"login": "bob"},
            "state": "APPROVED",
            "submitted_at": "2026-01-01T00:10:00Z",
            "body": "",
        }])))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadPullRequestComments {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "pull_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    let alice = result.find("alice").unwrap();
    let bob_comment = result.find("nit: typo").unwrap();
    let bob_review = result.find("APPROVED").unwrap();
    assert!(
        alice < bob_comment && bob_comment < bob_review,
        "expected chronological order (issue comment, then review comment, then review), got:\n{result}"
    );
}

#[tokio::test]
async fn comments_follows_pagination_across_multiple_pages() {
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
        // Without this, this mock (no query-param constraint) also matches
        // the `?page=2` request below, re-serving the same `Link: rel=
        // "next"` header forever — an infinite pagination loop, not a
        // one-off wrong answer, which is what actually happened here.
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
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadPullRequestComments {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "pull_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert!(result.contains("page one"));
    assert!(result.contains("page two"));
}

#[tokio::test]
async fn no_comments_or_reviews_is_reported_plainly_not_as_an_empty_string() {
    let server = MockServer::start().await;
    for endpoint in [
        "/repos/o/r/issues/1/comments",
        "/repos/o/r/pulls/1/comments",
        "/repos/o/r/pulls/1/reviews",
    ] {
        Mock::given(method("GET"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
    }

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadPullRequestComments {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "pull_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert_eq!(result, "No comments or reviews on this pull request.");
}

#[tokio::test]
async fn reads_a_pull_requests_file_diff() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1/files"))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "status": "modified",
            "filename": "src/lib.rs",
            "additions": 3,
            "deletions": 1,
            "patch": "@@ -1,3 +1,5 @@\n+use std::fmt;\n",
        }])))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadPullRequestDiff {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "pull_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert!(result.contains("modified: src/lib.rs (+3 -1)"));
    assert!(result.contains("+use std::fmt;"));
}

#[tokio::test]
async fn the_file_diff_paginates_across_multiple_pages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1/files"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{
                    "status": "added", "filename": "a.rs", "additions": 1, "deletions": 0,
                }]))
                .append_header(
                    "Link",
                    format!(
                        "<{}/repos/o/r/pulls/1/files?page=2>; rel=\"next\"",
                        server.uri()
                    ),
                ),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1/files"))
        .and(wiremock::matchers::query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "status": "added", "filename": "b.rs", "additions": 1, "deletions": 0,
        }])))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadPullRequestDiff {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "pull_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert!(result.contains("a.rs"));
    assert!(result.contains("b.rs"));
}

#[tokio::test]
async fn no_changed_files_is_reported_plainly() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadPullRequestDiff {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "pull_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert_eq!(result, "No files changed.");
}

#[tokio::test]
async fn reads_a_pull_requests_commits() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1/commits"))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "sha": "abcdef1234567890",
            "author": {"login": "pedro"},
            "commit": {
                "message": "Add rollback UI",
                "author": {"name": "Pedro Soares", "date": "2026-01-01T00:00:00Z"},
            },
        }])))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadPullRequestCommits {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "pull_number": 1}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert!(result.contains("abcdef1 by pedro"));
    assert!(result.contains("Add rollback UI"));
}
