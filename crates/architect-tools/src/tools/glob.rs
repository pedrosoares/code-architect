//! Find files by glob pattern, newest first.

use std::time::SystemTime;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::ToolContext,
    tool::{Tool, ToolOutput},
};

#[derive(Deserialize)]
struct Input {
    pattern: String,
}

pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &'static str {
        "glob"
    }

    fn description(&self) -> &'static str {
        "Find files by glob pattern (*, **, ?, [...], and {a,b} alternation), sorted newest-modified first."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "e.g. \"src/**/*.rs\" or \"**/*.{ts,tsx}\""},
            },
            "required": ["pattern"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;

        let mut matches: Vec<(std::path::PathBuf, SystemTime)> = Vec::new();

        for pattern in expand_braces(&input.pattern) {
            let rooted = ctx.workspace_root().join(&pattern).display().to_string();
            let paths =
                ::glob::glob(&rooted).map_err(|error| format!("invalid pattern: {error}"))?;

            for entry in paths {
                let Ok(path) = entry else { continue };
                let modified = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                matches.push((path, modified));
            }
        }

        matches.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
        matches.dedup_by(|a, b| a.0 == b.0);

        if matches.is_empty() {
            return Ok(format!("no files match {:?}", input.pattern).into());
        }

        Ok(matches
            .into_iter()
            .map(|(path, _)| ctx.display_path(&path))
            .collect::<Vec<_>>()
            .join("\n")
            .into())
    }
}

/// Expand one `{a,b,c}` group into several patterns. Only one group is
/// supported — enough for the common `**/*.{ts,tsx}` case without pulling in a
/// full brace-expansion grammar for a tool argument.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    if !pattern[open..].contains('}') {
        return vec![pattern.to_owned()];
    }

    let (prefix, rest) = pattern.split_at(open);
    let (options, suffix) = rest[1..].split_at(rest[1..].find('}').unwrap());
    let suffix = &suffix[1..];

    options
        .split(',')
        .map(|option| format!("{prefix}{option}{suffix}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn finds_files_matching_a_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let output = Glob.call(json!({"pattern": "*.rs"}), &ctx).await.unwrap();

        assert_eq!(output.text, "a.rs");
    }

    #[tokio::test]
    async fn expands_brace_alternation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ts"), "").unwrap();
        std::fs::write(dir.path().join("b.tsx"), "").unwrap();
        std::fs::write(dir.path().join("c.js"), "").unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let output = Glob
            .call(json!({"pattern": "*.{ts,tsx}"}), &ctx)
            .await
            .unwrap();
        let mut lines: Vec<&str> = output.text.lines().collect();
        lines.sort_unstable();

        assert_eq!(lines, ["a.ts", "b.tsx"]);
    }

    #[tokio::test]
    async fn reports_no_matches_without_erroring() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let output = Glob
            .call(json!({"pattern": "*.nonexistent"}), &ctx)
            .await
            .unwrap();

        assert!(output.text.contains("no files match"));
    }
}
