//! Read a pull request, and everything said about it.

use std::sync::Arc;

use architect_tools::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::GitHubClient;
use crate::format::{author, created_at, format_issue_comment};

#[derive(Deserialize)]
struct Input {
    owner: String,
    repo: String,
    pull_number: u64,
}

fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "owner": {"type": "string", "description": "Repository owner or organization"},
            "repo": {"type": "string", "description": "Repository name"},
            "pull_number": {"type": "integer", "description": "Pull request number"},
        },
        "required": ["owner", "repo", "pull_number"],
        "additionalProperties": false,
    })
}

pub struct ReadPullRequest {
    pub client: Arc<GitHubClient>,
}

#[async_trait]
impl Tool for ReadPullRequest {
    fn name(&self) -> &'static str {
        "github_read_pull_request"
    }

    fn description(&self) -> &'static str {
        "Read a GitHub pull request: title, description, author, status, base/head branches, \
         and diff stats."
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let pr = self
            .client
            .get(&format!(
                "/repos/{}/{}/pulls/{}",
                input.owner, input.repo, input.pull_number
            ))
            .await?;

        Ok(format_pull_request(&pr).into())
    }
}

pub struct ReadPullRequestComments {
    pub client: Arc<GitHubClient>,
}

#[async_trait]
impl Tool for ReadPullRequestComments {
    fn name(&self) -> &'static str {
        "github_read_pull_request_comments"
    }

    fn description(&self) -> &'static str {
        "Read a GitHub pull request's conversation comments, inline review comments, and review \
         summaries — merged into one chronological list, the same way a human sees them combined \
         in GitHub's own PR view."
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;
        let base = format!("/repos/{}/{}", input.owner, input.repo);

        let issue_comments = self
            .client
            .get_all_pages(&format!("{base}/issues/{}/comments", input.pull_number))
            .await?;
        let review_comments = self
            .client
            .get_all_pages(&format!("{base}/pulls/{}/comments", input.pull_number))
            .await?;
        let reviews = self
            .client
            .get_all_pages(&format!("{base}/pulls/{}/reviews", input.pull_number))
            .await?;

        let mut entries: Vec<(String, String)> = Vec::new();
        entries.extend(
            issue_comments
                .iter()
                .map(|comment| (created_at(comment), format_issue_comment(comment))),
        );
        entries.extend(
            review_comments
                .iter()
                .map(|comment| (created_at(comment), format_review_comment(comment))),
        );
        entries.extend(reviews.iter().map(|review| {
            (
                review["submitted_at"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                format_review(review),
            )
        }));

        if entries.is_empty() {
            return Ok("No comments or reviews on this pull request."
                .to_owned()
                .into());
        }

        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(entries
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
            .join("\n\n")
            .into())
    }
}

pub struct ReadPullRequestDiff {
    pub client: Arc<GitHubClient>,
}

#[async_trait]
impl Tool for ReadPullRequestDiff {
    fn name(&self) -> &'static str {
        "github_read_pull_request_diff"
    }

    fn description(&self) -> &'static str {
        "Read a GitHub pull request's actual code changes: per-file status and a unified diff \
         patch for each changed file. GitHub omits the patch for very large or binary files."
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let files = self
            .client
            .get_all_pages(&format!(
                "/repos/{}/{}/pulls/{}/files",
                input.owner, input.repo, input.pull_number
            ))
            .await?;

        if files.is_empty() {
            return Ok("No files changed.".to_owned().into());
        }

        Ok(files
            .iter()
            .map(format_file_diff)
            .collect::<Vec<_>>()
            .join("\n\n")
            .into())
    }
}

pub struct ReadPullRequestCommits {
    pub client: Arc<GitHubClient>,
}

