//! Run a shell command in the workspace.
//!
//! Not a sandbox — see [`crate::blocklist`]. This runs `bash -c` with the
//! workspace as its working directory, subject only to the deny-list and a
//! timeout.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::{
    blocklist,
    context::ToolContext,
    tool::{Tool, ToolOutput},
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_OUTPUT: usize = 100_000;

#[derive(Deserialize)]
struct Input {
    command: String,
    timeout: Option<u64>,
    #[allow(dead_code)]
    description: Option<String>,
}

pub struct RunCommand;

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &'static str {
        "run_command"
    }

    fn description(&self) -> &'static str {
        "Execute a bash command in the workspace root. Not a sandbox — a best-effort denylist \
         blocks the obvious destructive commands, nothing more. 30s default timeout, 120s max, \
         100KB output cap."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "timeout": {"type": "integer", "description": "Seconds, capped at 120"},
                "description": {"type": "string", "description": "A short human-readable summary of what this does"},
            },
            "required": ["command"],
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;

        blocklist::check(&input.command)?;

        let timeout = input
            .timeout
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_TIMEOUT)
            .min(MAX_TIMEOUT);

        let run = Command::new("bash")
            .arg("-c")
            .arg(&input.command)
            .current_dir(ctx.workspace_root())
            .output();

        let output = match tokio::time::timeout(timeout, run).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => return Err(format!("could not run the command: {error}")),
            Err(_) => return Err(format!("command timed out after {}s", timeout.as_secs())),
        };

        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        let combined = cap(combined);

        match output.status.code() {
            Some(0) => Ok(if combined.is_empty() {
                "(no output)".to_owned().into()
            } else {
                combined.into()
            }),
            Some(code) => Err(format!("exited with code {code}\n{combined}")),
            None => Err(format!("terminated by signal\n{combined}")),
        }
    }
}

fn cap(output: String) -> String {
    if output.len() <= MAX_OUTPUT {
        output
    } else {
        format!("{}\n... (truncated)", &output[..MAX_OUTPUT])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(dir).unwrap()
    }

    #[tokio::test]
    async fn runs_a_command_and_captures_stdout() {
        let dir = tempfile::tempdir().unwrap();

        let output = RunCommand
            .call(json!({"command": "echo hello"}), &ctx(dir.path()))
            .await
            .unwrap();

        assert_eq!(output.text.trim(), "hello");
    }

    #[tokio::test]
    async fn runs_with_the_workspace_as_cwd() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "").unwrap();

        let output = RunCommand
            .call(json!({"command": "ls"}), &ctx(dir.path()))
            .await
            .unwrap();

        assert!(output.text.contains("marker.txt"), "got: {}", output.text);
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_an_error() {
        let dir = tempfile::tempdir().unwrap();

        let error = RunCommand
            .call(json!({"command": "exit 3"}), &ctx(dir.path()))
            .await
            .unwrap_err();

        assert!(error.contains("code 3"), "got: {error}");
    }

    #[tokio::test]
    async fn a_blocked_command_never_runs() {
        let dir = tempfile::tempdir().unwrap();

        let error = RunCommand
            .call(json!({"command": "rm -rf /"}), &ctx(dir.path()))
            .await
            .unwrap_err();

        assert!(error.contains("not allowed"), "got: {error}");
    }

    #[tokio::test]
    async fn a_slow_command_times_out() {
        let dir = tempfile::tempdir().unwrap();

        let error = RunCommand
            .call(
                json!({"command": "sleep 5", "timeout": 1}),
                &ctx(dir.path()),
            )
            .await
            .unwrap_err();

        assert!(error.contains("timed out"), "got: {error}");
    }
}
