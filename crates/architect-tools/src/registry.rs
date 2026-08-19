//! Composes tools into one [`architect_agent::ToolExecutor`].

use std::{collections::HashMap, io, path::PathBuf, sync::Arc};

use architect_agent::ToolExecutor;
use architect_core::{ToolCall, ToolResult, ToolSchema};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    context::ToolContext,
    tool::Tool,
    tools::{
        edit_file::EditFile, glob::Glob, grep::Grep, list_dir::ListDir, read_file::ReadFile,
        run_command::RunCommand, screenshot::Screenshot, spawn_subagents::SpawnSubAgents,
        view_image::ViewImage, write_file::WriteFile,
    },
};

/// A named collection of tools, dispatched by the name the model calls.
///
/// This is the extension point: [`ToolRegistry::register`] takes anything
/// implementing [`Tool`], so a future MCP client adds remote tools the same
/// way the seven built-ins are added below — no change needed here or in
/// `architect-agent`.
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
    context: ToolContext,
}

impl ToolRegistry {
    /// An empty registry over the given workspace.
    pub fn new(context: ToolContext) -> Self {
        Self {
            tools: HashMap::new(),
            context,
        }
    }

    /// Add a tool. A second registration under the same name replaces the
    /// first — a programmer error caught by a duplicate-name test, not
    /// something worth making the public API fallible over.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> &mut Self {
        self.tools.insert(tool.name(), tool);
        self
    }

    /// A registry with the ten built-in tools, rooted at `workspace_root`.
    pub fn with_default_tools(workspace_root: impl Into<PathBuf>) -> io::Result<Self> {
        let mut registry = Self::new(ToolContext::new(workspace_root)?);

        registry
            .register(Arc::new(ReadFile))
            .register(Arc::new(WriteFile))
            .register(Arc::new(EditFile))
            .register(Arc::new(ListDir))
            .register(Arc::new(Glob))
            .register(Arc::new(Grep))
            .register(Arc::new(RunCommand))
            .register(Arc::new(ViewImage))
            .register(Arc::new(Screenshot))
            .register(Arc::new(SpawnSubAgents));

        Ok(registry)
    }

    /// A registry rooted at `workspace_root` with only read-only,
    /// investigation-focused tools — no writes, no shell, no
    /// `spawn_subagents` itself. What a sub-agent's own turn runs with:
    /// deliberately less trusted than a normal turn (nobody is watching it
    /// as closely as the person who started the conversation that spawned
    /// it), and excluding `spawn_subagents` is what makes a sub-agent
    /// spawning another one structurally impossible rather than merely
    /// discouraged.
    pub fn with_investigation_tools(workspace_root: impl Into<PathBuf>) -> io::Result<Self> {
        let mut registry = Self::new(ToolContext::new(workspace_root)?);

        registry
            .register(Arc::new(ReadFile))
            .register(Arc::new(ListDir))
            .register(Arc::new(Glob))
            .register(Arc::new(Grep))
            .register(Arc::new(ViewImage));

        Ok(registry)
    }

    /// Attach where file changes get reported. Builder-style so it composes
    /// with the constructor at the call site in the engine.
    pub fn with_recorder(mut self, recorder: Arc<dyn architect_core::ChangeRecorder>) -> Self {
        self.context = self.context.with_recorder(recorder);
        self
    }

    /// Attach where a saved plan gets reported — the `Plan` analog of
    /// [`Self::with_recorder`].
    pub fn with_plan_recorder(
        mut self,
        plan_recorder: Arc<dyn architect_core::PlanRecorder>,
    ) -> Self {
        self.context = self.context.with_plan_recorder(plan_recorder);
        self
    }

    /// Attach the plan `read_plan` should answer with, as of the start of
    /// this turn.
    pub fn with_current_plan(mut self, current_plan: Option<architect_core::Plan>) -> Self {
        self.context = self.context.with_current_plan(current_plan);
        self
    }

    /// Attach where `spawn_subagents` starts and awaits sub-agent sessions.
    pub fn with_sub_agent_spawner(mut self, spawner: Arc<dyn crate::SubAgentSpawner>) -> Self {
        self.context = self.context.with_sub_agent_spawner(spawner);
        self
    }
}

#[async_trait]
impl ToolExecutor for ToolRegistry {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .values()
            .map(|tool| ToolSchema {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                input_schema: tool.input_schema(),
            })
            .collect()
    }

    async fn execute(&self, call: ToolCall, _cancel: CancellationToken) -> ToolResult {
        let Some(tool) = self.tools.get(call.name.as_str()) else {
            return ToolResult::error(
                call.id,
                format!("no tool named {:?} is available", call.name),
            );
        };

        match tool.call(call.input, &self.context).await {
            Ok(output) => match output.image {
                Some(image) => ToolResult::ok_with_image(call.id, output.text, image),
                None => ToolResult::ok(call.id, output.text),
            },
            Err(message) => ToolResult::error(call.id, message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_the_ten_built_in_tools() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::with_default_tools(dir.path()).unwrap();

        let schemas = registry.schemas();
        let mut names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();

        assert_eq!(
            names,
            [
                "edit_file",
                "glob",
                "grep",
                "list_dir",
                "read_file",
                "run_command",
                "screenshot",
                "spawn_subagents",
                "view_image",
                "write_file"
            ]
        );
    }

    #[test]
    fn registers_only_the_read_only_investigation_tools() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::with_investigation_tools(dir.path()).unwrap();

        let schemas = registry.schemas();
        let mut names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();

        assert_eq!(
            names,
            ["glob", "grep", "list_dir", "read_file", "view_image"]
        );
    }

    #[tokio::test]
    async fn an_unknown_tool_name_is_reported_to_the_model_not_panicked() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::with_default_tools(dir.path()).unwrap();

        let result = registry
            .execute(
                ToolCall {
                    id: "x".into(),
                    name: "delete_everything".into(),
                    input: serde_json::json!({}),
                },
                CancellationToken::new(),
            )
            .await;

        assert!(result.is_error);
        assert_eq!(result.tool_use_id, "x");
    }
}
