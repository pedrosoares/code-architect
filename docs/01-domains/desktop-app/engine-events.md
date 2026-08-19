---
id: domain.desktop-app-engine-events
type: domain
title: Engine Events & Commands
---

The complete surface of the UI↔engine channel pair. The pair is unbounded in both directions; the worker `select!`s over its inputs, and the UI's event pump applies each event through the pure `Transcript::apply`. Full behavioral detail (per-variant side effects) is in [Desktop App Domain](overview.md).

## `Command` — 21 variants (UI → worker)

**Conversations**
| Variant | Effect |
|---|---|
| `Send { session, text, images }` | Runs one turn (first-ever id creates the session) |
| `Cancel(SessionId)` | Cancels the running turn (harmless if idle) |
| `LoadSession(SessionId)` | Loads a non-resident session's history into the UI |
| `DeleteSession(SessionId)` | Cancels, then hard-deletes (FK cascade) |
| `Rollback { session, up_to_seq }` | Restores recorded file changes to `up_to_seq` (refused while a turn runs) |
| `Compact(SessionId)` | Summarizes history into one assistant message (refused while running / empty) |

**Configuration**
| Variant | Effect |
|---|---|
| `ListProfiles` / `SaveProfile` / `DeleteProfile` | Profile CRUD |
| `ActivateProfile(Uuid)` / `DeactivateProfile` | Active profile overrides the env-derived provider |
| `UseAdHocModel { base_url, api_key, model }` | Provider rebuilt in place; nothing persisted |
| `ListModels { base_url, api_key }` | Populates the LM Studio picker |
| `ListMcpServers` / `SaveMcpServer` / `DeleteMcpServer` | MCP config CRUD + full reconnect |
| `ListIntegrations` / `SaveIntegrations` | GitHub/Slack/Linear credentials |
| `ListDocsConfig` / `SaveDocsConfig` | Docs vault config |
| `StartOAuthLogin(OAuthProvider)` | Opens the browser (RFC 8252 PKCE) |

`Attachment { media_type, bytes }` rides in `Send`; raw bytes end to end.

## `EngineEvent` — 19 variants (worker → UI)

| Variant | Carries |
|---|---|
| `Agent { session, event }` | every `AgentEvent` (text/reasoning deltas, tool calls/results, usage, done) — the session tag is what lets one shared stream serve many concurrent sessions |
| `Cancelled(session)` | turn cancelled |
| `Failed { session, message }` | `session == None` is reserved for genuinely global failures (bad startup provider, persistence, MCP connect, OAuth, gh-CLI, doc-tools construction) |
| `HistoryLoaded { session, messages }` | a loaded session's transcript |
| `SessionsListed(Vec<SessionSummary>)` | after create/delete/sub-agent spawn |
| `SessionDeleted(session)` | |
| `ProfilesListed { profiles, active }` | |
| `McpServersListed(Vec<McpServerConfig>)` | after any MCP rebuild |
| `FileChanged { session, entry }` | live, per file change mid-turn |
| `FileChangesLoaded { session, changes }` | on resume |
| `IntegrationsListed(IntegrationsConfig)` | after save / OAuth success (the list is the confirmation — there is no dedicated success event) |
| `DocsConfigListed(DocsConfig)` | |
| `Process(ProcessEvent)` | forwarded from the process registry |
| `Browser(BrowserEvent)` | forwarded from the headless browser (`architect_firefox`) — driver started, session started, navigated, closed, or failed; global, like `Process`, and replaces rather than accumulates (there is one shared browser) |
| `PlanUpdated { session, plan }` | live, mid-turn |
| `PlanLoaded { session, plan }` | on resume |
| `Compacted { session, summary }` | |
| `ModelsListed { base_url, models }` | |
| `AdHocModelActivated { model }` | |

## Ordering the UI relies on

At startup, for the most recent session: `HistoryLoaded` → `FileChangesLoaded` → `PlanLoaded` — that exact order is pinned by tests and relied on by `state.rs`.

## See also

- [Desktop App Domain](overview.md)
- [UI Core Seam rule](../../../03-business-rules/ui-core-seam.md)