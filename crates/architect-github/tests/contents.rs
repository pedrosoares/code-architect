//! Reading a file's content and listing a directory, pinned against a mock
//! server — real request headers, real percent-encoding of the path and
//! `ref` query param, real error mapping, no network.

use std::sync::Arc;

use architect_github::{
    GitHubClient,
    contents::{ListDirectory, ReadFile},
};
use architect_tools::{Tool, ToolContext};
use base64::Engine;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
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

fn encode(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

#[tokio::test]
async fn reads_a_files_content_at_the_default_branch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/contents/src/lib.rs"))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "file",
            "path": "src/lib.rs",
            "size": 11,
            "content": encode("hello world"),
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadFile {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "path": "src/lib.rs"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert!(result.contains("hello world"));
}

#[tokio::test]
async fn a_ref_is_sent_as_a_query_param() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/contents/lib.rs"))
        .and(query_param("ref", "feature/rollback"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "file",
            "path": "lib.rs",
            "size": 1,
            "content": encode("x"),
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadFile {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "path": "lib.rs", "ref": "feature/rollback"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert!(result.contains('x'));
}

#[tokio::test]
async fn a_path_with_a_space_is_percent_encoded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/contents/my%20file.rs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "file",
            "path": "my file.rs",
            "size": 1,
            "content": encode("x"),
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadFile {
        client: client(&server),
    };

    let result = tool
        .call(
            json!({"owner": "o", "repo": "r", "path": "my file.rs"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap()
        .text;

    assert!(result.contains('x'));
}

#[tokio::test]
async fn reading_a_directory_as_a_file_redirects_to_the_right_tool() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/contents/src"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"type": "file", "path": "src/lib.rs", "size": 10},
        ])))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ReadFile {
        client: client(&server),
    };

    let error = tool
        .call(
            json!({"owner": "o", "repo": "r", "path": "src"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap_err();

    assert!(error.contains("github_list_directory"));
}

#[tokio::test]
async fn lists_a_directory_at_the_repository_root() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/contents"))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"type": "dir", "path": "src", "size": 0},
            {"type": "file", "path": "Cargo.toml", "size": 42},
        ])))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ListDirectory {
        client: client(&server),
    };

    let result = tool
        .call(json!({"owner": "o", "repo": "r"}), &ctx(dir.path()))
        .await
        .unwrap()
        .text;

    assert!(result.contains("src/"));
    assert!(result.contains("Cargo.toml (42 bytes)"));
}

#[tokio::test]
async fn listing_a_file_as_a_directory_redirects_to_the_right_tool() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/contents/Cargo.toml"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "file",
            "path": "Cargo.toml",
            "size": 42,
            "content": encode("[package]"),
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let tool = ListDirectory {
        client: client(&server),
    };

    let error = tool
        .call(
            json!({"owner": "o", "repo": "r", "path": "Cargo.toml"}),
            &ctx(dir.path()),
        )
        .await
        .unwrap_err();

    assert!(error.contains("github_read_file"));
}
