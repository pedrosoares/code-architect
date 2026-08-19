---
id: flow.rollback
type: flow
title: Roll Back File Changes
depends_on:
- flow.session-lifecycle
- domain.file-changes
---

**Goal:** the user undoes a stretch of the agent's file edits from the Inspector — "roll back to before this change" — without touching the conversation.

## Steps

1. **Inspector → Files tab** — one deduped row per touched path: first `old_content` + most-recent `new_content`/`tool_name`/`message_seq` (the rollback anchor). Includes changes from resumed, previously-saved sessions.
2. **Click "Roll back" on a row** → `rollback_popup` (a `Popup`), gated by `rollback_target`: "Roll back to before this change?" — "This undoes every file change made after this point in the conversation, not just this file. Conversation messages are kept."
3. **Confirm** → `engine.rollback(session, up_to_seq)` where `up_to_seq = entry.message_seq - 1` (the clicked file's last-seen change) → `Command::Rollback { session, up_to_seq }`.
4. **Worker** —
   - If a turn is running for that session → `Failed` (the same guard `Command::Send` uses against a second concurrent turn).
   - Else `store.reverse_to_point(session, up_to_seq)`:
     - `SELECT id, file_path, old_content FROM file_changes WHERE session_id = ? AND message_seq > ? ORDER BY message_seq DESC, created_at DESC, id DESC` — only changes *after* the point, **newest first** (so a created-then-edited file un-edits then un-creates correctly).
     - Per row: `old_content: Some(c)` → write `c` back to `root.join(file_path)`, push to `restored`; `None` → delete the file (`NotFound` = success), push to `deleted`. **Immediately** `DELETE FROM file_changes WHERE id = ?` — the record is destroyed as it's undone.
   - Re-emits `FileChangesLoaded` for the session (the Inspector's Files/Diff tabs refresh).
5. **UI** — files restored/deleted on disk; the Files tab now lists only the surviving changes; the transcript is untouched.

## Rules

- **Scope is files only, never messages** — `messages` and `plans` are not read or written.
- **Per-turn granularity** — all of a turn's changes share `message_seq` (the turn's last message), so "roll back to before this change" undoes everything after that turn's point.
- **Irreversible, no redo** — the change rows are destroyed as they're undone; `delete_session` is a hard delete for the same reason.
- **No transaction** — filesystem writes and per-row DELETEs interleave with no DB transaction; a mid-loop failure leaves a partially-undone state (an accepted tradeoff).
- **Refused while busy** — never mid-turn (guard shared with `Send`).
- Paths are stored workspace-relative; the store re-joins against its root, so a moved workspace still rolls back correctly.