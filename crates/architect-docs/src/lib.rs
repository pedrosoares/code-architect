//! A pluggable documentation knowledge base for the agent: create, edit,
//! search, and browse a set of markdown docs carrying YAML frontmatter (a
//! stable `id`, a `type`, and explicit `depends_on`/`relations` links),
//! organized under a default folder structure — see `scaffold_docs`' tool
//! description — so a coding agent can traverse domain -> flow -> rule ->
//! entity relationships without re-deriving them from source every turn.
//!
//! [`DocDriver`] is the pluggable backend; [`obsidian::ObsidianDriver`] is
//! the v1 implementation (a local markdown folder Obsidian can open
//! directly, no credentials needed). Notion/Jira drivers follow the same
//! trait later. [`tools`] turns any driver into the six [`architect_tools::
//! Tool`]s the model calls — the same shape `architect_github::tools`/
//! `architect_slack::tools` use for their own credential-backed tool sets.

mod driver;
mod frontmatter;
pub mod obsidian;
mod scaffold;
mod tools;

use std::sync::Arc;

use architect_tools::Tool;

pub use driver::{DocDriver, DocMetadata, DocSearchHit, DocSummary};

/// Every doc tool, backed by `driver` — the engine's extension point, the
/// same role `architect_github::tools(token)` plays for GitHub credentials.
pub fn tools(driver: Arc<dyn DocDriver>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(tools::write_doc::WriteDoc {
            driver: driver.clone(),
        }),
        Arc::new(tools::edit_doc::EditDoc {
            driver: driver.clone(),
        }),
        Arc::new(tools::read_doc::ReadDoc {
            driver: driver.clone(),
        }),
        Arc::new(tools::search_docs::SearchDocs {
            driver: driver.clone(),
        }),
        Arc::new(tools::list_docs::ListDocs {
            driver: driver.clone(),
        }),
        Arc::new(tools::scaffold_docs::ScaffoldDocs { driver }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obsidian::ObsidianDriver;

    #[test]
    fn tools_returns_the_six_doc_tools() {
        let dir = tempfile::tempdir().unwrap();
        let driver = Arc::new(ObsidianDriver::new(dir.path()).unwrap());

        let mut names: Vec<&str> = tools(driver).iter().map(|tool| tool.name()).collect();
        names.sort_unstable();

        assert_eq!(
            names,
            [
                "edit_doc",
                "list_docs",
                "read_doc",
                "scaffold_docs",
                "search_docs",
                "write_doc",
            ]
        );
    }
}
