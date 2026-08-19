//! The pluggable documentation backend every doc tool talks to.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The YAML frontmatter every doc carries — a stable `id` plus explicit
/// links to other docs (`depends_on`, and any other relation kind under
/// `relations`, e.g. `related_flows`/`related_rules`/`related_entities`),
/// so the knowledge base forms a navigable graph rather than a pile of
/// unstructured prose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocMetadata {
    /// e.g. `"domain.orders"`, `"flow.order-creation"` — unique across the
    /// whole knowledge base, stable across renames/moves.
    pub id: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub relations: HashMap<String, Vec<String>>,
}

/// One `search_docs` match.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DocSearchHit {
    pub id: String,
    pub doc_type: String,
    pub title: String,
    pub path: String,
    pub snippet: String,
}

/// One `list_docs` row — the structural index a model can traverse via
/// `depends_on` without a separate "domain map" tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DocSummary {
    pub id: String,
    pub doc_type: String,
    pub title: String,
    pub path: String,
    pub depends_on: Vec<String>,
}

/// A backend that can store and retrieve documentation.
///
/// [`crate::obsidian::ObsidianDriver`] is the v1 implementation — a local
/// markdown vault, no credentials needed. Notion and Jira drivers follow
/// later behind this same trait, which is why every method takes plain
/// strings/structs rather than anything filesystem-shaped: a `path` here is
/// a driver-defined identifier (a vault-relative file path for Obsidian, a
/// page id for Notion), not necessarily a real filesystem path.
#[async_trait]
pub trait DocDriver: Send + Sync {
    /// `"obsidian"` | `"notion"` | `"jira"`.
    fn kind(&self) -> &'static str;

    /// Create a doc, or fully replace one already at `path`.
    async fn write_doc(&self, path: &str, meta: DocMetadata, body: String) -> Result<(), String>;

    /// Replace an exact substring within a doc's body — the frontmatter is
    /// left untouched. Same contract as `architect_tools`' `edit_file`:
    /// fails if `old` is not found, or is ambiguous unless `replace_all` is
    /// set. Returns a unified diff of the body.
    async fn edit_doc(
        &self,
        path: &str,
        old: &str,
        new: &str,
        replace_all: bool,
    ) -> Result<String, String>;

    async fn read_doc(&self, path: &str) -> Result<(DocMetadata, String), String>;

    /// Case-insensitive match against a doc's `id`, `title`, and body,
    /// optionally restricted to one `doc_type`.
    async fn search_docs(
        &self,
        query: &str,
        doc_type: Option<&str>,
    ) -> Result<Vec<DocSearchHit>, String>;

    /// Every doc's metadata, optionally restricted to one `doc_type` — the
    /// domain/dependency map, generated on demand instead of hand-maintained.
    async fn list_docs(&self, doc_type: Option<&str>) -> Result<Vec<DocSummary>, String>;
}
