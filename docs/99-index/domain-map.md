---
id: index.domain-map
type: index
title: Domain Map
---

A human-written map of the domains, for orientation. `list_docs` with `doc_type: "domain"` gives the current, generated version — prefer that when precision matters.

```
                                ┌─────────────────────────┐
                                │   System (00-system)    │
                                │ overview·architecture·  │
                                │ conventions·glossary    │
                                └────────────┬────────────┘
                                             │
   ┌────────────────────────────┬────────────┴───────────┬───────────────────────────┐
   │                            │                        │                           │
┌──▼──────────┐          ┌──────▼───────┐         ┌──────▼───────┐            ┌──────▼──────┐
│  Agent core │          │  Desktop app │         │  Integrate   │            │  Present    │
├─────────────┤          ├──────────────┤         ├──────────────┤            ├─────────────┤
│ agent-loop  │◄─────────│ desktop-app  │         │ github       │            │ ui-         │
│ conversation│          │  ·-panels    │         │ slack        │            │ presentation│
│ llm-providers│         │  ·-engine-   │         │ linear       │            └─────────────┘
│ tools       │          │  events      │         │ mcp          │
│ file-changes│          └──────────────┘         │ firefox      │
│ plans       │                                    │ docs-vault   │
└─────────────┘                                    └──────────────┘
```

## Core agent

- [Agent Loop](../01-domains/agent-loop/overview.md) — the `Agent::run_turn` loop, `AgentEvent`s, `Reasoning`.
- [Conversation & Message](../01-domains/conversation/overview.md) — `Session`/`Message`, history, roles.
- [LLM Providers](../01-domains/llm-providers/overview.md) — `Provider`, `ProviderRegistry`, openai/anthropic, `is_local`.
- [Tools](../01-domains/tools/overview.md) — the `Tool` trait, `ToolRegistry`, the 15 built-ins, `ToolContext`.
- [File Changes & Rollback](../01-domains/file-changes/overview.md) — `FileChange`, `ChangeRecorder`, the rollback primitive.
- [Plans](../01-domains/plans/overview.md) — `Plan`, `write_plan`/`read_plan`, the plan recorder.

## Desktop app

- [Desktop App](../01-domains/desktop-app/overview.md) — the engine, `Command`/`EngineEvent`, `SessionSlot`.
  - [Desktop App Panels](../01-domains/desktop-app/panels.md) — the Freya panels.
  - [Engine Events & Commands](../01-domains/desktop-app/engine-events.md) — the full 21+18 variant contract.
- [UI Presentation](../01-domains/ui-presentation/overview.md) — `architect-ui`: theme tokens + widgets.

## Integrations

- [GitHub](../01-domains/github/overview.md) · [Slack](../01-domains/slack/overview.md) · [Linear](../01-domains/linear/overview.md) — one domain per external service.
- [MCP Client](../01-domains/mcp/overview.md) — `connect`, `McpToolAdapter`.
- [Documentation Knowledge Base](../01-domains/docs-vault/overview.md) — `DocDriver`, `ObsidianDriver`, the vault graph.
- [Browser Tools (headless Firefox)](../01-domains/firefox/overview.md) — the seven `firefox_*` tools: open, screenshot, logs, click, fill, eval, close — driven by the engine-owned `Browser` (see [ADR 0004](../06-decisions/0004-browser-tools.md)).