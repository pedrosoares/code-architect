---
id: domain.conversation
type: domain
title: Conversation & Message Domain
depends_on:
- system.glossary
- domain.llm-providers
relations:
  related_rules:
  - rule.usage-cost
  related_flows:
  - flow.send-message
  - flow.session-lifecycle
---

The conversation domain owns the shared message vocabulary every other layer speaks, plus the UI's per-session transcript state.

## Core types (`architect-core::message`)

Provider-neutral; each provider adapter translates to/from its wire format (differences live in the LLM domain).

- **`Role`** — `System | User | Assistant | Tool`; serializes lowercase.
- **`ContentBlock`** — internally tagged by `type` (snake_case):
  - `Text { text }`
  - `Reasoning { text, signature: Option<String> }` — the signature carries Anthropic's thinking-block signature, which **must be echoed back unchanged** when the conversation continues on the same model.
  - `ToolUse(ToolCall)` / `ToolResult(ToolResult)` (newtype variants)
  - `Image { media_type, data }` — user attachments only; `data` is always **base64** by the time a value reaches this layer.
- **`ToolCall`** — `{ id, name, input: Value }`. The provider-assigned `id` must be echoed back on the matching result — a mismatch desynchronizes the conversation and the next request fails. `input` is always parsed JSON, never string-matched (providers escape payloads differently).
- **`ToolResult`** — `{ tool_use_id, content: String, image: Option<ToolResultImage>, is_error: bool }`. Invariant: **a failed tool still returns a result, flagged** — dropping it would leave the model waiting on an id that never comes back. Constructors: `ok` / `ok_with_image` / `error`.
- **`ToolResultImage`** — `{ media_type, data }` (base64). Deliberately its own type: a tool result should only ever carry an image.
- **`Message`** — `{ role, content: Vec<ContentBlock> }`. Key helpers:
  - `user_with_images(text, images)` — images listed first, text appended only if non-empty.
  - `tool_results(results)` — all results of one turn as a **single `Role::User` message**; Anthropic requires this batching, and the OpenAI adapter unpacks it back to one message per result.
  - `text()` — concatenates `Text` blocks only.
- **`StopReason`** — `end_turn | tool_use | max_tokens | stop_sequence | refusal | pause_turn`. Invariants:
  - `Refusal` is a safety decline arriving as HTTP 200 — a stop reason, **not** an error; must not be rendered as ordinary text.
  - `PauseTurn` means the server-side tool loop paused; resume by re-sending history with no extra user message.
  - `wants_continuation()` is true for exactly **`ToolUse` and `PauseTurn`** — the only reasons the agent loop runs tools and goes around again.

## Serialization conventions

- Enums serialize as lowercase/snake_case string tags.
- Optional additive fields (`Reasoning.signature`, `ToolResult.image`/`is_error`, `Usage.cache_*`, `Plan.goal`, step/substep `status` and `substeps`) all use `#[serde(default)]` so older persisted sessions keep deserializing.
- `ToolCall.input` and `ToolSchema.input_schema` are opaque `serde_json::Value`.

## UI transcript state (`apps/desktop/src/state.rs`)

`Transcript` holds:

- `conversations: HashMap<SessionId, Conversation>` — one per session, never "the active one".
- `Conversation` — `{ rows: Vec<Row>, status: Status, usage: Usage (cumulative), cost: Option<Cost>, context_tokens: u64 (replaced each turn), context_window: Option<u64>, plan: Option<Plan> }`.
- `Row` — `User { text, images: Vec<Attachment> }` | `Assistant { reasoning, text }` | `Tool { id, index, name, arguments, status: ToolStatus, output }` | `Error { message }` | `Compacted { summary }`.
- `Status` — `Idle | Waiting | Streaming | RunningTools | Compacting | Failed`; `is_busy()` covers everything but `Idle`/`Failed` for the Stop button.
- App-global fields: `sessions` (sidebar list), `profiles`/`active_profile`, `discovered_models`/`active_adhoc_model` (mutually exclusive), `mcp_servers`, `integrations`, `docs_config`, `processes: Vec<ProcessSummary>` (global, not session-owned).

`Transcript::apply(&mut self, &EngineEvent)` is the reducer — pure, no IO, no Freya types. Notable behaviors:

- Routes every event into the conversation it **names by session**, so background sessions keep accumulating off-screen.
- `Row::Assistant` accumulates streaming deltas (reasoning and text in separate strings); a turn that produced only reasoning renders nothing (no orphan avatar).
- `Row::Tool` rows are keyed by `(index)` within a turn; `ToolCallStart` opens one (`Running`), `ToolCallEnd` fills the name/arguments, `ToolFinished` flips status to `Ok`/`Failed` and sets `output`.
- `EngineEvent::Compacted` **replaces all rows** with the single `Row::Compacted` and resets `context_tokens` to 0.
- `apply_process_event` find-or-creates by process id; log strings grow with a 200,000-byte cap (belt-and-suspenders match of the registry's own cap).
- `push_user` sets `Waiting` optimistically before the engine round-trip; `start_compacting` sets `Compacting` the same way.

## Rules

- `context_tokens` (last request's input size) is what the status bar's context-window figure uses; `usage` (cumulative) is for cost. A turn with several tool round-trips resends the whole growing history on each request, so summing usage double-counts for fill purposes.
- `cost` is `None` for unpriced (local) models — the status bar shows "unpriced model" rather than a misleading $0.00. Same "unknown, don't guess" treatment for `context_window` → "unsized model".
- `supports_vision` drives the header's "vision" badge only — **informational, never a gate** on the Attach button or on `view_image`/`screenshot`.
- Images: raw `Vec<u8>` bytes end-to-end in the UI and `Command::Send`; base64 happens exactly twice — encoding when the engine builds the `Message`, decoding in `Conversation::from_history` for resumed sessions.