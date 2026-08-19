//! Exact string replacement, with a diff in the result.

use architect_core::FileChange;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use similar::TextDiff;

use crate::{
    context::ToolContext,
    tool::{Tool, ToolOutput},
};

#[derive(Deserialize)]
struct Input {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Replace an exact substring in a file. Fails if old_string is not found, or is ambiguous \
         unless replace_all is set."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the workspace root"},
                "old_string": {"type": "string", "description": "Exact text to find"},
                "new_string": {"type": "string", "description": "Text to replace it with"},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence instead of requiring exactly one"},
            },
            "required": ["path", "old_string", "new_string"],
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;
        let path = ctx.resolve(&input.path)?;

        let old_content = std::fs::read_to_string(&path)
            .map_err(|error| format!("could not read {}: {error}", input.path))?;

        let (matched, occurrences) = locate(&old_content, &input.old_string);
        if occurrences == 0 {
            return Err(format!("old_string was not found in {}", input.path));
        }
        if occurrences > 1 && !input.replace_all {
            return Err(format!(
                "old_string appears {occurrences} times in {} — pass replace_all or include more context to make it unique",
                input.path
            ));
        }

        let new_content = if input.replace_all {
            old_content.replace(matched, &input.new_string)
        } else {
            old_content.replacen(matched, &input.new_string, 1)
        };

        std::fs::write(&path, &new_content)
            .map_err(|error| format!("could not write {}: {error}", input.path))?;

        let diff = unified_diff(&input.path, &old_content, &new_content);

        ctx.record_change(FileChange {
            file_path: path,
            old_content: Some(old_content),
            new_content,
            tool_name: "edit_file",
        });

        Ok(diff.into())
    }
}

/// Find `old_string` in `content`, falling back to a version with curly quotes
/// normalized to straight ones. Models frequently retype a quote from a file
/// they just read as the "typographic" version their tokenizer prefers; V1
/// found this fallback fixed a real, recurring failure mode.
fn locate<'a>(content: &'a str, old_string: &'a str) -> (&'a str, usize) {
    let exact = content.matches(old_string).count();
    if exact > 0 {
        return (old_string, exact);
    }

    // Normalize *both* sides to the same quote style — the file may have
    // curly quotes where the model typed straight ones, or the reverse — and
    // search on that. `normalize_quotes` maps one char to one char, so char
    // positions still line up between the normalized and original text even
    // though a curly quote is 3 UTF-8 bytes and a straight one is 1: the
    // match is found by byte offset in the normalized string, converted to a
    // char offset, then walked back to a byte range in the *original*.
    let normalized_needle = normalize_quotes(old_string);
    if normalized_needle.is_empty() {
        return (old_string, 0);
    }

    let normalized_content = normalize_quotes(content);
    let count = normalized_content
        .matches(normalized_needle.as_str())
        .count();
    let Some(byte_start_normalized) = normalized_content.find(normalized_needle.as_str()) else {
        return (old_string, 0);
    };

    let char_start = normalized_content[..byte_start_normalized].chars().count();
    let needle_char_len = normalized_needle.chars().count();

    let mut chars = content.char_indices();
    let Some((byte_start, _)) = chars.by_ref().nth(char_start) else {
        return (old_string, 0);
    };
    // `chars.nth(skip)` yields the first char *after* the needle — its byte
    // index is already the correct exclusive end, not `index + len_utf8()`.
    let byte_end = needle_char_len
        .checked_sub(1)
        .and_then(|skip| chars.nth(skip))
        .map(|(index, _)| index)
        .unwrap_or(content.len());

    (&content[byte_start..byte_end], count)
}

fn normalize_quotes(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{201C}' | '\u{201D}' => '"',
            other => other,
        })
        .collect()
}

fn unified_diff(path: &str, old: &str, new: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(path, path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(dir).unwrap()
    }

    #[tokio::test]
    async fn replaces_a_unique_match_and_returns_a_diff() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {\n    old();\n}\n").unwrap();

        let diff = EditFile
            .call(
                json!({"path": "a.rs", "old_string": "old();", "new_string": "new();"}),
                &ctx(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "fn main() {\n    new();\n}\n"
        );
        assert!(diff.text.contains("-    old();"));
        assert!(diff.text.contains("+    new();"));
    }

    #[tokio::test]
    async fn rejects_an_ambiguous_match_without_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x();\nx();\n").unwrap();

        let error = EditFile
            .call(
                json!({"path": "a.rs", "old_string": "x();", "new_string": "y();"}),
                &ctx(dir.path()),
            )
            .await
            .unwrap_err();

        assert!(error.contains("appears 2 times"), "got: {error}");
    }

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x();\nx();\n").unwrap();

        EditFile
            .call(
                json!({"path": "a.rs", "old_string": "x();", "new_string": "y();", "replace_all": true}),
                &ctx(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "y();\ny();\n"
        );
    }

    #[tokio::test]
    async fn falls_back_to_a_curly_quote_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let s = \u{201C}hello\u{201D};\n").unwrap();

        // The model asks for straight quotes; the file has curly ones.
        EditFile
            .call(
                json!({"path": "a.rs", "old_string": "\"hello\"", "new_string": "\"world\""}),
                &ctx(dir.path()),
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "let s = \"world\";\n"
        );
    }

    #[tokio::test]
    async fn reports_when_old_string_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();

        let error = EditFile
            .call(
                json!({"path": "a.rs", "old_string": "missing", "new_string": "x"}),
                &ctx(dir.path()),
            )
            .await
            .unwrap_err();

        assert!(error.contains("not found"), "got: {error}");
    }
}
