---
id: system.architecture
type: system
title: Architecture
depends_on:
- system.overview
relations:
  related_rules:
  - rule.persistence
  - rule.ui-core-seam
  related_flows:
  - flow.send-message
---

## The one rule: dependencies point one way

Nothing below ever imports something above it.

```
apps/desktop ──► architect-ui ──► freya
     │
     ├────────► architect-tools ──► architect-agent ──► architect-llm ──► architect-core
     │              ▲              └───────────────────▲
     │              │                                  │
     │         architect-mcp ─────────────► rmcp       │
     │         architect-github ──────────► reqwest    │
     │         architect-slack ───────────► reqwest    │
     │         architect-linear ──────────► reqwest    │
     │         architect-docs ────────────► (tools)    │
     │
     ├────────► architect-session ─────────────────────► architect-core
     └────────► architect-config ────────────────────(depends on nothing)
```

- `architect-tools` depends on `architect-agent`, **not the other way around**: its `ToolRegistry` *implements* `architect-agent`'s `ToolExecutor` trait — the agent defines the extension point, tools fills it.
- `architect-mcp` has the same relationship to `architect-tools`: its `McpToolAdapter` implements `Tool`, so `architect-tools` never knows MCP exists. `architect-github`/`-slack`/`-linear`/`-docs` fill the same extension point again, one crate per external service.
- `architect-tools`, `architect-mcp`, the integration crates, `architect-session` and `architect-config` **do not depend on each other or on `architect-llm`**. They only share plain data: `architect-core`'s `FileChange`/`ChangeRecorder`; `architect-config::Profile`'s fields mirror `architect_llm::ProviderConfig` by name; `McpServerConfig` mirrors `architect-mcp::connect`'s parameters; `IntegrationsConfig` mirrors the three tool crates' bare tokens.
- **`apps/desktop/src/engine.rs` is the one place that knows all of these exist** and bridges between them — the same way it bridges a `ChannelRecorder` between tools and the session store, and a `Profile` into a `ProviderConfig`.
- `architect-core` never imports Freya. `architect-ui` never imports the agent, the LLM client, or storage. `apps/desktop` holds composition and window setup — no business logic.

That is what keeps the harness modular: the same core can later run behind a CLI or a socket daemon without the UI knowing, and the UI can be restyled or replaced without touching the agent.

## Runtime topology

Freya owns the main thread and runs its own executor; `reqwest` needs a Tokio reactor. So the agent lives on a **dedicated runtime thread** and the two sides talk only over channels:

```
Freya UI (main thread) ──Command──►  worker on a Tokio runtime thread
                       ◄─EngineEvent──
```

Both channels are unbounded — a busy UI must never stall the model stream, and a queued command must never block a render. `main.rs` constructs the `Engine` into a `static ENGINE: OnceLock<Engine>` **before** `launch(...)`, so after the window closes it can call `Engine::shutdown()` (kills every tracked background process).

- The worker's main loop `tokio::select!`s over: `commands`, `task_rx` (finished turns), `compact_rx`, `subagent_rx`, `oauth_rx`, `process_rx`, and `shutdown_rx`.
- **Every session's turn runs as its own `tokio::spawn`ed task** — the command loop only does quick work (look up/create the slot, persist the user message, spawn) before returning to the select. That is what lets the UI use one session while another keeps streaming.
- Per-session state: `SessionSlot { history, persisted_len, plan }` in a `HashMap`, taken out via `mem::take` for the duration of its spawned task and merged back from `TaskOutcome`. Slots are **append-only, never replaced**, so switching back to a mid-turn session shows live state, never a stale DB snapshot.
- One concurrent turn per session (enforced by a `running: HashMap<SessionId, CancellationToken>` map); many across sessions.
- `Transcript::apply` (in `state.rs`) folds `EngineEvent`s into UI state — a pure function with no IO and no Freya types, which is what makes the streaming behavior testable without a model or a window.

## The turn pipeline (shape)

`Command::Send` → mint/lookup `SessionSlot` → push + persist the user message → `spawn_turn` (fresh per-turn tool registry wired with per-turn `FileChange`/plan channels, an `Agent` with the active provider, a local `AgentEvent` channel with a forwarder task that session-tags every event as `EngineEvent::Agent { session, event }`) → `TaskOutcome` back on `task_rx` → persist the new tail, record file changes (all attributed to the turn's last message), save the plan, resolve any waiting sub-agent, surface `Failed`/`Cancelled` as needed.

Full detail in [Send a Message](#).

## Storage layout

| Store | Location | Scope | Contents |
|---|---|---|---|
| `SessionStore` (SQLite) | `<workspace>/.coder/sessions.db` | per-workspace | sessions, messages (content as JSON), file changes (full old/new content), plans (one row each) |
| `ConfigStore` (JSON) | `~/.config/code-architect/profiles.json` (or `$XDG_CONFIG_HOME/...`) | machine-global | API profiles, MCP servers, integrations credentials, docs config, active-profile pointer |

File paths in `sessions.db` are stored **relative to the workspace root**, so the database stays meaningful if the workspace is moved or copied.

## Extension points

| Extension point | Defined by | Filled by |
|---|---|---|
| `Provider` (one method: `stream`) | `architect-llm` | `openai`/`anthropic` adapters; a genuinely different API registers a factory via `ProviderRegistry::register` |
| `Tool` (name, description, JSON schema, `call`) | `architect-tools` | the 15 built-ins (the 10 core ones in `with_default_tools`, plus `start_process`/`get_process_logs`/`stop_process` and `write_plan`/`read_plan`, which the engine registers separately); `McpToolAdapter`; GitHub/Slack/Linear/doc tool crates; the seven `firefox_*` browser tools (`architect-firefox`, engine-registered like the process tools) |
| `ToolExecutor` | `architect-agent` | `ToolRegistry` |
| `ChangeRecorder` / `PlanRecorder` | `architect-core` | `ChannelRecorder`/`ChannelPlanRecorder` (bridged by the engine into the session store); `NoRecorder`/`NoPlanRecorder` for tests |
| `SubAgentSpawner` (async) | `architect-tools` | the engine's `EngineSubAgentSpawner`; `NoSubAgentSpawner` for tests |
| `DocDriver` | `architect-docs` | `ObsidianDriver` (Notion/Jira planned) |
| Provider kind factories | `ProviderRegistry::default()` | `openai`, `anthropic`; `register(kind, factory)` adds more |