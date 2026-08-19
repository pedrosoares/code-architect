//! Read a file, with line numbers.

use std::io::ErrorKind;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::ToolContext,
    tool::{Tool, ToolOutput},
};

/// Bytes sniffed to decide whether a file looks binary. 8000 is what most
/// editors and `file(1)` use as a cheap heuristic window.
const SNIFF_LEN: usize = 8000;

#[derive(Deserialize)]
struct Input {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read a file's contents with line numbers. Supports an optional offset and limit for large files."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the workspace root"},
                "offset": {"type": "integer", "description": "0-based line to start from"},
                "limit": {"type": "integer", "description": "Maximum number of lines to return"},
            },
            "required": ["path"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;
        let path = ctx.resolve(&input.path)?;

        let bytes = std::fs::read(&path).map_err(|error| describe(&error, &input.path))?;

        if looks_binary(&bytes) {
            return Ok(format!(
                "{} appears to be a binary file ({} bytes) — not shown.",
                input.path,
                bytes.len()
            )
            .into());
        }

        let text = String::from_utf8_lossy(&bytes);
        let offset = input.offset.unwrap_or(0);
        let lines = text.lines().enumerate().skip(offset);

        let numbered: Vec<String> = match input.limit {
            Some(limit) => lines.take(limit).map(render_line).collect(),
            None => lines.map(render_line).collect(),
        };

        if numbered.is_empty() {
            return Err(format!("{} has no content at offset {offset}", input.path));
        }

        Ok(numbered.join("\n").into())
    }
}

fn render_line((index, text): (usize, &str)) -> String {
    format!("{:>6}\t{}", index + 1, text)
}

/// A cheap sniff, not a MIME check: any control byte other than the common
/// whitespace ones in the first [`SNIFF_LEN`] bytes is treated as binary.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .take(SNIFF_LEN)
        .any(|&byte| byte == 0 || (byte < 0x20 && !matches!(byte, b'\n' | b'\r' | b'\t')))
}

fn describe(error: &std::io::Error, path: &str) -> String {
    match error.kind() {
        ErrorKind::NotFound => format!("{path} does not exist"),
        ErrorKind::PermissionDenied => format!("permission denied reading {path}"),
        _ => format!("could not read {path}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(dir).unwrap()
    }

    #[tokio::test]
    async fn reads_a_file_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree").unwrap();

        let output = ReadFile
            .call(json!({"path": "a.txt"}), &ctx(dir.path()))
            .await
            .unwrap();

        assert_eq!(output.text, "     1\tone\n     2\ttwo\n     3\tthree");
    }

    #[tokio::test]
    async fn honors_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\nfour").unwrap();

        let output = ReadFile
            .call(
                json!({"path": "a.txt", "offset": 1, "limit": 2}),
                &ctx(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(output.text, "     2\ttwo\n     3\tthree");
    }

    #[tokio::test]
    async fn refuses_to_read_outside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();

        let error = ReadFile
            .call(json!({"path": "../../etc/passwd"}), &ctx(dir.path()))
            .await
            .unwrap_err();

        assert!(error.contains("workspace"), "got: {error}");
    }

    #[tokio::test]
    async fn stubs_binary_content_instead_of_dumping_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2, 159, 3]).unwrap();

        let output = ReadFile
            .call(json!({"path": "bin.dat"}), &ctx(dir.path()))
            .await
            .unwrap();

        assert!(output.text.contains("binary"), "got: {}", output.text);
    }
}
