---
id: flow.session-lifecycle
type: flow
title: Session Lifecycle
depends_on:
- domain.desktop-app
- domain.file-changes
- rule.persistence
---

**Goal:** sessions are created, persisted, resumable across app runs, switchable mid-stream, deletable, and rollbackable — with no data loss from concurrent use.

## Create

- "+ New Chat" is **pure client-side**: `Transcript::start_new_chat()` mints `SessionId::new()` (client-side so the worker can key everything from the first message), creates a blank `Conversation`, sets it active. Nothing durable exists until the first send — no engine round-trip.
- First `Command::Send` for that id → `store.create_session_with_id(session, kind, model)` + title from the first message + `SessionsListed`.

## Persist

- Every message (user and assistant, all content blocks as JSON) is appended as it happens — the unsaved tail tracked by `SessionSlot.persisted_len` and flushed via `store.append_message` at turn boundaries.
- File changes and the plan are recorded at turn end (see Send a Message).
- A session's `SessionSlot` is **append-only, never replaced** — so switching back to a mid-turn session shows live state, never a stale DB snapshot.

## Resume (startup and switch)

- **Startup**: the worker lists sessions, loads the most recent one → `HistoryLoaded { session, messages }` → `FileChangesLoaded` → `PlanLoaded` (that ordering is pinned by tests and relied on by `state.rs`) → `SessionsListed` → the rest of the `*Listed` events. `Conversation::from_history` rebuilds rows (decoding base64 images back to bytes for display).
- **Switch**: the sessions panel decides client-side — if `transcript.conversations.contains_key(&id)` (resident: loaded earlier or running in the background right now) → a pure local `Transcript::switch_to`, no engine call, no risk of a stale DB read clobbering live state. Otherwise → `engine.load_session(id)` → `Command::LoadSession` → the same `HistoryLoaded`/`FileChangesLoaded`/`PlanLoaded` sequence as startup. Switching to a not-yet-resident session and resuming on launch are **one code path**.
- A busy row shows a dot (`conversation.status.is_busy()`) — the visible proof a background session is still working.

## Delete

- Delete button → `engine.delete_session(id)` → `Command::DeleteSession`:
  1. Cancel any in-flight turn first (nothing left to persist its result against once the row is gone).
  2. `store.delete_session(id)` — hard delete; `messages`/`file_changes`/`plans` cascade via FKs (enforced by `PRAGMA foreign_keys = ON` at open). Deleting an unknown id is not an error (plain SQL semantics).
  3. `SessionDeleted(session)` — if the deleted session was active, `Transcript::apply` starts a fresh new chat (the same blank state "+ New Chat" produces). `SessionsListed` alone can't carry that signal: a deleted id is exactly what's missing from the list.

## Rollback (per-file, per-turn)

- Inspector Files tab → per-file **Roll Back** → confirmation dialog ("This undoes every file change made after this point in the conversation, not just this file. Conversation messages are kept.") → `engine.rollback(session, up_to_seq)` with `up_to_seq = entry.message_seq - 1`.
- Engine: refused while a turn is running for that session (same guard as `Send`). Else `store.reverse_to_point(session, up_to_seq)` — walk changes with `message_seq > up_to_seq` newest-first; restore (`old_content: Some`) or delete (`None`); destroy each change record as it's undone; return `ReverseOutcome { restored, deleted }`.
- Effects: on-disk files restored/deleted; `file_changes` rows removed; `FileChangesLoaded` re-emitted so the Inspector refreshes; **messages and the plan are untouched**. No redo.

## Invariants

- Session identity survives compaction, profile switches, and model switches — only what gets sent as history changes.
- A session's history lives exactly one place in memory (its slot) while a turn runs; everything else (UI, disk) is a projection.
- `delete_session` and `reverse_to_point` are both irreversible by design.