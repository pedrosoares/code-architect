//! Conversation types.
//!
//! These are provider-neutral. Each provider adapter is responsible for
//! translating them to and from its own wire format — the differences between
//! those formats are real (see `architect-llm`), and hiding them here would
//! leak one provider's shape into the other's.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A piece of a message.
///
/// A single assistant turn can mix these: reasoning, then text, then several
/// tool calls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Model reasoning — Anthropic `thinking` blocks, OpenAI/DeepSeek
    /// `reasoning_content`.
    Reasoning {
        text: String,
        /// Anthropic signs thinking blocks. The signature must be echoed back
        /// unchanged when the conversation continues on the same model, so it
        /// is carried here rather than dropped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolUse(ToolCall),
    ToolResult(ToolResult),
    /// An image attached to a user turn — the composer's "Attach" button.
    /// Never produced by a model in this app (no image generation), only
    /// sent.
    Image {
        /// e.g. `"image/png"`, `"image/jpeg"`.
        media_type: String,
        /// Base64-encoded bytes — the shape both Anthropic's and OpenAI's
        /// wire formats want directly, so callers are expected to have
        /// already encoded by the time they reach this layer.
        data: String,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn reasoning(text: impl Into<String>) -> Self {
        Self::Reasoning {
            text: text.into(),
            signature: None,
        }
    }

    pub fn image(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image {
            media_type: media_type.into(),
            data: data.into(),
        }
    }
}

/// A model's request to run a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned id. Must be echoed back on the matching result — a
    /// mismatch desynchronizes the conversation and the next request fails.
    pub id: String,
    pub name: String,
    /// Parsed arguments. Always parsed from JSON rather than string-matched:
    /// providers differ in how they escape the payload.
    pub input: serde_json::Value,
}

/// An image a tool produced alongside its text result — `view_image` reading
/// one off disk, `screenshot` capturing the screen. Its own small struct
/// rather than reusing [`ContentBlock`] directly: `ToolResult.image` should
/// only ever be an image, never any other content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResultImage {
    /// e.g. `"image/png"`, `"image/jpeg"`.
    pub media_type: String,
    /// Base64-encoded bytes, same convention as [`ContentBlock::Image`].
    pub data: String,
}

/// The outcome of running a tool, fed back to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    /// Set by a tool that can also produce an image (`view_image`,
    /// `screenshot`) — `#[serde(default)]` so session history saved before
    /// this field existed still deserializes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ToolResultImage>,
    /// A failed tool still returns a result, flagged. Dropping it would leave
    /// the model waiting on an id that never comes back.
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            image: None,
            is_error: false,
        }
    }

    /// Same as [`Self::ok`], with an image the model should see alongside
    /// the text — the `Message.user_with_images` analog for a tool result.
    pub fn ok_with_image(
        tool_use_id: impl Into<String>,
        content: impl Into<String>,
        image: ToolResultImage,
    ) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            image: Some(image),
            is_error: false,
        }
    }

    pub fn error(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            image: None,
            is_error: true,
        }
    }
}

/// A tool offered to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments.
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Self { role, content }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, vec![ContentBlock::text(text)])
    }

    /// A user turn with one or more attached images, plus optional text —
    /// the composer's "Attach" flow. Images are listed first: a caption
    /// naturally reads as commentary on what precedes it, and an empty
    /// `text` is valid (an image with no caption at all).
    pub fn user_with_images(
        text: impl Into<String>,
        images: impl IntoIterator<Item = ContentBlock>,
    ) -> Self {
        let mut content: Vec<ContentBlock> = images.into_iter().collect();
        let text = text.into();
        if !text.is_empty() {
            content.push(ContentBlock::text(text));
        }
        Self::new(Role::User, content)
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::new(Role::Assistant, vec![ContentBlock::text(text)])
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::new(Role::System, vec![ContentBlock::text(text)])
    }

    /// All results from one turn, as a single user message.
    ///
    /// Anthropic requires this batching — splitting a turn's results across
    /// several messages teaches the model to stop calling tools in parallel.
    /// The OpenAI adapter unpacks it back into one message per result.
    pub fn tool_results(results: impl IntoIterator<Item = ToolResult>) -> Self {
        Self::new(
            Role::User,
            results.into_iter().map(ContentBlock::ToolResult).collect(),
        )
    }

    /// Concatenation of every text block, ignoring reasoning and tools.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|block| match block {
            ContentBlock::ToolUse(call) => Some(call),
            _ => None,
        })
    }
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    /// The model wants tools run; the loop continues after results are returned.
    ToolUse,
    MaxTokens,
    StopSequence,
    /// Anthropic safety decline. Arrives as HTTP 200 — it is a stop reason, not
    /// an error, and must not be rendered as ordinary text.
    Refusal,
    /// The server-side tool loop paused. Resume by re-sending the history with
    /// no extra user message; the server picks up where it left off.
    PauseTurn,
}

impl StopReason {
    /// Whether the agent loop should run tools and go around again.
    pub fn wants_continuation(self) -> bool {
        matches!(self, Self::ToolUse | Self::PauseTurn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_with_images_puts_images_before_the_caption() {
        let message = Message::user_with_images(
            "what is this?",
            [ContentBlock::image("image/png", "aGVsbG8=")],
        );

        assert_eq!(message.role, Role::User);
        assert_eq!(
            message.content,
            vec![
                ContentBlock::image("image/png", "aGVsbG8="),
                ContentBlock::text("what is this?"),
            ]
        );
    }

    #[test]
    fn user_with_images_and_no_caption_carries_no_text_block() {
        let message = Message::user_with_images("", [ContentBlock::image("image/png", "aGVsbG8=")]);

        assert_eq!(
            message.content,
            vec![ContentBlock::image("image/png", "aGVsbG8=")]
        );
    }

    #[test]
    fn text_ignores_image_blocks() {
        let message =
            Message::user_with_images("describe it", [ContentBlock::image("image/jpeg", "Zm9v")]);

        assert_eq!(message.text(), "describe it");
    }

    #[test]
    fn an_image_block_round_trips_through_json() {
        let block = ContentBlock::image("image/png", "aGVsbG8=");

        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(
            json,
            r#"{"type":"image","media_type":"image/png","data":"aGVsbG8="}"#
        );
        assert_eq!(serde_json::from_str::<ContentBlock>(&json).unwrap(), block);
    }
}
