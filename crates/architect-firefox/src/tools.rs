//! The seven tools the model uses to drive the [`crate::Browser`]:
//! `firefox_open`, `firefox_logs`, `firefox_click`, `firefox_fill`,
//! `firefox_eval`, `firefox_screenshot`, `firefox_close`.
//!
//! Every one of them is a thin, `Send + Sync` wrapper over a shared
//! `Arc<Browser>` — the *state* (the geckodriver child process and the
//! WebDriver session) lives in the [`crate::Browser`] the engine worker owns,
//! and these structs only carry a handle to it. That's what makes them safe
//! to register on every turn, and what keeps `ToolContext` free of any
//! browser state (the same reason `ProcessRegistry` can't live in
//! `ToolContext` either — it's rebuilt fresh every turn).
//!
//! The browser is lazy: none of these starts `geckodriver` at construction.
//! The first `firefox_open`/`firefox_logs`/… call pays for it.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};

use architect_tools::{Tool, ToolContext, ToolOutput};

use crate::browser::Browser;

/// Every tool this crate offers, all driving the one shared `browser` — the
/// engine's extension point, the same role `architect_linear::tools(key)` and
/// `architect_mcp::connect(..).tools` play for their credentials. The engine
/// keeps the `browser` (to `close()` it at shutdown) and registers these
/// `Arc<dyn Tool>`s on every turn.
pub fn tools(browser: Arc<Browser>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(FirefoxOpen {
            browser: browser.clone(),
        }),
        Arc::new(FirefoxLogs {
            browser: browser.clone(),
        }),
        Arc::new(FirefoxClick {
            browser: browser.clone(),
        }),
        Arc::new(FirefoxFill {
            browser: browser.clone(),
        }),
        Arc::new(FirefoxEval {
            browser: browser.clone(),
        }),
        Arc::new(FirefoxScreenshot {
            browser: browser.clone(),
        }),
        Arc::new(FirefoxClose {
            browser: browser.clone(),
        }),
    ]
}

// -- firefox_open ----------------------------------------------------------

#[derive(Deserialize)]
struct OpenInput {
    url: String,
}

pub struct FirefoxOpen {
    browser: Arc<Browser>,
}

#[async_trait]
impl Tool for FirefoxOpen {
    fn name(&self) -> &'static str {
        "firefox_open"
    }

    fn description(&self) -> &'static str {
        "Open a URL in a headless Firefox (starting the browser on first use), \
         and return the page's title. This is the browser's entry point — call it \
         before firefox_logs/firefox_click/firefox_fill/firefox_eval/firefox_\
         screenshot. The same browser session is kept between calls, so you can \
         navigate to a new URL later and pick up where you left off."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to load, e.g. http://localhost:3000/ or https://example.com.",
                },
            },
            "required": ["url"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: OpenInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
        let (url, title) = self.browser.open(&input.url).await?;
        let title = if title.trim().is_empty() {
            "(no title)"
        } else {
            title.as_str()
        };
        Ok(format!("Opened {url} — title: {title}").into())
    }
}

// -- firefox_logs ----------------------------------------------------------

#[derive(Deserialize)]
struct LogsInput {
    #[serde(default)]
    clear: bool,
}

pub struct FirefoxLogs {
    browser: Arc<Browser>,
}

#[async_trait]
impl Tool for FirefoxLogs {
    fn name(&self) -> &'static str {
        "firefox_logs"
    }

    fn description(&self) -> &'static str {
        "Read the page's console: every console.log/info/warn/error/debug call \
         since it loaded (plus uncaught errors and unhandled promise rejections), \
         along with the current URL and title. This is how you see what a page \
         logged — network failures, JS errors, warnings. Pass clear=true to read \
         and then drop those entries, so the next call shows only new output."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "clear": {
                    "type": "boolean",
                    "description": "If true, also clear the captured entries after reading them (default false).",
                },
            },
            "additionalProperties": false,
        })
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: LogsInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
        let logs = self.browser.logs(input.clear).await?;

        if logs.entries.is_empty() {
            return Ok(format!(
                "No console output captured yet (URL: {}, title: {}). The page may not \
                 have logged anything, or it loaded before you opened it — navigate again \
                 to start capturing.",
                logs.url.as_deref().unwrap_or("?"),
                logs.title.as_deref().unwrap_or("?"),
            )
            .into());
        }

        let mut text = format!(
            "Console ({} entries) — URL: {}, title: {}\n",
            logs.entries.len(),
            logs.url.as_deref().unwrap_or("?"),
            logs.title.as_deref().unwrap_or("?"),
        );
        for entry in &logs.entries {
            text.push_str(&format!("[{}] {}\n", entry.level, entry.text));
        }
        Ok(ToolOutput {
            text: text.trim_end().to_owned(),
            image: None,
        })
    }
}

