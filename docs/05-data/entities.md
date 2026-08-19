---
id: data.entities
type: index
title: Entities
---

Index of the entities in this system. Each is documented in the domain that owns it — the links go to the doc that defines its fields and storage. Use `list_docs` for the live structural view.

## Persisted in `sessions.db` (per-workspace, SQLite)

| Entity | Documented in | One-line |
|---|---|---|
| `Session` | [Conversation & Message](../01-domains/conversation/overview.md) | A conversation: id, title, created-at; the unit of history and per-session state. |
| `Message` | [Conversation & Message](../01-domains/conversation/overview.md) | One turn entry: role, content as JSON, usage; appended, never mutated. |
| `FileChange` | [File Changes & Rollback](../01-domains/file-changes/overview.md) | A file mutation with full old/new content, attributed to the turn's last message — the rollback primitive. |
| `Plan` | [Plans](../01-domains/plans/overview.md) | A session's current plan (goal + steps), one row, upserted on each `write_plan`. |

## Persisted in `config.json` (machine-global, JSON)

| Entity | Documented in | One-line |
|---|---|---|
| `Profile` | [Configuration](../01-domains/configuration/overview.md) | A model/provider config: kind, model, base URL, API key (plaintext), system prompt, context window. |
| `McpServerConfig` | [MCP Client](../01-domains/mcp/overview.md) | A saved MCP server: command+args (stdio) or URL (HTTP). |
| `IntegrationsConfig` | [Configuration](../01-domains/configuration/overview.md) | The three integration credentials (GitHub/Slack/Linear), stored plaintext. |
| `DocsConfig` | [Documentation Knowledge Base](../01-domains/docs-vault/overview.md) | The vault path the doc tools read/write. |

## Runtime (in-memory, not persisted)

| Entity | Documented in | One-line |
|---|---|---|
| `SessionSlot` | [Desktop App](../01-domains/desktop-app/overview.md) | Per-session live state in the worker: `history`, `persisted_len`, `plan` — append-only. |
| `Transcript` / `Conversation` / `Row` | [Desktop App](../01-domains/desktop-app/overview.md), [UI Presentation](../01-domains/ui-presentation/overview.md) | The UI's folded view of `EngineEvent`s. |
| `Doc` / `DocMetadata` | [Documentation Knowledge Base](../01-domains/docs-vault/overview.md) | A knowledge-base doc: frontmatter (id, type, title, `depends_on`, `relations`) + markdown body. |
| `ProcessEntry` | [Tools](../01-domains/tools/overview.md) | A background process started by `start_process`: id, status, accumulated log. |

## Note on entity docs

Entities are documented **inline in their owning domain's overview** rather than as separate `entity`-type docs — the fields, storage location, and invariants are all in those docs. This index exists so you can find the right domain without knowing it. If an entity grows complex enough to deserve its own doc, split it out under `01-domains/<domain>/entities.md` and link it here.