//! Token accounting.

use std::ops::{Add, AddAssign};

use serde::{Deserialize, Serialize};

/// Tokens consumed by one request, or accumulated across a turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Tokens written to the prompt cache — billed above the input rate.
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Tokens served from the prompt cache — billed well below the input rate.
    #[serde(default)]
    pub cache_read_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_write_tokens + self.cache_read_tokens
    }

    /// Fraction of input served from cache, `None` when nothing was read or
    /// written. Useful for spotting a silently invalidated cache prefix.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let cached = self.cache_read_tokens;
        let uncached = self.input_tokens + self.cache_write_tokens;
        match cached + uncached {
            0 => None,
            total => Some(cached as f64 / total as f64),
        }
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, rhs: Self) {
        self.input_tokens += rhs.input_tokens;
        self.output_tokens += rhs.output_tokens;
        self.cache_write_tokens += rhs.cache_write_tokens;
        self.cache_read_tokens += rhs.cache_read_tokens;
    }
}

impl Add for Usage {
    type Output = Self;

    fn add(mut self, rhs: Self) -> Self {
        self += rhs;
        self
    }
}
