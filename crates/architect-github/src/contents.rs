//! Read a file's real content, or list a directory, from a repo at a given
//! ref — GitHub's "contents" API returns an object for a file and an array
//! for a directory, so the two tools here share one request and branch on
//! which shape came back.

use std::sync::Arc;

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::GitHubClient;

#[derive(Deserialize)]
struct Input {
    owner: String,
    repo: String,
    #[serde(default)]
    path: String,
    #[serde(rename = "ref")]
    git_ref: Option<String>,
}

fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "owner": {"type": "string", "description": "Repository owner or organization"},
            "repo": {"type": "string", "description": "Repository name"},
            "path": {
                "type": "string",
                "description": "Path within the repository. Empty or omitted means the repository root.",
            },
            "ref": {
                "type": "string",
                "description": "Branch, tag, or commit SHA. Defaults to the repository's default branch.",
            },
        },
        "required": ["owner", "repo"],
        "additionalProperties": false,
    })
}

async fn fetch(client: &GitHubClient, input: &Input) -> Result<Value, String> {
    let mut segments = vec!["repos", &input.owner, &input.repo, "contents"];
    let path_segments: Vec<&str> = input.path.split('/').filter(|s| !s.is_empty()).collect();
    segments.extend(path_segments);

    let query: Vec<(&str, &str)> = match &input.git_ref {
        Some(git_ref) => vec![("ref", git_ref.as_str())],
        None => Vec::new(),
    };

    let url = client.build_url(&segments, &query)?;
    client.get_url(url).await
}

fn display_path(path: &str) -> &str {
    if path.is_empty() { "/" } else { path }
}

pub struct ReadFile {
    pub client: Arc<GitHubClient>,
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "github_read_file"
    }

    fn description(&self) -> &'static str {
        "Read a file's content from a GitHub repository at a given ref (branch, tag, or commit \
         SHA), defaulting to the repository's default branch."
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;
        let path = input.path.clone();
        let body = fetch(&self.client, &input).await?;

        format_file(&path, &body).map(Into::into)
    }
}

pub struct ListDirectory {
    pub client: Arc<GitHubClient>,
}

#[async_trait]
impl Tool for ListDirectory {
    fn name(&self) -> &'static str {
        "github_list_directory"
    }

    fn description(&self) -> &'static str {
        "List the files and subdirectories at a path in a GitHub repository at a given ref, \
         defaulting to the repository's default branch."
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;
        let path = input.path.clone();
        let body = fetch(&self.client, &input).await?;

        format_directory(&path, &body).map(Into::into)
    }
}

fn format_file(path: &str, body: &Value) -> Result<String, String> {
    if body.is_array() {
        return Err(format!(
            "{} is a directory, not a file — use github_list_directory instead",
            display_path(path)
        ));
    }

    let entry_type = body["type"].as_str().unwrap_or("file");
    let size = body["size"].as_u64().unwrap_or(0);

    match entry_type {
        "symlink" => {
            let target = decode_content(body)?.unwrap_or_default();
            Ok(format!("{} is a symlink -> {target}", display_path(path)))
        }
        "submodule" => Ok(format!(
            "{} is a git submodule (no file content to read)",
            display_path(path)
        )),
        _ => match decode_content(body)? {
            None => Err(format!(
                "{} ({size} bytes) is too large for GitHub's contents API to return \
                 (over its 1MB limit) — nothing to read",
                display_path(path)
            )),
            Some(bytes_as_text) => Ok(format!(
                "{} ({size} bytes)\n\n{bytes_as_text}",
                display_path(path)
            )),
        },
    }
}

