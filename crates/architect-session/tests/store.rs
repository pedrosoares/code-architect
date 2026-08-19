//! `SessionStore` round-trips, against a tempdir workspace.

use architect_core::{ContentBlock, FileChange, Message, Plan, PlanStep, Role, StepStatus};
use architect_session::{SessionId, SessionStore};

async fn store() -> (tempfile::TempDir, SessionStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await.unwrap();
    (dir, store)
}

#[tokio::test]
async fn creates_appends_and_lists_a_session() {
    let (_dir, store) = store().await;

    let session = store
        .create_session("openai", "qwen/qwen3.8-27b")
        .await
        .unwrap();
    store
        .append_message(session, 0, &Message::user("hello"))
        .await
        .unwrap();
    store
        .append_message(session, 1, &Message::assistant("hi there"))
        .await
        .unwrap();

    let sessions = store.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session);
    assert_eq!(sessions[0].provider_kind, "openai");
    assert_eq!(sessions[0].model, "qwen/qwen3.8-27b");

    let messages = store.load_messages(session).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(messages[0].text(), "hello");
    assert_eq!(messages[1].text(), "hi there");
}

#[tokio::test]
async fn replace_messages_discards_the_old_history_wholesale() {
    let (_dir, store) = store().await;

    let session = store
        .create_session("openai", "qwen/qwen3.8-27b")
        .await
        .unwrap();
    store
        .append_message(session, 0, &Message::user("hello"))
        .await
        .unwrap();
    store
        .append_message(session, 1, &Message::assistant("hi there"))
        .await
        .unwrap();
    store
        .append_message(session, 2, &Message::user("now do the thing"))
        .await
        .unwrap();

    store
        .replace_messages(session, &[Message::assistant("a summary of the above")])
        .await
        .unwrap();

    let messages = store.load_messages(session).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, Role::Assistant);
    assert_eq!(messages[0].text(), "a summary of the above");
}

#[tokio::test]
async fn creating_a_session_with_a_caller_chosen_id_uses_that_id() {
    let (_dir, store) = store().await;
    // Client-minted — the desktop engine mints a `SessionId` for "New Chat"
    // before either side of the UI/worker channel does any work, so both
    // agree on its identity from the first message.
    let id = SessionId::new();

    store
        .create_session_with_id(id, "openai", "m")
        .await
        .unwrap();

    let sessions = store.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, id);
    assert_eq!(sessions[0].parent_id, None);
}

#[tokio::test]
async fn a_child_session_round_trips_its_parent_id() {
    let (_dir, store) = store().await;
    let parent = store.create_session("openai", "m").await.unwrap();
    let child = SessionId::new();

    store
        .create_child_session_with_id(child, parent, "openai", "m")
        .await
        .unwrap();

    let sessions = store.list_sessions().await.unwrap();
    let child_row = sessions.iter().find(|s| s.id == child).unwrap();
    assert_eq!(child_row.parent_id, Some(parent));
    let parent_row = sessions.iter().find(|s| s.id == parent).unwrap();
    assert_eq!(parent_row.parent_id, None);
}

#[tokio::test]
async fn deleting_a_parent_session_cascades_to_its_children() {
    let (_dir, store) = store().await;
    let parent = store.create_session("openai", "m").await.unwrap();
    let child = SessionId::new();
    store
        .create_child_session_with_id(child, parent, "openai", "m")
        .await
        .unwrap();

    store.delete_session(parent).await.unwrap();

    let sessions = store.list_sessions().await.unwrap();
    assert!(sessions.is_empty(), "the child should be gone too");
}

#[tokio::test]
async fn deleting_a_session_removes_it_from_the_list() {
    let (_dir, store) = store().await;
    let keep = store.create_session("openai", "m").await.unwrap();
    let doomed = store.create_session("openai", "m").await.unwrap();

    store.delete_session(doomed).await.unwrap();

    let sessions = store.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, keep);
    assert!(store.load_messages(doomed).await.unwrap().is_empty());
}

#[tokio::test]
async fn deleting_a_session_cascades_to_its_messages_and_file_changes() {
    let (dir, store) = store().await;
    let session = store.create_session("openai", "m").await.unwrap();
    store
        .append_message(session, 0, &Message::user("hello"))
        .await
        .unwrap();
    store
        .record_file_change(
            session,
            0,
            &FileChange {
                file_path: dir.path().join("a.txt"),
                old_content: None,
                new_content: "hi".into(),
                tool_name: "write_file",
            },
        )
        .await
        .unwrap();

    store.delete_session(session).await.unwrap();

    // Checked against the raw database, not just this store's own methods —
    // proof the schema's `ON DELETE CASCADE` actually fired (it is declared
    // but inert unless `PRAGMA foreign_keys = ON` is set per connection),
    // not merely that `load_messages` treats a missing session as empty.
    let conn = rusqlite::Connection::open(dir.path().join(".coder/sessions.db")).unwrap();
    let messages: i64 = conn
        .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    let file_changes: i64 = conn
        .query_row("SELECT count(*) FROM file_changes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        messages, 0,
        "messages must cascade-delete with their session"
    );
    assert_eq!(
        file_changes, 0,
        "file_changes must cascade-delete with their session"
    );
}

