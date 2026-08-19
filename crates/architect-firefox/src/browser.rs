//! Drive a real, headless Firefox on the LLM's behalf — open pages, read the
//! console, click, fill, evaluate JS, screenshot.
//!
//! This crate is the *only* place in the codebase that knows geckodriver
//! exists. Everything else talks to the [`Browser`] handle the engine worker
//! owns (see `apps/desktop/src/engine.rs`): it lazily starts a
//! `geckodriver` child process on the first browser tool call, creates one
//! WebDriver session against it, and routes every operation through the
//! standard W3C WebDriver HTTP API on `127.0.0.1:<port>`.
//!
//! # Logging
//!
//! `firefox_logs` does **not** use the W3C `se/log` endpoint — geckodriver
//! does not implement it (verified against geckodriver 0.37.1 / the
//! `webdriver` 0.54.0 crate it depends on), and page `console.*` output does
//! not leak into geckodriver's stderr. Instead a small JavaScript hook
//! ([`CONSOLE_HOOK`]) is injected after each navigation via
//! `execute/sync` and read back the same way: robust across geckodriver
//! versions, and it captures exactly what a human sees in DevTools.

use std::{sync::Arc, time::Duration};

use base64::Engine;
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};

/// Events the [`Browser`] emits as its state changes — `ProcessEvent`'s
/// analog, for the browser. The engine forwards each one to the UI through
/// its own `EngineEvent` channel, exactly like it does for `process_rx`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserEvent {
    /// `geckodriver` was spawned and is answering on `<port>`.
    DriverStarted { port: u16 },
    /// A WebDriver session is live (created, or already there and re-used).
    SessionStarted,
    /// A page finished loading.
    Navigated { url: String, title: String },
    /// The session was ended and `geckodriver` terminated.
    Closed,
    /// An operation failed; the message is the one the tool surfaced to the
    /// model.
    Failed { message: String },
}

impl BrowserEvent {
    /// A stable, UI-friendly label for this kind of event.
    pub fn kind(&self) -> &'static str {
        match self {
            BrowserEvent::DriverStarted { .. } => "driver_started",
            BrowserEvent::SessionStarted => "session_started",
            BrowserEvent::Navigated { .. } => "navigated",
            BrowserEvent::Closed => "closed",
            BrowserEvent::Failed { .. } => "failed",
        }
    }
}

/// The one-and-only WebDriver session this app holds: lazily started, shared
/// across tool calls and turns, and torn down by `close()` (which the engine
/// calls at shutdown, next to `ProcessRegistry::kill_all`).
///
/// `Send + Sync` — a `reqwest::Client` and two `tokio::sync::Mutex`es — so
/// it is safe to hold as `Arc<Browser>` inside `Tool`s that the engine
/// registers on every turn, and clippy will flag any regression of that.
pub struct Browser {
    client: Client,
    base_url: String,
    port: u16,
    /// The `geckodriver` child process. `None` until the first tool call
    /// needs it — starting it (and the session) eagerly would make app
    /// startup pay for a feature most sessions never use.
    driver: Arc<Mutex<Option<tokio::process::Child>>>,
    /// The live session's id, if any. Cleared on `close()` and re-created on
    /// the next `open`.
    session_id: Arc<Mutex<Option<String>>>,
    /// Where to deliver lifecycle events — the engine pipes this into its
    /// `EngineEvent` stream.
    event_tx: mpsc::UnboundedSender<BrowserEvent>,
}

