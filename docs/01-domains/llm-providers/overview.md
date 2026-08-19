---
id: domain.llm-providers
type: domain
title: LLM Providers Domain
depends_on:
- domain.conversation
relations:
  related_flows:
  - flow.model-config
  related_integrations:
  - integration.openai-compat
  - integration.anthropic
---

The LLM domain (`architect-llm`) is a provider-agnostic LLM access layer: one `Provider` trait, one normalized `StreamEvent` stream, and adapters for OpenAI-compatible and Anthropic endpoints. It deliberately does not know what tools do or when to call them.

## Public API

- **`Provider`** (trait) — `id() -> &'static str` and `async fn stream(&self, request: ChatRequest, cancel: CancellationToken) -> Result<EventStream, LlmError>`; a provided `complete()` derives from `stream` by collecting, so there is one code path per provider. `EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>`.
- **`ChatRequest`** — `{ model, system: Option<String> (kept out of messages; the natural cache prefix), messages, tools, max_tokens: Option<u32>, params: SamplingParams, cache: CachePolicy, reasoning: Reasoning }`.
- **`ChatResponse`** — `{ model, message, stop_reason, usage }` (the assembled reply; there is no `StreamResponse` type).
- **`ProviderConfig`** — `{ kind, base_url, api_key, extra_headers, model }` with `new(kind)`/`base_url`/`api_key`/`model` builders.
- **`ProviderRegistry`** — `default()` registers exactly two kinds:
  - `"openai"` — base_url defaults to `https://api.openai.com/v1`; any OpenAI-compatible server (LM Studio, DeepSeek, OpenRouter, vLLM, Ollama) is this kind with a different base_url.
  - `"anthropic"` — requires `api_key` (missing → `LlmError::Config`).
  - `register(kind, factory)` is the extension point for genuinely different APIs.
- **`is_local(config)`** — true iff the base_url host is exactly `localhost`, `127.0.0.1`, `[::1]`, or `0.0.0.0`. Rationale: local inference servers can't serve concurrent requests, so sub-agent spawning queues against them.
- **`list_models(base_url, api_key)`** — `GET {base_url}/models`, returns the `id`s in order. A bare function (not a `Provider` method) because its only caller (the LM Studio settings page) has a URL typed into a form before any provider exists.

## The stream protocol

`StreamEvent` (internally tagged `type`, snake_case):

| Variant | Payload |
|---|---|
| `MessageStart` | `{ id, model }` |
| `TextDelta` | `{ text }` |
| `ReasoningDelta` | `{ text }` |
| `ToolCallStart` | `{ index, id, name }` |
| `ToolCallInputDelta` | `{ index, partial_json }` |
| `ToolCallEnd` | `{ index, call: ToolCall }` |
| `Finished` | `{ stop_reason, usage }` |

Tool calls arrive in three parts (start → argument fragments → completed call) because that is how both providers stream them. Assembly in `collect`/`collect_forwarding`: content order reasoning → text → tool calls; `MessageStart`/`ToolCallStart`/`ToolCallInputDelta` are ignored during collection; a stream ending without `Finished` falls back to `EndTurn` with a warning.

## Error model — `LlmError`

| Variant | Meaning |
|---|---|
| `Http { provider, status, body }` | non-2xx (body kept verbatim) |
| `RateLimited { provider, retry_after, body }` | HTTP 429 |
| `Transport(reqwest::Error)` | network failures |
| `Decode { context, message, raw }` | malformed payload (raw truncated to 400 chars) |
| `Api { provider, message }` | provider error inside an otherwise-successful stream |
| `Cancelled` | user stop |
| `UnknownProvider(String)` | kind not in the registry |
| `Config(String)` | header/config problems |

`is_retryable()`: `RateLimited` always; `Http` iff status ≥ 500; `Transport` iff timeout/connect. **Retries** (`http::send_retrying`, `MAX_RETRIES = 3`): 429 → `RateLimited` with `retry_after` parsed from the header (delay-seconds form only); retry while `is_retryable()`, sleeping `retry_after` or exponential backoff `min(30s, 500ms · 2^attempt)`. This is the **only** retry logic in the whole system.

## Option translation

- `SamplingParams { temperature, top_p, top_k }` — OpenAI: each `Some` value sent under its own key. **Anthropic: all three dropped** (current Claude models reject them).
- `Reasoning`: `Auto` (send nothing) | `Off` | `Adaptive { effort: Low|Medium|High|XHigh|Max, display: Summarized|Omitted }`; `Reasoning::VISIBLE = Adaptive { High, Summarized }`. OpenAI: only `Adaptive` → `reasoning_effort`. Anthropic: `Adaptive` → `thinking: {type: adaptive, display}` **and** `output_config: {effort}` (never `budget_tokens`, which 400s); `Off` → `thinking: {type: disabled}`.
- `CachePolicy` (defined in `architect-core`: `tools`, `system`, `history_prefix`; `MAX_BREAKPOINTS = 4`) — honored only by the Anthropic adapter; the OpenAI dialect ignores it. Caching is prefix-matched: one changed byte before a breakpoint invalidates it and everything after, so adapters serialize in stable order (tools → system → messages) and keep volatile content after the last breakpoint.
- **Note:** the desktop engine currently sets no `CachePolicy` and no `SamplingParams` — only `Reasoning::VISIBLE`.

## Provider details (summary)

- **OpenAI-compatible** — `POST {base_url}/chat/completions` with `stream: true` and `stream_options.include_usage: true` (without it usage never arrives); SSE `data:` lines, `[DONE]` sentinel; per-index tool-call accumulation in a `BTreeMap`; cached tokens subtracted from the prompt total for `input_tokens`; `finish_reason` mapping (`tool_calls`→ToolUse, `length`→MaxTokens, `content_filter`→Refusal, other→EndTurn); one `tool`-role message per result, `is_error` rendered as `"ERROR: {content}"`; images switch `content` to the array-of-parts shape; malformed tool arguments survive as `Value::String(raw)`. See the integration doc for wire-level detail.
- **Anthropic** — `POST {base_url}/v1/messages`, headers `x-api-key` + `anthropic-version: 2023-06-01`; required `max_tokens` (default 64,000); system hoisted to top-level `system`; SSE events `message_start`/`content_block_*`/`message_delta`/`message_stop`/`error`/`ping`; stop-reason mapping includes `refusal` and `pause_turn`; `refusal` is a stop reason on HTTP 200, not an error; unsigned thinking blocks are dropped (the API rejects them).

## Wire tests

`tests/openai_wire.rs` (13 tests) and `tests/anthropic_wire.rs` (8 tests) replay recorded SSE against wiremock — covering fragmented parallel tool-call reassembly, usage/cache-token accounting, retry-after-429, images, refusal/pause-turn, and exact request-body shapes. Plus unit tests for the registry, SSE framing (chunk boundaries anywhere, split multibyte characters, CRLF), and backoff.