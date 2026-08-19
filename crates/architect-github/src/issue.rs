//! Read a plain GitHub issue and its comments — distinct from a pull
//! request, though GitHub's REST API shares the underlying `/issues/{n}`
//! and `/issues/{n}/comments` endpoints between the two (a PR is an issue
//! under the hood; see the `pull_request` hint in [`format_issue`]).

use std::sync::Arc;

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::GitHubClient;
use crate::format::{author, format_issue_comment};

#[derive(Deserialize)]
struct Input {
    owner: String,
    repo: String,
    issue_number: u64,
}

fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "owner": {"type": "string", "description": "Repository owner or organization"},
            "repo": {"type": "string", "description": "Repository name"},
            "issue_number": {"type": "integer", "description": "Issue number"},
        },
        "required": ["owner", "repo", "issue_number"],
        "additionalProperties": false,
    })
}

pub struct ReadIssue {
    pub client: Arc<GitHubClient>,
}

#[async_trait]
impl Tool for ReadIssue {
    fn name(&self) -> &'static str {
        "github_read_issue"
    }

    fn description(&self) -> &'static str {
        "Read a GitHub issue: title, author, state, labels, assignees, and body."
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let issue = self
            .client
            .get(&format!(
                "/repos/{}/{}/issues/{}",
                input.owner, input.repo, input.issue_number
            ))
            .await?;

        Ok(format_issue(&issue).into())
    }
}

pub struct ReadIssueComments {
    pub client: Arc<GitHubClient>,
}

#[async_trait]
impl Tool for ReadIssueComments {
    fn name(&self) -> &'static str {
        "github_read_issue_comments"
    }

    fn description(&self) -> &'static str {
        "Read every comment on a GitHub issue, in chronological order."
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let comments = self
            .client
            .get_all_pages(&format!(
                "/repos/{}/{}/issues/{}/comments",
                input.owner, input.repo, input.issue_number
            ))
            .await?;

        if comments.is_empty() {
            return Ok("No comments on this issue.".to_owned().into());
        }

        Ok(comments
            .iter()
            .map(format_issue_comment)
            .collect::<Vec<_>>()
            .join("\n\n")
            .into())
    }
}

fn format_issue(issue: &Value) -> String {
    let title = issue["title"].as_str().unwrap_or_default();
    let state = issue["state"].as_str().unwrap_or("unknown");
    let body = issue["body"].as_str().unwrap_or_default().trim();
    let url = issue["html_url"].as_str().unwrap_or_default();

    let labels = issue["labels"]
        .as_array()
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|labels| !labels.is_empty())
        .unwrap_or_else(|| "none".to_owned());

    let assignees = issue["assignees"]
        .as_array()
        .map(|assignees| {
            assignees
                .iter()
                .filter_map(|assignee| assignee["login"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|assignees| !assignees.is_empty())
        .unwrap_or_else(|| "unassigned".to_owned());

    let mut text = format!(
        "{title}\nby {} — {state}\nLabels: {labels}    Assignees: {assignees}\n{url}\n\n{body}",
        author(issue),
    );

    if !issue["pull_request"].is_null() {
        text.push_str(
            "\n\n(this issue number is actually a pull request — use github_read_pull_request \
             for diff stats and review status.)",
        );
    }

    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_an_issue_with_labels_and_assignees() {
        let issue = json!({
            "title": "Rollback fails on renamed files",
            "user": {"login": "pedro"},
            "state": "open",
            "labels": [{"name": "bug"}, {"name": "p1"}],
            "assignees": [{"login": "pedro"}],
            "body": "Steps to reproduce...",
            "html_url": "https://github.com/o/r/issues/1",
        });

        let text = format_issue(&issue);
        assert!(text.contains("Rollback fails on renamed files"));
        assert!(text.contains("by pedro — open"));
        assert!(text.contains("Labels: bug, p1"));
        assert!(text.contains("Assignees: pedro"));
        assert!(text.contains("Steps to reproduce..."));
        assert!(!text.contains("pull request"));
    }

    #[test]
    fn an_issue_with_no_labels_or_assignees_says_so() {
        let issue = json!({
            "title": "x", "user": {"login": "a"}, "state": "open",
            "labels": [], "assignees": [], "body": "", "html_url": "",
        });

        let text = format_issue(&issue);
        assert!(text.contains("Labels: none"));
        assert!(text.contains("Assignees: unassigned"));
    }

    #[test]
    fn an_issue_that_is_really_a_pull_request_says_so() {
        let issue = json!({
            "title": "x", "user": {"login": "a"}, "state": "open",
            "labels": [], "assignees": [], "body": "", "html_url": "",
            "pull_request": {"url": "https://api.github.com/repos/o/r/pulls/1"},
        });

        assert!(format_issue(&issue).contains("this issue number is actually a pull request"));
    }
}