fn format_directory(path: &str, body: &Value) -> Result<String, String> {
    let entries = match body.as_array() {
        Some(entries) => entries,
        None => {
            return Err(format!(
                "{} is a file, not a directory — use github_read_file instead",
                display_path(path)
            ));
        }
    };

    if entries.is_empty() {
        return Ok(format!("{} is empty.", display_path(path)));
    }

    let mut lines: Vec<String> = entries
        .iter()
        .map(|entry| {
            let entry_type = entry["type"].as_str().unwrap_or("file");
            let entry_path = entry["path"].as_str().unwrap_or("?");
            if entry_type == "dir" {
                format!("{entry_type:<4} {entry_path}/")
            } else {
                let size = entry["size"].as_u64().unwrap_or(0);
                format!("{entry_type:<4} {entry_path} ({size} bytes)")
            }
        })
        .collect();
    lines.sort();

    Ok(lines.join("\n"))
}

/// Decodes the contents API's base64 `content` field (GitHub inserts a
/// newline every ~60 characters, which the decoder rejects unless
/// stripped first). Returns `Ok(None)` when GitHub omitted `content`
/// entirely (over its 1MB contents-API size limit), and an `Err` only when
/// binary content can't be shown as text.
fn decode_content(body: &Value) -> Result<Option<String>, String> {
    let Some(raw) = body["content"].as_str() else {
        return Ok(None);
    };

    let stripped: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(stripped)
        .map_err(|error| format!("could not decode GitHub's base64 content: {error}"))?;

    match String::from_utf8(bytes) {
        Ok(text) => Ok(Some(text)),
        Err(_) => {
            let size = body["size"].as_u64().unwrap_or(0);
            let path = body["path"].as_str().unwrap_or("this file");
            Err(format!(
                "{path} ({size} bytes) is a binary file — cannot display as text"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    #[test]
    fn formats_a_text_file() {
        let body = json!({
            "type": "file",
            "path": "src/lib.rs",
            "size": 11,
            "content": encode("hello world"),
        });

        assert_eq!(
            format_file("src/lib.rs", &body).unwrap(),
            "src/lib.rs (11 bytes)\n\nhello world"
        );
    }

    #[test]
    fn a_binary_file_is_reported_not_decoded() {
        let body = json!({
            "type": "file",
            "path": "assets/logo.png",
            "size": 4,
            "content": base64::engine::general_purpose::STANDARD.encode([0xFF, 0xD8, 0xFF, 0xE0]),
        });

        let error = format_file("assets/logo.png", &body).unwrap_err();
        assert!(error.contains("binary file"));
    }

    #[test]
    fn a_file_over_the_size_limit_has_no_content_field() {
        let body = json!({"type": "file", "path": "big.bin", "size": 5_000_000});

        let error = format_file("big.bin", &body).unwrap_err();
        assert!(error.contains("too large"));
    }

    #[test]
    fn a_symlink_shows_its_target() {
        let body = json!({
            "type": "symlink",
            "path": "link",
            "size": 0,
            "content": encode("target/file.rs"),
        });

        assert_eq!(
            format_file("link", &body).unwrap(),
            "link is a symlink -> target/file.rs"
        );
    }

    #[test]
    fn asking_to_read_a_directory_as_a_file_redirects_to_the_right_tool() {
        let body = json!([{"type": "file", "path": "a", "size": 1}]);

        let error = format_file("dir", &body).unwrap_err();
        assert!(error.contains("use github_list_directory instead"));
    }

    #[test]
    fn asking_to_list_a_file_as_a_directory_redirects_to_the_right_tool() {
        let body = json!({"type": "file", "path": "a", "size": 1, "content": encode("x")});

        let error = format_directory("a", &body).unwrap_err();
        assert!(error.contains("use github_read_file instead"));
    }

    #[test]
    fn an_empty_directory_says_so() {
        let body = json!([]);
        assert_eq!(format_directory("empty", &body).unwrap(), "empty is empty.");
    }

    #[test]
    fn lists_files_and_directories_distinctly() {
        let body = json!([
            {"type": "dir", "path": "src", "size": 0},
            {"type": "file", "path": "Cargo.toml", "size": 42},
        ]);

        let text = format_directory("", &body).unwrap();
        assert!(text.contains("dir  src/"));
        assert!(text.contains("file Cargo.toml (42 bytes)"));
    }
}