impl Browser {
    /// Build a handle for the browser owned by this process.
    ///
    /// Nothing is started yet — the first `open`/`ensure` call spawns
    /// `geckodriver`. `event_tx` receives the lifecycle events (the engine
    /// forwards them to the UI).
    pub fn new(event_tx: mpsc::UnboundedSender<BrowserEvent>) -> Self {
        let port = Self::port_for(std::process::id());
        Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(90))
                .build()
                .expect("a default reqwest client builds"),
            base_url: format!("http://127.0.0.1:{port}"),
            port,
            driver: Arc::new(Mutex::new(None)),
            session_id: Arc::new(Mutex::new(None)),
            event_tx,
        }
    }

    /// The local port this instance will run `geckodriver` on.
    ///
    /// Derived from the PID rather than scanned: deterministic within a
    /// process (so the URL is known before the process exists), and spread
    /// across `14949..16948` so concurrent app instances (tests, a second
    /// window) don't collide. 4444 — geckodriver's own default — is never
    /// used, so a driver a user runs by hand is never clobbered.
    pub fn port_for(pid: u32) -> u16 {
        14_949 + (pid % 2_000) as u16
    }

    fn emit(&self, event: BrowserEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Is `geckodriver` up, and is a session live?
    pub async fn status(&self) -> BrowserStatus {
        let driver_running = match self.driver.lock().await.as_mut() {
            Some(child) => Self::is_running(child),
            None => false,
        };
        let has_session = self.session_id.lock().await.is_some();
        BrowserStatus {
            running: driver_running,
            has_session,
            port: self.port,
        }
    }

    /// True while `child` is still running; false if it has exited (or we
    /// can't tell). `try_wait` is non-blocking, so this is a cheap liveness
    /// probe.
    fn is_running(child: &mut tokio::process::Child) -> bool {
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(_) => false,
        }
    }

    /// Make sure a session is live, starting `geckodriver` and/or the
    /// session if either is missing. Every operation routes through this
    /// first — so a session that went away (browser crash, a `close` the
    /// model didn't know about) is transparently re-created.
    async fn ensure_session(&self) -> Result<String, String> {
        if let Some(session) = self.session_id.lock().await.clone() {
            return Ok(session);
        }
        // No live session: the driver must be running (or be started).
        let mut driver = self.driver.lock().await;
        // Probe liveness and reap a dead driver in one pass. `child`'s borrow
        // ends when this match ends, so the `*driver = None` below is legal.
        let alive = match driver.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(None) => true,
                Ok(Some(_)) | Err(_) => false,
            },
            None => false,
        };
        if !alive {
            *driver = None; // drop any dead handle, then start fresh below
            let geckodriver = self.geckodriver_command().await?;
            tracing::info!(?geckodriver, port = self.port, "starting geckodriver");
            let mut child = tokio::process::Command::new(geckodriver)
                .arg("--port")
                .arg(self.port.to_string())
                .arg("--log")
                .arg("warn")
                .spawn()
                .map_err(|error| format!("could not start geckodriver: {error}"))?;
            if !self.wait_until_ready(&mut child).await {
                let _ = child.kill().await;
                *driver = None;
                let message = "geckodriver started but never became ready — is `geckodriver` a valid WebDriver driver for Firefox?".to_owned();
                self.emit(BrowserEvent::Failed {
                    message: message.clone(),
                });
                return Err(message);
            }
            self.emit(BrowserEvent::DriverStarted { port: self.port });
            *driver = Some(child);
        }
        drop(driver);

        let session = self.create_session().await?;
        *self.session_id.lock().await = Some(session.clone());
        self.emit(BrowserEvent::SessionStarted);
        Ok(session)
    }

    /// `geckodriver` on `PATH`, or the `FIREFOX_GECKODRIVER` override.
    ///
    /// Checked at first use, not at construction, so `Browser::new` never
    /// fails and the error (with the exact thing to install) is the model's
    /// to read at the moment it actually needs a browser.
    async fn geckodriver_command(&self) -> Result<String, String> {
        if let Ok(path) = std::env::var("FIREFOX_GECKODRIVER")
            && !path.is_empty()
        {
            return Ok(path);
        }
        Ok("geckodriver".to_owned())
    }

    /// Poll `GET /status` until the driver answers `ready:true`, or the
    /// process exits, or ~30s pass.
    async fn wait_until_ready(&self, child: &mut tokio::process::Child) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if !Self::is_running(child) {
                return false;
            }
            // `None` = the request failed (driver not up yet), `Some(b)` = it
            // answered `ready: b`.
            if let Some(true) = self.status_ready().await {
                return true;
            }
            if std::time::Instant::now() > deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    /// `GET /status` → `Some(ready)`, or `None` if the request itself failed
    /// (driver not up yet).
    async fn status_ready(&self) -> Option<bool> {
        let response = self
            .client
            .get(format!("{}/status", self.base_url))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return Some(false);
        }
        let body: Value = response.json().await.ok()?;
        Some(body.pointer("/value/ready").and_then(Value::as_bool) == Some(true))
    }

    async fn create_session(&self) -> Result<String, String> {
        let body = json!({
            "capabilities": {
                "alwaysMatch": {
                    "browserName": "firefox",
                    "moz:firefoxOptions": { "args": ["-headless"] }
                }
            }
        });
        let response = self
            .client
            .post(format!("{}/session", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("could not create a WebDriver session: {error}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!(
                "creating the WebDriver session failed ({status}): {text}"
            ));
        }

        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("could not parse the session response: {error}"))?;

        body.pointer("/value/sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "geckodriver accepted the session but returned no session id".to_owned())
    }

    async fn delete_session(&self) {
        if let Some(session) = self.session_id.lock().await.clone() {
            let _ = self
                .client
                .delete(format!("{}/session/{}", self.base_url, session))
                .send()
                .await;
        }
        *self.session_id.lock().await = None;
    }

    /// End the session and terminate `geckodriver`. Idempotent — safe to
    /// call from both `firefox_close` and the engine's shutdown path.
    pub async fn close(&self) {
        self.delete_session().await;
        if let Some(mut child) = self.driver.lock().await.take() {
            // `geckodriver` exits on its own once the last session goes —
            // give it a moment, then make sure it's really gone.
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        self.emit(BrowserEvent::Closed);
    }

    /// Navigate to `url` (creating the session on first use) and report the
    /// page's title.
    pub async fn open(&self, url: &str) -> Result<(String, String), String> {
        let session = self.ensure_session().await?;
        let response = self
            .client
            .post(format!("{}/session/{session}/url", self.base_url))
            .json(&json!({ "url": url }))
            .send()
            .await
            .map_err(|error| format!("could not navigate to {url}: {error}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            let message = format!("navigation to {url} failed ({status}): {text}");
            self.emit(BrowserEvent::Failed {
                message: message.clone(),
            });
            return Err(message);
        }
        // Let the page's own scripts run a beat before we read the title —
        // an SPA that sets `document.title` on load would otherwise report
        // the pre-paint empty string.
        tokio::time::sleep(Duration::from_millis(400)).await;

        // Install the console hook *now*, while the model is still between
        // tool calls — so the very next interaction (a `click`, a `fill`, a
        // `eval` that logs) is captured. A navigation wipes `window`, which is
        // why this runs on every `open`, not just the first. Fire-and-forget:
        // if the page isn't ready yet the call fails and `logs()` re-injects
        // as a safety net (see `inject_console_hook`). Without this, the
        // natural `open` → interact → `firefox_logs` order would read an empty
        // buffer, because the hook only comes into being on the first `logs`.
        let _ = self.inject_console_hook(&session).await;

        let title = self.get_title(&session).await.unwrap_or_default();
        let emitted = BrowserEvent::Navigated {
            url: url.to_owned(),
            title: title.clone(),
        };
        self.emit(emitted);
        Ok((url.to_owned(), title))
    }

    async fn get_title(&self, session: &str) -> Result<String, String> {
        let response = self
            .client
            .get(format!("{}/session/{session}/title", self.base_url))
            .send()
            .await
            .map_err(|error| format!("could not read the page title: {error}"))?;
        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("could not parse the title response: {error}"))?;
        Ok(body
            .pointer("/value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    async fn current_url(&self, session: &str) -> Result<String, String> {
        let response = self
            .client
            .get(format!("{}/session/{session}/url", self.base_url))
            .send()
            .await
            .map_err(|error| format!("could not read the current URL: {error}"))?;
        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("could not parse the URL response: {error}"))?;
        Ok(body
            .pointer("/value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    /// The page console captured since the last `firefox_logs` call (plus
    /// the page title/URL). Re-injects the console hook first, since a
    /// navigation wipes it.
    pub async fn logs(&self, clear: bool) -> Result<BrowserLogs, String> {
        let session = self.ensure_session().await?;
        // A fresh navigation (or the very first load) starts with no hook —
        // inject one now, and it survives until the next load.
        let _ = self.inject_console_hook(&session).await;
        let url = self.current_url(&session).await.ok();
        let title = self.get_title(&session).await.ok();

        let entries_script = if clear {
            "let e=window.__caConsole||[]; window.__caConsole=[]; return e;"
        } else {
            "return window.__caConsole||[];"
        };
        let result = self
            .execute(&session, entries_script, &[])
            .await
            .map_err(|error| format!("could not read the page console: {error}"))?;

        let entries = result
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        Some(ConsoleEntry {
                            level: entry.get("level")?.as_str()?.to_owned(),
                            text: entry.get("text")?.as_str()?.to_owned(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(BrowserLogs {
            url,
            title,
            entries,
        })
    }

    /// Inject [`CONSOLE_HOOK`] into the page. Fire-and-forget: if the page
    /// hasn't finished loading the call may fail — that's fine, the *next*
    /// `logs`/`click`/… call re-injects, and the hook is idempotent.
    async fn inject_console_hook(&self, session: &str) -> Result<(), String> {
        self.execute(session, CONSOLE_HOOK, &[]).await.map(|_| ())
    }

    async fn find_element(&self, session: &str, selector: &str) -> Result<String, String> {
        let response = self
            .client
            .post(format!("{}/session/{session}/element", self.base_url))
            .json(&json!({ "using": "css selector", "value": selector }))
            .send()
            .await
            .map_err(|error| format!("could not find the element '{selector}': {error}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!(
                "no element matched the CSS selector '{selector}' ({status}): {text}"
            ));
        }
        let body: Value = response.json().await.unwrap_or(Value::Null);
        let element = body
            .pointer("/value")
            .and_then(Value::as_object)
            .and_then(|value| value.iter().next())
            .and_then(|(_, id)| id.as_str())
            .ok_or_else(|| {
                format!("geckodriver found the element but returned no id for '{selector}'")
            })?
            .to_owned();
        Ok(element)
    }

    /// Click the first element matching `selector`.
    pub async fn click(&self, selector: &str) -> Result<(), String> {
        let session = self.ensure_session().await?;
        let element = self.find_element(&session, selector).await?;
        let response = self
            .client
            .post(format!(
                "{}/session/{session}/element/{element}/click",
                self.base_url
            ))
            .json(&json!({}))
            .send()
            .await
            .map_err(|error| format!("could not click the element '{selector}': {error}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!(
                "clicking the element '{selector}' failed ({status}): {text}"
            ));
        }
        Ok(())
    }

    /// Set the value of the first element matching `selector`.
    ///
    /// A `<select>` can't be cleared or typed into (Marionette rejects it with
    /// "Unable to clear element that cannot be edited"), so it's set by
    /// matching the option by `value` — then by visible label — and firing the
    /// page's own `change` event ([`select_option`]). Any other element uses
    /// `clear` + send-keys, which likewise fires `input`/`change` so a
    /// React/Vue controlled input sees the new text (setting `.value`
    /// directly would not).
    pub async fn fill(&self, selector: &str, value: &str) -> Result<(), String> {
        let session = self.ensure_session().await?;
        let element = self.find_element(&session, selector).await?;

        if self.element_tag(&session, &element).await? == "select" {
            return self.select_option(&session, selector, value).await;
        }

        let clear = self
            .client
            .post(format!(
                "{}/session/{session}/element/{element}/clear",
                self.base_url
            ))
            .json(&json!({}))
            .send()
            .await
            .map_err(|error| format!("could not clear the element '{selector}': {error}"))?;
        if !clear.status().is_success() {
            let status = clear.status();
            let text = clear.text().await.unwrap_or_default();
            return Err(format!(
                "clearing the element '{selector}' failed ({status}): {text}"
            ));
        }
        let keys = self
            .client
            .post(format!(
                "{}/session/{session}/element/{element}/value",
                self.base_url
            ))
            .json(&json!({ "text": value }))
            .send()
            .await
            .map_err(|error| format!("could not fill the element '{selector}': {error}"))?;
        if !keys.status().is_success() {
            let status = keys.status();
            let text = keys.text().await.unwrap_or_default();
            return Err(format!(
                "filling the element '{selector}' failed ({status}): {text}"
            ));
        }
        Ok(())
    }

    /// The element's tag name in lower case (e.g. `select`, `input`).
    ///
    /// Reads `property/tagName`, not `attribute/tagname`: on geckodriver 0.37
    /// the *attribute* read returns `{"value": null}` for a tag name while the
    /// *property* endpoint returns it (uppercase) — the attribute read silently
    /// failed the "is this a select" check and selects fell through to `clear`.
    async fn element_tag(&self, session: &str, element: &str) -> Result<String, String> {
        let response = self
            .client
            .get(format!(
                "{}/session/{session}/element/{element}/property/tagName",
                self.base_url
            ))
            .send()
            .await
            .map_err(|error| format!("could not read the element's tag: {error}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("reading the element tag failed ({status}): {text}"));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("could not parse the element tag: {error}"))?;
        Ok(body
            .pointer("/value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase())
    }

    /// Set a `<select>` to `value` — match the option by its `value`
    /// attribute first, then its visible label, then fire the page's own
    /// `change` event so a React/Vue controlled select reacts.
    ///
    /// Not the W3C "Select an Element Option" endpoint
    /// (`POST /element/{id}/element`): on geckodriver 0.37 it locates the
    /// option but does *not* change the select's value (and requires a `using`
    /// field the spec doesn't document here), so we set the value in-page
    /// instead. `selector` is the same selector the caller matched with; it is
    /// passed as an *argument* (never inlined) so a quoted CSS selector can't
    /// break or inject into the script.
    async fn select_option(
        &self,
        session: &str,
        selector: &str,
        value: &str,
    ) -> Result<(), String> {
        const SCRIPT: &str = "var el = document.querySelector(arguments[0]); \
                             var want = arguments[1]; \
                             var opt = null; \
                             for (var i = 0; i < el.options.length; i++) { if (el.options[i].value === want) { opt = el.options[i]; break; } } \
                             if (!opt) { for (var i = 0; i < el.options.length; i++) { if (el.options[i].textContent.trim() === want) { opt = el.options[i]; break; } } } \
                             if (!opt) { throw new Error('no option in <select> matching value or label: ' + want); } \
                             el.value = opt.value; \
                             el.dispatchEvent(new Event('change', { bubbles: true })); \
                             return el.value;";
        self.execute(
            session,
            SCRIPT,
            &[
                Value::String(selector.to_owned()),
                Value::String(value.to_owned()),
            ],
        )
        .await
        .map(|_| ())
        .map_err(|error| format!("setting the select to '{value}' failed: {error}"))
    }

    /// Run a JavaScript expression in the page and return its JSON value.
    ///
    /// `args` are exposed to the script as `arguments[0..]` (the W3C
    /// `execute/sync` convention). Geckodriver runs the script as a function
    /// body, so a bare expression (`document.title`, an IIFE, `(1+2)`) is
    /// wrapped in `return (…)` to come back, while a multi-statement script or
    /// one led by a statement keyword (`let x = 1; return x`) is left to its
    /// own `return` — see [`wrap_expression`].
    pub async fn eval(&self, script: &str, args: &[Value]) -> Result<Value, String> {
        let session = self.ensure_session().await?;
        let wrapped = Self::wrap_expression(script);
        self.execute(&session, &wrapped, args).await
    }

    /// Wrap a bare expression in `return (…)` so geckodriver's function-body
    /// execution actually returns it.
    ///
    /// Geckodriver runs the script as a *function body*. Two inputs arrive:
    /// an **expression** whose value the model wants (`document.title`, an IIFE
    /// `(fn(){return 42})()`, `(1+2)`, `a ? b : c`), and a **statement script**
    /// that carries its own `return` (`let x=1; return x`,
    /// `if (cond) return a; return b;`). A bare *statement* is a valid
    /// statement (`42;`) that evaluates and is **discarded** — so an IIFE or
    /// `(1+2)` returned `null`, and a multi-line `a; b` was a `SyntaxError`
    /// when naively wrapped as `return (a; b)`. The rule:
    ///
    /// - **multi-statement** (a top-level `;` or newline outside a string
    ///   literal) → leave as-is: the model wrote a script with its own `return`;
    /// - **a statement keyword as the first token** (`let`, `if`, `function`, …)
    ///   → leave as-is, for the same reason;
    /// - **anything else is an expression** → `return (…);` so its value comes
    ///   back, IIFEs and parenthesized expressions included.
    fn wrap_expression(script: &str) -> String {
        if Self::has_top_level_statement_separator(script) {
            return script.to_owned();
        }
        let trimmed = script.trim_start();
        // A statement keyword as the *whole first token* carries its own
        // `return`/flow. Whole-word so `do` ≠ `document`, `var` ≠ `variables`.
        const KEYWORDS: [&str; 14] = [
            "function", "async", "let", "var", "const", "return", "if", "for", "while", "do",
            "try", "switch", "class", "throw",
        ];
        if KEYWORDS.iter().any(|kw| {
            trimmed.starts_with(kw)
                && trimmed[kw.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
        }) {
            return script.to_owned();
        }
        format!("return ({script});")
    }

    /// True if `script` has a top-level statement separator — a `;` or a line
    /// break **outside** a string literal. That marks a multi-statement script
    /// (one with its own `return`) rather than a single expression. Expressions
    /// may still contain `;`/newlines *inside* their strings (e.g.
    /// `JSON.parse('{"a":1;}')`), so those don't count: we track `'`/`"`
    /// strings (and their escapes) and only flag separators outside them.
    /// Backticks/template literals aren't tracked — a multi-line template passed
    /// to `firefox_eval` is an edge case left to an explicit `return`.
    fn has_top_level_statement_separator(script: &str) -> bool {
        let mut chars = script.chars().peekable();
        let mut in_string: Option<char> = None;
        while let Some(ch) = chars.next() {
            match in_string {
                Some(quote) => {
                    if ch == '\\' {
                        chars.next(); // skip the escaped char (e.g. an escaped ')
                    } else if ch == quote {
                        in_string = None;
                    }
                }
                None => match ch {
                    '\'' | '"' => in_string = Some(ch),
                    ';' | '\n' | '\r' => return true,
                    _ => {}
                },
            }
        }
        false
    }

    async fn execute(&self, session: &str, script: &str, args: &[Value]) -> Result<Value, String> {
        let response = self
            .client
            .post(format!("{}/session/{session}/execute/sync", self.base_url))
            .json(&json!({ "script": script, "args": args }))
            .send()
            .await
            .map_err(|error| format!("could not run the script: {error}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("the script failed to run ({status}): {text}"));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("could not parse the script response: {error}"))?;
        // A string `value` is a legitimate result, not an error: a script that
        // throws surfaces as a non-2xx status (handled above), so by the time
        // we're here the `value` is the script's real return value. (Treating
        // any string as an error used to turn every string result into an
        // error — the reason `firefox_eval` "couldn't return a string".)
        Ok(body.get("value").cloned().unwrap_or(Value::Null))
    }

    /// A page screenshot as raw PNG bytes — `firefox_screenshot` base64s it
    /// into a [`architect_core::ToolResultImage`].
    pub async fn screenshot(&self) -> Result<Vec<u8>, String> {
        let session = self.ensure_session().await?;
        let response = self
            .client
            .get(format!("{}/session/{session}/screenshot", self.base_url))
            .send()
            .await
            .map_err(|error| format!("could not take a page screenshot: {error}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!(
                "taking a page screenshot failed ({status}): {text}"
            ));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("could not parse the screenshot response: {error}"))?;
        let data = body
            .pointer("/value")
            .and_then(Value::as_str)
            .ok_or_else(|| "geckodriver returned no screenshot data".to_owned())?;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| format!("could not decode the screenshot: {error}"))
    }
}

/// `status()`'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserStatus {
    pub running: bool,
    pub has_session: bool,
    pub port: u16,
}

/// One captured `console.*` line (or uncaught error) from the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleEntry {
    pub level: String,
    pub text: String,
}

/// The result of `Browser::logs` — the page's identity plus its captured
/// console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserLogs {
    pub url: Option<String>,
    pub title: Option<String>,
    pub entries: Vec<ConsoleEntry>,
}

/// The JS that captures a page's console output into a bounded ring buffer.
///
/// Idempotent (`window.__caConsole` guard) and defensive: it never throws,
/// and it serializes objects best-effort so a page that logs a cyclic
/// structure can't take the capture down with it.
pub const CONSOLE_HOOK: &str = r#"
if(!window.__caConsole){
  window.__caConsole=[];
  var cap=function(level){return function(){
    var text=Array.prototype.map.call(arguments,function(x){
      try{return typeof x==="object"&&x!==null?JSON.stringify(x):String(x)}catch(e){return String(x)}}).join(" ");
    window.__caConsole.push({level:level,text:text});
    if(window.__caConsole.length>200){window.__caConsole.shift();}
  }};
  ["log","info","warn","error","debug"].forEach(function(k){
    try{console[k]=cap(k)}catch(e){}});
  window.addEventListener("error",function(e){
    window.__caConsole.push({level:"error",text:"uncaught: "+(e.message||"error")});
  });
  window.addEventListener("unhandledrejection",function(e){
    window.__caConsole.push({level:"error",text:"unhandledrejection: "+((e.reason&&e.reason.message)||e.reason)});
  });
}
return true;
"#;

#[cfg(test)]
mod tests {
    use super::Browser;

    /// The whole point of the wrapper: a bare expression (the most common
    /// `firefox_eval` input) must come back as a `return (…)` statement so
    /// geckodriver's function-body execution actually returns it.
    #[test]
    fn wraps_bare_expressions() {
        assert_eq!(
            Browser::wrap_expression("document.title"),
            "return (document.title);"
        );
        assert_eq!(
            Browser::wrap_expression("  window.__caConsole.length  "),
            "return (  window.__caConsole.length  );"
        );
        assert_eq!(Browser::wrap_expression("a ? b : c"), "return (a ? b : c);");
    }

    /// Scripts that already carry their own flow/return are left as-is —
    /// multi-statement scripts (a top-level `;`/newline) and ones led by a
    /// statement keyword.
    #[test]
    fn leaves_multi_statement_and_keyword_scripts_alone() {
        assert_eq!(
            Browser::wrap_expression("let x = 1; return x"),
            "let x = 1; return x"
        );
        assert_eq!(
            Browser::wrap_expression("function f(){return 1} return f()"),
            "function f(){return 1} return f()"
        );
        assert_eq!(
            Browser::wrap_expression("{ var a = 1; return a; }"),
            "{ var a = 1; return a; }"
        );
        assert_eq!(
            Browser::wrap_expression("if (x) return 1; return 2;"),
            "if (x) return 1; return 2;"
        );
        // A multi-line statement script is left as-is (its own `return`) rather
        // than mangled into `return (a; b)` — the old "SyntaxError" case.
        assert_eq!(
            Browser::wrap_expression("window.__t = 99; window.__t"),
            "window.__t = 99; window.__t"
        );
    }

    /// IIFEs and parenthesized expressions are *expressions* — they must be
    /// wrapped so geckodriver's function-body execution returns their value
    /// instead of evaluating them as discarded statements (the original
    /// "IIFE returns null" bug).
    #[test]
    fn wraps_iifes_and_parenthesized_expressions() {
        assert_eq!(
            Browser::wrap_expression("(() => 42)()"),
            "return ((() => 42)());"
        );
        assert_eq!(
            Browser::wrap_expression("(function(){return 5})()"),
            "return ((function(){return 5})());"
        );
        assert_eq!(Browser::wrap_expression("(1 + 2)"), "return ((1 + 2));");
        assert_eq!(
            Browser::wrap_expression("((x) => x+1)(10)"),
            "return (((x) => x+1)(10));"
        );
    }

    /// A `;` or newline *inside* a string literal is not a statement separator —
    /// `JSON.parse('{"a":1;}')` is a single expression and must still be wrapped.
    #[test]
    fn a_semicolon_inside_a_string_is_not_a_separator() {
        assert_eq!(
            Browser::wrap_expression("JSON.parse('{\"a\":1;}')"),
            "return (JSON.parse('{\"a\":1;}'));"
        );
    }

    /// Whole-word matching: `do` must not match `document`, `var` must not
    /// match `variables`, etc. — otherwise a bare expression would be wrongly
    /// treated as a statement and dropped (the original "returns null" bug).
    #[test]
    fn keyword_match_is_whole_word() {
        // `document` starts with the `do` keyword but is not itself a keyword.
        assert_eq!(
            Browser::wrap_expression("document.title"),
            "return (document.title);"
        );
        // `variables` starts with `var` but is not itself a keyword.
        assert_eq!(
            Browser::wrap_expression("variables[0]"),
            "return (variables[0]);"
        );
        // A bare keyword is a statement and stays alone.
        assert_eq!(Browser::wrap_expression("return 1"), "return 1");
    }
}
