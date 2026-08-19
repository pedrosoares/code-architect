//! Shared state and sandboxing every tool gets.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use architect_core::{ChangeRecorder, FileChange, NoPlanRecorder, NoRecorder, Plan, PlanRecorder};
use tokio::sync::mpsc::UnboundedSender;

use crate::sub_agent::{NoSubAgentSpawner, SubAgentSpawner};

/// What every [`crate::Tool`] runs with.
///
/// Holding the canonicalized root once and exposing [`ToolContext::resolve`] as
/// the single path-checking entry point is deliberate: V1 canonicalized and
/// checked `starts_with` inside every file tool separately, which is easy to
/// get subtly wrong in one of them. Here there is exactly one implementation
/// to trust.
pub struct ToolContext {
    workspace_root: PathBuf,
    recorder: Arc<dyn ChangeRecorder>,
    plan_recorder: Arc<dyn PlanRecorder>,
    /// Whatever `write_plan` last saved for this session, as of the start
    /// of this turn — handed in, not fetched: `architect-tools` has no
    /// persistence dependency, so this is `read_plan`'s only way to see a
    /// plan written in an earlier turn. Stale within the same turn a
    /// `write_plan` call happens in (see `tools::plan`'s docs) — a known,
    /// accepted limitation.
    current_plan: Option<Plan>,
    sub_agent_spawner: Arc<dyn SubAgentSpawner>,
}

impl ToolContext {
    /// Canonicalizes `workspace_root` once. Fails if the root does not exist —
    /// every later `resolve` call depends on this having succeeded.
    pub fn new(workspace_root: impl Into<PathBuf>) -> io::Result<Self> {
        Ok(Self {
            workspace_root: workspace_root.into().canonicalize()?,
            recorder: Arc::new(NoRecorder),
            plan_recorder: Arc::new(NoPlanRecorder),
            current_plan: None,
            sub_agent_spawner: Arc::new(NoSubAgentSpawner),
        })
    }

    pub fn with_recorder(mut self, recorder: Arc<dyn ChangeRecorder>) -> Self {
        self.recorder = recorder;
        self
    }

    pub fn with_plan_recorder(mut self, plan_recorder: Arc<dyn PlanRecorder>) -> Self {
        self.plan_recorder = plan_recorder;
        self
    }

    pub fn with_current_plan(mut self, current_plan: Option<Plan>) -> Self {
        self.current_plan = current_plan;
        self
    }

    pub fn with_sub_agent_spawner(mut self, sub_agent_spawner: Arc<dyn SubAgentSpawner>) -> Self {
        self.sub_agent_spawner = sub_agent_spawner;
        self
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Resolve a model-supplied path against the workspace root, rejecting
    /// anything that escapes it — `../../etc/passwd`, an absolute path outside
    /// the root, or a symlink that points out.
    ///
    /// The path need not exist yet — `write_file` creates new files — so this
    /// canonicalizes the existing prefix and only requires the parent
    /// directory to actually live inside the root.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let requested = self.workspace_root.join(path);

        let existing = requested
            .ancestors()
            .find(|ancestor| ancestor.exists())
            .ok_or_else(|| format!("{path:?} is not inside the workspace"))?;

        let canonical_existing = existing
            .canonicalize()
            .map_err(|error| format!("could not resolve {path:?}: {error}"))?;

        if !canonical_existing.starts_with(&self.workspace_root) {
            return Err(format!("{path:?} is outside the workspace"));
        }

        // Re-attach whatever suffix didn't exist yet (e.g. a new file's name).
        // `existing` itself is the common case (the path already exists) —
        // `Path::join("")` would append a trailing slash and turn a file into
        // something only a directory-expecting call would accept.
        let suffix = requested.strip_prefix(existing).unwrap_or(Path::new(""));
        if suffix.as_os_str().is_empty() {
            Ok(canonical_existing)
        } else {
            Ok(canonical_existing.join(suffix))
        }
    }

    /// Path to show the model or the user: relative to the workspace root when
    /// possible, so tool output doesn't leak the host's absolute directory
    /// layout.
    pub fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.workspace_root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    pub fn record_change(&self, change: FileChange) {
        self.recorder.record(change);
    }

    pub fn record_plan(&self, plan: Plan) {
        self.plan_recorder.record(plan);
    }

    pub fn current_plan(&self) -> Option<&Plan> {
        self.current_plan.as_ref()
    }

    pub fn sub_agent_spawner(&self) -> &Arc<dyn SubAgentSpawner> {
        &self.sub_agent_spawner
    }
}

/// Routes changes down a plain `mpsc` channel — what the desktop engine uses
/// to forward them to the session store, the same shape as its existing
/// `AgentEvent` bridge.
///
/// A newtype rather than `impl ChangeRecorder for UnboundedSender<..>`
/// directly: the orphan rule blocks that from this crate, since neither the
/// trait (`architect-core`) nor `UnboundedSender` (`tokio`) is defined here.
/// The alternative — giving `architect-core` a tokio dependency just to host
/// that impl — would break its "no async runtime" design.
pub struct ChannelRecorder(pub UnboundedSender<FileChange>);

impl ChangeRecorder for ChannelRecorder {
    fn record(&self, change: FileChange) {
        let _ = self.0.send(change);
    }
}

/// The `Plan` analog of [`ChannelRecorder`], same reasoning: a newtype
/// rather than an `impl PlanRecorder for UnboundedSender<..>` directly,
/// since the orphan rule blocks that (neither the trait nor the sender
/// type is defined in this crate).
pub struct ChannelPlanRecorder(pub UnboundedSender<Plan>);

impl PlanRecorder for ChannelPlanRecorder {
    fn record(&self, plan: Plan) {
        let _ = self.0.send(plan);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(dir.path()).expect("existing dir canonicalizes");
        (dir, ctx)
    }

    #[test]
    fn resolves_a_path_inside_the_workspace() {
        let (dir, ctx) = context();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();

        let resolved = ctx.resolve("a.txt").expect("inside the workspace");

        assert_eq!(resolved, dir.path().canonicalize().unwrap().join("a.txt"));
    }

    #[test]
    fn resolves_a_path_that_does_not_exist_yet() {
        let (_dir, ctx) = context();

        let resolved = ctx.resolve("new/nested/file.txt").expect("parent exists");

        assert!(resolved.ends_with("new/nested/file.txt"));
    }

    #[test]
    fn rejects_a_relative_escape() {
        let (_dir, ctx) = context();

        assert!(ctx.resolve("../../etc/passwd").is_err());
    }

    #[test]
    fn rejects_an_absolute_path_outside_the_root() {
        let (_dir, ctx) = context();

        assert!(ctx.resolve("/etc/passwd").is_err());
    }

    #[test]
    fn display_path_strips_the_workspace_root() {
        let (dir, ctx) = context();
        let absolute = dir.path().canonicalize().unwrap().join("src/main.rs");

        assert_eq!(ctx.display_path(&absolute), "src/main.rs");
    }
}
