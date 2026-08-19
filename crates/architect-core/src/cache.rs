//! Prompt-caching intent, expressed provider-neutrally.

use serde::{Deserialize, Serialize};

/// Anthropic allows at most four cache breakpoints per request.
pub const MAX_BREAKPOINTS: usize = 4;

/// Which parts of a request should be cached.
///
/// Caching is **prefix-matched**: a single changed byte anywhere before a
/// breakpoint invalidates it and everything after. Adapters therefore serialize
/// in a stable order (tools, then system, then messages) and must keep volatile
/// content — timestamps, per-request ids — after the last breakpoint.
///
/// Providers without prompt caching ignore this entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePolicy {
    /// Cache the tool definitions. Worth it whenever the tool set is stable,
    /// which for a code harness is nearly always.
    pub tools: bool,
    /// Cache the system prompt.
    pub system: bool,
    /// Cache the first N messages of history. `None` disables history caching.
    pub history_prefix: Option<usize>,
}

impl CachePolicy {
    /// No caching at all.
    pub const OFF: Self = Self {
        tools: false,
        system: false,
        history_prefix: None,
    };

    pub fn breakpoints(&self) -> usize {
        usize::from(self.tools)
            + usize::from(self.system)
            + usize::from(self.history_prefix.is_some())
    }
}

impl Default for CachePolicy {
    /// Cache the stable prefix — tools and system prompt — and leave history
    /// alone, since the agent loop appends to it on every iteration.
    fn default() -> Self {
        Self {
            tools: true,
            system: true,
            history_prefix: None,
        }
    }
}
