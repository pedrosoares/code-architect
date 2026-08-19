---
id: system.conventions
type: system
title: Conventions
---

Coding and documentation conventions for this project — the things that make a change read like the rest of the codebase.

## Crate layout

- **`architect-core` is the dependency hub.** Every crate depends on it; it depends on nothing in this workspace. Cross-crate extension points (`ChangeRecorder`, `PlanRecorder`) live here so the agent and the tool layer never import each other.
- **One crate per concern, thin seams.** `architect-agent` knows nothing about tools, persistence, or storage; it's wired to them through traits it defines (`Provider`, `ToolExecutor`) or imports from core. New integrations fill a seam; they don't reach across crates.
- **`apps/desktop` is the composition root** (the `Engine`): the only place that owns the worker runtime and bridges UI ⇄ agent/tools/integrations/storage.

## Extension points (the seams)

| Seam | Trait | Where defined | Filled by |
|---|---|---|---|
| Model backend | `Provider` | `architect-llm` | `openai`/`anthropic` adapters, `ProviderRegistry` |
| Tools | `ToolExecutor` | `architect-agent` | `ToolRegistry` (`architect-tools`) |
| Tool | `Tool` | `architect-tools` | built-ins + MCP + GitHub/Slack/Linear/docs |
| File-change sink | `ChangeRecorder` | `architect-core` | desktop's SQLite recorder |
| Plan sink | `PlanRecorder` | `architect-core` | desktop's channel recorder |
| Sub-agent launch | `SubAgentSpawner` | `architect-tools` | desktop's worker spawner |
| Doc storage | `DocDriver` | `architect-docs` | `ObsidianDriver` |

## Error handling

- **Tool failures are data, not panics.** A tool that can't do its job returns a tool error the model can see and react to; the turn continues.
- **Graceful degradation over hard failure** in integration clients: report the unexpected shape (binary file, submodule, null ticket) with a pointer to the right action, instead of erroring opaquely.
- **Global vs session-scoped `Failed` events:** `session == None` is reserved for genuinely global failures (bad startup provider, persistence, MCP connect, OAuth, gh-CLI, doc-tools). Anything tied to a conversation carries its `SessionId`.
- Non-2xx HTTP and missing JSON fields become descriptive errors with status/body excerpts or `?`/`unknown` placeholders — never a crash.

## Concurrency & lifetimes

- **Per-turn registries, shared external tools.** Each turn builds a fresh `ToolRegistry` (short-lived `Arc`); long-lived, shared tools (MCP, integrations) are registered by shared `Arc` so they aren't dropped between turns.
- **The UI is a pure reducer.** Events are applied through `Transcript::apply(&EngineEvent)`; the only bridge to the engine is the `Command`/`EngineEvent` channel pair (see [UI Core Seam rule](../03-business-rules/ui-core-seam.md)).
- **Append-only history.** `SessionSlot` grows `history`/`persisted_len` monotonically; rollback and compaction are explicit, separate operations.

## Testing

- **Behavioral tests over implementation.** The registry test asserts the exact set of tool names, not how they're stored; frontmatter tests assert `render`/`parse` round-trip, not the YAML.
- **`tempfile::tempdir()`** for any filesystem test; **`reqwest` mock servers** (via the `mockito`-style local server) for the integration clients.
- **Ordering is pinned by tests where the UI relies on it** — e.g. the startup sequence `HistoryLoaded → FileChangesLoaded → PlanLoaded`.
- Sub-agent concurrency has a test proving **local providers run sequentially** (one at a time) while hosted ones run concurrently.

## Documentation

- **Docs are a graph, not a tree.** Every doc has a stable `id` (`domain.orders`, `flow.order-creation`), a `type`, and `depends_on` / `relations` links.
- **Frontmatter is exact-invertible.** The `DocMetadata` struct and `frontmatter::render`/`parse` are inverses by construction — don't hand-edit frontmatter into a shape `parse` can't read.
- **Folder convention:** `00-system/` (overview, glossary, architecture, conventions), `01-domains/<name>/`, `02-flows/`, `03-business-rules/`, `04-integrations/`, `05-data/` (entities, relationships, invariants), `06-decisions/` (ADRs), `99-index/` (domain/flow/dependency maps).
- **Doc types:** `system`, `domain`, `flow`, `rule`, `entity`, `integration`, `adr`, `index`.
- **Update docs as part of the change** that alters behavior, rules, entities, or flows — they're part of the change, not an afterthought.

## Security

- **Plaintext credentials, on purpose** — see [ADR: Plaintext credential storage](../06-decisions/0003-plaintext-credential-storage.md) and the [Plaintext credentials rule](../03-business-rules/plaintext-credentials.md).
- **Tool paths are sandboxed** to the workspace root (the parent of `.coder/`); `run_command` has a best-effort denylist.
- **Sub-agents get a deliberately less-trusted toolset** (read-only, no shell, no `spawn_subagents`).

## See also

- [Architecture](architecture.md)
- [Glossary](glossary.md)