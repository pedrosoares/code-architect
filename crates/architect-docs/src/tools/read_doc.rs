//! Read a doc back, frontmatter and all.

use std::sync::Arc;

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{driver::DocDriver, frontmatter};

#[derive(Deserialize)]
struct Input {
    path: String,
}

pub struct ReadDoc {
    pub driver: Arc<dyn DocDriver>,
}

#[async_trait]
impl Tool for ReadDoc {
    fn name(&self) -> &'static str {
        "read_doc"
    }

    fn description(&self) -> &'static str {
        "Read a documentation doc, including its frontmatter (id, type, title, depends_on, ...)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
            },
            "required": ["path"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let (meta, body) = self.driver.read_doc(&input.path).await?;

        Ok(frontmatter::render(&meta, &body).into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{driver::DocMetadata, obsidian::ObsidianDriver};

    #[tokio::test]
    async fn reads_a_doc_back_with_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let driver = Arc::new(ObsidianDriver::new(dir.path()).unwrap());
        driver
            .write_doc(
                "a.md",
                DocMetadata {
                    id: "domain.orders".to_owned(),
                    doc_type: "domain".to_owned(),
                    title: "Orders".to_owned(),
                    status: None,
                    owner: None,
                    depends_on: Vec::new(),
                    relations: HashMap::new(),
                },
                "The orders domain.".to_owned(),
            )
            .await
            .unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();
        let tool = ReadDoc { driver };

        let result = tool.call(json!({"path": "a.md"}), &ctx).await.unwrap();

        assert!(result.text.contains("id: domain.orders"));
        assert!(result.text.contains("The orders domain."));
    }

    #[tokio::test]
    async fn reports_a_missing_doc_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let driver = Arc::new(ObsidianDriver::new(dir.path()).unwrap());
        let ctx = ToolContext::new(dir.path()).unwrap();
        let tool = ReadDoc { driver };

        let error = tool
            .call(json!({"path": "missing.md"}), &ctx)
            .await
            .unwrap_err();

        assert!(error.contains("missing.md"), "got: {error}");
    }
}
