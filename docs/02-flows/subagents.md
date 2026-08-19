---
id: flow.subagents
type: flow
title: Sub-agents (spawn_subagents)
depends_on:
- domain.desktop-app
- domain.tools
- domain.agent-loop
---

**Goal:** a turn fans investigation work out to focused sub-agents — e.g. one per crate — each a genuine first-class child session, and the parent's tool call returns their findings.

## Steps

1. **Model calls `spawn_subagents`** — input: `tasks` (1..=8), each `{ prompt, path }` (workspace-relative scope). Sequential vs concurrent is decided by the harness, not the model: `spawner.run_sequentially()` is true when `architect_llm::is_local(&provider_config)` (local inference servers can't serve overlapping requests).
2. **`SpawnSubAgents::call`** — for each task, `ctx.sub_agent_spawner().spawn(prompt, path)`; sequential = `await` in a loop, concurrent = `futures_util::join_all`. Output: blocks `"## {path}\n\n{summary}"` joined by blank lines; a failed task becomes `"## {path}\n\nFailed: {error}"` — **per-task failure is not a hard error** (the tool call as a whole still succeeds).
3. **`EngineSubAgentSpawner::spawn`** (engine side) — submits `SpawnSubAgentRequest { parent, prompt, path, reply: oneshot::Sender<Result<String, String>> }` on its own dedicated channel (**not** a `Command` variant — `Command` derives `PartialEq/Eq`, which no `oneshot::Sender` implements) and awaits the oneshot — the parent's tool call blocks here until the child's `TaskOutcome` lands.
4. **Worker's `subagent_rx` arm** —
   - Containment-checks `path` with the same `ToolContext::resolve` used by file tools, **plus** requires the resolved path to be an **existing directory** (it becomes the child's whole workspace root). A non-existent path or a file fails fast (a real-world gap that used to quietly spawn a child into an unusable scope).
   - Requires a working provider.
   - Mints `child = SessionId::new()`; `store.create_child_session_with_id(child, parent, kind, model)` + title from the prompt; wraps the prompt in a `scoped_prompt` preamble ("Your tools for this task are scoped to `{path}` — that path already IS your workspace root here…"); persists it.
   - Re-broadcasts `SessionsListed` (the child appears nested under its parent in the sidebar, in spawn order).
   - Parks the reply in `pending_subagent_replies[child]`, then `spawn_turn` — the exact same call a normal `Command::Send` does (own `ToolRegistry`, own `Agent`, its `AgentEvent`s streamed to the real UI the same way), just seeded with only the task prompt (**no shared history** with its parent), rooted at the sub-path, using `with_investigation_tools` for its registry, `SUBAGENT_SYSTEM` for its system prompt, and **only the three read-only doc tools** from `external_tools` (children can consult the KB, not mutate it; no process/plan tools).
5. **The child runs in the background** as an ordinary session — its events stream to the UI; the sidebar shows it nested with a busy dot.
6. **Child's `TaskOutcome`** (worker's `task_rx` arm) — the parked oneshot resolves:
   - Success = the child's last message text.
   - `TurnOutcome.hit_iteration_limit` → `"sub-agent hit its iteration limit before finishing"` (failure).
   - `stop_reason == MaxTokens` → `"sub-agent's answer was cut off at the model's token limit"` (failure).
   - Empty text → `"sub-agent finished without producing any text"` (failure).
   - `AgentError` → its `to_string()` (failure).
   (These classifications fix a former gap where each came back as a silent `Ok("")` or narration fragment, indistinguishable from a real answer.)
   The parent's blocked tool call then returns — the parent's turn continues.
7. **Child side effects are real**: the child's file changes and plan persist through the same generic `task_rx` path (into its own session's rows).

## Design notes

- **The recursion guard is structural**: `with_investigation_tools` deliberately excludes `spawn_subagents` itself — nested spawning is "structurally impossible rather than merely discouraged". (The engine's spawner is still wired, so grandchildren are *possible* in principle, but the tool simply isn't offered to children.)
- **Nothing about how a child's turn runs is special-cased** — same `spawn_turn`, same event streaming, same persistence.
- The child's `workspace_root` is the scoped sub-path — its sandbox root, so its file tools can't escape it either.

## Known model-level limitation

A sub-agent's *enumeration* (a list of files, tests, exports) is reliable, but a self-reported *count or total* derived from that list can still be a plain arithmetic slip even when the underlying data was correct — confirmed non-deterministic by re-running the identical prompt against unchanged ground truth. `SUBAGENT_SYSTEM` tells sub-agents to recompute totals explicitly rather than trust a single mental tally, but a parent orchestrating `spawn_subagents` calls that depend on an exact number is better off re-deriving it mechanically (re-running the count command itself) rather than trusting a sub-agent's self-reported total outright.