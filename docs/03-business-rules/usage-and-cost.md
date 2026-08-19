---
id: rule.usage-cost
type: rule
title: Usage, Cost & Context-Window Rules
---

**Two numbers that look alike and mean different things — plus the "unknown, don't guess" discipline.**

## `usage` vs `context_tokens`

- **`Usage`** (`architect-core`) — `{ input_tokens, output_tokens, cache_creation_input_tokens: u64 (default 0), cache_read_input_tokens: u64 (default 0) }` with an `AddAssign` impl. It is a **running sum across every request** a turn or session ever made — right for cost, **wrong for "how full is the window right now"**, because a turn with several tool round-trips resends the whole growing history on each request, so summing double- (triple-, …) counts it.
- **`context_tokens`** — each provider response's own `usage.input_tokens` (+ cache read/write) is already the size of *that* request's whole prompt — a provider reports the total, not a delta — so the **last** response in a turn is what the next request starts from. `agent.rs` tracks this separately as `latest_usage`, **replaced (not `+=`)** each iteration, and reports it on `TurnOutcome` as `context_tokens` alongside `context_window: Option<u64>`.
- **`Conversation.context_tokens`/`context_window`** mirror this one-for-one, **replaced on every `TurnCompleted`** the same way `cost` is — unlike cumulative `usage`.
- A successful `Compacted` resets `Conversation.context_tokens` to **0** — the next request starts from far less than before.

## The status bar

- `<tokens> / <window> context (<percent>%)` when the model has a published context window (`pricing::context_window_for`), **"unsized model"** for a local one with no published ceiling.
- Running token totals and spend sit alongside; `cost` is `None` for unpriced models (status bar shows "unpriced model", never a misleading $0.00).

## The pricing table (`architect-core::pricing`)

- `pricing_for(model_id) -> Option<ModelPricing>` — `ModelPricing { input_per_mtok, output_per_mtok, cache_read_per_mtok, cache_write_per_mtok: f64 }` (USD per 1M tokens).
- **Unknown models return `None`, never a guess** — the same "unknown, don't guess" treatment as `context_window_for` and `supports_vision`. `cost` is `None` when pricing is unknown.
- `supports_vision(model_id) -> bool` — same lookup-table shape, `false` for anything not in the table. Drives **only** the header's "vision" badge (currently the Claude 5 family) — **never a gate** on the Attach button or on `view_image`/`screenshot`: most real usage is a local LM Studio model with no published capability data, and treating "not in the table" as "definitely can't" would block the app's primary audience far more often than it would protect anyone.
- Cache read/write prices are tracked separately because providers price cached input below fresh input.

## Invariants

- `usage` is cumulative per conversation (and per turn on `TurnOutcome`); `context_tokens` is instantaneous. Mixing them up is the bug this doc exists to prevent.
- `cache_creation`/`cache_read` are optional-additive (`#[serde(default)]`) so pre-cache-era persisted sessions keep deserializing.
- Video is explicitly out of scope — neither provider API accepts raw video, and client-side frame extraction was ruled out as a separate, much larger feature.