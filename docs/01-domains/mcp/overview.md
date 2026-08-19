---
id: domain.mcp
type: domain
title: MCP Client Domain
relations:
  related_flows:
  - flow.send-message
  related_integrations:
  - integration.mcp
---

The MCP domain (`architect-mcp`) is a **client** for the Model Context Protocol: it connects to external MCP servers and turns their tools into `architect_tools::Tool` implementations, so the agent invokes them exactly like built-ins (`read_file`, `write_file`). It deliberately implements `architect-tools`' `Tool` trait — "filling an extension point rather than the other way round" — and does not depend on `architect-config` or `apps/desktop`; the desktop engine is the sole place that knows saved server configs, MCP connections, and the tool registry all exist and bridges them.

## Public API

- **`pub async fn connect(command: &str, args: &[String], env: &HashMap<String, String>) -> Result<McpConnection, McpError>`** — spawn a local server process over stdio, complete the handshake, discover all tools.
- **`pub async fn connect_http(url: &str, bearer_token: Option<&str>) -> Result<McpConnection, McpError>`** — remote server over streamable HTTP with an optional static Bearer header (no OAuth flow).
- **`pub struct McpConnection`** — fields: `_service: RunningService<RoleClient, ()>` (private, never read — held only to keep the connection alive; it owns the child process/stdio pipes or HTTP session) and **`pub tools: Vec<Arc<dyn Tool>>`** (a public field, not a `tools()` method; the struct has no other methods, no `close()`).
- **`pub enum McpError`** — `Transport(#[from] rmcp::RmcpError)` ("could not start the process: {0}") · `Initialize(#[from] ClientInitializeError)` ("MCP handshake failed: {0}") · `ListTools(#[from] ServiceError)` ("could not list the server's tools: {0}") — connect-phase failures only.
- **`McpToolAdapter`** — `pub` but not constructible outside the crate (private constructor + fields).

Call routing: at connect time each discovered tool stores a clone of `service.peer()` (an `rmcp` `Peer<RoleClient>` handle, `Clone + Send + Sync`, so one connection's handle is shared safely across concurrent turns). `Tool::call` sends `self.peer.call_tool(CallToolRequestParams::new(self.name).with_arguments(arguments))`; `_service` is never touched after construction.

## Transports and handshake

- **stdio**: `rmcp`'s `TokioChildProcess` transport wrapping `tokio::process::Command` — `args` and each `env` entry applied via a `configure` closure (cloned up front to move into the closure).
- **HTTP**: `StreamableHttpClientTransport::from_config(config)`; `config.auth_header(token)` when a bearer token is present (a plain `Authorization: Bearer` header).
- **Handshake (identical for both)**: `().serve(transport).await` runs the MCP `initialize` handshake with the no-op `ClientHandler` blanket impl on `()` (a tool-only client needs no sampling or roots support), yielding a `RunningService`; then `service.list_all_tools().await` (paginates internally). All-or-nothing: if listing fails the service is dropped and the child process killed.

## Result mapping

`Tool::call` maps the remote `CallToolResult`: input must be a JSON object — **non-object input is silently coerced to `{}`** (not an error the model sees). On success, `content_to_text` walks `content`, keeps only `Text` blocks, joins with `"\n"` — images and embedded resources are **dropped** (every built-in tool speaks plain text, so this is what the rest of the app expects; `Ok(String::into())` yields a text-only `ToolOutput`). If `is_error == Some(true)`, the joined text is returned as `Err(text)` — the server's own error text, no prefix. Transport/protocol errors mid-call bypass `McpError` entirely: `.map_err(|e| e.to_string())` — a plain `String` the model sees.

## Tool naming

**No prefixing or namespacing.** The adapter stores the server's name verbatim (`Box::leak` — `name()`/`description()` must be `&'static str`, but MCP tool names are only known once connected; a fixed, one-time allocation per discovered tool, not a growing leak). An MCP server exposing a tool named `read_file` collides with the built-in at the name level; disambiguation is left to the registry assembler (the desktop engine). `mutates()` returns `true` unconditionally — conservative: an MCP tool's side effects are unknown to us. `ToolContext` is deliberately unused (`_ctx`).

## Lifecycle

- **Established** only by `connect`/`connect_http` (spawn + initialize + full tool listing must all succeed).
- **Closed by dropping** the `McpConnection` — no `close()`, no `Drop` impl, no idle timeout.
- **No reconnection logic in this crate** — "a reconnect is just 'build a new one and let the old one drop'"; that's the caller's job (the desktop engine's `rebuild_external_tools` drops all connections wholesale and reconnects on config change).
- **If a server dies mid-call**: `call_tool` returns an rmcp error → `Err(error.to_string())`. The `McpConnection` value still exists but its tools are dead — no health check, no dead-peer detection.
- **The `tools` list is a connect-time snapshot** — tools a server adds later are invisible until a full reconnect.
- **No timeouts anywhere in production code** — a hung server hangs the tool call indefinitely (the only timeout in the crate is a 5-second `connect` guard in one test).

## Tests

- **Unit** (`adapter.rs`): no mocks — `rmcp::model` values constructed directly. `leaking_the_same_value_twice_gives_independent_static_strings` · `converts_a_json_object_schema_into_a_value` · `joins_every_text_block_and_ignores_the_rest` · `a_successful_result_with_no_text_content_is_an_empty_string` · `an_error_result_is_reported_as_the_tools_own_error_text`.
- **Live** (`tests/live.rs`, ignored by default) against the official demo server: `connecting_to_a_nonexistent_command_fails_fast` (runs by default — 5s timeout, must not hang) and `connects_lists_tools_and_calls_one` (`#[ignore]`, spawns `npx -y @modelcontextprotocol/server-everything`, finds tool `get-sum`, calls it with `{a: 5, b: 3}`, asserts the output contains `8`).

## Integration with the engine

Saved `McpServerConfig`s (`architect-config`) live in the same `ConfigStore`/`profiles.json` as profiles; multiple servers can be `enabled` at once. The desktop engine's `reconnect_mcp` connects to every enabled server at startup and after any `SaveMcpServer`/`DeleteMcpServer`/`SaveIntegrations`/`SaveDocsConfig`/successful-OAuth command, registers each tool into every turn's `ToolRegistry` alongside the built-ins, and — unlike the per-turn built-in registry — the connections are **long-lived and shared**, captured once when a turn is spawned. A failed server is reported once via the global-failure path and skipped; the rest still connect.

## Notable / surprising

- `McpError::Transport`'s message "could not start the process: {0}" is also used for `connect_http` transport-creation failures — misleading for HTTP.
- Non-object tool input is silently coerced to empty arguments rather than an error the model can see.
- `McpConnection` has zero methods; `_service` is provably never read (only dropped).
- `input_schema()` clones the schema on every call.
- `McpToolAdapter` being `pub` is vestigial — impossible to construct externally.
- `rmcp`'s exact version is workspace-inherited (v3, per root `Cargo.toml`).