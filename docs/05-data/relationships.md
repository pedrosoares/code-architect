---
id: data.relationships
type: index
title: Entity Relationships
---

How the entities relate to each other, beyond what each entity's own doc states.

## The conversation spine

```
Session 1 ──── * Message 1 ──── * FileChange
   │  (owns)     (role, content)      (old/new content,
   │                                    message_id → last msg)
   │
   └── 0..1 Plan  (goal + steps, upserted)
```

- A **`Session`** owns many **`Message`s**, appended in order. The worker keeps a per-session **`SessionSlot`** in memory that mirrors the persisted `Session`'s message history (`persisted_len` marks how much is already in the DB).
- Each **`Message`** can own zero or more **`FileChange`s**. All file changes from a turn are attributed to that turn's **last** message — so a rollback is "undo every `FileChange` whose `message_id` is this one."
- A **`Session`** owns at most one current **`Plan`** (upserted, not a list).

## Config → runtime

```
Profile ──(active)──► ProviderConfig ──► Provider (openai/anthropic adapter)
McpServerConfig ──► McpToolAdapter ──► Tool (merged into each turn's registry)
IntegrationsConfig ──► GitHub/Slack/Linear tool crates ──► Tool
DocsConfig ──► ObsidianDriver ──► doc Tool crates
```

- The **`Profile`** that is marked active is bridged by the engine into an `architect_llm::ProviderConfig`, which selects a **`Provider`** (the `openai` or `anthropic` adapter). Profile fields mirror `ProviderConfig` by name deliberately.
- Each saved **`McpServerConfig`** and each entry of **`IntegrationsConfig`** becomes a long-lived **`Tool`** in the shared `external_tools` set, merged into every turn's fresh registry.
- **`DocsConfig`** points at the vault; the `ObsidianDriver` reads it and the doc tool crates expose it as tools.

## UI mirror

- **`Transcript`** (UI) is a pure fold of **`EngineEvent`s** and mirrors the worker's **`SessionSlot`**: `history`/`rows` on each side, `plan` on each side. They are kept consistent by the channel contract, not by sharing state.
- **`Doc`** has no UI entity of its own — the desktop app treats the vault as an external store the agent's doc tools read/write; the UI only surfaces doc-related tool calls as transcript rows.

## Cross-cutting

- **`FileChange`** is the only entity that spans the tools → session-store → UI boundary: written by a file tool through the `ChangeRecorder` (a `ChannelRecorder`), persisted by the engine into `sessions.db`, and rendered by the Diff tab. See [File Changes & Rollback](../01-domains/file-changes/overview.md) and the [Rollback](../02-flows/rollback.md) flow.