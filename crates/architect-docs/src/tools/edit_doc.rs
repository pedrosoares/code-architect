//! Replace an exact substring in a doc's body — the doc analog of
//! `architect_tools`' `edit_file`.

use std::sync::Arc;

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::driver::DocDriver;

#[derive(Deserialize)]
struct Input {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

pub struct EditDoc {
    pub driver: Arc<dyn DocDriver>,
}

#[async_trait]
impl Tool for EditDoc {
    fn name(&self) -> &'static str {
        "edit_doc"
    }

    fn description(&self) -> &'static str {
        "Replace an exact substring in a doc's body (frontmatter is left untouched). Fails if \
         old_string is not found, or is ambiguous unless replace_all is set."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old_string": {"type": "string", "description": "Exact text to find in the body"},
                "new_string": {"type": "string", "description": "Text to replace it with"},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence instead of requiring exactly one"},
            },
            "required": ["path", "old_string", "new_string"],
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let diff = self
            .driver
            .edit_doc(
                &input.path,
                &input.old_string,
                &input.new_string,
                input.replace_all,
            )
            .await?;

        Ok(diff.into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{driver::DocMetadata, obsidian::ObsidianDriver};

    #[tokio::test]
    async fn replaces_text_in_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let driver = Arc::new(ObsidianDriver::new(dir.path()).unwrap());
        driver
            .write_doc(
                "a.md",
                DocMetadata {
                    id: "a".to_owned(),
                    doc_type: "domain".to_owned(),
                    title: "A".to_owned(),
                    status: None,
                    owner: None,
                    depends_on: Vec::new(),
                    relations: HashMap::new(),
                },
                "old text".to_owned(),
            )
            .await
            .unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();
        let tool = EditDoc {
            driver: driver.clone(),
        };

        let result = tool
            .call(
                json!({"path": "a.md", "old_string": "old", "new_string": "new"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.text.contains("-old text"));
        assert!(result.text.contains("+new text"));
        let (_meta, body) = driver.read_doc("a.md").await.unwrap();
        assert_eq!(body, "new text");
    }
}
