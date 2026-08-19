//! Fan investigation work out to focused sub-agents, each its own real
//! session.

use async_trait::async_trait;
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::ToolContext,
    tool::{Tool, ToolOutput},
};

/// One call spawns at most this many sub-agents — a small, deliberate cap
/// so a single tool call can't fan out unboundedly (mirrors `grep`'s
/// `MAX_FILES`-style safety caps elsewhere in this crate).
const MAX_TASKS: usize = 8;

#[derive(Deserialize)]
struct Task {
    prompt: String,
    path: String,
}

#[derive(Deserialize)]
struct Input {
    tasks: Vec<Task>,
}

pub struct SpawnSubAgents;

#[async_trait]
impl Tool for SpawnSubAgents {
    fn name(&self) -> &'static str {
        "spawn_subagents"
    }

    fn description(&self) -> &'static str {
        "Spawn one or more focused sub-agents to investigate in parallel — e.g. one per crate, \
         each given its own path. Each sub-agent runs as its own real session, seeded with only \
         its task prompt (no shared history with you), using read-only tools scoped to its path. \
         Its final answer comes back here as that task's summary. Whether tasks run one at a \
         time or all at once is decided automatically (a local model runs them sequentially; a \
         hosted API runs them concurrently) — not something you control."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_TASKS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "prompt": {
                                "type": "string",
                                "description": "The sub-agent's task — its entire context, since it starts with no shared history.",
                            },
                            "path": {
                                "type": "string",
                                "description": "Path relative to the workspace root the sub-agent's tools are scoped to, e.g. a single crate's directory.",
                            },
                        },
                        "required": ["prompt", "path"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["tasks"],
            "additionalProperties": false,
        })
    }

    /// Not a file/workspace mutation — the sub-agents it starts get a
    /// read-only tool registry — but it does have a real, durable side
    /// effect (persisted sessions get created), which is exactly what this
    /// flag is for. `mutates()` isn't enforced anywhere today, so this is
    /// currently documentation rather than a behavior change — see the
    /// trait's own doc comment for why it's still worth getting right now
    /// rather than retrofitting later under time pressure.
    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;

        if input.tasks.is_empty() {
            return Err("at least one task is required".to_owned());
        }
        if input.tasks.len() > MAX_TASKS {
            return Err(format!("at most {MAX_TASKS} tasks per call"));
        }

        let spawner = ctx.sub_agent_spawner();

        let summaries: Vec<(String, Result<String, String>)> = if spawner.run_sequentially() {
            let mut summaries = Vec::with_capacity(input.tasks.len());
            for task in input.tasks {
                let result = spawner.spawn(task.prompt, task.path.clone()).await;
                summaries.push((task.path, result));
            }
            summaries
        } else {
            let paths: Vec<String> = input.tasks.iter().map(|task| task.path.clone()).collect();
            let futures = input
                .tasks
                .into_iter()
                .map(|task| spawner.spawn(task.prompt, task.path));
            let results = join_all(futures).await;
            paths.into_iter().zip(results).collect()
        };

        let text = summaries
            .into_iter()
            .map(|(path, result)| match result {
                Ok(summary) => format!("## {path}\n\n{summary}"),
                Err(error) => format!("## {path}\n\nFailed: {error}"),
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        Ok(text.into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use tokio::sync::Mutex;

    use super::*;
    use crate::sub_agent::SubAgentSpawner;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(dir).unwrap()
    }

    /// Records, in completion order, when each task started and finished —
    /// enough to tell "ran one at a time" from "ran overlapped" apart.
    struct RecordingSpawner {
        sequential: bool,
        log: Arc<Mutex<Vec<String>>>,
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SubAgentSpawner for RecordingSpawner {
        fn run_sequentially(&self) -> bool {
            self.sequential
        }

        async fn spawn(&self, prompt: String, path: String) -> Result<String, String> {
            let now_in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight
                .fetch_max(now_in_flight, Ordering::SeqCst);

            // Yield so a concurrent caller actually gets a chance to
            // overlap with this one before it finishes.
            tokio::task::yield_now().await;

            self.log.lock().await.push(path.clone());
            self.in_flight.fetch_sub(1, Ordering::SeqCst);

            if prompt == "fail" {
                Err(format!("boom in {path}"))
            } else {
                Ok(format!("investigated {path}"))
            }
        }
    }

    fn tasks_input(paths: &[&str]) -> Value {
        json!({
            "tasks": paths
                .iter()
                .map(|path| json!({"prompt": "investigate", "path": path}))
                .collect::<Vec<_>>(),
        })
    }

    #[tokio::test]
    async fn sequential_mode_runs_one_task_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let spawner = RecordingSpawner {
            sequential: true,
            log: Arc::new(Mutex::new(Vec::new())),
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: max_in_flight.clone(),
        };
        let context = ctx(dir.path()).with_sub_agent_spawner(Arc::new(spawner));

        SpawnSubAgents
            .call(tasks_input(&["a", "b", "c"]), &context)
            .await
            .unwrap();

        assert_eq!(max_in_flight.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_mode_overlaps_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let spawner = RecordingSpawner {
            sequential: false,
            log: Arc::new(Mutex::new(Vec::new())),
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: max_in_flight.clone(),
        };
        let context = ctx(dir.path()).with_sub_agent_spawner(Arc::new(spawner));

        SpawnSubAgents
            .call(tasks_input(&["a", "b", "c"]), &context)
            .await
            .unwrap();

        assert!(
            max_in_flight.load(Ordering::SeqCst) > 1,
            "expected tasks to overlap"
        );
    }

    #[tokio::test]
    async fn a_failed_task_is_labeled_not_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let spawner = RecordingSpawner {
            sequential: true,
            log: Arc::new(Mutex::new(Vec::new())),
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: Arc::new(AtomicUsize::new(0)),
        };
        let context = ctx(dir.path()).with_sub_agent_spawner(Arc::new(spawner));

        let output = SpawnSubAgents
            .call(
                json!({"tasks": [
                    {"prompt": "investigate", "path": "good"},
                    {"prompt": "fail", "path": "bad"},
                ]}),
                &context,
            )
            .await
            .unwrap();

        assert!(output.text.contains("investigated good"));
        assert!(output.text.contains("Failed: boom in bad"));
    }

    #[tokio::test]
    async fn more_than_the_cap_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<&str> = vec!["a"; MAX_TASKS + 1];

        let error = SpawnSubAgents
            .call(tasks_input(&paths), &ctx(dir.path()))
            .await
            .unwrap_err();

        assert!(error.contains("at most"), "got: {error}");
    }

    #[tokio::test]
    async fn no_tasks_is_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let error = SpawnSubAgents
            .call(json!({"tasks": []}), &ctx(dir.path()))
            .await
            .unwrap_err();

        assert!(error.contains("at least one"), "got: {error}");
    }

    #[tokio::test]
    async fn without_a_wired_spawner_the_default_error_surfaces() {
        let dir = tempfile::tempdir().unwrap();

        let error = SpawnSubAgents
            .call(tasks_input(&["a"]), &ctx(dir.path()))
            .await
            .unwrap();

        assert!(error.text.contains("Failed: sub-agents are not available"));
    }
}