#[tokio::test]
async fn load_file_changes_returns_them_in_order_with_absolute_paths() {
    let (dir, store) = store().await;
    let session = store.create_session("openai", "m").await.unwrap();

    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");

    store
        .record_file_change(
            session,
            0,
            &FileChange {
                file_path: a.clone(),
                old_content: None,
                new_content: "first".into(),
                tool_name: "write_file",
            },
        )
        .await
        .unwrap();
    store
        .record_file_change(
            session,
            1,
            &FileChange {
                file_path: b.clone(),
                old_content: None,
                new_content: "b content".into(),
                tool_name: "write_file",
            },
        )
        .await
        .unwrap();
    // The same path again, later in the conversation — proves this returns
    // every recorded change in order, not deduplicated by path.
    store
        .record_file_change(
            session,
            2,
            &FileChange {
                file_path: a.clone(),
                old_content: Some("first".into()),
                new_content: "second".into(),
                tool_name: "edit_file",
            },
        )
        .await
        .unwrap();

    let changes = store.load_file_changes(session).await.unwrap();

    assert_eq!(changes.len(), 3);
    assert_eq!(changes[0].message_seq, 0);
    assert_eq!(changes[0].change.file_path, a);
    assert_eq!(changes[0].change.new_content, "first");
    assert_eq!(changes[1].message_seq, 1);
    assert_eq!(changes[1].change.file_path, b);
    assert_eq!(changes[2].message_seq, 2);
    assert_eq!(changes[2].change.file_path, a);
    assert_eq!(changes[2].change.old_content.as_deref(), Some("first"));
    assert_eq!(changes[2].change.new_content, "second");
    assert_eq!(changes[2].change.tool_name, "edit_file");
}

#[tokio::test]
async fn load_file_changes_for_an_unknown_session_is_empty() {
    let (_dir, store) = store().await;

    let changes = store
        .load_file_changes("00000000-0000-0000-0000-000000000001".parse().unwrap())
        .await
        .unwrap();

    assert!(changes.is_empty());
}

#[tokio::test]
async fn a_session_with_no_saved_plan_loads_none() {
    let (_dir, store) = store().await;
    let session = store.create_session("openai", "m").await.unwrap();

    let plan = store.load_plan(session).await.unwrap();

    assert!(plan.is_none());
}

#[tokio::test]
async fn saving_a_plan_round_trips_it() {
    let (_dir, store) = store().await;
    let session = store.create_session("openai", "m").await.unwrap();

    let plan = Plan {
        goal: Some("Ship the feature".to_owned()),
        steps: vec![PlanStep {
            description: "Write the code".to_owned(),
            status: StepStatus::InProgress,
            substeps: vec![],
        }],
    };
    store.save_plan(session, &plan).await.unwrap();

    let loaded = store.load_plan(session).await.unwrap();

    assert_eq!(loaded, Some(plan));
}

#[tokio::test]
async fn saving_a_plan_again_replaces_rather_than_duplicates() {
    let (_dir, store) = store().await;
    let session = store.create_session("openai", "m").await.unwrap();

    store
        .save_plan(
            session,
            &Plan {
                goal: Some("First draft".to_owned()),
                steps: vec![],
            },
        )
        .await
        .unwrap();
    store
        .save_plan(
            session,
            &Plan {
                goal: Some("Revised".to_owned()),
                steps: vec![],
            },
        )
        .await
        .unwrap();

    let loaded = store.load_plan(session).await.unwrap().unwrap();
    assert_eq!(loaded.goal.as_deref(), Some("Revised"));
}

#[tokio::test]
async fn deleting_a_session_cascades_to_its_plan() {
    let (_dir, store) = store().await;
    let session = store.create_session("openai", "m").await.unwrap();
    store
        .save_plan(
            session,
            &Plan {
                goal: None,
                steps: vec![],
            },
        )
        .await
        .unwrap();

    store.delete_session(session).await.unwrap();

    let loaded = store.load_plan(session).await.unwrap();
    assert!(loaded.is_none());
}

