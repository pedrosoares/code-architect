---
id: data.invariants
type: index
title: System Invariants
---

Rules that must always remain true across the whole system — the ones an agent should never assume it's safe to break. Each links to the doc (rule, flow, or ADR) that states and enforces it.

## Concurrency

- **One concurrent turn per session, many across sessions.** Enforced by the `running: HashMap<SessionId, CancellationToken>` map. See [Cancellation & Single-Flight](../03-business-rules/cancellation.md).
- **`SessionSlot` is append-only, never replaced.** `history`/`persisted_len` only grow; rollback and compaction are explicit, separate operations. Switching back to a mid-turn session shows live state, never a stale DB snapshot. See [Session Lifecycle](../02-flows/session-lifecycle.md).
- **The UI is a pure reducer.** `Transcript::apply(&EngineEvent)` has no IO and no Freya types; panels never hold an engine handle or call the worker. See [UI ↔ Engine Seam](../03-business-rules/ui-core-seam.md) and [ADR: UI Core Seam](../06-decisions/0001-ui-core-seam.md).

## Persistence

- **File paths in `sessions.db` are stored relative to the workspace root**, so the database stays meaningful if the workspace moves. See [Persistence Rules](../03-business-rules/persistence.md).
- **Message content is stored as JSON**, not rendered text — the raw structure is the source of truth.
- **All file changes in a turn are attributed to that turn's last message** — the invariant that makes rollback well-defined. See [File Changes & Rollback](../01-domains/file-changes/overview.md).

## Security & sandboxing

- **Tool paths are sandboxed to the workspace root** (the parent of `.coder/`). See [Sandboxing & Shell Safety](../03-business-rules/sandboxing.md).
- **Integration tools are read-only** — they may fetch and report, never mutate, the external service. See [Read-Only Integration Rule](../03-business-rules/read-only-integrations.md).
- **Sub-agents get a deliberately less-trusted toolset**: read-only, no shell, no `spawn_subagents`. See [Sub-agents](../02-flows/subagents.md).
- **Credentials are stored plaintext, on purpose** — do not add encryption without revisiting the ADR. See [ADR: Plaintext Credential Storage](../06-decisions/0003-plaintext-credential-storage.md).

## Extension points

- **Dependencies point one way** (see [Architecture](../00-system/architecture.md)). A new integration fills a seam (`Tool`, `Provider`, `DocDriver`); it does not reach across crates.
- **`architect-core` never imports Freya; `architect-ui` never imports the agent/LLM/storage.**

## Tooling

- **Tool failures are data, not panics** — a tool that can't do its job returns a tool error the model sees; the turn continues. See [Conventions](../00-system/conventions.md).
- **`run_command` has a best-effort denylist**, not a sandbox — it is a convenience guard, not a security boundary.