//! Write a file, creating it (and its parent directories) if needed.

use architect_core::FileChange;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::ToolContext,
    tool::{Tool, ToolOutput},
};

#[derive(Deserialize)]
struct Input {
    path: String,
    content: String,
}

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Write content to a file, creating it and any missing parent directories. Overwrites an existing file entirely."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the workspace root"},
                "content": {"type": "string", "description": "The file's new, complete content"},
            },
            "required": ["path", "content"],
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;
        let path = ctx.resolve(&input.path)?;

        // Read before write: this is the file's previous content for the
        // change record, and its absence is how a rollback later tells
        // "restore this" from "this was created — delete it".
        let old_content = match std::fs::read_to_string(&path) {
            Ok(content) => Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!(
                    "could not read the existing {}: {error}",
                    input.path
                ));
            }
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }

        std::fs::write(&path, &input.content)
            .map_err(|error| format!("could not write {}: {error}", input.path))?;

        ctx.record_change(FileChange {
            file_path: path,
            old_content,
            new_content: input.content,
            tool_name: "write_file",
        });

        Ok(format!("Wrote {}", input.path).into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use architect_core::ChangeRecorder;

    use super::*;

    #[derive(Default)]
    struct Captured(Mutex<Vec<FileChange>>);

    impl ChangeRecorder for Captured {
        fn record(&self, change: FileChange) {
            self.0.lock().unwrap().push(change);
        }
    }

    #[tokio::test]
    async fn creates_a_new_file_and_its_parents() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        WriteFile
            .call(
                json!({"path": "nested/dir/a.txt", "content": "hello"}),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("nested/dir/a.txt")).unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn records_none_as_old_content_for_a_created_file() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(Captured::default());
        let ctx = ToolContext::new(dir.path())
            .unwrap()
            .with_recorder(recorder.clone());

        WriteFile
            .call(json!({"path": "a.txt", "content": "new"}), &ctx)
            .await
            .unwrap();

        let changes = recorder.0.lock().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].old_content, None);
        assert_eq!(changes[0].new_content, "new");
    }

    #[tokio::test]
    async fn records_the_previous_content_for_an_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old").unwrap();
        let recorder = Arc::new(Captured::default());
        let ctx = ToolContext::new(dir.path())
            .unwrap()
            .with_recorder(recorder.clone());

        WriteFile
            .call(json!({"path": "a.txt", "content": "new"}), &ctx)
            .await
            .unwrap();

        assert_eq!(
            recorder.0.lock().unwrap()[0].old_content.as_deref(),
            Some("old")
        );
    }

    #[tokio::test]
    async fn refuses_to_write_outside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let error = WriteFile
            .call(json!({"path": "../escape.txt", "content": "x"}), &ctx)
            .await
            .unwrap_err();

        assert!(error.contains("workspace"), "got: {error}");
    }
}
