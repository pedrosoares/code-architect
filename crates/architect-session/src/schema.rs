//! `sessions.db` schema.
//!
//! One JSON column for a message's content rather than V1's OpenAI-shaped
//! columns (`reasoning_content`/`tool_call_id`/`tool_calls`): V2's
//! `Vec<ContentBlock>` already round-trips through serde, so there is nothing
//! to gain from denormalizing it.
//!
//! Migrations stay V1's pragmatic style — `CREATE TABLE IF NOT EXISTS` run at
//! every [`crate::store::SessionStore::open`], no migration framework. That is
//! appropriate for a single-node desktop app; it would not be for a service
//! with concurrent writers or a need to roll a schema back.

use rusqlite::Connection;

const STATEMENTS: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '',
    provider_kind TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS file_changes (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    message_seq INTEGER NOT NULL,
    file_path TEXT NOT NULL,
    old_content TEXT,
    new_content TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- One row per session, upserted on every `write_plan` call — a snapshot of
-- the current plan, not a log of edits (unlike `file_changes`, where every
-- edit matters for rollback). `data` is the whole `Plan` as JSON, same
-- no-denormalizing choice `messages.content` already makes.
CREATE TABLE IF NOT EXISTS plans (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    data TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
CREATE INDEX IF NOT EXISTS idx_file_changes_session ON file_changes(session_id, message_seq);
";

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(STATEMENTS)?;

    // `parent_id` (a sub-agent's session, spawned by a `spawn_subagents`
    // tool call in another session's turn — NULL for an ordinary,
    // user-started chat) was added after this table already existed in the
    // wild. `CREATE TABLE IF NOT EXISTS` above is a no-op against a
    // pre-existing `sessions` table, so a database created before this
    // column existed needs it added explicitly here. SQLite has no `ADD
    // COLUMN IF NOT EXISTS`, so this just tries the `ALTER TABLE` and
    // tolerates "already there" — the same pragmatic, no-migration-
    // framework style this module's doc comment already describes,
    // extended the one step it needs to actually add a column rather than
    // only ever create new tables.
    match conn.execute(
        "ALTER TABLE sessions ADD COLUMN parent_id TEXT REFERENCES sessions(id) ON DELETE CASCADE",
        [],
    ) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, Some(ref message)))
            if message.contains("duplicate column name") =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_twice_does_not_error() {
        let conn = Connection::open_in_memory().unwrap();

        init(&conn).unwrap();
        init(&conn).unwrap();
    }

    /// The exact shape of a database written before `parent_id` existed —
    /// `init` must add the column rather than silently doing nothing, or
    /// every query naming it (`list_sessions`) fails against a real,
    /// already-in-use `.coder/sessions.db` the next time the app opens it.
    #[test]
    fn adds_parent_id_to_a_database_created_before_it_existed() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL DEFAULT '',
                provider_kind TEXT NOT NULL,
                model TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();

        init(&conn).unwrap();

        // Fails at prepare time if the column is still missing.
        conn.prepare("SELECT parent_id FROM sessions").unwrap();
    }
}
