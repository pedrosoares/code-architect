//! View an image file already on disk — the model's own "Attach" button.

use architect_core::ToolResultImage;
use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::ToolContext,
    tool::{Tool, ToolOutput},
};

#[derive(Deserialize)]
struct Input {
    path: String,
}

pub struct ViewImage;

#[async_trait]
impl Tool for ViewImage {
    fn name(&self) -> &'static str {
        "view_image"
    }

    fn description(&self) -> &'static str {
        "View an image file — a screenshot, diagram, or photo — so you can see it directly, the \
         same as an image attached in chat. Supports png, jpg, gif, and webp."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the image, relative to the workspace root"},
            },
            "required": ["path"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: Input = serde_json::from_value(input).map_err(|e| e.to_string())?;
        let path = ctx.resolve(&input.path)?;

        let media_type = media_type_for(&path)?;

        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|error| format!("could not read {}: {error}", input.path))?;
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);

        Ok(ToolOutput {
            text: format!("Showing {}.", input.path),
            image: Some(ToolResultImage { media_type, data }),
        })
    }
}

/// Same four-way extension mapping the composer's "Attach" button uses
/// (`apps/desktop/src/panels/composer.rs`'s `media_type_for`) — not shared
/// code, since that one lives in the desktop crate and this one here, but
/// the same set of extensions either dialect actually accepts as an image.
fn media_type_for(path: &std::path::Path) -> Result<String, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match extension.as_str() {
        "png" => Ok("image/png".to_owned()),
        "jpg" | "jpeg" => Ok("image/jpeg".to_owned()),
        "gif" => Ok("image/gif".to_owned()),
        "webp" => Ok("image/webp".to_owned()),
        _ => Err(format!("unsupported image type: .{extension}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(dir).unwrap()
    }

    /// The smallest possible valid PNG — a 1x1 transparent pixel — so the
    /// test proves real bytes round-trip through base64, not just that some
    /// string comes back.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[tokio::test]
    async fn views_a_png_and_base64_round_trips_the_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), TINY_PNG).unwrap();

        let output = ViewImage
            .call(json!({"path": "shot.png"}), &ctx(dir.path()))
            .await
            .unwrap();

        let image = output.image.expect("an image");
        assert_eq!(image.media_type, "image/png");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&image.data)
                .unwrap(),
            TINY_PNG
        );
        assert!(output.text.contains("shot.png"));
    }

    #[tokio::test]
    async fn refuses_an_unsupported_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hi").unwrap();

        let error = ViewImage
            .call(json!({"path": "notes.txt"}), &ctx(dir.path()))
            .await
            .unwrap_err();

        assert!(error.contains("unsupported"), "got: {error}");
    }

    #[tokio::test]
    async fn reports_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();

        let error = ViewImage
            .call(json!({"path": "missing.png"}), &ctx(dir.path()))
            .await
            .unwrap_err();

        assert!(error.contains("could not read"), "got: {error}");
    }

    #[tokio::test]
    async fn refuses_to_view_outside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();

        let error = ViewImage
            .call(json!({"path": "../../etc/passwd"}), &ctx(dir.path()))
            .await
            .unwrap_err();

        assert!(error.contains("workspace"), "got: {error}");
    }
}
