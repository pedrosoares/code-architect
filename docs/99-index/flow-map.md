---
id: index.flow-map
type: index
title: Flow Map
---

A human-written map of the flows, for orientation. `list_docs` with `doc_type: "flow"` gives the current, generated version.

```
   send a message ──────────►  the turn pipeline
        │
        ├──► session lifecycle   (create / load / delete / resume)
        ├──► model & provider config  (profiles, ad-hoc, compaction)
        ├──► sub-agents            (spawn_subagents)
        ├──► roll back file changes
        └──► OAuth login           (GitHub / Slack / Linear)
```

| Flow | What it covers |
|---|---|
| [Send a Message](../02-flows/send-message.md) | The turn pipeline end-to-end: `Command::Send` → per-turn registry → `Agent::run_turn` → `TaskOutcome` → persist + record. The central flow everything else hangs off. |
| [Session Lifecycle](../02-flows/session-lifecycle.md) | Creating, loading, deleting, and resuming sessions; `persisted_len`; the append-only invariant. |
| [Model & Provider Configuration](../02-flows/model-config.md) | Profiles, the active-profile pointer, ad-hoc model activation, and context-window compaction. |
| [Sub-agents](../02-flows/subagents.md) | `spawn_subagents`: sequential (local provider) vs concurrent (hosted), the reduced toolset, result summaries. |
| [Roll Back File Changes](../02-flows/rollback.md) | Undoing a turn's `FileChange`s, why the plan is untouched, and the event sequence. |
| [OAuth Login](../02-flows/oauth-login.md) | How the credentials in `IntegrationsConfig` get created via the browser flow. |

## Cross-cutting flows

- **Compaction** is a flow-within-a-flow: it's driven from the [Model & Provider Configuration](../02-flows/model-config.md) flow but operates on the [Session Lifecycle](../02-flows/session-lifecycle.md)'s history.
- **Startup resume** (HistoryLoaded → FileChangesLoaded → PlanLoaded) is part of [Session Lifecycle](../02-flows/session-lifecycle.md) and is ordering-pinned by a test.