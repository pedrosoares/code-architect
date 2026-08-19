---
id: flow.send-message
type: flow
title: Send a Message (the Turn Pipeline)
depends_on:
- domain.desktop-app
- domain.agent-loop
- domain.tools
- domain.conversation
---

**Goal:** the user sends a message (with optional image attachments) and the agent responds — streaming text and reasoning, calling tools as needed — until the turn completes or is cancelled.

## Steps

1. **UI (composer)** — `submit()`: trim the draft, take pending images, bail if both empty. Resolve the active session (or `start_new_chat()`, which **mints a `SessionId` client-side**). Locally `transcript.push_user(...)` (sets `Status::Waiting` optimistically) and then `engine.send(session, text, images)` → `Command::Send { session, text, images }` on the command channel. The composer is cleared; Send becomes Stop while busy.
2. **Worker (command arm)** — look up or create the `SessionSlot`. First-ever id → `store.create_session_with_id(session, kind, model)` + a title from the first message (`title_from`: whitespace-collapsed, 60 chars, `…`; `"Image"` for image-only) + a fresh `SessionsListed`. Push the user message (text + images as `ContentBlock::Image`, base64-encoded here — the only place a `Message` is built from attachments) into `slot.history` and persist the unsaved tail via `append_message`.
   - If the provider is `None` → `Failed { session: Some, "no working API configuration — open Settings to add or fix one" }` and stop (the user message is kept).
3. **Worker — `mem::take` the history**, insert a fresh `CancellationToken` into `running[session]`, and `tokio::spawn(spawn_turn(..))` — the command loop never blocks on the turn.
4. **`spawn_turn` (spawned task)** — build a **fresh per-turn `ToolRegistry`** (`with_default_tools` for user turns), register the shared long-lived `external_tools` (MCP + GitHub/Slack/Linear + docs), `process_tools`, `plan_tools`, and `browser_tools` (the seven `firefox_*` tools — see the [Browser Tools Integration](../04-integrations/firefox.md)); configure the context: `with_recorder(ChannelRecorder(file_change_tx))`, `with_plan_recorder(ChannelPlanRecorder(plan_tx))`, `with_current_plan(slot's plan)`, `with_sub_agent_spawner(EngineSubAgentSpawner { parent: session, run_sequentially: is_local(&provider_config) })`.
5. **Assemble the `Agent`** — `AgentConfig::new(model).system(config.system [+ docs protocol if doc tools present]).reasoning(Reasoning::VISIBLE)`.
6. **Run the loop** — `Agent::run_turn(&mut history, &raw_tx, cancel)`:
   - Each provider `StreamEvent` is forwarded as `EngineEvent::Agent { session, event }` (a forwarder task tags the local `AgentEvent` channel) → the UI renders reasoning/text deltas, opens `Tool` rows on `ToolCallStart`, fills name/arguments on `ToolCallEnd`, flips status + output on `ToolFinished`.
   - Tool calls execute via `ToolRegistry::execute` (unknown name → error result to the model; `view_image`/`screenshot` may return an image; `write_file`/`edit_file` record `FileChange`s on the per-turn channel).
   - `write_plan` records the whole plan on the plan channel → a second forwarder emits `EngineEvent::PlanUpdated` **live, mid-turn**.
   - Each `FileChange` is drained post-turn; each `ProcessEvent` from the registry arrives independently via `process_rx`.
7. **`TaskOutcome` back on `task_rx`** — remove the session from `running`; put the history back in the slot and persist the new tail; **record every file change** via `store.record_file_change(session, message_seq, &change)` with `message_seq = slot.history.len() - 1` (all changes attributed to the turn's last message) and emit `FileChanged { session, entry }` per change; persist the last plan (`save_plan`); if this session is a child with a parked oneshot, resolve it (see Sub-agents); then:
   - `Ok(TurnOutcome)` → nothing more (status returns to `Idle`).
   - `Err(AgentError::Cancelled)` → `EngineEvent::Cancelled(session)`.
   - `Err(other)` → `EngineEvent::Failed { session: Some, message: error.to_string() }`.
   - `tools_error` (registry build failure) → `Failed { session: Some, "tools unavailable: {message}" }`.

## Rules applied

- **One concurrent turn per session** — a second `Send` while one is running → `Failed "a turn is already running for this session"`.
- **Concurrency across sessions is the point** — other sessions' commands are processed immediately while this one streams.
- **No retry logic** — any `LlmError`/`AgentError` is a single-shot `Failed`.
- Errors on the outer channel share the same stream as deltas and carry a session tag precisely so a failure can't overtake them.

## Variants

- **Image attachments**: raw `Vec<u8>` bytes travel UI → `Command::Send`; base64 only at message-build time (encode) and on resume (decode in `Conversation::from_history`).
- **Sub-agent turns**: identical pipeline, but the Investigation registry, `SUBAGENT_SYSTEM`, a scoped workspace root, and the child's oneshot resolved at the end.
- **Compaction**: same `run_turn` machinery with `NoTools` + 1 iteration (see the model-config flow's Compact section, or the rules doc).