//! [`ObsidianDriver`]: a local folder of markdown files, readable and
//! writable directly by Obsidian — the v1 [`crate::DocDriver`].

use std::{
    io,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use ignore::WalkBuilder;
use similar::TextDiff;

use crate::{
    driver::{DocDriver, DocMetadata, DocSearchHit, DocSummary},
    frontmatter,
};

/// A vault is just a directory; every doc tool's `path` is resolved against
/// its own canonicalized root, sandboxed the same way `architect_tools::
/// ToolContext::resolve` sandboxes the workspace root — this driver has no
/// `ToolContext` of its own (a vault may live outside the turn's workspace
/// entirely), so the same check is reimplemented here rather than shared.
pub struct ObsidianDriver {
    root: PathBuf,
}

impl ObsidianDriver {
    /// Creates `vault_path` if it does not exist yet, then canonicalizes it.
    pub fn new(vault_path: impl Into<PathBuf>) -> io::Result<Self> {
        let vault_path = vault_path.into();
        std::fs::create_dir_all(&vault_path)?;
        Ok(Self {
            root: vault_path.canonicalize()?,
        })
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let requested = self.root.join(path);

        let existing = requested
            .ancestors()
            .find(|ancestor| ancestor.exists())
            .ok_or_else(|| format!("{path:?} is not inside the vault"))?;

        let canonical_existing = existing
            .canonicalize()
            .map_err(|error| format!("could not resolve {path:?}: {error}"))?;

        if !canonical_existing.starts_with(&self.root) {
            return Err(format!("{path:?} is outside the vault"));
        }

        let suffix = requested.strip_prefix(existing).unwrap_or(Path::new(""));
        if suffix.as_os_str().is_empty() {
            Ok(canonical_existing)
        } else {
            Ok(canonical_existing.join(suffix))
        }
    }

    fn display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

#[async_trait]
impl DocDriver for ObsidianDriver {
    fn kind(&self) -> &'static str {
        "obsidian"
    }

    async fn write_doc(&self, path: &str, meta: DocMetadata, body: String) -> Result<(), String> {
        let resolved = self.resolve(path)?;
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        tokio::fs::write(&resolved, frontmatter::render(&meta, &body))
            .await
            .map_err(|error| format!("could not write {path:?}: {error}"))
    }

    async fn edit_doc(
        &self,
        path: &str,
        old: &str,
        new: &str,
        replace_all: bool,
    ) -> Result<String, String> {
        let resolved = self.resolve(path)?;
        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|error| format!("could not read {path:?}: {error}"))?;
        let (meta, old_body) = frontmatter::parse(&content)?;

        let occurrences = old_body.matches(old).count();
        if occurrences == 0 {
            return Err(format!("old text was not found in {path:?}"));
        }
        if occurrences > 1 && !replace_all {
            return Err(format!(
                "old text appears {occurrences} times in {path:?} — pass replace_all or include \
                 more context to make it unique"
            ));
        }

        let new_body = if replace_all {
            old_body.replace(old, new)
        } else {
            old_body.replacen(old, new, 1)
        };

        tokio::fs::write(&resolved, frontmatter::render(&meta, &new_body))
            .await
            .map_err(|error| format!("could not write {path:?}: {error}"))?;

        Ok(TextDiff::from_lines(&old_body, &new_body)
            .unified_diff()
            .context_radius(3)
            .header(path, path)
            .to_string())
    }

    async fn read_doc(&self, path: &str) -> Result<(DocMetadata, String), String> {
        let resolved = self.resolve(path)?;
        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|error| format!("could not read {path:?}: {error}"))?;
        frontmatter::parse(&content)
    }

    async fn search_docs(
        &self,
        query: &str,
        doc_type: Option<&str>,
    ) -> Result<Vec<DocSearchHit>, String> {
        let mut hits = Vec::new();

        for path in self.walk_markdown_files() {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok((meta, body)) = frontmatter::parse(&content) else {
                continue;
            };
            if let Some(doc_type) = doc_type
                && meta.doc_type != doc_type
            {
                continue;
            }

            let snippet = if contains_ci(&meta.id, query) {
                format!("(matched id) {}", meta.id)
            } else if contains_ci(&meta.title, query) {
                format!("(matched title) {}", meta.title)
            } else if let Some(snippet) = snippet_for(&body, query) {
                snippet
            } else {
                continue;
            };

            hits.push(DocSearchHit {
                id: meta.id,
                doc_type: meta.doc_type,
                title: meta.title,
                path: self.display(&path),
                snippet,
            });
        }

        Ok(hits)
    }

    async fn list_docs(&self, doc_type: Option<&str>) -> Result<Vec<DocSummary>, String> {
        let mut summaries = Vec::new();

        for path in self.walk_markdown_files() {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok((meta, _body)) = frontmatter::parse(&content) else {
                continue;
            };
            if let Some(doc_type) = doc_type
                && meta.doc_type != doc_type
            {
                continue;
            }

            summaries.push(DocSummary {
                id: meta.id,
                doc_type: meta.doc_type,
                title: meta.title,
                path: self.display(&path),
                depends_on: meta.depends_on,
            });
        }

        Ok(summaries)
    }
}

impl ObsidianDriver {
    fn walk_markdown_files(&self) -> Vec<PathBuf> {
        WalkBuilder::new(&self.root)
            .hidden(false)
            .require_git(false)
            .build()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
            .map(|entry| entry.path().to_path_buf())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
            .collect()
    }
}

/// Case-insensitive substring search over `char`s rather than bytes, so a
/// non-ASCII query never splits a multi-byte character mid-match.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    find_ci(haystack, needle).is_some()
}

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let haystack: Vec<char> = haystack.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| {
        window
            .iter()
            .zip(&needle)
            .all(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
    })
}

