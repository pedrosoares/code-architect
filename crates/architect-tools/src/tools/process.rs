//! Start a long-running process, read its accumulated output, stop it.
//!
//! Unlike `run_command` (run-to-completion, one-shot capture), these three
//! tools are for anything meant to keep running — a dev server, a watcher.
//! `start_process` returns immediately; the process keeps going in the
//! background, tracked by the shared [`crate::process::ProcessRegistry`]
//! these tools hold an `Arc` to. That registry is constructed once (in
//! `apps/desktop/src/engine.rs`'s `worker()`, not per-turn) — see
//! [`crate::process`]'s module docs for why per-turn `ToolContext` state
//! can't survive from a `start_process` call to a later
//! `get_process_logs` call.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    blocklist,
    context::ToolContext,
    process::ProcessRegistry,
    tool::{Tool, ToolOutput},
};

#[derive(Deserialize)]
struct StartInput {
    command: String,
    description: Option<String>,
}

pub struct StartProcess {
    pub registry: Arc<ProcessRegistry>,
}

#[async_trait]
impl Tool for StartProcess {
    fn name(&self) -> &'static str {
        "start_process"
    }

    fn description(&self) -> &'static str {
        "Start a long-running process (a dev server, a watcher, anything not meant to exit) in \
         the background and return immediately. Use get_process_logs to check its output later, \
         and stop_process to end it."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to run, e.g. \"npm run dev\"",
                },
                "description": {
                    "type": "string",
                    "description": "A short human-readable label shown in the UI, e.g. \"dev server\"",
                },
            },
            "required": ["command"],
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: StartInput = serde_json::from_value(input).map_err(|error| error.to_string())?;

        blocklist::check(&input.command)?;

        let id = self
            .registry
            .start(ctx.workspace_root(), input.command.clone())
            .await?;

        let label = input.description.unwrap_or(input.command);
        Ok(format!(
            "Started {id} ({label}). Use get_process_logs with id={id} to check its output, or \
             stop_process to end it."
        )
        .into())
    }
}

#[derive(Deserialize)]
struct IdInput {
    id: String,
}

fn id_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "description": "The process id returned by start_process"},
        },
        "required": ["id"],
        "additionalProperties": false,
    })
}

pub struct GetProcessLogs {
    pub registry: Arc<ProcessRegistry>,
}

#[async_trait]
impl Tool for GetProcessLogs {
    fn name(&self) -> &'static str {
        "get_process_logs"
    }

    fn description(&self) -> &'static str {
        "Read a process's accumulated stdout/stderr and current status, for a process started \
         with start_process."
    }

    fn input_schema(&self) -> Value {
        id_input_schema()
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: IdInput = serde_json::from_value(input).map_err(|error| error.to_string())?;

        let (command, status, log) = self.registry.logs(&input.id).await?;
        let log = if log.is_empty() {
            "(no output yet)".to_owned()
        } else {
            log
        };

        Ok(format!("{} ({command}) — {status}\n\n{log}", input.id).into())
    }
}

pub struct StopProcess {
    pub registry: Arc<ProcessRegistry>,
}

#[async_trait]
impl Tool for StopProcess {
    fn name(&self) -> &'static str {
        "stop_process"
    }

    fn description(&self) -> &'static str {
        "Stop a process started with start_process. Requests termination and returns \
         immediately — use get_process_logs afterward to confirm it actually exited."
    }

    fn input_schema(&self) -> Value {
        id_input_schema()
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: IdInput = serde_json::from_value(input).map_err(|error| error.to_string())?;
        self.registry.stop(&input.id).await.map(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(dir).unwrap()
    }

    #[tokio::test]
    async fn starts_a_process_and_reads_its_logs() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _events) = ProcessRegistry::new();
        let registry = Arc::new(registry);

        let start_result = StartProcess {
            registry: registry.clone(),
        }
        .call(json!({"command": "echo hi"}), &ctx(dir.path()))
        .await
        .unwrap();
        assert!(
            start_result.text.starts_with("Started proc-1"),
            "got: {}",
            start_result.text
        );

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let logs = GetProcessLogs { registry }
            .call(json!({"id": "proc-1"}), &ctx(dir.path()))
            .await
            .unwrap();
        assert!(logs.text.contains("hi"), "got: {}", logs.text);
        assert!(
            logs.text.contains("exited with code 0"),
            "got: {}",
            logs.text
        );
    }

    #[tokio::test]
    async fn get_logs_for_an_unknown_id_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _events) = ProcessRegistry::new();

        let error = GetProcessLogs {
            registry: Arc::new(registry),
        }
        .call(json!({"id": "proc-999"}), &ctx(dir.path()))
        .await
        .unwrap_err();

        assert!(error.contains("no such process"), "got: {error}");
    }

    #[tokio::test]
    async fn stop_process_actually_kills_a_running_process() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _events) = ProcessRegistry::new();
        let registry = Arc::new(registry);

        StartProcess {
            registry: registry.clone(),
        }
        .call(json!({"command": "sleep 30"}), &ctx(dir.path()))
        .await
        .unwrap();

        let stop_result = StopProcess {
            registry: registry.clone(),
        }
        .call(json!({"id": "proc-1"}), &ctx(dir.path()))
        .await
        .unwrap();
        assert!(
            stop_result.text.contains("Stop requested"),
            "got: {}",
            stop_result.text
        );

        let started = std::time::Instant::now();
        loop {
            let logs = GetProcessLogs {
                registry: registry.clone(),
            }
            .call(json!({"id": "proc-1"}), &ctx(dir.path()))
            .await
            .unwrap();
            if logs.text.contains("stopped") {
                break;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "got: {}",
                logs.text
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn a_blocked_command_never_starts() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _events) = ProcessRegistry::new();

        let error = StartProcess {
            registry: Arc::new(registry),
        }
        .call(json!({"command": "rm -rf /"}), &ctx(dir.path()))
        .await
        .unwrap_err();

        assert!(error.contains("not allowed"), "got: {error}");
    }
}
