//! Formatting helpers shared by more than one tool — currently just the
//! issue-comment shape, since GitHub's `.../issues/{n}/comments` endpoint
//! is hit both by a pull request (PRs are issues under the hood) and by a
//! plain issue.

use serde_json::Value;

pub(crate) fn author(value: &Value) -> &str {
    value["user"]["login"].as_str().unwrap_or("unknown")
}

pub(crate) fn created_at(value: &Value) -> String {
    value["created_at"].as_str().unwrap_or_default().to_owned()
}

pub(crate) fn format_issue_comment(comment: &Value) -> String {
    format!(
        "[comment] {} at {}:\n{}",
        author(comment),
        created_at(comment),
        comment["body"].as_str().unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_an_issue_comment() {
        let comment = json!({
            "user": {"login": "pedro"},
            "created_at": "2026-01-01T00:00:00Z",
            "body": "Looks good to me.",
        });

        assert_eq!(
            format_issue_comment(&comment),
            "[comment] pedro at 2026-01-01T00:00:00Z:\nLooks good to me."
        );
    }

    #[test]
    fn author_falls_back_to_unknown_when_the_user_is_missing() {
        assert_eq!(author(&json!({})), "unknown");
    }
}