fn snippet_for(body: &str, query: &str) -> Option<String> {
    let chars: Vec<char> = body.chars().collect();
    let idx = find_ci(body, query)?;
    let start = idx.saturating_sub(40);
    let end = (idx + query.chars().count() + 40).min(chars.len());
    let text: String = chars[start..end].iter().collect();
    Some(format!("...{}...", text.trim()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn meta(id: &str, doc_type: &str, title: &str) -> DocMetadata {
        DocMetadata {
            id: id.to_owned(),
            doc_type: doc_type.to_owned(),
            title: title.to_owned(),
            status: None,
            owner: None,
            depends_on: Vec::new(),
            relations: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn writes_and_reads_a_doc_back() {
        let dir = tempfile::tempdir().unwrap();
        let driver = ObsidianDriver::new(dir.path()).unwrap();

        driver
            .write_doc(
                "01-domains/orders/overview.md",
                meta("domain.orders", "domain", "Orders"),
                "The orders domain.".to_owned(),
            )
            .await
            .unwrap();

        let (read_meta, body) = driver
            .read_doc("01-domains/orders/overview.md")
            .await
            .unwrap();
        assert_eq!(read_meta.id, "domain.orders");
        assert_eq!(body, "The orders domain.");
    }

    #[tokio::test]
    async fn write_doc_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let driver = ObsidianDriver::new(dir.path()).unwrap();

        driver
            .write_doc(
                "02-flows/checkout.md",
                meta("flow.checkout", "flow", "Checkout"),
                String::new(),
            )
            .await
            .unwrap();

        assert!(dir.path().join("02-flows/checkout.md").exists());
    }

    #[tokio::test]
    async fn rejects_a_path_that_escapes_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        let driver = ObsidianDriver::new(dir.path()).unwrap();

        let error = driver
            .write_doc("../escape.md", meta("x", "domain", "X"), String::new())
            .await
            .unwrap_err();

        assert!(
            error.contains("vault") || error.contains("resolve"),
            "got: {error}"
        );
    }

    #[tokio::test]
    async fn edit_doc_replaces_a_unique_match_and_leaves_frontmatter_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let driver = ObsidianDriver::new(dir.path()).unwrap();
        driver
            .write_doc("a.md", meta("a", "domain", "A"), "old text here".to_owned())
            .await
            .unwrap();

        driver.edit_doc("a.md", "old", "new", false).await.unwrap();

        let (read_meta, body) = driver.read_doc("a.md").await.unwrap();
        assert_eq!(body, "new text here");
        assert_eq!(read_meta.id, "a");
    }

    #[tokio::test]
    async fn edit_doc_refuses_an_ambiguous_match_without_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let driver = ObsidianDriver::new(dir.path()).unwrap();
        driver
            .write_doc("a.md", meta("a", "domain", "A"), "dup dup".to_owned())
            .await
            .unwrap();

        let error = driver
            .edit_doc("a.md", "dup", "x", false)
            .await
            .unwrap_err();

        assert!(error.contains("replace_all"), "got: {error}");
    }

    #[tokio::test]
    async fn search_docs_matches_by_id_title_and_body() {
        let dir = tempfile::tempdir().unwrap();
        let driver = ObsidianDriver::new(dir.path()).unwrap();
        driver
            .write_doc(
                "orders.md",
                meta("domain.orders", "domain", "Orders"),
                "Customers must be active to place an order.".to_owned(),
            )
            .await
            .unwrap();

        let by_id = driver.search_docs("domain.orders", None).await.unwrap();
        assert_eq!(by_id.len(), 1);

        let by_title = driver.search_docs("Orders", None).await.unwrap();
        assert_eq!(by_title.len(), 1);

        let by_body = driver.search_docs("must be active", None).await.unwrap();
        assert_eq!(by_body.len(), 1);
        assert!(by_body[0].snippet.contains("must be active"));

        let no_match = driver.search_docs("nonexistent", None).await.unwrap();
        assert!(no_match.is_empty());
    }

    #[tokio::test]
    async fn search_docs_filters_by_doc_type() {
        let dir = tempfile::tempdir().unwrap();
        let driver = ObsidianDriver::new(dir.path()).unwrap();
        driver
            .write_doc("a.md", meta("domain.a", "domain", "Match"), String::new())
            .await
            .unwrap();
        driver
            .write_doc("b.md", meta("flow.b", "flow", "Match"), String::new())
            .await
            .unwrap();

        let hits = driver.search_docs("Match", Some("flow")).await.unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "flow.b");
    }

    #[tokio::test]
    async fn list_docs_returns_depends_on() {
        let dir = tempfile::tempdir().unwrap();
        let driver = ObsidianDriver::new(dir.path()).unwrap();
        let mut with_deps = meta("domain.orders", "domain", "Orders");
        with_deps.depends_on = vec!["domain.customers".to_owned()];
        driver
            .write_doc("orders.md", with_deps, String::new())
            .await
            .unwrap();

        let summaries = driver.list_docs(None).await.unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].depends_on, vec!["domain.customers".to_owned()]);
    }

    #[tokio::test]
    async fn list_docs_filters_by_doc_type() {
        let dir = tempfile::tempdir().unwrap();
        let driver = ObsidianDriver::new(dir.path()).unwrap();
        driver
            .write_doc("a.md", meta("domain.a", "domain", "A"), String::new())
            .await
            .unwrap();
        driver
            .write_doc("b.md", meta("flow.b", "flow", "B"), String::new())
            .await
            .unwrap();

        let domains = driver.list_docs(Some("domain")).await.unwrap();

        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].id, "domain.a");
    }
}
