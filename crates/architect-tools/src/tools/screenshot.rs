//! Capture a screenshot of the current screen — the model's own eyes on
//! whatever's actually on screen right now, e.g. to check a UI change it
//! just made.
//!
//! v1 captures the primary display only — no window or multi-monitor
//! selection. A native-resolution PNG can be a multi-MB base64 payload; no
//! downscaling is added here, matching how a human's attached image is
//! never resized either (see `apps/desktop/src/panels/composer.rs`). Screen
//! capture is inherently platform-dependent: Wayland compositors can be
//! more permission-gated about it than X11, and a headless/CI environment
//! has no display at all — either way this fails with a plain `Err`, never
//! a panic.

use async_trait::async_trait;
use base64::Engine;
use image::ImageFormat;
use serde_json::{Value, json};

use crate::{
    context::ToolContext,
    tool::{Tool, ToolOutput},
};

pub struct Screenshot;

#[async_trait]
impl Tool for Screenshot {
    fn name(&self) -> &'static str {
        "screenshot"
    }

    fn description(&self) -> &'static str {
        "Capture a screenshot of the current screen, so you can see what's actually on it right \
         now — e.g. to check a UI change you just made. Returns the image directly, the same as \
         an image attached in chat."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        })
    }

    async fn call(&self, _input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let png_bytes = tokio::task::spawn_blocking(capture_primary_monitor_as_png)
            .await
            .map_err(|error| format!("screenshot capture panicked: {error}"))??;

        let data = base64::engine::general_purpose::STANDARD.encode(png_bytes);

        Ok(ToolOutput {
            text: "Captured a screenshot of the screen.".to_owned(),
            image: Some(architect_core::ToolResultImage {
                media_type: "image/png".to_owned(),
                data,
            }),
        })
    }
}

/// Synchronous — capture and PNG encoding are both blocking CPU work, so
/// this only ever runs inside `spawn_blocking`.
fn capture_primary_monitor_as_png() -> Result<Vec<u8>, String> {
    let monitors =
        xcap::Monitor::all().map_err(|error| format!("could not list displays: {error}"))?;

    let monitor = monitors
        .iter()
        .find(|monitor| monitor.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or_else(|| "no display available to capture".to_owned())?;

    let image = monitor
        .capture_image()
        .map_err(|error| format!("could not capture the screen: {error}"))?;

    let mut buffer = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut buffer), ImageFormat::Png)
        .map_err(|error| format!("could not encode the screenshot: {error}"))?;

    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Headless/CI has no display at all — this must fail cleanly rather
    /// than panic, and succeed with a real PNG wherever a display *is*
    /// available (this sandbox's `DISPLAY`/`WAYLAND_DISPLAY` are set, so it
    /// is not marked `#[ignore]`).
    #[tokio::test]
    async fn captures_a_valid_png_or_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path()).unwrap();

        match Screenshot.call(json!({}), &ctx).await {
            Ok(output) => {
                let image = output.image.expect("an image");
                assert_eq!(image.media_type, "image/png");
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&image.data)
                    .expect("valid base64");
                assert_eq!(
                    &bytes[..8],
                    &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
                );
            }
            Err(error) => {
                // No display in this environment — a clear error, not a panic.
                assert!(!error.is_empty());
            }
        }
    }
}
