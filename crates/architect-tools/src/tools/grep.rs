//! Regex search across the workspace.
//!
//! Walks with the `ignore` crate rather than hand-rolling directory traversal:
//! it respects `.gitignore` for free, which V1's walker did not, and it is the
//! same, well-exercised walker ripgrep itself uses.

use async_trait::async_trait;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::ToolContext,
    tool::{Tool, ToolOutput},
};

/// Caps mirrored from V1: enough for a useful answer, small enough to never
/// blow the context window on an overly broad pattern.
const MAX_FILES: usize = 500;
const MAX_OUTPUT_LEN: usize = 20_000;

#[derive(Deserialize)]
struct Input {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    #[serde(default)]
    output_mode: OutputMode,
    #[serde(default, rename = "-i")]
    ignore_case: bool,
    /// Symmetric context; distinct before/after are also accepted.
    #[serde(rename = "-C")]
    context: Option<usize>,
    #[serde(rename = "-A")]
    after: Option<usize>,
    #[serde(rename = "-B")]
    before: Option<usize>,
    head_limit: Option<usize>,
}

#[derive(Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
enum OutputMode {
    #[default]
    Content,
    FilesWithMatches,
    Count,
}

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents with a regular expression. Respects .gitignore. Supports context \
         lines, a glob filter, and three output modes: content, files_with_matches, count."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "Directory to search; defaults to the workspace root"},
                "glob": {"type": "string", "description": "Restrict to files matching this glob"},
                "output_mode": {"type": "string", "enum": ["content", "files_with_matches", "count"]},
                "-i": {"type": "boolean", "description": "Case-insensitive"},
                "-C": {"type": "integer", "description": "Lines of context before and after each match"},
                "-A": {"type": "integer", "description": "Lines of context after each match"},
                "-B": {"type": "integer", "description": "Lines of context before each match"},
                "head_limit": {"type": "integer", "description": "Stop after this many matching lines"},
            },
            "required": ["pattern"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;

        let regex = RegexBuilder::new(&input.pattern)
            .case_insensitive(input.ignore_case)
            .build()
            .map_err(|error| format!("invalid pattern: {error}"))?;

        let glob = input.glob.as_deref().map(build_glob_matcher).transpose()?;
        let root = ctx.resolve(input.path.as_deref().unwrap_or("."))?;

        let before = input.before.or(input.context).unwrap_or(0);
        let after = input.after.or(input.context).unwrap_or(0);

        let mut file_matches: Vec<(String, Vec<String>, usize)> = Vec::new();

        for entry in WalkBuilder::new(&root)
            .hidden(false)
            // `.gitignore` should apply to any workspace, not only ones that
            // happen to have a `.git` directory at the searched root.
            .require_git(false)
            .build()
            .filter_map(|e| e.ok())
        {
            if file_matches.len() >= MAX_FILES {
                break;
            }
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if let Some(glob) = &glob
                && !glob.is_match(entry.path())
            {
                continue;
            }

            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();

            let hits: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| regex.is_match(line))
                .map(|(i, _)| i)
                .collect();
            if hits.is_empty() {
                continue;
            }

            let display = ctx.display_path(entry.path());
            let rendered = render_matches(&lines, &hits, before, after);
            file_matches.push((display, rendered, hits.len()));
        }

        if file_matches.is_empty() {
            return Ok(format!("no matches for {:?}", input.pattern).into());
        }

        if let Some(limit) = input.head_limit {
            file_matches.truncate(limit);
        }

        Ok(cap(&render_output(file_matches, input.output_mode)).into())
    }
}

/// Render one file's matches with context, merging overlapping ranges so a
/// dense cluster of hits doesn't repeat the same lines.
fn render_matches(lines: &[&str], hits: &[usize], before: usize, after: usize) -> Vec<String> {
    let mut ranges: Vec<(usize, usize)> = hits
        .iter()
        .map(|&i| {
            (
                i.saturating_sub(before),
                (i + after).min(lines.len().saturating_sub(1)),
            )
        })
        .collect();
    ranges.dedup();

    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges.drain(..) {
        match merged.last_mut() {
            Some((_, last_end)) if start <= *last_end + 1 => *last_end = end.max(*last_end),
            _ => merged.push((start, end)),
        }
    }

    merged
        .into_iter()
        .flat_map(|(start, end)| (start..=end).map(move |i| format!("{:>6}:{}", i + 1, lines[i])))
        .collect()
}

fn render_output(files: Vec<(String, Vec<String>, usize)>, mode: OutputMode) -> String {
    match mode {
        OutputMode::FilesWithMatches => files
            .into_iter()
            .map(|(path, ..)| path)
            .collect::<Vec<_>>()
            .join("\n"),
        OutputMode::Count => files
            .into_iter()
            .map(|(path, _, count)| format!("{path}: {count}"))
            .collect::<Vec<_>>()
            .join("\n"),
        OutputMode::Content => files
            .into_iter()
            .map(|(path, rendered, _)| format!("{path}\n{}", rendered.join("\n")))
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

fn cap(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_LEN {
        output.to_owned()
    } else {
        format!("{}\n... (truncated)", &output[..MAX_OUTPUT_LEN])
    }
}

fn build_glob_matcher(pattern: &str) -> Result<globset::GlobMatcher, String> {
    globset::Glob::new(pattern)
        .map(|g| g.compile_matcher())
        .map_err(|error| format!("invalid glob: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn finds_a_pattern_in_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world\nfoo\n").unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let output = Grep.call(json!({"pattern": "hello"}), &ctx).await.unwrap();

        assert!(output.text.contains("hello world"), "got: {}", output.text);
    }

    #[tokio::test]
    async fn respects_before_and_after_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nMATCH\nfour\nfive\n").unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let output = Grep
            .call(json!({"pattern": "MATCH", "-B": 1, "-A": 1}), &ctx)
            .await
            .unwrap();

        // V1's after-context path was dead code; this pins that after really
        // does include the following line.
        assert!(output.text.contains("two"), "got: {}", output.text);
        assert!(output.text.contains("MATCH"), "got: {}", output.text);
        assert!(output.text.contains("four"), "got: {}", output.text);
        assert!(
            !output.text.contains("one"),
            "before-context should not reach two lines back: {}",
            output.text
        );
        assert!(
            !output.text.contains("five"),
            "after-context should not reach two lines forward: {}",
            output.text
        );
    }

    #[tokio::test]
    async fn files_with_matches_mode_lists_only_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing\n").unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let output = Grep
            .call(
                json!({"pattern": "needle", "output_mode": "files_with_matches"}),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(output.text, "a.txt");
    }

    #[tokio::test]
    async fn respects_a_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "needle\n").unwrap();
        std::fs::write(dir.path().join("kept.txt"), "needle\n").unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let output = Grep
            .call(
                json!({"pattern": "needle", "output_mode": "files_with_matches"}),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(output.text, "kept.txt");
    }

    #[tokio::test]
    async fn filters_by_glob() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "needle\n").unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        let output = Grep
            .call(
                json!({"pattern": "needle", "glob": "*.rs", "output_mode": "files_with_matches"}),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(output.text, "a.rs");
    }
}
