//! Render/parse a doc's leading YAML frontmatter block.
//!
//! [`render`] and [`parse`] are exact inverses of each other by
//! construction — every doc this crate writes goes through [`render`], so
//! [`parse`] only ever needs to handle that one format, not general
//! Jekyll/Obsidian frontmatter in the wild.

use crate::driver::DocMetadata;

/// `---\n{yaml}---\n\n{body}` — the blank line after the closing `---` is
/// what Obsidian (and most other tools) expect between frontmatter and body.
pub fn render(meta: &DocMetadata, body: &str) -> String {
    let yaml = serde_yaml::to_string(meta).expect("DocMetadata always serializes to YAML");
    format!("---\n{yaml}---\n\n{body}")
}

pub fn parse(content: &str) -> Result<(DocMetadata, String), String> {
    let rest = content
        .strip_prefix("---\n")
        .ok_or_else(|| "doc is missing YAML frontmatter (must start with '---')".to_owned())?;
    let (yaml, body) = rest
        .split_once("\n---\n")
        .ok_or_else(|| "doc frontmatter has no closing '---'".to_owned())?;
    let meta: DocMetadata =
        serde_yaml::from_str(yaml).map_err(|error| format!("invalid frontmatter: {error}"))?;
    // `render` always inserts a blank line after the closing delimiter.
    let body = body.strip_prefix('\n').unwrap_or(body);
    Ok((meta, body.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn sample_meta() -> DocMetadata {
        DocMetadata {
            id: "domain.orders".to_owned(),
            doc_type: "domain".to_owned(),
            title: "Orders".to_owned(),
            status: Some("active".to_owned()),
            owner: None,
            depends_on: vec!["domain.customers".to_owned()],
            relations: HashMap::from([(
                "related_flows".to_owned(),
                vec!["flow.order-creation".to_owned()],
            )]),
        }
    }

    #[test]
    fn round_trips_metadata_and_body() {
        let rendered = render(&sample_meta(), "Some body text.\n\nMore text.");

        let (meta, body) = parse(&rendered).unwrap();

        assert_eq!(meta, sample_meta());
        assert_eq!(body, "Some body text.\n\nMore text.");
    }

    #[test]
    fn round_trips_an_empty_body() {
        let rendered = render(&sample_meta(), "");

        let (_meta, body) = parse(&rendered).unwrap();

        assert_eq!(body, "");
    }

    #[test]
    fn rejects_content_with_no_frontmatter() {
        let error = parse("just some text").unwrap_err();

        assert!(error.contains("frontmatter"), "got: {error}");
    }

    #[test]
    fn rejects_frontmatter_with_no_closing_delimiter() {
        let error = parse("---\nid: x\ntype: domain\ntitle: X\n").unwrap_err();

        assert!(error.contains("closing"), "got: {error}");
    }
}