// -- firefox_click ---------------------------------------------------------

#[derive(Deserialize)]
struct SelectorInput {
    selector: String,
}

pub struct FirefoxClick {
    browser: Arc<Browser>,
}

#[async_trait]
impl Tool for FirefoxClick {
    fn name(&self) -> &'static str {
        "firefox_click"
    }

    fn description(&self) -> &'static str {
        "Click the first element matching a CSS selector. Use this to submit \
         forms, follow links, open menus, toggle things — the same way a user \
         would click. The page's own click handlers and navigation fire."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "A CSS selector, e.g. \"#submit\", \".add-item button\", or \"form[action=\\\"/cart\\\"] button\".",
                },
            },
            "required": ["selector"],
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: SelectorInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
        self.browser.click(&input.selector).await?;
        Ok(format!(
            "Clicked the first element matching {input_selector}",
            input_selector = input.selector
        )
        .into())
    }
}

// -- firefox_fill ----------------------------------------------------------

#[derive(Deserialize)]
struct FillInput {
    selector: String,
    value: String,
}

pub struct FirefoxFill {
    browser: Arc<Browser>,
}

#[async_trait]
impl Tool for FirefoxFill {
    fn name(&self) -> &'static str {
        "firefox_fill"
    }

    fn description(&self) -> &'static str {
        "Set the value of the first element matching a CSS selector — \
         typically a text input, textarea, or a select. For a `<select>` the \
         matching option is chosen (by its `value` attribute, else its label); \
         for any other element the field is cleared and the value typed in, \
         firing the page's own input/change events so a React/Vue controlled \
         input sees the new value."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "A CSS selector for the field to fill, e.g. \"#email\" or \"input[name=\\\"q\\\"]\".",
                },
                "value": {
                    "type": "string",
                    "description": "The text to put in the field.",
                },
            },
            "required": ["selector", "value"],
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: FillInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
        self.browser.fill(&input.selector, &input.value).await?;
        Ok(format!(
            "Filled the first element matching {selector} with {value}.",
            selector = input.selector,
            value = input.value
        )
        .into())
    }
}

// -- firefox_eval ----------------------------------------------------------

#[derive(Deserialize)]
struct EvalInput {
    script: String,
    #[serde(default)]
    args: Vec<Value>,
}

pub struct FirefoxEval {
    browser: Arc<Browser>,
}

#[async_trait]
impl Tool for FirefoxEval {
    fn name(&self) -> &'static str {
        "firefox_eval"
    }

    fn description(&self) -> &'static str {
        "Run JavaScript in the page and return its JSON value. A bare \
         expression (e.g. \"document.title\") is auto-returned for you; a \
         statement (e.g. \"let x = 1; return x + 1\") uses its own return. \
         Use it to read computed state (textContent, style, data attributes, \
         localStorage), or to drive the page when click/fill can't (a canvas, \
         a custom widget). Not a sandbox — it runs in the page's context with \
         full access to the DOM and window, the same trust level as \
         run_command."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "A JavaScript expression, e.g. \"document.title\" or \"Array.from(document.querySelectorAll('a')).map(a=>a.href)\". Return a JSON-serializable value.",
                },
                "args": {
                    "type": "array",
                    "description": "Optional values passed as arguments[0..] to the script.",
                },
            },
            "required": ["script"],
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        let input: EvalInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
        let result = self.browser.eval(&input.script, &input.args).await?;
        let rendered = if result.is_null() {
            "null".to_owned()
        } else {
            serde_json::to_string(&result).unwrap_or_else(|_| result.to_string())
        };
        Ok(rendered.into())
    }
}

// -- firefox_screenshot ----------------------------------------------------

pub struct FirefoxScreenshot {
    browser: Arc<Browser>,
}

