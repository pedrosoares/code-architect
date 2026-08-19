//! The tool interface.

use async_trait::async_trait;
use serde_json::Value;

use crate::context::ToolContext;

/// What a tool call hands back on success: text, and optionally an image —
/// `view_image` and `screenshot` are the only tools that set the latter
/// today. `From<String>` is what lets every text-only tool keep returning a
/// bare string at its call site (`Ok(some_string.into())`) instead of
/// naming this type everywhere.
#[derive(Debug)]
pub struct ToolOutput {
    pub text: String,
    pub image: Option<architect_core::ToolResultImage>,
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        Self { text, image: None }
    }
}

/// One capability offered to the model.
///
/// The shape is deliberately the same one MCP's `tools/call` uses — name,
/// description, a JSON-schema input, and a text result — so a future MCP
/// client needs only a thin adapter implementing this trait per remote tool,
/// not a redesign. See [`crate::registry::ToolRegistry`] for how tools compose.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The name the model calls it by. Stable — it becomes part of the cached
    /// prompt prefix and of every stored [`architect_core::ToolCall`].
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    /// JSON Schema for the arguments.
    fn input_schema(&self) -> Value;

    /// Whether this tool changes state — writes files, runs commands.
    ///
    /// Documented, not yet enforced: nothing gates on this today. It exists as
    /// the seam a future permission prompt reads, so tools are conservative
    /// about it now rather than retrofitted under time pressure later.
    fn mutates(&self) -> bool {
        false
    }

    /// Run the tool.
    ///
    /// A plain string error rather than a typed one: the error text is exactly
    /// what the model should see to recover, and there is only one caller
    /// ([`crate::registry::ToolRegistry::execute`]) that needs to distinguish
    /// success from failure — it does that with the `Result` itself.
    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String>;
}
