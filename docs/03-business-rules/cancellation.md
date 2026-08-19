---
id: rule.cancellation
type: rule
title: Cancellation & Single-Flight Rules
---

**How turns, compactions, and sessions are stopped — and what never happens: a retry.**

## Cancellation

- **One `CancellationToken` per in-flight operation**, kept in the worker's `running: HashMap<SessionId, CancellationToken>` — turns, compactions. (`Rollback` has no token; it's refused while a turn runs instead.)
- `Command::Cancel(session)` → `token.cancel()` — harmless if the session is idle. The UI's Stop button is shown exactly when `conversation.status.is_busy()`.
- The token is checked at the **provider stream level** (`LlmError::Cancelled` when the stream is dropped/aborted) — a cancelled turn surfaces as `AgentError::Cancelled` → `EngineEvent::Cancelled(session)`.
- **`ToolRegistry::execute` accepts the cancellation token but ignores it** (`_cancel`) — a tool already running runs to completion. Cancellation is cooperative at the network/iteration boundaries, not inside tool bodies.
- `Command::DeleteSession` cancels the session's in-flight turn first — "nothing is left to persist its result against once the row is gone".
- `Status::Compacting` folds into `is_busy()` (set optimistically client-side), so the same Stop button cancels a compaction via the same `running` map.

## Single-flight per session

- **At most one concurrent turn per session** — a second `Command::Send`, `Command::Compact`, or `Command::Rollback` while one is in flight is refused via `Failed` ("a turn is already running for this session" / "nothing to compact yet").
- **Across sessions, many at once** — that's the point of the spawn-per-turn design; a slow session's stream never blocks a fast one's commands.

## No retries, no rate-limit recovery (UI layer)

- **There is no retry or rate-limit handling anywhere in the desktop layer.** Every `LlmError`/`AgentError` is a single-shot `Failed` string in the transcript. The only retry logic in the whole system is `architect-llm`'s `http::send_retrying` (3 attempts, exponential backoff, honors `Retry-After` on 429) — which applies *within* a single provider request, before the error ever reaches the agent loop.
- A bad token, a 404 from an integration, or a dead local server all surface exactly once, as text, in the normal failure path.

## Error routing

- `EngineEvent::Failed { session: Option<SessionId>, message }` — `Some` = attributable to a turn (shown in that session's transcript); `None` = genuinely global (bad startup provider, persistence unavailable, MCP connect failure, OAuth failure, gh-CLI failure, doc-tools construction — shown wherever the UI currently is).
- Errors share the outer event stream with deltas and carry the session tag **precisely so a failure can't overtake the deltas it follows** — there is deliberately no second error channel on the outer pair.
- Error text is written to be read by the model (tools) or by a human (engine) — no translation layer in between.