#[async_trait]
impl Tool for FirefoxScreenshot {
    fn name(&self) -> &'static str {
        "firefox_screenshot"
    }

    fn description(&self) -> &'static str {
        "Capture a screenshot of the current page and return the image inline, \
         so you can see it directly in the chat, the same as the screenshot tool \
         or view_image. The PNG is also saved to `.coder/screenshots/` in the \
         workspace (the path is returned) so it can be re-viewed later with \
         view_image. Use it to see what a web page actually renders — any visual \
         change you just made, layout bugs, or a broken build."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, _input: Value, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let png = self.browser.screenshot().await?;

        // Write the PNG to the workspace so it is a real, re-viewable artifact
        // (and so `view_image` can re-open it later), AND attach it to the
        // result — the same way the `screenshot` and `view_image` tools do —
        // so the chat renders it directly and the model sees it without an
        // extra `view_image` round-trip. The path stays in the text so both
        // the human and the model can re-reference the saved file.
        let dir = ctx.workspace_root().join(".coder").join("screenshots");
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("could not create the screenshot directory: {error}"))?;
        let relative = format!(
            ".coder/screenshots/firefox-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let absolute = ctx.workspace_root().join(&relative);
        std::fs::write(&absolute, &png)
            .map_err(|error| format!("could not save the screenshot: {error}"))?;

        Ok(ToolOutput {
            text: format!(
                "Captured a screenshot of the current page (shown below). Also saved to \
                 {relative} ({n} bytes) — re-view it later with view_image.",
                n = png.len()
            ),
            image: Some(architect_core::ToolResultImage {
                media_type: "image/png".to_owned(),
                data: base64::engine::general_purpose::STANDARD.encode(&png),
            }),
        })
    }
}

// -- firefox_close ---------------------------------------------------------

pub struct FirefoxClose {
    browser: Arc<Browser>,
}

#[async_trait]
impl Tool for FirefoxClose {
    fn name(&self) -> &'static str {
        "firefox_close"
    }

    fn description(&self) -> &'static str {
        "End the browser session and shut down the headless Firefox. Call this \
         when you're done with the page — it frees the browser and its memory. \
         The browser comes back on the next firefox_open."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn call(&self, _input: Value, _ctx: &ToolContext) -> Result<ToolOutput, String> {
        self.browser.close().await;
        Ok("Closed the browser session.".to_owned().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::CONSOLE_HOOK;

    #[test]
    fn exposes_all_seven_tools_in_order() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let browser = Arc::new(Browser::new(tx));
        let names: Vec<&str> = tools(browser).iter().map(|tool| tool.name()).collect();
        assert_eq!(
            names,
            [
                "firefox_open",
                "firefox_logs",
                "firefox_click",
                "firefox_fill",
                "firefox_eval",
                "firefox_screenshot",
                "firefox_close",
            ]
        );
    }

    #[test]
    fn mutates_is_set_only_on_the_tools_that_change_state() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let browser = Arc::new(Browser::new(tx));
        let mutates: std::collections::HashMap<&str, bool> = tools(browser)
            .iter()
            .map(|tool| (tool.name(), tool.mutates()))
            .collect();
        assert!(!mutates["firefox_open"]);
        assert!(!mutates["firefox_logs"]);
        // `firefox_screenshot` writes the PNG to the workspace, so it mutates.
        assert!(mutates["firefox_screenshot"]);
        assert!(mutates["firefox_click"]);
        assert!(mutates["firefox_fill"]);
        assert!(mutates["firefox_eval"]);
        assert!(mutates["firefox_close"]);
    }

    #[test]
    fn port_is_deterministic_within_a_process_and_never_the_default() {
        // The same PID always maps to the same port (so the URL is known
        // before geckodriver exists), and it is never 4444 — geckodriver's
        // own default — so a driver a user runs by hand is never clobbered.
        for pid in [1u32, 1000, 4096, 65535, 4_294_967_295] {
            assert_eq!(Browser::port_for(pid), Browser::port_for(pid));
            assert_ne!(Browser::port_for(pid), 4444);
        }
        // And two different PIDs don't have to collide, but if they do the
        // ports are still distinct within the 14949..16948 band.
        let (a, b) = (Browser::port_for(1), Browser::port_for(2));
        assert!((14_949..=16_948).contains(&a));
        assert!((14_949..=16_948).contains(&b));
    }

    /// The console hook is the load-bearing part of `firefox_logs` — it must
    /// be idempotent (re-injecting is a no-op) and defensive (it never throws
    /// on a page with no console, and it bounds the ring buffer).
    #[test]
    fn console_hook_is_idempotent_and_bounded() {
        // `__caConsole` guard means a second injection can't double-wrap.
        assert!(CONSOLE_HOOK.contains("if(!window.__caConsole)"));
        // The ring buffer is bounded so a chatty page can't grow it forever.
        assert!(CONSOLE_HOOK.contains("length>200"));
        // It wraps the real methods rather than replacing them wholesale.
        for level in ["log", "info", "warn", "error", "debug"] {
            assert!(CONSOLE_HOOK.contains(level), "should wrap {level}");
        }
        // It captures uncaught errors and unhandled rejections too.
        assert!(CONSOLE_HOOK.contains("unhandledrejection"));
        assert!(CONSOLE_HOOK.contains("addEventListener(\"error\""));
    }
}
