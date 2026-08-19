---
id: rule.persistence
type: rule
title: Persistence Rules
---

**Where state lives, what survives, and what is irreversibly destroyed.**

## Two stores, two scopes

| Store | Location | Scope | Contents |
|---|---|---|---|
| `SessionStore` (SQLite via `rusqlite`, bundled) | `<workspace>/.coder/sessions.db` | **per-workspace** | `sessions`, `messages`, `file_changes`, `plans` |
| `ConfigStore` (JSON) | `~/.config/code-architect/profiles.json` (or `$XDG_CONFIG_HOME/code-architect/`) | **machine-global** | profiles, MCP servers, integrations, docs config, active-profile pointer — one `Document` in one file |

Why the split: an API key or an MCP server is set up once and reused across every project; a conversation belongs to the directory it was had in. V1's `.coder/sessions.db` location is preserved deliberately.

## `SessionStore` rules

- **`PRAGMA foreign_keys = ON` per connection** (SQLite defaults it off — the schema's `ON DELETE CASCADE` is otherwise inert). Deleting a session cascades to its `messages`, `file_changes`, and `plans`.
- **`messages.content` holds `serde_json::to_string(&Vec<ContentBlock>)`** — provider-neutral, round-tripped through the same serde derives as everything else. (V1 stored OpenAI-shaped columns; V2's `Message` is provider-neutral.)
- **File paths in `file_changes` are relative to the workspace root** — the database stays meaningful if the workspace is moved or copied. `tool_name` (`&'static str` in memory) is a `TEXT` column; reading it back leaks one `&'static str` per row (acknowledged tradeoff).
- **`old_content: NULL` is the "this change created the file" sentinel** — the same signal `reverse_to_point` uses to delete on rollback instead of restoring.
- **Every public method is `async` over blocking rusqlite via `tokio::task::spawn_blocking`** — V1 called blocking SQLite straight from async handlers, which blocked the executor thread; this doesn't.
- **Append, don't rewrite, except two deliberate wholesale operations**: `replace_messages` (compaction: DELETE then re-insert at `seq` 0..) and `delete_session` (hard delete). Both are "no undo" tradeoffs, like `reverse_to_point`.
- **Best-effort in the desktop layer**: a store that can't open (read-only filesystem) yields one global `Failed "persistence unavailable: {error}"` and the chat still works — just unsaved. Failures are reported once, not per call.

## `ConfigStore` rules

- File created lazily on first write; **missing on read → empty defaults** (not an error).
- **Corrupt JSON is a hard error on every subsequent read/write** (`ConfigError::Serialize`) — never a silent reset to defaults.
- An `Arc<Mutex<()>>` serializes read-modify-write per store instance.
- `set_active` on an unknown profile → `UnknownProfile`; `remove` clears `active` if it removed the active profile.
- **All credentials at rest are plaintext** — `api_key`, MCP `bearer_token`, GitHub/Slack/Linear tokens; nothing in this codebase encrypts them at rest (documented tradeoff, matching V1). OAuth client secrets are different: compiled into the binary at build time, never in the config file.

## Irreversible operations (by design, no redo)

- `delete_session` (cascades everything).
- `reverse_to_point` (destroys each `file_changes` row as it undoes it).
- `replace_messages` (compaction).
- `delete_profile` / `remove_mcp_server`.

## Invariants

- Session identity (id, title, provider kind/model at creation, file changes, plan) survives compaction, profile switches, and model switches.
- `plans` is one row per session, upserted — exactly one current plan, not a history.
- `messages` are never written by rollback or compaction-adjacent paths except compaction's own replace.
- A session's in-memory slot is the single source of truth while resident; the DB is a projection it's flushed to at turn boundaries.