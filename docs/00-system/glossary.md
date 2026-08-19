---
id: system.glossary
type: system
title: Glossary
---

Working vocabulary for the codebase and its docs. Grouped by layer.

## Core types (`architect-core`)

| Term | Meaning |
|---|---|
| **Message** | One entry in a session's history (user / assistant / system). Persisted as JSON in `sessions.db`. |
| **Turn** | One user prompt and everything the agent does in response: model requests, tool calls, results, until a `StopReason`. |
| **Iteration** | One model request within a turn. Iteration 0 is the first reply; it increments after each round of tool results. |
| **ToolCall / ToolResult** | The model's request to run a tool (`id`, `name`, `input`) and the tool's answer (`tool_use_id`, `text`, optional image, `is_error`). |
| **StopReason** | Why the turn ended (model finished, cancelled, iteration limit, …). |
| **Usage / Cost** | Token counts across the turn; cost is `None` for unpriced (local) models. |
| **FileChange** | A before/after record of a file write — the raw material for the Diff tab and rollback. |
| **ChangeRecorder / PlanRecorder** | The two recording extension points (see [Extension Points](architecture.md#extension-points)); the agent never knows where changes land. |
| **Plan** | The structured task plan the `write_plan`/`read_plan` tools maintain, shown in the inspector's Plan tab. |

## Agent & tools

| Term | Meaning |
|---|---|
| **Agent** | The loop in `architect-agent`: send a request, stream the reply, execute requested tools, repeat. |
| **AgentEvent** | The loop's live report: `IterationStarted`, `Stream`, `ToolStarted`, `ToolFinished`, `TurnCompleted`. |
| **TurnOutcome** | How a turn ended: `stop_reason`, `iterations`, `usage`, `cost`, `context_tokens`, `context_window`, `hit_iteration_limit`. |
| **Tool / ToolRegistry** | A named capability (schema + `call`); the registry composes many into one `ToolExecutor`. |
| **ToolExecutor** | `architect-agent`'s extension point; `ToolRegistry` is the production implementation. |
| **Built-in tools** | The ten in `with_default_tools` (files, search, shell, `view_image`, `screenshot`, `spawn_subagents`). |
| **Process tools** | `start_process`, `get_process_logs`, `stop_process` — long-running background processes. |
| **Plan tools** | `read_plan`, `write_plan`. |
| **External tools** | MCP + GitHub/Slack/Linear/docs — long-lived, shared across turns, rebuilt on config change. |
| **Investigation tools** | The read-only subset a sub-agent runs with (no writes, no shell, no `spawn_subagents`). |

## LLM layer

| Term | Meaning |
|---|---|
| **Provider** | `architect-llm`'s single-method trait (`stream`). `openai`/`anthropic` are the built-in kinds. |
| **ProviderConfig / Profile** | The connection settings (kind, base_url, api_key, model); a `Profile` is the persisted form. |
| **ProviderRegistry** | Maps a `kind` string to a factory; `register` adds new kinds. |
| **StreamEvent** | The normalized stream the adapters emit (reasoning, text, tool-call, usage). |
| **is_local** | True when the base URL is loopback/LM-Studio — drives sequential sub-agents and unpriced cost. |

## Desktop engine & UI

| Term | Meaning |
|---|---|
| **Engine** | The composition root: owns the worker runtime and bridges UI ⇄ agent/tools/integrations/storage. |
| **Command / EngineEvent** | The two channel directions (21 / 18 variants — see [Engine Events & Commands](../01-domains/desktop-app/engine-events.md)). |
| **SessionSlot** | Per-session worker state: `history`, `persisted_len`, `plan`. Append-only. |
| **TaskOutcome** | What a spawned turn hands back on `task_rx`. |
| **Transcript / Conversation / Row** | The pure UI state `apply` folds events into; `Row` is one rendered unit. |
| **Panel** | One of the five UI surfaces: sessions, transcript, inspector, composer, settings (plus `shell` chrome). |
| **Ad-hoc model** | A provider configured in-memory for the session (LM Studio), never persisted. |

## Docs & integrations

| Term | Meaning |
|---|---|
| **Doc / doc graph** | A markdown file with frontmatter (`id`, `type`, `depends_on`, `relations`) in the Obsidian vault. |
| **DocDriver** | `architect-docs`'s extension point; `ObsidianDriver` is the implementation. |
| **MCP server** | An external tool source over stdio or HTTP; `McpToolAdapter` wraps each remote tool as a `Tool`. |
| **Integration** | A one-crate-per-service tool set (GitHub/Slack/Linear), read-only, credential-gated. |

## Cross-cutting

| Term | Meaning |
|---|---|
| **Workspace root** | The sandbox root — the parent of `.coder/`; all tool paths are resolved and confined to it. |
| **Sub-agent** | A child session spawned by `spawn_subagents`, confined to a sub-path, running investigation tools. |
| **Rollback** | Restoring recorded `FileChange`s to a prior `message_seq`. |
| **Compaction** | Summarizing a session's history into one assistant message. |

## See also

- [System Overview](overview.md)
- [Architecture](architecture.md)