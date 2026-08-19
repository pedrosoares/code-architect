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
//! ([`browser::CONSOLE_HOOK`]) is injected after each navigation via
//! `execute/sync` and read back the same way: robust across geckodriver
//! versions, and it captures exactly what a human sees in DevTools.
//!
//! # Ownership
//!
//! [`Browser`] is `Send + Sync` and holds no async runtime of its own — a
//! `reqwest::Client` plus two `tokio::sync::Mutex`es — so the engine can own
//! it as `Arc<Browser>` and hand its seven tools ([`tools`]) to the agent on
//! every turn, the same way it does for `architect-tools`' `process_tools`.
//! The browser is started lazily (first use), shared across calls and turns,
//! and torn down by [`Browser::close`], which the engine calls at shutdown
//! next to `ProcessRegistry::kill_all`.

mod browser;
mod tools;

pub use browser::{Browser, BrowserEvent, BrowserLogs, BrowserStatus, CONSOLE_HOOK, ConsoleEntry};
pub use tools::tools;
