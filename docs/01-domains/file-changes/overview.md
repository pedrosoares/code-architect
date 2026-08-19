---
id: domain.file-changes
type: domain
title: File Changes & Rollback Domain
depends_on:
- domain.tools
- rule.persistence
relations:
  related_flows:
  - flow.rollback
---

File changes are the audit trail of everything the agent writes to disk — the basis for the Inspector's Files/Diff tabs and for rollback.

## The record — `FileChange`

Defined in `architect-core::change`:

- `file_path: PathBuf` — the **absolute resolved** path (as `ToolContext::resolve` produced it).
- `old_content: Option<String>` — sentinel invariant: **`None` means the file did not exist before**. On undo: restore if it existed, delete if it didn't.
- `new_content: String` — the full post-change content.
- `tool_name: &'static str` — `"write_file"` or `"edit_file"`.

`FileChangeEntry { message_seq: i64, change: FileChange }` tags a change with its conversation position, so callers can offer "roll back to before this" without touching the database. Deliberately **not** `Serialize`/`Deserialize` — it is never persisted or sent over the wire, and `&'static str` would make a derived `Deserialize` a real lifetime problem.

## Recording path

1. A file-mutating tool (`write_file`/`edit_file`) calls `ctx.record_change(change)` — the result it returns to the model is separate ("the result is what the model sees, this is what persistence sees").
2. `ToolContext` forwards to its `ChangeRecorder` — in the app, a `ChannelRecorder` over a **per-turn** `UnboundedSender<FileChange>` (per-turn is why tool registries are rebuilt each turn: a shared instance couldn't tell two concurrent turns' changes apart).
3. The engine's `spawn_turn` drains the channel into the `TaskOutcome` after the turn.
4. The worker's `task_rx` arm persists each via `store.record_file_change(session, message_seq, &change)` and emits `EngineEvent::FileChanged { session, entry }` live for the Inspector.

**Rollback granularity is per-turn**: all of a turn's changes are attributed to `message_seq = slot.history.len() - 1` — the turn's last message — because "undo everything after this point in the conversation" is the unit of undo, not "undo this one tool call".

## Storage (SQLite)

`file_changes` table: `id` (v4 UUID per row), `session_id` (FK cascade), `message_seq`, `file_path` **relative to the workspace root** (`strip_prefix`, falling back to the full path), `old_content` (NULL = created), `new_content`, `tool_name`, `created_at`. Ordered back by `message_seq ASC, created_at ASC, id ASC`; paths re-joined against the store's root on load (a `&'static str` `tool_name` read back from storage is leaked once per row — an acknowledged tradeoff).

The same path can appear many times (create, then edit, then edit) — there is no dedup by path. The **UI** dedupes for display: the Inspector's Files tab keeps the first `old_content` and the most recent `new_content`/`tool_name`/`message_seq` per path, which is also the rollback anchor.

## Rollback — `reverse_to_point(session, up_to_seq)`

Exact algorithm (in `architect-session`):

1. `SELECT id, file_path, old_content FROM file_changes WHERE session_id = ? AND message_seq > ? ORDER BY message_seq DESC, created_at DESC, id DESC` — only changes **after** the point, newest first.
2. For each row: resolve `root.join(file_path)`:
   - `old_content: Some(content)` → write the old content back; push the path to `outcome.restored`.
   - `old_content: None` → delete the file (`NotFound` treated as success); push to `outcome.deleted`.
3. Immediately after undoing each row, `DELETE FROM file_changes WHERE id = ?` — the record is destroyed as it is undone (no redo; same behavior V1 had).
4. Returns `ReverseOutcome { restored, deleted }` — absolute paths, newest-first.

Properties:

- **Created-then-edited files** work because rows undo newest-first: the edit (Some) writes back the created content, then the create (None) deletes the file.
- **Messages are never touched** — rollback is filesystem-only.
- **Transactional guarantees: effectively none** — the loop interleaves filesystem writes and per-row DELETEs with no transaction; a mid-loop failure leaves a partially-undone state.
- The engine **refuses rollback while a turn is running** for that session (same guard as `Send` against a second turn), and the UI requires a confirmation dialog that names exactly what will be undone.
- `Command::Rollback` carries `up_to_seq = entry.message_seq - 1` for the clicked file's last-seen change.

## Design notes

- File changes are a **log** (every edit matters for rollback), in deliberate contrast to the plan, which is a **snapshot**.
- `delete_session` cascades to `file_changes` via FK (only enforced because `PRAGMA foreign_keys = ON` is set at open).
- Deleting a session is "a delete, not a `reverse_to_point`" — there is no undo for either.