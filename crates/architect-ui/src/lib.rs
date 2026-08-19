//! Shared presentation layer for Code Architect.
//!
//! This crate owns the design tokens and the reusable widgets. It depends on
//! `freya` and nothing else — never on the agent, the LLM client or session
//! storage. Keeping that direction one-way is what lets the UI be reused by a
//! second window, a settings app or a future headless-client shell.

pub mod theme;
pub mod widgets;
