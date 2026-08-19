//! `.coder/sessions.db` — one store per workspace.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use architect_core::{ContentBlock, FileChange, FileChangeEntry, Message, Plan, Role};
use rusqlite::{Connection, params};

use crate::{
    error::SessionError,
    schema,
    types::{ReverseOutcome, SessionId, SessionSummary},
};

/// Durable storage for one workspace's conversations, rooted at
/// `<workspace_root>/.coder/sessions.db` — the same location V1 used.
pub struct SessionStore {
    conn: Arc<Mutex<Connection>>,
    /// Canonicalized workspace root. File paths are stored relative to this so
    /// the database stays meaningful if the workspace is moved or copied.
    root: PathBuf,
}

impl SessionStore {
    /// Creates `<workspace_root>/.coder/` if needed and opens (or
    /// initializes) `sessions.db` inside it.
    pub async fn open(workspace_root: impl Into<PathBuf>) -> Result<Self, SessionError> {
        let workspace_root = workspace_root.into();

        tokio::task::spawn_blocking(move || {
            let root = workspace_root
                .canonicalize()
                .map_err(|source| SessionError::Io {
                    path: workspace_root.clone(),
                    source,
                })?;

            let coder_dir = root.join(".coder");
            std::fs::create_dir_all(&coder_dir).map_err(|source| SessionError::Io {
                path: coder_dir.clone(),
                source,
            })?;

            let conn = Connection::open(coder_dir.join("sessions.db"))?;
            // Off by default per connection in SQLite — without this, the
            // schema's `ON DELETE CASCADE` on `messages`/`file_changes` is
            // declared but never enforced, and `delete_session` would leave
            // orphaned rows behind.
            conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            schema::init(&conn)?;

            Ok(Self {
                conn: Arc::new(Mutex::new(conn)),
                root,
            })
        })
        .await
        .map_err(|error| SessionError::TaskPanicked(error.to_string()))?
    }

    /// Path to the workspace this store's sessions belong to.
    pub fn workspace_root(&self) -> &Path {
        &self.root
    }

    pub async fn create_session(
        &self,
        provider_kind: &str,
        model: &str,
    ) -> Result<SessionId, SessionError> {
        let id = SessionId::new();
        self.create_session_with_id(id, provider_kind, model)
            .await?;
        Ok(id)
    }

