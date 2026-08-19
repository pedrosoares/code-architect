//! Adapts one MCP server's tool into `architect_tools::Tool`.

use architect_tools::{Tool, ToolContext, ToolOutput};
use rmcp::{
    model::{CallToolRequestParams, CallToolResult, ContentBlock},
    service::{Peer, RoleClient},
};
use serde_json::Value;

/// One tool discovered from a connected MCP server.
///
/// `Tool::name`/`description` must return `&'static str`, but an MCP
/// server's tool names and descriptions are only known once connected, at
/// runtime — so they are leaked once here (see [`leak`]) rather than the
/// trait being reshaped for the sake of one implementer. A fixed, one-time
/// allocation per discovered tool for the process's lifetime, not a growing
/// leak: tools are discovered once per connection, not once per call.
pub struct McpToolAdapter {
    peer: Peer<RoleClient>,
    name: &'static str,
    description: &'static str,
    input_schema: Value,
}

impl McpToolAdapter {
    pub(crate) fn new(peer: Peer<RoleClient>, tool: rmcp::model::Tool) -> Self {
        Self {
            peer,
            name: leak(tool.name.into_owned()),
            description: leak(tool.description.map(|d| d.into_owned()).unwrap_or_default()),
            input_schema: schema_to_value(&tool.input_schema),
        }
    }
}

#[async_trait::async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn input_schema(&self) -> Value {
        self.input_schema.clone()
    }

    /// Conservative: an MCP tool's side effects are unknown to us, unlike
    /// the built-ins where each one declares its own. Only affects a seam
    /// nothing enforces yet (see `architect_tools::Tool::mutates`'s docs).
    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let arguments = input.as_object().cloned().unwrap_or_default();

        let result = self
            .peer
            .call_tool(CallToolRequestParams::new(self.name).with_arguments(arguments))
            .await
            .map_err(|error| error.to_string())?;

        let text = content_to_text(&result);
        if result.is_error == Some(true) {
            Err(text)
        } else {
            Ok(text.into())
        }
    }
}

/// Leaks an owned string into a `&'static str` — see [`McpToolAdapter`]'s
/// docs for why this is a bounded, one-time cost rather than a real leak.
fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn schema_to_value(schema: &rmcp::model::JsonObject) -> Value {
    Value::Object(schema.clone())
}

/// Join every text block in a tool result into one string — an MCP result
/// can carry images or embedded resources too, but every built-in tool
/// already speaks in plain text, so this is what the rest of the app expects
/// back. Non-text blocks are dropped rather than guessed at.
fn content_to_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaking_the_same_value_twice_gives_independent_static_strings() {
        assert_eq!(leak("read_file".to_owned()), "read_file");
        assert_eq!(leak(String::new()), "");
    }

    #[test]
    fn converts_a_json_object_schema_into_a_value() {
        let mut schema = rmcp::model::JsonObject::new();
        schema.insert("type".into(), serde_json::json!("object"));

        assert_eq!(
            schema_to_value(&schema),
            serde_json::json!({"type": "object"})
        );
    }

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock::text(text.to_owned())
    }

    #[test]
    fn joins_every_text_block_and_ignores_the_rest() {
        let result = CallToolResult::success(vec![text_block("first"), text_block("second")]);

        assert_eq!(content_to_text(&result), "first\nsecond");
    }

    #[test]
    fn a_successful_result_with_no_text_content_is_an_empty_string() {
        let result = CallToolResult::success(vec![]);

        assert_eq!(content_to_text(&result), "");
    }

    #[test]
    fn an_error_result_is_reported_as_the_tools_own_error_text() {
        let result = CallToolResult::error(vec![text_block("no such file")]);

        assert_eq!(result.is_error, Some(true));
        assert_eq!(content_to_text(&result), "no such file");
    }
}