#[tokio::test]
async fn deleting_an_unknown_session_is_not_an_error() {
    let (_dir, store) = store().await;

    // Matches SQL DELETE semantics (zero rows affected is not a failure) —
    // the UI can retry a delete without special-casing "already gone".
    store
        .delete_session("00000000-0000-0000-0000-000000000001".parse().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn set_title_renames_a_session() {
    let (_dir, store) = store().await;
    let session = store.create_session("openai", "m").await.unwrap();

    let before = store.list_sessions().await.unwrap();
    assert_eq!(before[0].title, "", "a new session starts untitled");

    store
        .set_title(session, "Fix the layout bug")
        .await
        .unwrap();

    let after = store.list_sessions().await.unwrap();
    assert_eq!(after[0].title, "Fix the layout bug");
}

#[tokio::test]
async fn round_trips_tool_content_through_json() {
    let (_dir, store) = store().await;
    let session = store
        .create_session("anthropic", "claude-opus-5")
        .await
        .unwrap();

    let message = Message::new(
        Role::Assistant,
        vec![
            ContentBlock::reasoning("thinking it over"),
            ContentBlock::text("done"),
            ContentBlock::ToolUse(architect_core::ToolCall {
                id: "call_a".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "a.rs"}),
            }),
        ],
    );
    store.append_message(session, 0, &message).await.unwrap();

    let loaded = store.load_messages(session).await.unwrap();
    assert_eq!(loaded[0], message);
}

#[tokio::test]
async fn creating_a_new_store_over_an_existing_db_does_not_error() {
    let dir = tempfile::tempdir().unwrap();

    let first = SessionStore::open(dir.path()).await.unwrap();
    first.create_session("openai", "m").await.unwrap();
    drop(first);

    let second = SessionStore::open(dir.path()).await.unwrap();
    assert_eq!(second.list_sessions().await.unwrap().len(), 1);
}

#[tokio::test]
async fn restores_an_overwritten_file_on_reverse() {
    let (dir, store) = store().await;
    let session = store.create_session("openai", "m").await.unwrap();

    let path = dir.path().join("a.txt");
    std::fs::write(&path, "new content").unwrap();

    store
        .record_file_change(
            session,
            1,
            &FileChange {
                file_path: path.clone(),
                old_content: Some("original content".into()),
                new_content: "new content".into(),
                tool_name: "write_file",
            },
        )
        .await
        .unwrap();

    let outcome = store.reverse_to_point(session, 0).await.unwrap();

    assert_eq!(outcome.restored, std::slice::from_ref(&path));
    assert!(outcome.deleted.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original content");
}

#[tokio::test]
async fn deletes_a_created_file_on_reverse() {
    let (dir, store) = store().await;
    let session = store.create_session("openai", "m").await.unwrap();

    let path = dir.path().join("created.txt");
    std::fs::write(&path, "brand new").unwrap();

    store
        .record_file_change(
            session,
            1,
            &FileChange {
                file_path: path.clone(),
                old_content: None, // the file did not exist before this change
                new_content: "brand new".into(),
                tool_name: "write_file",
            },
        )
        .await
        .unwrap();

    let outcome = store.reverse_to_point(session, 0).await.unwrap();

    assert_eq!(outcome.deleted, std::slice::from_ref(&path));
    assert!(!path.exists());
}

#[tokio::test]
async fn reverse_only_undoes_changes_after_the_given_point_newest_first() {
    let (dir, store) = store().await;
    let session = store.create_session("openai", "m").await.unwrap();

    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, "a-v2").unwrap();
    std::fs::write(&b, "b-v2").unwrap();

    // Change at seq 1 should survive; changes at seq 2 and 3 get undone.
    store
        .record_file_change(
            session,
            1,
            &FileChange {
                file_path: a.clone(),
                old_content: Some("a-v0".into()),
                new_content: "a-v1".into(),
                tool_name: "edit_file",
            },
        )
        .await
        .unwrap();
    store
        .record_file_change(
            session,
            2,
            &FileChange {
                file_path: a.clone(),
                old_content: Some("a-v1".into()),
                new_content: "a-v2".into(),
                tool_name: "edit_file",
            },
        )
        .await
        .unwrap();
    store
        .record_file_change(
            session,
            3,
            &FileChange {
                file_path: b.clone(),
                old_content: None,
                new_content: "b-v2".into(),
                tool_name: "write_file",
            },
        )
        .await
        .unwrap();

    let outcome = store.reverse_to_point(session, 1).await.unwrap();

    assert_eq!(std::fs::read_to_string(&a).unwrap(), "a-v1");
    assert!(!b.exists());
    assert_eq!(outcome.restored, [a]);
    assert_eq!(outcome.deleted, [b]);
}

#[tokio::test]
async fn file_paths_are_stored_relative_to_the_workspace() {
    let (dir, store) = store().await;
    let session = store.create_session("openai", "m").await.unwrap();
    let nested = dir.path().join("src/main.rs");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    std::fs::write(&nested, "fn main() {}").unwrap();

    store
        .record_file_change(
            session,
            1,
            &FileChange {
                file_path: nested.clone(),
                old_content: None,
                new_content: "fn main() {}".into(),
                tool_name: "write_file",
            },
        )
        .await
        .unwrap();

    // Reversing must resolve the stored relative path back against this
    // store's own workspace root, not wherever the absolute path pointed
    // originally — this is what makes the database portable if the
    // workspace is moved or copied.
    let outcome = store.reverse_to_point(session, 0).await.unwrap();
    assert_eq!(outcome.deleted, [nested]);
}
