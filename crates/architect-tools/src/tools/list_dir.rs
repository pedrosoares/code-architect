//! List a directory's immediate entries.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::ToolContext,
    tool::{Tool, ToolOutput},
};

#[derive(Deserialize)]
struct Input {
    #[serde(default = "default_path")]
    path: String,
}

fn default_path() -> String {
    ".".to_owned()
}

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }

    fn description(&self) -> &'static str {
        "List a directory's immediate entries, each marked as a file or a directory."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory relative to the workspace root; defaults to the root"},
            },
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;
        let path = ctx.resolve(&input.path)?;

        let mut entries: Vec<(String, bool)> = std::fs::read_dir(&path)
            .map_err(|error| format!("could not list {}: {error}", input.path))?
            .filter_map(|entry| entry.ok())
            .map(|entry| {
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                (entry.file_name().to_string_lossy().into_owned(), is_dir)
            })
            .collect();

        entries.sort_by(|a, b| a.0.cmp(&b.0));

        if entries.is_empty() {
            return Ok(format!("{} is empty", input.path).into());
        }

        Ok(entries
            .into_iter()
            .map(|(name, is_dir)| if is_dir { format!("{name}/") } else { name })
            .collect::<Vec<_>>()
            .join("\n")
            .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lists_files_and_directories_sorted_with_a_trailing_slash_on_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::create_dir(dir.path().join("a_dir")).unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let output = ListDir.call(json!({}), &ctx).await.unwrap();

        assert_eq!(output.text, "a_dir/\nb.txt");
    }

    #[tokio::test]
    async fn reports_an_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let output = ListDir.call(json!({}), &ctx).await.unwrap();

        assert_eq!(output.text, ". is empty");
    }
}
