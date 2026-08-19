//! Idempotently seed the default knowledge-base skeleton.

use std::{collections::HashMap, sync::Arc};

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{
    driver::{DocDriver, DocMetadata},
    scaffold,
};

pub struct ScaffoldDocs {
    pub driver: Arc<dyn DocDriver>,
}

#[async_trait]
impl Tool for ScaffoldDocs {
    fn name(&self) -> &'static str {
        "scaffold_docs"
    }

    fn description(&self) -> &'static str {
        "Create the default documentation folder structure (00-system, 01-domains, 02-flows, \
         03-business-rules, 04-integrations, 05-data, 06-decisions, 99-index) if it doesn't \
         exist yet. Safe to call more than once — an existing doc at one of these paths is \
         left untouched."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, _input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let mut created = Vec::new();
        let mut skipped = Vec::new();

        for doc in scaffold::skeleton() {
            if self.driver.read_doc(doc.path).await.is_ok() {
                skipped.push(doc.path);
                continue;
            }

            self.driver
                .write_doc(
                    doc.path,
                    DocMetadata {
                        id: doc.id.to_owned(),
                        doc_type: doc.doc_type.to_owned(),
                        title: doc.title.to_owned(),
                        status: None,
                        owner: None,
                        depends_on: Vec::new(),
                        relations: HashMap::new(),
                    },
                    doc.body.to_owned(),
                )
                .await?;
            created.push(doc.path);
        }

        let mut summary = if created.is_empty() {
            "Nothing to create — the skeleton already exists.".to_owned()
        } else {
            format!("Created:\n{}", created.join("\n"))
        };
        if !skipped.is_empty() {
            summary.push_str(&format!(
                "\n\nAlready present, left untouched:\n{}",
                skipped.join("\n")
            ));
        }

        Ok(summary.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obsidian::ObsidianDriver;

    #[tokio::test]
    async fn creates_the_full_skeleton() {
        let dir = tempfile::tempdir().unwrap();
        let driver = Arc::new(ObsidianDriver::new(dir.path()).unwrap());
        let ctx = ToolContext::new(dir.path()).unwrap();
        let tool = ScaffoldDocs {
            driver: driver.clone(),
        };

        tool.call(json!({}), &ctx).await.unwrap();

        for doc in scaffold::skeleton() {
            assert!(
                driver.read_doc(doc.path).await.is_ok(),
                "missing {}",
                doc.path
            );
        }
    }

    #[tokio::test]
    async fn is_idempotent_and_does_not_clobber_edits() {
        let dir = tempfile::tempdir().unwrap();
        let driver = Arc::new(ObsidianDriver::new(dir.path()).unwrap());
        let ctx = ToolContext::new(dir.path()).unwrap();
        let tool = ScaffoldDocs {
            driver: driver.clone(),
        };

        tool.call(json!({}), &ctx).await.unwrap();
        driver
            .edit_doc(
                "00-system/overview.md",
                "who it's for.",
                "who it's for. Edited by hand.",
                false,
            )
            .await
            .unwrap();

        let second = tool.call(json!({}), &ctx).await.unwrap();

        let (_meta, body) = driver.read_doc("00-system/overview.md").await.unwrap();
        assert!(body.contains("Edited by hand."));
        assert!(second.text.contains("Already present"));
    }
}
