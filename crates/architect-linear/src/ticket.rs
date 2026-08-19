//! Read a ticket: its description, comments, and blocking relations, in one
//! call — Linear's GraphQL API fetches all of it in a single round-trip.

use std::sync::Arc;

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::LinearClient;

/// `relations`/`inverseRelations` carry every relation type Linear has
/// (`duplicate`, `related`, `similar` too) — only `"blocks"` is a blocking
/// relationship, which is what this tool reports.
const BLOCKS: &str = "blocks";

const QUERY: &str = r#"
query Issue($id: String!) {
  issue(id: $id) {
    identifier
    title
    description
    priority
    state { name }
    assignee { name }
    comments(first: 50) {
      nodes { body createdAt user { name } }
    }
    relations(first: 50) {
      nodes { type relatedIssue { identifier title } }
    }
    inverseRelations(first: 50) {
      nodes { type issue { identifier title } }
    }
  }
}
"#;

#[derive(Deserialize)]
struct Input {
    id: String,
}

pub struct ReadTicket {
    pub client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for ReadTicket {
    fn name(&self) -> &'static str {
        "linear_read_ticket"
    }

    fn description(&self) -> &'static str {
        "Read a Linear ticket: its title, description, state, assignee, comments, and what it's \
         blocked by or blocking."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The ticket's identifier (e.g. ENG-123) or its Linear id",
                },
            },
            "required": ["id"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let data = self.client.query(QUERY, json!({"id": input.id})).await?;
        let issue = &data["issue"];
        if issue.is_null() {
            return Err(format!("no Linear ticket found for {:?}", input.id));
        }

        Ok(format_ticket(issue).into())
    }
}

fn format_ticket(issue: &Value) -> String {
    let identifier = issue["identifier"].as_str().unwrap_or("?");
    let title = issue["title"].as_str().unwrap_or_default();
    let state = issue["state"]["name"].as_str().unwrap_or("unknown");
    let assignee = issue["assignee"]["name"].as_str().unwrap_or("Unassigned");
    let priority = issue["priority"].as_f64().unwrap_or(0.0);
    let description = issue["description"].as_str().unwrap_or_default().trim();

    let mut sections = vec![format!(
        "{identifier}: {title}\nState: {state}    Assignee: {assignee}    Priority: {priority}\n\n{description}"
    )];

    let blocked_by = relation_lines(&issue["inverseRelations"]["nodes"], "issue");
    if !blocked_by.is_empty() {
        sections.push(format!("Blocked by:\n{}", blocked_by.join("\n")));
    }

    let blocks = relation_lines(&issue["relations"]["nodes"], "relatedIssue");
    if !blocks.is_empty() {
        sections.push(format!("Blocks:\n{}", blocks.join("\n")));
    }

    sections.push(format_comments(&issue["comments"]["nodes"]));

    sections.join("\n\n")
}

/// `field` is `"issue"` for `inverseRelations` or `"relatedIssue"` for
/// `relations` — the other issue in the relationship sits under a
/// different field name on each connection.
fn relation_lines(nodes: &Value, field: &str) -> Vec<String> {
    nodes
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["type"].as_str() == Some(BLOCKS))
        .map(|node| {
            format!(
                "- {}: {}",
                node[field]["identifier"].as_str().unwrap_or("?"),
                node[field]["title"].as_str().unwrap_or_default(),
            )
        })
        .collect()
}

fn format_comments(nodes: &Value) -> String {
    let comments = nodes.as_array().cloned().unwrap_or_default();
    if comments.is_empty() {
        return "No comments.".to_owned();
    }

    let lines: Vec<String> = comments
        .iter()
        .map(|comment| {
            format!(
                "[{}] {}: {}",
                comment["createdAt"].as_str().unwrap_or_default(),
                comment["user"]["name"].as_str().unwrap_or("unknown"),
                comment["body"].as_str().unwrap_or_default(),
            )
        })
        .collect();

    format!("Comments:\n{}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_issue() -> Value {
        json!({
            "identifier": "ENG-123",
            "title": "Fix the layout bug",
            "state": {"name": "In Progress"},
            "assignee": {"name": "Pedro"},
            "priority": 2.0,
            "description": "Panel overlaps on narrow windows.",
            "comments": {"nodes": [
                {"body": "Repro'd on 1024px", "createdAt": "2026-01-01T00:00:00Z", "user": {"name": "Ana"}},
            ]},
            "relations": {"nodes": [
                {"type": "blocks", "relatedIssue": {"identifier": "ENG-200", "title": "Release v2"}},
                {"type": "related", "relatedIssue": {"identifier": "ENG-9", "title": "Unrelated-ish"}},
            ]},
            "inverseRelations": {"nodes": [
                {"type": "blocks", "issue": {"identifier": "ENG-100", "title": "Design the fix"}},
            ]},
        })
    }

    #[test]
    fn formats_the_header_and_description() {
        let text = format_ticket(&sample_issue());
        assert!(text.contains("ENG-123: Fix the layout bug"));
        assert!(text.contains("State: In Progress"));
        assert!(text.contains("Assignee: Pedro"));
        assert!(text.contains("Panel overlaps on narrow windows."));
    }

    #[test]
    fn only_blocks_relations_are_reported_not_related_or_duplicate() {
        let text = format_ticket(&sample_issue());
        assert!(text.contains("Blocked by:\n- ENG-100: Design the fix"));
        assert!(text.contains("Blocks:\n- ENG-200: Release v2"));
        assert!(
            !text.contains("ENG-9"),
            "a merely-related issue must not appear as a blocker"
        );
    }

    #[test]
    fn comments_are_included() {
        let text = format_ticket(&sample_issue());
        assert!(text.contains("Comments:"));
        assert!(text.contains("Ana: Repro'd on 1024px"));
    }

    #[test]
    fn a_ticket_with_no_blockers_omits_both_sections() {
        let mut issue = sample_issue();
        issue["relations"]["nodes"] = json!([]);
        issue["inverseRelations"]["nodes"] = json!([]);

        let text = format_ticket(&issue);
        assert!(!text.contains("Blocked by:"));
        assert!(!text.contains("Blocks:"));
    }

    #[test]
    fn a_ticket_with_no_comments_says_so_plainly() {
        let mut issue = sample_issue();
        issue["comments"]["nodes"] = json!([]);

        assert!(format_ticket(&issue).contains("No comments."));
    }
}
