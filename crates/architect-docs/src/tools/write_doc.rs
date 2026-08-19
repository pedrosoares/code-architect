//! Create a doc, or fully replace an existing one.

use std::{collections::HashMap, sync::Arc};

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::driver::{DocDriver, DocMetadata};

#[derive(Deserialize)]
struct Input {
    path: String,
    id: String,
    #[serde(rename = "type")]
    doc_type: String,
    title: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    relations: HashMap<String, Vec<String>>,
    body: String,
}

pub struct WriteDoc {
    pub driver: Arc<dyn DocDriver>,
}

#[async_trait]
impl Tool for WriteDoc {
    fn name(&self) -> &'static str {
        "write_doc"
    }

    fn description(&self) -> &'static str {
        "Create a documentation doc, or fully replace an existing one. Every doc lives in a \
         graph, not a tree: give it a stable `id` (e.g. \"domain.orders\", \"flow.order-\
         creation\"), a `type` (system, domain, flow, rule, entity, integration, adr, or \
         index), and link it to what it depends on or relates to. Convention for `path`: \
         00-system/ (overview, glossary, architecture, conventions), 01-domains/<name>/ \
         (overview, rules, entities, integrations), 02-flows/, 03-business-rules/, \
         04-integrations/, 05-data/, 06-decisions/ (ADRs), 99-index/. Use scaffold_docs to \
         create this skeleton in a fresh project."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Vault-relative path, e.g. \"01-domains/orders/overview.md\""},
                "id": {"type": "string", "description": "Stable id, e.g. \"domain.orders\""},
                "type": {"type": "string", "enum": ["system", "domain", "flow", "rule", "entity", "integration", "adr", "index"]},
                "title": {"type": "string"},
                "status": {"type": "string", "description": "e.g. \"active\", \"deprecated\""},
                "owner": {"type": "string"},
                "depends_on": {"type": "array", "items": {"type": "string"}, "description": "ids of docs this one depends on"},
                "relations": {
                    "type": "object",
                    "description": "Other relation kinds, e.g. {\"related_flows\": [\"flow.order-creation\"]}",
                    "additionalProperties": {"type": "array", "items": {"type": "string"}},
                },
                "body": {"type": "string", "description": "The doc's content, in markdown"},
            },
            "required": ["path", "id", "type", "title", "body"],
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        self.driver
            .write_doc(
                &input.path,
                DocMetadata {
                    id: input.id,
                    doc_type: input.doc_type,
                    title: input.title,
                    status: input.status,
                    owner: input.owner,
                    depends_on: input.depends_on,
                    relations: input.relations,
                },
                input.body,
            )
            .await?;

        Ok(format!("Wrote {}", input.path).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obsidian::ObsidianDriver;

    #[tokio::test]
    async fn writes_a_doc_with_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let driver = Arc::new(ObsidianDriver::new(dir.path()).unwrap());
        let ctx = ToolContext::new(dir.path()).unwrap();
        let tool = WriteDoc {
            driver: driver.clone(),
        };

        let result = tool
            .call(
                json!({
                    "path": "01-domains/orders/overview.md",
                    "id": "domain.orders",
                    "type": "domain",
                    "title": "Orders",
                    "depends_on": ["domain.customers"],
                    "body": "The orders domain.",
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.text.contains("01-domains/orders/overview.md"));
        let (meta, body) = driver
            .read_doc("01-domains/orders/overview.md")
            .await
            .unwrap();
        assert_eq!(meta.id, "domain.orders");
        assert_eq!(meta.depends_on, vec!["domain.customers".to_owned()]);
        assert_eq!(body, "The orders domain.");
    }
}
