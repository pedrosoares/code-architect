---
id: integration.mcp
type: integration
title: MCP Integration
depends_on:
- domain.mcp
relations:
  related_domains:
  - domain.mcp
  - domain.desktop-app
---

MCP (Model Context Protocol) integration: live connections to external MCP servers, each contributing its own tools to the agent. The crate is `architect-mcp`, built on `rmcp`. This is the generic tool extension path — the integration crates (GitHub/Slack/Linear/docs) fill the same `Tool` seam one-crate-per-service; MCP fills it with an *unbounded, runtime-configured* set.

## Connections

Two transports, both from the `McpServerConfig` list saved in `IntegrationsConfig`/`McpServerConfig` (managed in Settings → MCP servers):

| Transport | Config | Connector |
|---|---|---|
| `Stdio { command, args, env }` | spawn a local process, speak MCP over stdin/stdout | `architect_mcp::connect` |
| `Http { url, bearer_token }` | streamable-HTTP MCP endpoint | `architect_mcp::connect_http` |

Only **enabled** servers connect. Connection failures are best-effort per server: one failing server reports a global `Failed` event and the rest still connect.

## Lifecycle

Owned by the desktop engine's worker, **not** by the tool registry:
- `reconnect_mcp` (called from `rebuild_external_tools`) drops every connection and reconnects the whole list. Rebuild triggers: startup, `SaveMcpServer`, `DeleteMcpServer`, `SaveIntegrations`, `SaveDocsConfig`, and after a successful OAuth login. Whole-list rebuild (not a diff) is deliberate — the lists are small and this is not a hot path.
- Connections are shared `Arc`s registered into each per-turn `ToolRegistry`.

## Adaptation

`McpToolAdapter` implements `architect_tools::Tool` over one remote tool: it forwards `input` and maps the remote result back to `ToolOutput`. Tool names from different servers can collide; a later server's tool replaces an earlier one under the same name (registry semantics).

## See also

- [MCP Domain](../mcp/overview.md) — the full state machine and schema details
- [Desktop App Domain](../desktop-app/overview.md) — connection management