#[async_trait]
impl Tool for ReadPullRequestCommits {
    fn name(&self) -> &'static str {
        "github_read_pull_request_commits"
    }

    fn description(&self) -> &'static str {
        "Read the list of commits on a GitHub pull request: each commit's SHA, author, date, \
         and full commit message."
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let commits = self
            .client
            .get_all_pages(&format!(
                "/repos/{}/{}/pulls/{}/commits",
                input.owner, input.repo, input.pull_number
            ))
            .await?;

        if commits.is_empty() {
            return Ok("No commits.".to_owned().into());
        }

        Ok(commits
            .iter()
            .map(format_commit)
            .collect::<Vec<_>>()
            .join("\n\n")
            .into())
    }
}

fn format_file_diff(file: &Value) -> String {
    let status = file["status"].as_str().unwrap_or("modified");
    let filename = file["filename"].as_str().unwrap_or("?");
    let additions = file["additions"].as_u64().unwrap_or(0);
    let deletions = file["deletions"].as_u64().unwrap_or(0);

    let header = match file["previous_filename"].as_str() {
        Some(previous) => format!("{status}: {previous} -> {filename} (+{additions} -{deletions})"),
        None => format!("{status}: {filename} (+{additions} -{deletions})"),
    };

    match file["patch"].as_str() {
        Some(patch) => format!("{header}\n{patch}"),
        None => format!(
            "{header}\n[diff not available — file is binary or too large for GitHub to include \
             a patch]"
        ),
    }
}

fn format_commit(commit: &Value) -> String {
    let sha = commit["sha"].as_str().unwrap_or("???????");
    let short_sha = &sha[..sha.len().min(7)];
    let message = commit["commit"]["message"].as_str().unwrap_or_default();
    let date = commit["commit"]["author"]["date"].as_str().unwrap_or("");

    let author = commit["author"]["login"]
        .as_str()
        .or_else(|| commit["commit"]["author"]["name"].as_str())
        .unwrap_or("unknown");

    format!("{short_sha} by {author} at {date}:\n{message}")
}

fn format_pull_request(pr: &Value) -> String {
    let title = pr["title"].as_str().unwrap_or_default();
    let status = if pr["merged"].as_bool().unwrap_or(false) {
        "merged".to_owned()
    } else {
        pr["state"].as_str().unwrap_or("unknown").to_owned()
    };
    let base = pr["base"]["ref"].as_str().unwrap_or("?");
    let head = pr["head"]["ref"].as_str().unwrap_or("?");
    let additions = pr["additions"].as_u64().unwrap_or(0);
    let deletions = pr["deletions"].as_u64().unwrap_or(0);
    let changed_files = pr["changed_files"].as_u64().unwrap_or(0);
    let body = pr["body"].as_str().unwrap_or_default().trim();
    let url = pr["html_url"].as_str().unwrap_or_default();

    format!(
        "{title}\nby {} — {status} — {base} \u{2190} {head}\n\
         +{additions} -{deletions} across {changed_files} file(s)\n{url}\n\n{body}",
        author(pr),
    )
}

fn format_review_comment(comment: &Value) -> String {
    let path = comment["path"].as_str().unwrap_or("?");
    let location = match comment["line"].as_u64() {
        Some(line) => format!("{path}:{line}"),
        None => path.to_owned(),
    };

    format!(
        "[review comment on {location}] {} at {}:\n{}",
        author(comment),
        created_at(comment),
        comment["body"].as_str().unwrap_or_default(),
    )
}