    /// Same as [`Self::create_session`], but with a caller-chosen id rather
    /// than one generated here — how the desktop engine creates a session
    /// row for an id the UI already minted (see [`SessionId::new`]'s docs),
    /// so both sides agree on its identity before either does any work.
    pub async fn create_session_with_id(
        &self,
        id: SessionId,
        provider_kind: &str,
        model: &str,
    ) -> Result<(), SessionError> {
        let conn = self.conn.clone();
        let provider_kind = provider_kind.to_owned();
        let model = model.to_owned();

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            conn.execute(
                "INSERT INTO sessions (id, provider_kind, model) VALUES (?1, ?2, ?3)",
                params![id.to_string(), provider_kind, model],
            )?;
            Ok(())
        })
        .await
    }

    /// Same as [`Self::create_session_with_id`], but tagged as a sub-agent's
    /// session, spawned by a `spawn_subagents` tool call within `parent`'s
    /// own turn — how a spawned session's row gets `parent_id` set so it
    /// shows up nested under `parent` in the sidebar.
    pub async fn create_child_session_with_id(
        &self,
        id: SessionId,
        parent: SessionId,
        provider_kind: &str,
        model: &str,
    ) -> Result<(), SessionError> {
        let conn = self.conn.clone();
        let provider_kind = provider_kind.to_owned();
        let model = model.to_owned();

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            conn.execute(
                "INSERT INTO sessions (id, provider_kind, model, parent_id) VALUES (?1, ?2, ?3, ?4)",
                params![id.to_string(), provider_kind, model, parent.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    /// Rename a session — how the engine gives a freshly created session a
    /// readable label, derived from the user's first message, instead of the
    /// empty string every session starts with.
    pub async fn set_title(&self, session: SessionId, title: &str) -> Result<(), SessionError> {
        let conn = self.conn.clone();
        let title = title.to_owned();

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            conn.execute(
                "UPDATE sessions SET title = ?1 WHERE id = ?2",
                params![title, session.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    /// Append one message. `seq` is the caller's position for it within the
    /// session — the engine's `history` index — so messages replay in the
    /// order they were actually appended even if two are persisted out of
    /// order under load.
    pub async fn append_message(
        &self,
        session: SessionId,
        seq: i64,
        message: &Message,
    ) -> Result<(), SessionError> {
        let conn = self.conn.clone();
        let role = role_to_str(message.role);
        let content = serde_json::to_string(&message.content)?;

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            conn.execute(
                "INSERT INTO messages (session_id, seq, role, content) VALUES (?1, ?2, ?3, ?4)",
                params![session.to_string(), seq, role, content],
            )?;
            touch_session(&conn, session)?;
            Ok(())
        })
        .await
    }

    /// Discard every message recorded for a session and replace them with
    /// `messages` — how `Command::Compact` collapses a full transcript down
    /// to a single summary. Unlike `append_message`, this doesn't add to
    /// what's there, it replaces it outright; there is no undo, the same
    /// tradeoff `delete_session` makes. File changes and the plan are
    /// untouched — compacting only ever shrinks the chat history, not
    /// anything recorded against it.
    pub async fn replace_messages(
        &self,
        session: SessionId,
        messages: &[Message],
    ) -> Result<(), SessionError> {
        let conn = self.conn.clone();
        let rows = messages
            .iter()
            .map(|message| {
                let content = serde_json::to_string(&message.content)?;
                Ok::<_, SessionError>((role_to_str(message.role), content))
            })
            .collect::<Result<Vec<_>, _>>()?;

        blocking(move || {
            // A transaction, not a bare sequence of statements: without one,
            // a failure partway through (the process dying, a disk error on
            // one of the inserts) leaves the session with whatever prefix of
            // `messages` happened to make it in — silent, permanent loss of
            // the rest of its history. `tx.commit()` is the only thing that
            // makes any of this durable; dropping `tx` without calling it
            // rolls everything back instead.
            let mut conn = conn.lock().expect("session db lock");
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM messages WHERE session_id = ?1",
                params![session.to_string()],
            )?;
            for (seq, (role, content)) in rows.iter().enumerate() {
                tx.execute(
                    "INSERT INTO messages (session_id, seq, role, content) VALUES (?1, ?2, ?3, ?4)",
                    params![session.to_string(), seq as i64, role, content],
                )?;
            }
            touch_session(&tx, session)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Every message in a session, in the order they were appended.
    pub async fn load_messages(&self, session: SessionId) -> Result<Vec<Message>, SessionError> {
        let conn = self.conn.clone();

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            let mut statement = conn.prepare(
                "SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY seq ASC",
            )?;

            let rows = statement
                .query_map(params![session.to_string()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            rows.into_iter()
                .map(|(role, content)| {
                    let role = str_to_role(&role)?;
                    let content: Vec<ContentBlock> = serde_json::from_str(&content)?;
                    Ok(Message { role, content })
                })
                .collect()
        })
        .await
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let conn = self.conn.clone();

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            let mut statement = conn.prepare(
                "SELECT id, title, provider_kind, model, parent_id, created_at, updated_at \
                 FROM sessions ORDER BY updated_at DESC",
            )?;

            let rows = statement.query_map([], |row| {
                let id: String = row.get(0)?;
                let parent_id: Option<String> = row.get(4)?;
                Ok(SessionSummary {
                    id: id.parse().map_err(|_| {
                        rusqlite::Error::InvalidColumnType(
                            0,
                            "id".into(),
                            rusqlite::types::Type::Text,
                        )
                    })?,
                    title: row.get(1)?,
                    provider_kind: row.get(2)?,
                    model: row.get(3)?,
                    parent_id: parent_id
                        .map(|parent_id| parent_id.parse())
                        .transpose()
                        .map_err(|_| {
                            rusqlite::Error::InvalidColumnType(
                                4,
                                "parent_id".into(),
                                rusqlite::types::Type::Text,
                            )
                        })?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })?;

            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(SessionError::from)
        })
        .await
    }

    /// Permanently remove a session and everything recorded against it — its
    /// messages and file changes cascade via the schema's foreign keys. There
    /// is no undo: this is a delete, not a `reverse_to_point`.
    pub async fn delete_session(&self, session: SessionId) -> Result<(), SessionError> {
        let conn = self.conn.clone();

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            conn.execute(
                "DELETE FROM sessions WHERE id = ?1",
                params![session.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    /// Record one file mutation against a session, keyed to the message that
    /// caused it. `change.file_path` is stored relative to the workspace root
    /// so the database stays valid if the workspace is later moved.
    pub async fn record_file_change(
        &self,
        session: SessionId,
        message_seq: i64,
        change: &FileChange,
    ) -> Result<(), SessionError> {
        let conn = self.conn.clone();
        let relative = self.relative_path(&change.file_path);
        let change = change.clone();

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            conn.execute(
                "INSERT INTO file_changes \
                 (id, session_id, message_seq, file_path, old_content, new_content, tool_name) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    session.to_string(),
                    message_seq,
                    relative,
                    change.old_content,
                    change.new_content,
                    change.tool_name,
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Every file change recorded against a session, in the order they
    /// happened — what the Inspector panel shows for a session that wasn't
    /// created this app run. `file_path` is resolved back to absolute
    /// against this store's own workspace root, the same resolution
    /// `reverse_to_point` already does.
    pub async fn load_file_changes(
        &self,
        session: SessionId,
    ) -> Result<Vec<FileChangeEntry>, SessionError> {
        let conn = self.conn.clone();
        let root = self.root.clone();

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            let mut statement = conn.prepare(
                "SELECT message_seq, file_path, old_content, new_content, tool_name \
                 FROM file_changes \
                 WHERE session_id = ?1 ORDER BY message_seq ASC, created_at ASC, id ASC",
            )?;

            let rows = statement
                .query_map(params![session.to_string()], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            Ok(rows
                .into_iter()
                .map(
                    |(message_seq, file_path, old_content, new_content, tool_name)| {
                        FileChangeEntry {
                            message_seq,
                            change: FileChange {
                                file_path: root.join(file_path),
                                old_content,
                                new_content,
                                // Leaked: `FileChange::tool_name` is
                                // `&'static str` for the tools that
                                // construct one at compile time (a literal
                                // costs nothing); a value read back from
                                // storage has no such lifetime to borrow
                                // from, so it's leaked once per row instead
                                // — the same tradeoff `architect-mcp`'s tool
                                // names make, for the same reason.
                                tool_name: Box::leak(tool_name.into_boxed_str()),
                            },
                        }
                    },
                )
                .collect())
        })
        .await
    }

    /// Save the current plan for a session, replacing whatever was saved
    /// before — a snapshot, not an append, matching `write_plan`'s own
    /// full-replace semantics. `ON CONFLICT` makes this a plain upsert
    /// with no separate "does a row exist yet" check.
    pub async fn save_plan(&self, session: SessionId, plan: &Plan) -> Result<(), SessionError> {
        let conn = self.conn.clone();
        let data = serde_json::to_string(plan)?;

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            conn.execute(
                "INSERT INTO plans (session_id, data, updated_at) \
                 VALUES (?1, ?2, datetime('now')) \
                 ON CONFLICT(session_id) DO UPDATE SET \
                 data = excluded.data, updated_at = excluded.updated_at",
                params![session.to_string(), data],
            )?;
            Ok(())
        })
        .await
    }

    /// The plan currently saved for a session, if `write_plan` was ever
    /// called for it — what a resumed session's Inspector Plan tab (and
    /// `read_plan`'s next call) shows.
    pub async fn load_plan(&self, session: SessionId) -> Result<Option<Plan>, SessionError> {
        let conn = self.conn.clone();

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            let mut statement = conn.prepare("SELECT data FROM plans WHERE session_id = ?1")?;
            let mut rows = statement.query(params![session.to_string()])?;

            match rows.next()? {
                Some(row) => {
                    let data: String = row.get(0)?;
                    Ok(Some(serde_json::from_str(&data)?))
                }
                None => Ok(None),
            }
        })
        .await
    }

    /// Undo every file change recorded after `up_to_seq`, newest first,
    /// deleting each row as it is undone — the same "no redo" behavior V1's
    /// `reverse_to_point` had. `old_content: None` means the change created
    /// the file, so undoing it deletes the file rather than restoring it.
    pub async fn reverse_to_point(
        &self,
        session: SessionId,
        up_to_seq: i64,
    ) -> Result<ReverseOutcome, SessionError> {
        let conn = self.conn.clone();
        let root = self.root.clone();

        blocking(move || {
            let conn = conn.lock().expect("session db lock");
            let mut statement = conn.prepare(
                "SELECT id, file_path, old_content FROM file_changes \
                 WHERE session_id = ?1 AND message_seq > ?2 \
                 ORDER BY message_seq DESC, created_at DESC, id DESC",
            )?;

            let rows = statement
                .query_map(params![session.to_string(), up_to_seq], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            let mut outcome = ReverseOutcome::default();

            for (row_id, file_path, old_content) in rows {
                let absolute = root.join(&file_path);

                match old_content {
                    Some(content) => {
                        std::fs::write(&absolute, content).map_err(|source| {
                            SessionError::Restore {
                                path: absolute.clone(),
                                source,
                            }
                        })?;
                        outcome.restored.push(absolute);
                    }
                    None => {
                        match std::fs::remove_file(&absolute) {
                            Ok(()) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(source) => {
                                return Err(SessionError::Restore {
                                    path: absolute.clone(),
                                    source,
                                });
                            }
                        }
                        outcome.deleted.push(absolute);
                    }
                }

                conn.execute("DELETE FROM file_changes WHERE id = ?1", params![row_id])?;
            }

            Ok(outcome)
        })
        .await
    }

    fn relative_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

/// Run a blocking closure on the blocking pool, flattening the `JoinError`
/// into [`SessionError`] so every store method has one error type.
async fn blocking<T, F>(f: F) -> Result<T, SessionError>
where
    F: FnOnce() -> Result<T, SessionError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|error| SessionError::TaskPanicked(error.to_string()))?
}

fn touch_session(conn: &Connection, session: SessionId) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET updated_at = datetime('now') WHERE id = ?1",
        params![session.to_string()],
    )?;
    Ok(())
}

fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn str_to_role(value: &str) -> Result<Role, SessionError> {
    Ok(match value {
        "system" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        other => {
            return Err(SessionError::Sql(rusqlite::Error::InvalidColumnType(
                0,
                format!("unknown role {other:?}"),
                rusqlite::types::Type::Text,
            )));
        }
    })
}
