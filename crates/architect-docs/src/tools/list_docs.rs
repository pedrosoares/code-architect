//! The structural index of every doc — the domain/flow/dependency map,
//! generated on demand rather than hand-maintained.

use std::sync::Arc;

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::driver::{DocDriver, DocSummary};

#[derive(Deserialize)]
struct Input {
    #[serde(default)]
    doc_type: Option<String>,
}

pub struct ListDocs {
    pub driver: Arc<dyn DocDriver>,
}

#[async_trait]
impl Tool for ListDocs {
    fn name(&self) -> &'static str {
        "list_docs"
    }

    fn description(&self) -> &'static str {
        "List every doc's id, type, title, path, and depends_on — the doc analog of list_dir. \
         Use this to browse the knowledge base structurally, e.g. to see every domain or every \
         flow, or to trace what a doc depends on."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "doc_type": {"type": "string", "description": "Restrict to one type: system, domain, flow, rule, entity, integration, adr, index"},
            },
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let docs = self.driver.list_docs(input.doc_type.as_deref()).await?;

        if docs.is_empty() {
            return Ok("no docs found".to_owned().into());
        }

        Ok(render(&docs).into())
    }
}

fn render(docs: &[DocSummary]) -> String {
    docs.iter()
        .map(|doc| {
            let depends_on = if doc.depends_on.is_empty() {
                String::new()
            } else {
                format!("\n  depends_on: {}", doc.depends_on.join(", "))
            };
            format!(
                "{} [{}] {}\n  {}{}",
                doc.id, doc.doc_type, doc.title, doc.path, depends_on
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{driver::DocMetadata, obsidian::ObsidianDriver};

    #[tokio::test]
    async fn lists_every_doc() {
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
                    depends_on: vec!["domain.customers".to_owned()],
                    relations: HashMap::new(),
                },
                String::new(),
            )
            .await
            .unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();
        let tool = ListDocs { driver };

        let result = tool.call(json!({}), &ctx).await.unwrap();

        assert!(result.text.contains("domain.orders"));
        assert!(result.text.contains("domain.customers"));
    }

    #[tokio::test]
    async fn reports_an_empty_vault_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let driver = Arc::new(ObsidianDriver::new(dir.path()).unwrap());
        let ctx = ToolContext::new(dir.path()).unwrap();
        let tool = ListDocs { driver };

        let result = tool.call(json!({}), &ctx).await.unwrap();

        assert_eq!(result.text, "no docs found");
    }
}
