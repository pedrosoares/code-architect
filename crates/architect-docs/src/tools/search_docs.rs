//! Full-text and id/title search across the knowledge base.

use std::sync::Arc;

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::driver::{DocDriver, DocSearchHit};

#[derive(Deserialize)]
struct Input {
    query: String,
    #[serde(default)]
    doc_type: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

pub struct SearchDocs {
    pub driver: Arc<dyn DocDriver>,
}

#[async_trait]
impl Tool for SearchDocs {
    fn name(&self) -> &'static str {
        "search_docs"
    }

    fn description(&self) -> &'static str {
        "Search the documentation knowledge base by id, title, or body text — the doc analog \
         of grep. Searching an id directly (e.g. \"domain.orders\") finds that doc."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "doc_type": {"type": "string", "description": "Restrict to one type: system, domain, flow, rule, entity, integration, adr, index"},
                "limit": {"type": "integer", "description": "Stop after this many hits"},
            },
            "required": ["query"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let mut hits = self
            .driver
            .search_docs(&input.query, input.doc_type.as_deref())
            .await?;

        if let Some(limit) = input.limit {
            hits.truncate(limit);
        }

        if hits.is_empty() {
            return Ok(format!("no docs match {:?}", input.query).into());
        }

        Ok(render(&hits).into())
    }
}

fn render(hits: &[DocSearchHit]) -> String {
    hits.iter()
        .map(|hit| {
            format!(
                "{} [{}] {}\n  {}\n  {}",
                hit.id, hit.doc_type, hit.title, hit.path, hit.snippet
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
    async fn finds_a_doc_by_id() {
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
                String::new(),
            )
            .await
            .unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();
        let tool = SearchDocs { driver };

        let result = tool
            .call(json!({"query": "domain.orders"}), &ctx)
            .await
            .unwrap();

        assert!(result.text.contains("domain.orders"));
    }

    #[tokio::test]
    async fn reports_no_matches_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let driver = Arc::new(ObsidianDriver::new(dir.path()).unwrap());
        let ctx = ToolContext::new(dir.path()).unwrap();
        let tool = SearchDocs { driver };

        let result = tool
            .call(json!({"query": "nonexistent"}), &ctx)
            .await
            .unwrap();

        assert!(result.text.contains("no docs match"));
    }
}