fn format_review(review: &Value) -> String {
    let state = review["state"].as_str().unwrap_or("COMMENTED");
    let submitted_at = review["submitted_at"].as_str().unwrap_or_default();
    let body = review["body"].as_str().unwrap_or_default().trim();

    if body.is_empty() {
        format!("[review: {state}] {} at {submitted_at}", author(review))
    } else {
        format!(
            "[review: {state}] {} at {submitted_at}:\n{body}",
            author(review)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_pull_request_summary() {
        let pr = json!({
            "title": "Add rollback UI",
            "user": {"login": "pedro"},
            "state": "open",
            "merged": false,
            "base": {"ref": "main"},
            "head": {"ref": "feature/rollback"},
            "additions": 120,
            "deletions": 4,
            "changed_files": 6,
            "body": "Wires up the Rollback button.",
            "html_url": "https://github.com/o/r/pull/1",
        });

        let text = format_pull_request(&pr);
        assert!(text.contains("Add rollback UI"));
        assert!(text.contains("by pedro"));
        assert!(text.contains("open"));
        assert!(text.contains("main \u{2190} feature/rollback"));
        assert!(text.contains("+120 -4 across 6 file(s)"));
        assert!(text.contains("Wires up the Rollback button."));
    }

    #[test]
    fn a_merged_pull_request_says_merged_not_closed() {
        let pr = json!({
            "title": "x", "user": {"login": "a"}, "state": "closed", "merged": true,
            "base": {"ref": "main"}, "head": {"ref": "x"},
            "additions": 1, "deletions": 1, "changed_files": 1,
            "body": "", "html_url": "",
        });

        assert!(format_pull_request(&pr).contains("merged"));
        assert!(!format_pull_request(&pr).contains("closed"));
    }

    #[test]
    fn formats_a_normal_file_diff() {
        let file = json!({
            "status": "modified",
            "filename": "src/lib.rs",
            "additions": 3,
            "deletions": 1,
            "patch": "@@ -1,3 +1,5 @@\n+use std::fmt;\n",
        });

        assert_eq!(
            format_file_diff(&file),
            "modified: src/lib.rs (+3 -1)\n@@ -1,3 +1,5 @@\n+use std::fmt;\n"
        );
    }

    #[test]
    fn a_renamed_file_shows_both_names() {
        let file = json!({
            "status": "renamed",
            "filename": "src/new.rs",
            "previous_filename": "src/old.rs",
            "additions": 0,
            "deletions": 0,
            "patch": "",
        });

        assert!(format_file_diff(&file).starts_with("renamed: src/old.rs -> src/new.rs (+0 -0)"));
    }

    #[test]
    fn a_file_with_no_patch_says_so_instead_of_showing_nothing() {
        let file = json!({
            "status": "modified",
            "filename": "assets/logo.png",
            "additions": 0,
            "deletions": 0,
        });

        assert!(format_file_diff(&file).contains(
            "[diff not available — file is binary or too large for GitHub to include a patch]"
        ));
    }

    #[test]
    fn formats_a_commit_with_a_linked_github_login() {
        let commit = json!({
            "sha": "abcdef1234567890",
            "author": {"login": "pedro"},
            "commit": {
                "message": "Add rollback UI",
                "author": {"name": "Pedro Soares", "date": "2026-01-01T00:00:00Z"},
            },
        });

        assert_eq!(
            format_commit(&commit),
            "abcdef1 by pedro at 2026-01-01T00:00:00Z:\nAdd rollback UI"
        );
    }

    #[test]
    fn a_commit_with_no_linked_account_falls_back_to_the_git_author_name() {
        let commit = json!({
            "sha": "abcdef1234567890",
            "author": Value::Null,
            "commit": {
                "message": "Add rollback UI",
                "author": {"name": "Pedro Soares", "date": "2026-01-01T00:00:00Z"},
            },
        });

        assert!(format_commit(&commit).contains("by Pedro Soares"));
    }

    #[test]
    fn review_comments_show_their_file_and_line() {
        let comment = json!({
            "user": {"login": "a"}, "created_at": "2026-01-01T00:00:00Z",
            "path": "src/lib.rs", "line": 42, "body": "nit: rename this",
        });

        assert_eq!(
            format_review_comment(&comment),
            "[review comment on src/lib.rs:42] a at 2026-01-01T00:00:00Z:\nnit: rename this"
        );
    }

    #[test]
    fn a_review_with_no_body_still_shows_its_verdict() {
        let review = json!({
            "user": {"login": "a"}, "state": "APPROVED", "submitted_at": "2026-01-01T00:00:00Z",
            "body": "",
        });

        assert_eq!(
            format_review(&review),
            "[review: APPROVED] a at 2026-01-01T00:00:00Z"
        );
    }
}
