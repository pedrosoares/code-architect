//! Model pricing and cost accounting.
//!
//! The table below is a **cached snapshot** (2026-06-24) of Anthropic's
//! first-party rates. It is not authoritative and does not cover partner
//! platforms (Bedrock, Vertex) or third-party gateways, which bill their own
//! rates. Check <https://claude.com/pricing> before relying on it for billing.

use serde::{Deserialize, Serialize};

use crate::usage::Usage;

/// USD per one million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
}

impl Pricing {
    /// Standard Anthropic cache multipliers: writes cost 1.25x the input rate,
    /// reads 0.1x.
    pub const fn new(input: f64, output: f64) -> Self {
        Self {
            input,
            output,
            cache_write: input * 1.25,
            cache_read: input * 0.1,
        }
    }
}

/// What a request cost, in USD.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
    pub total: f64,
}

impl Usage {
    pub fn cost(&self, pricing: &Pricing) -> Cost {
        let per_million = |tokens: u64, rate: f64| tokens as f64 * rate / 1_000_000.0;

        let input = per_million(self.input_tokens, pricing.input);
        let output = per_million(self.output_tokens, pricing.output);
        let cache_write = per_million(self.cache_write_tokens, pricing.cache_write);
        let cache_read = per_million(self.cache_read_tokens, pricing.cache_read);

        Cost {
            input,
            output,
            cache_write,
            cache_read,
            total: input + output + cache_write + cache_read,
        }
    }
}

/// Published rates for a model, or `None` when the model is not in the table.
///
/// Local and self-hosted models return `None` — reporting a $0.00 cost for them
/// would be indistinguishable from a genuinely free hosted model, so callers are
/// made to handle "unknown" explicitly.
///
/// Gateway-prefixed ids (`anthropic/claude-opus-5` from OpenRouter and friends)
/// are matched on the trailing segment.
pub fn pricing_for(model: &str) -> Option<Pricing> {
    let model = model.rsplit('/').next().unwrap_or(model);

    Some(match model {
        "claude-fable-5" | "claude-mythos-5" => Pricing::new(10.0, 50.0),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => {
            Pricing::new(5.0, 25.0)
        }
        "claude-sonnet-5" | "claude-sonnet-4-6" => Pricing::new(3.0, 15.0),
        "claude-haiku-4-5" => Pricing::new(1.0, 5.0),
        _ => return None,
    })
}

/// Cost of `usage` under `model`'s published rates, or `None` for an unpriced model.
pub fn cost_for(model: &str, usage: &Usage) -> Option<Cost> {
    pricing_for(model).map(|pricing| usage.cost(&pricing))
}

/// A model's context window, in tokens — the same "cached snapshot, not
/// authoritative" caveat as [`pricing_for`] applies. `None` for anything not
/// in this table, the same "unknown, not zero" reasoning: a local model's
/// actual ceiling isn't zero, it's just not published anywhere this table
/// could read it from, so callers show the raw token count instead of a
/// possibly-wrong percentage.
///
/// Gateway-prefixed ids are matched the same way `pricing_for` matches them.
pub fn context_window_for(model: &str) -> Option<u64> {
    let model = model.rsplit('/').next().unwrap_or(model);

    Some(match model {
        "claude-fable-5" | "claude-mythos-5" | "claude-opus-5" | "claude-opus-4-8"
        | "claude-opus-4-7" | "claude-opus-4-6" | "claude-sonnet-5" | "claude-sonnet-4-6"
        | "claude-haiku-4-5" => 200_000,
        _ => return None,
    })
}

/// Whether a model is known to accept image input. Unlike `pricing_for`/
/// `context_window_for`, there is no meaningful "unknown" value to return
/// here — this is purely informational (e.g. a badge next to the model
/// name), never used to block attaching an image, since most real usage
/// of this app is local models this table has no data for at all. `false`
/// for anything not recognized; it does not mean the model can't actually
/// see images, only that this table doesn't know it can.
///
/// Gateway-prefixed ids are matched the same way `pricing_for` matches them.
pub fn supports_vision(model: &str) -> bool {
    let model = model.rsplit('/').next().unwrap_or(model);

    matches!(
        model,
        "claude-fable-5"
            | "claude-mythos-5"
            | "claude-opus-5"
            | "claude-opus-4-8"
            | "claude-opus-4-7"
            | "claude-opus-4-6"
            | "claude-sonnet-5"
            | "claude-sonnet-4-6"
            | "claude-haiku-4-5"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_a_known_model() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Usage::default()
        };
        let cost = cost_for("claude-opus-5", &usage).expect("opus 5 is priced");

        assert_eq!(cost.input, 5.0);
        assert_eq!(cost.output, 25.0);
        assert_eq!(cost.total, 30.0);
    }

    #[test]
    fn strips_a_gateway_prefix() {
        assert_eq!(
            pricing_for("anthropic/claude-sonnet-5"),
            pricing_for("claude-sonnet-5")
        );
    }

    #[test]
    fn local_models_are_unpriced_rather_than_free() {
        assert!(pricing_for("qwen/qwen3.8-27b").is_none());
        assert!(cost_for("deepseek-v4-flash", &Usage::default()).is_none());
    }

    #[test]
    fn reports_a_known_models_context_window() {
        assert_eq!(context_window_for("claude-sonnet-5"), Some(200_000));
        assert_eq!(
            context_window_for("anthropic/claude-sonnet-5"),
            context_window_for("claude-sonnet-5")
        );
    }

    #[test]
    fn local_models_have_no_known_context_window() {
        assert!(context_window_for("qwen/qwen3.8-27b").is_none());
    }

    #[test]
    fn reports_a_known_models_vision_support() {
        assert!(supports_vision("claude-sonnet-5"));
        assert!(supports_vision("anthropic/claude-sonnet-5"));
    }

    #[test]
    fn local_models_are_not_assumed_to_support_vision() {
        // Not "confirmed unsupported" — just no data either way, the same
        // "unknown, don't guess" convention `pricing_for` already follows.
        assert!(!supports_vision("qwen/qwen3.8-27b"));
    }

    #[test]
    fn cache_reads_are_a_tenth_of_input() {
        let usage = Usage {
            cache_read_tokens: 1_000_000,
            ..Usage::default()
        };
        let cost = cost_for("claude-opus-5", &usage).unwrap();

        assert!((cost.total - 0.5).abs() < f64::EPSILON);
    }
}
