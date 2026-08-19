mod tests {
    use std::time::Duration;

    use architect_agent::AgentEvent;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, method, path},
    };

    use super::*;
    use crate::state::Transcript;

    #[test]
    fn defaults_to_a_local_server() {
        let config = EngineConfig::default();

        assert_eq!(config.kind, "openai");
        assert_eq!(config.base_url.as_deref(), Some("http://localhost:1234/v1"));
        assert!(config.api_key.is_none(), "a local server needs no key");
    }

    #[test]
    fn a_bad_configuration_is_reported_rather_than_fatal() {
        // Anthropic without a key: the window still opens and the failure shows
        // up in the transcript instead of taking the process down.
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            kind: "anthropic".into(),
            base_url: None,
            api_key: None,
            model: "claude-opus-5".into(),
            system: String::new(),
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
        });

        let mut events = engine
            .take_events()
            .expect("the receiver is available once");
        let event = loop {
            if let Some(event) = events.blocking_recv() {
                break event;
            }
        };

        assert!(
            matches!(&event, EngineEvent::Failed { session: None, message } if message.contains("api_key"))
        );

        // The worker does not die here — it keeps draining commands with
        // `provider: None`, so the settings panel stays usable. A `Send`
        // still fails the same way every time, tagged with the session that
        // tried to send. (Startup also emits `SessionsListed`/
        // `ProfilesListed` before the command loop opens, which this skips
        // past rather than asserts on.)
        let session = SessionId::new();
        engine.send(session, "hello?", Vec::new());
        let event = loop {
            match events.blocking_recv() {
                Some(EngineEvent::Failed {
                    session: Some(id),
                    message,
                }) if id == session && message.contains("no working API configuration") => {
                    break message;
                }
                Some(_) => {}
                None => panic!("engine closed before answering the send"),
            }
        };
        assert!(event.contains("no working API configuration"));
    }

    /// The whole point of this rewrite, proven hermetically: a turn in
    /// flight for one session must not delay the worker from starting a
    /// turn for a different one. Session `slow`'s mock response is
    /// deliberately delayed; `fast`'s is not. Under the old design — where
    /// `Command::Send` awaited `run_turn` inline in the command loop —
    /// `fast`'s send couldn't even be *processed* until `slow`'s finished,
    /// so its `TurnCompleted` would arrive second regardless of the delay.
    #[tokio::test]
    async fn a_turn_for_one_session_does_not_block_sending_to_another() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("please respond slowly"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_reply("slow", "done A"), "text/event-stream")
                    .set_delay(Duration::from_millis(300)),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("please respond quickly"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_reply("fast", "done B"), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            base_url: Some(format!("{}/v1", server.uri())),
            ..EngineConfig::default()
        });
        let mut events = engine.take_events().expect("receiver");

        let slow = SessionId::new();
        let fast = SessionId::new();

        engine.send(slow, "please respond slowly", Vec::new());
        // If `Send` were still handled inline, this would sit behind the
        // 300ms response above instead of starting immediately.
        engine.send(fast, "please respond quickly", Vec::new());

        let mut completed = Vec::new();
        while completed.len() < 2 {
            match events.recv().await {
                Some(EngineEvent::Agent {
                    session,
                    event: AgentEvent::TurnCompleted { .. },
                }) => completed.push(session),
                Some(EngineEvent::Failed { message, .. }) => panic!("turn failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before both turns finished"),
            }
        }

        assert_eq!(
            completed,
            [fast, slow],
            "the fast session's turn must finish first — proof the slow \
             one being in flight never blocked the worker from starting \
             the other"
        );
    }

    /// Saving, listing, and activating a configuration — the settings panel's
    /// whole CRUD surface — needs no network at all: `ProviderRegistry::build`
    /// only constructs a provider object, it never connects. Hermetic, no
    /// `#[ignore]`.
    #[tokio::test]
    async fn saving_a_profile_lists_it_and_activating_it_switches_the_agent() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let profile = architect_config::Profile::new("Local", "openai", "test-model")
            .base_url("http://localhost:1/v1");
        engine.save_profile(profile.clone());

        let (profiles, active) = loop {
            match events.recv().await {
                Some(EngineEvent::ProfilesListed { profiles, active }) if !profiles.is_empty() => {
                    break (profiles, active);
                }
                Some(EngineEvent::Failed { message, .. }) => panic!("saving failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before the profile was saved"),
            }
        };
        assert_eq!(profiles, std::slice::from_ref(&profile));
        assert_eq!(active, None, "saving a profile does not activate it");

        engine.activate_profile(profile.id);
        let (profiles, active) = loop {
            match events.recv().await {
                Some(EngineEvent::ProfilesListed { profiles, active }) if active.is_some() => {
                    break (profiles, active);
                }
                Some(EngineEvent::Failed { message, .. }) => panic!("activation failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before activation was reported"),
            }
        };
        assert_eq!(profiles, std::slice::from_ref(&profile));
        assert_eq!(active, Some(profile.id));

        // The settings panel's "Default" row: go back to the engine's own
        // startup configuration without deleting the saved profile.
        engine.deactivate_profile();
        let (profiles, active) = loop {
            match events.recv().await {
                Some(EngineEvent::ProfilesListed { profiles, active }) if active.is_none() => {
                    break (profiles, active);
                }
                Some(EngineEvent::Failed { message, .. }) => {
                    panic!("deactivation failed: {message}")
                }
                Some(_) => {}
                None => panic!("engine closed before deactivation was reported"),
            }
        };
        assert_eq!(
            profiles,
            std::slice::from_ref(&profile),
            "deactivating must not delete the profile"
        );
        assert_eq!(active, None);

        engine.delete_profile(profile.id);
        let (profiles, active) = loop {
            match events.recv().await {
                Some(EngineEvent::ProfilesListed { profiles, active }) => break (profiles, active),
                Some(EngineEvent::Failed { message, .. }) => panic!("deletion failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before deletion was reported"),
            }
        };
        assert!(profiles.is_empty());
        assert_eq!(
            active, None,
            "deleting the active profile must clear it, not leave a dangling id"
        );
    }

    /// Saving, listing, and deleting an MCP server — hermetic, no
    /// `#[ignore]`: the configured command doesn't exist, so
    /// `reconnect_mcp`'s connect attempt fails fast and predictably (and is
    /// reported via the same global-failure path a bad provider config
    /// uses). That failure is exactly what's asserted here — this test
    /// covers the CRUD/list/event flow, not a real connection (that's
    /// `architect-mcp`'s own live test's job).
    #[tokio::test]
    async fn saving_an_mcp_server_lists_it_and_reports_the_failed_connection() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let server =
            architect_config::McpServerConfig::stdio("Nonexistent", "definitely-not-a-real-binary");
        engine.save_mcp_server(server.clone());

        // `reconnect_mcp` reports the failed connection *before*
        // `send_mcp_servers_listed` runs — a loop that only watched for
        // `McpServersListed` would silently swallow this in its catch-all,
        // and a second loop watching for it afterward would then wait
        // forever for an event that already went by. (This is exactly what
        // an earlier version of this test did — it hung, which is what
        // caught the ordering in the first place.)
        let message = loop {
            match events.recv().await {
                Some(EngineEvent::Failed {
                    session: None,
                    message,
                }) => break message,
                Some(_) => {}
                None => panic!("engine closed before the connection failure was reported"),
            }
        };
        assert!(message.contains("Nonexistent"), "got: {message}");

        let servers = loop {
            match events.recv().await {
                Some(EngineEvent::McpServersListed(servers)) if !servers.is_empty() => {
                    break servers;
                }
                Some(_) => {}
                None => panic!("engine closed before the server was saved"),
            }
        };
        assert_eq!(servers, std::slice::from_ref(&server));

        engine.delete_mcp_server(server.id);
        let servers = loop {
            match events.recv().await {
                Some(EngineEvent::McpServersListed(servers)) => break servers,
                Some(_) => {}
                None => panic!("engine closed before deletion was reported"),
            }
        };
        assert!(servers.is_empty());
    }

    /// Same proof as `saving_an_mcp_server_lists_it_and_reports_the_
    /// failed_connection`, for the `Http` transport — `reconnect_mcp`
    /// must actually dispatch to `architect_mcp::connect_http` for this
    /// variant, not silently fall through to the stdio path. `.invalid` is
    /// a reserved TLD (RFC 2606) guaranteed to never resolve, so this needs
    /// no real network access and fails fast and deterministically.
    #[tokio::test]
    async fn saving_an_http_mcp_server_reports_the_failed_connection() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let server =
            architect_config::McpServerConfig::http("Unreachable", "http://mcp.invalid/mcp");
        engine.save_mcp_server(server.clone());

        let message = loop {
            match events.recv().await {
                Some(EngineEvent::Failed {
                    session: None,
                    message,
                }) => break message,
                Some(_) => {}
                None => panic!("engine closed before the connection failure was reported"),
            }
        };
        assert!(message.contains("Unreachable"), "got: {message}");
    }

    /// No network needed: `IntegrationsConfig` holds plain tokens, building
    /// the GitHub/Slack/Linear tools from them is synchronous and
    /// infallible (unlike MCP's connect-and-handshake) — nothing here can
    /// fail the way a bad MCP server command can, so there is no
    /// `EngineEvent::Failed` half to this test, only persist-then-confirm.
    #[tokio::test]
    async fn saving_integrations_persists_and_lists_them() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let integrations = architect_config::IntegrationsConfig {
            github_token: Some("ghp_test".into()),
            github_use_gh_cli: false,
            slack_token: Some("xoxb-test".into()),
            linear_api_key: None,
        };
        engine.save_integrations(integrations.clone());

        let listed = loop {
            match events.recv().await {
                Some(EngineEvent::IntegrationsListed(listed)) if listed.github_token.is_some() => {
                    break listed;
                }
                Some(EngineEvent::Failed { message, .. }) => {
                    panic!("saving integrations failed: {message}")
                }
                Some(_) => {}
                None => panic!("engine closed before integrations were saved"),
            }
        };
        assert_eq!(listed, integrations);
    }

    /// Same shape as `saving_integrations_persists_and_lists_them` — no
    /// network, `ObsidianDriver::new` only needs a directory it can create.
    #[tokio::test]
    async fn saving_docs_config_persists_and_lists_them() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let vault = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let docs = architect_config::DocsConfig {
            enabled: true,
            driver: "obsidian".to_owned(),
            vault_path: Some(vault.path().display().to_string()),
        };
        engine.save_docs_config(docs.clone());

        let listed = loop {
            match events.recv().await {
                Some(EngineEvent::DocsConfigListed(listed)) if listed == docs => break listed,
                Some(EngineEvent::Failed { message, .. }) => {
                    panic!("saving docs config failed: {message}")
                }
                Some(_) => {}
                None => panic!("engine closed before docs config was saved"),
            }
        };
        assert_eq!(listed, docs);
    }

    /// `apply_oauth_result` directly — no `Engine`, no browser, no network:
    /// this is the function `Command::StartOAuthLogin`'s real `oauth::
    /// run_login` result eventually reaches, pulled out specifically so
    /// this path is testable without either.
    #[tokio::test]
    async fn apply_oauth_result_persists_a_successful_login() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_store = Some(
            ConfigStore::open(dir.path().join("profiles.json"))
                .await
                .expect("open store"),
        );
        let (events, mut events_rx) = unbounded_channel::<EngineEvent>();

        let saved = apply_oauth_result(
            &config_store,
            &events,
            OAuthProvider::GitHub,
            Ok("gho_test_token".to_owned()),
        )
        .await;

        assert!(saved, "a successful login should report saved = true");
        let integrations = config_store.as_ref().unwrap().integrations().await.unwrap();
        assert_eq!(integrations.github_token.as_deref(), Some("gho_test_token"));
        assert!(
            events_rx.try_recv().is_err(),
            "no event needed on success — the caller's own FileChangesLoaded-style \
             refresh is the confirmation, same as Command::Rollback"
        );
    }

    #[tokio::test]
    async fn apply_oauth_result_reports_a_failed_login() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_store = Some(
            ConfigStore::open(dir.path().join("profiles.json"))
                .await
                .expect("open store"),
        );
        let (events, mut events_rx) = unbounded_channel::<EngineEvent>();

        let saved = apply_oauth_result(
            &config_store,
            &events,
            OAuthProvider::Slack,
            Err("access_denied".to_owned()),
        )
        .await;

        assert!(!saved);
        let message = match events_rx.recv().await {
            Some(EngineEvent::Failed {
                session: None,
                message,
            }) => message,
            other => panic!("expected a global Failed event, got {other:?}"),
        };
        assert!(message.contains("Slack login failed"), "got: {message}");
        assert!(message.contains("access_denied"), "got: {message}");
    }

    /// `integration_tools` itself, with no engine or network involved at
    /// all — each credential's tools appear only when that credential is
    /// actually set, and under the expected prefixed names. `github_use_gh_cli`
    /// stays false throughout, so this only exercises the plain-token path;
    /// see `resolve_github_token_uses_gh_auth_token_when_gh_cli_is_enabled`
    /// and its sibling for the gh-CLI path.
    #[tokio::test]
    async fn integration_tools_registers_only_whats_configured() {
        let (events, _events_rx) = unbounded_channel::<EngineEvent>();

        let none =
            integration_tools(&architect_config::IntegrationsConfig::default(), &events).await;
        assert!(none.is_empty());

        let github_only = integration_tools(
            &architect_config::IntegrationsConfig {
                github_token: Some("t".into()),
                ..Default::default()
            },
            &events,
        )
        .await;
        let names: Vec<&str> = github_only.iter().map(|tool| tool.name()).collect();
        assert_eq!(
            names,
            [
                "github_read_pull_request",
                "github_read_pull_request_comments",
                "github_read_pull_request_diff",
                "github_read_pull_request_commits",
                "github_read_issue",
                "github_read_issue_comments",
                "github_read_file",
                "github_list_directory",
            ]
        );

        let all = integration_tools(
            &architect_config::IntegrationsConfig {
                github_token: Some("t".into()),
                github_use_gh_cli: false,
                slack_token: Some("t".into()),
                linear_api_key: Some("t".into()),
            },
            &events,
        )
        .await;
        let names: Vec<&str> = all.iter().map(|tool| tool.name()).collect();
        assert_eq!(
            names,
            [
                "github_read_pull_request",
                "github_read_pull_request_comments",
                "github_read_pull_request_diff",
                "github_read_pull_request_commits",
                "github_read_issue",
                "github_read_issue_comments",
                "github_read_file",
                "github_list_directory",
                "slack_read_thread",
                "slack_list_channels",
                "slack_read_channel_history",
                "linear_read_ticket",
            ]
        );
    }

    #[test]
    fn system_prompt_for_appends_the_docs_protocol_only_when_doc_tools_are_present() {
        let without = system_prompt_for("base", &[]);
        assert_eq!(without, "base");

        let dir = tempfile::tempdir().unwrap();
        let (events, _rx) = unbounded_channel::<EngineEvent>();
        let tools = doc_tools(
            dir.path(),
            &architect_config::DocsConfig::default(),
            &events,
        );
        let with = system_prompt_for("base", &tools);
        assert!(with.starts_with("base"));
        assert!(with.contains("search_docs"));
    }

    #[tokio::test]
    async fn doc_tools_registers_the_six_doc_tools_by_default() {
        let (events, _events_rx) = unbounded_channel::<EngineEvent>();
        let dir = tempfile::tempdir().unwrap();

        let tools = doc_tools(
            dir.path(),
            &architect_config::DocsConfig::default(),
            &events,
        );

        let mut names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "edit_doc",
                "list_docs",
                "read_doc",
                "scaffold_docs",
                "search_docs",
                "write_doc",
            ]
        );
        assert!(
            dir.path().join("docs").is_dir(),
            "defaults to <workspace_root>/docs"
        );
    }

    #[tokio::test]
    async fn doc_tools_is_empty_when_disabled() {
        let (events, _events_rx) = unbounded_channel::<EngineEvent>();
        let dir = tempfile::tempdir().unwrap();

        let tools = doc_tools(
            dir.path(),
            &architect_config::DocsConfig {
                enabled: false,
                ..Default::default()
            },
            &events,
        );

        assert!(tools.is_empty());
        assert!(!dir.path().join("docs").exists());
    }

    #[tokio::test]
    async fn doc_tools_respects_an_overridden_vault_path() {
        let (events, _events_rx) = unbounded_channel::<EngineEvent>();
        let workspace = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();

        doc_tools(
            workspace.path(),
            &architect_config::DocsConfig {
                enabled: true,
                driver: "obsidian".to_owned(),
                vault_path: Some(vault.path().display().to_string()),
            },
            &events,
        );

        assert!(vault.path().is_dir());
        assert!(!workspace.path().join("docs").exists());
    }

    #[test]
    fn read_only_doc_tools_keeps_only_the_three_read_only_ones() {
        let dir = tempfile::tempdir().unwrap();
        let (events, _rx) = unbounded_channel::<EngineEvent>();
        let tools = doc_tools(
            dir.path(),
            &architect_config::DocsConfig::default(),
            &events,
        );

        let mut names: Vec<&str> = read_only_doc_tools(&tools)
            .iter()
            .map(|tool| tool.name())
            .collect();
        names.sort_unstable();

        assert_eq!(names, ["list_docs", "read_doc", "search_docs"]);
    }

    #[test]
    fn read_only_doc_tools_is_empty_when_docs_are_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let (events, _rx) = unbounded_channel::<EngineEvent>();
        let tools = doc_tools(
            dir.path(),
            &architect_config::DocsConfig {
                enabled: false,
                ..Default::default()
            },
            &events,
        );

        assert!(read_only_doc_tools(&tools).is_empty());
    }

    /// A fake `gh` — a shell script written to a tempdir for the duration
    /// of the test — rather than mutating `$PATH` (this suite runs tests
    /// in parallel, so a process-wide env mutation would race with other
    /// tests). `run_gh_auth_token` takes the command by path, so this
    /// never touches whatever real `gh` this machine may or may not have.
    fn fake_gh_script(dir: &std::path::Path, body: &str) -> PathBuf {
        use std::{fs, os::unix::fs::PermissionsExt};

        let path = dir.join("gh");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake gh script");
        let mut permissions = fs::metadata(&path).expect("stat fake gh").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod fake gh");
        path
    }

    #[tokio::test]
    async fn run_gh_auth_token_returns_the_tokens_stdout_on_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gh = fake_gh_script(dir.path(), "echo gho_faketoken");

        let token = run_gh_auth_token(gh.to_str().expect("utf8 path"))
            .await
            .expect("gh auth token should succeed");

        assert_eq!(token.as_deref(), Some("gho_faketoken"));
    }

    #[tokio::test]
    async fn run_gh_auth_token_surfaces_a_failed_login_as_a_clear_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gh = fake_gh_script(dir.path(), "echo 'not logged in' 1>&2; exit 1");

        let error = run_gh_auth_token(gh.to_str().expect("utf8 path"))
            .await
            .expect_err("gh auth token should fail");

        assert!(error.contains("not logged in"), "got: {error}");
    }

    /// `ProcessRegistry` → `EngineEvent::Process` → `Transcript::apply`,
    /// wired the exact way `worker()`'s `process_rx` arm does — the one
    /// slice of the real pipeline the ignored live test above this can't
    /// exercise without a real model. Real process (a real `sleep 30`,
    /// real `Notify`-based kill), no LLM: this is what actually proves a
    /// stopped process stops showing as running in the UI, not just that
    /// the registry's own internal state flips (already covered in
    /// `architect_tools::process`'s own tests).
    #[tokio::test]
    async fn a_stopped_process_stops_showing_as_running_in_the_transcript() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (registry, mut process_rx) = architect_tools::ProcessRegistry::new();
        let registry = Arc::new(registry);
        let mut transcript = Transcript::default();

        let id = registry
            .start(dir.path(), "sleep 30".to_owned())
            .await
            .expect("spawn");

        // Drain exactly like `worker()`'s `process_rx` arm does, until the
        // Started event lands — proves the registry→event leg works before
        // moving on to stop.
        loop {
            let event = process_rx.recv().await.expect("channel open");
            transcript.apply(&EngineEvent::Process(event.clone()));
            if matches!(event, architect_tools::ProcessEvent::Started { .. }) {
                break;
            }
        }
        assert_eq!(
            transcript.processes[0].status,
            architect_tools::ProcessStatus::Running
        );

        registry.stop(&id).await.expect("stop");

        let started = std::time::Instant::now();
        loop {
            let event = process_rx.recv().await.expect("channel open");
            transcript.apply(&EngineEvent::Process(event));
            if transcript.processes[0].status != architect_tools::ProcessStatus::Running {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the transcript should reflect the stop almost instantly, \
                 not anywhere close to the process's own 30s sleep"
            );
        }

        assert_eq!(
            transcript.processes[0].status,
            architect_tools::ProcessStatus::Stopped
        );
    }

    /// Deleting a session — needs no network: two sessions are seeded
    /// directly through `SessionStore`, not via a real turn. Covers both
    /// halves of `Command::DeleteSession`: deleting a session that isn't on
    /// screen only trims the sidebar, deleting the one that is also clears
    /// it (`EngineEvent::SessionDeleted`, which `Transcript::apply` reacts
    /// to). Hermetic, no `#[ignore]`.
    #[tokio::test]
    async fn deleting_a_session_updates_the_list_and_reports_if_it_was_active() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");

        let store = SessionStore::open(workspace.path())
            .await
            .expect("open store");
        let first = store.create_session("openai", "m").await.unwrap();
        store
            .append_message(first, 0, &Message::user("hello"))
            .await
            .unwrap();
        let second = store.create_session("openai", "m").await.unwrap();
        store
            .append_message(second, 0, &Message::user("hi"))
            .await
            .unwrap();
        drop(store);

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        // Which one resumes is whichever `updated_at` sorts first — not
        // asserted here, since both were touched within the same test and
        // SQLite's `datetime('now')` only has second resolution, so the two
        // can tie. Either is a valid "most recent"; what matters below is
        // only that deleting the *other* one behaves differently from
        // deleting the one actually on screen.
        let active_at_start = loop {
            match events.recv().await {
                Some(EngineEvent::HistoryLoaded { session, .. }) => break session,
                Some(EngineEvent::Failed { message, .. }) => panic!("startup failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before startup finished"),
            }
        };
        let inactive = if active_at_start == first {
            second
        } else {
            first
        };

        // Deleting the session that is *not* on screen must not report a
        // `SessionDeleted` the UI would mistake for its own.
        engine.delete_session(inactive);
        let deleted = loop {
            match events.recv().await {
                Some(EngineEvent::SessionDeleted(id)) => break id,
                Some(EngineEvent::Failed { message, .. }) => panic!("deletion failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before deletion was reported"),
            }
        };
        assert_eq!(deleted, inactive);
        let sessions = loop {
            match events.recv().await {
                Some(EngineEvent::SessionsListed(list)) => break list,
                Some(_) => {}
                None => panic!("engine closed before the list was refreshed"),
            }
        };
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, active_at_start);

        // Deleting the *active* session must be reported too, so the UI
        // knows to clear whatever it's showing.
        engine.delete_session(active_at_start);
        let deleted = loop {
            match events.recv().await {
                Some(EngineEvent::SessionDeleted(id)) => break id,
                Some(EngineEvent::Failed { message, .. }) => panic!("deletion failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before deletion was reported"),
            }
        };
        assert_eq!(deleted, active_at_start);
        let sessions = loop {
            match events.recv().await {
                Some(EngineEvent::SessionsListed(list)) => break list,
                Some(_) => {}
                None => panic!("engine closed before the list was refreshed"),
            }
        };
        assert!(sessions.is_empty());
    }

    /// End-to-end proof of `Command::Compact`: a session seeded with a
    /// couple of messages, resumed, then compacted against a mocked model
    /// response — the summary must both come back on `EngineEvent::
    /// Compacted` and be what `SessionStore::load_messages` now finds,
    /// wholesale replacing what was seeded.
    #[tokio::test]
    async fn compacting_replaces_the_session_history_with_a_summary() {
        use architect_core::Role;

        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_reply("compact", "a tidy summary"), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let store = SessionStore::open(workspace.path())
            .await
            .expect("open store");
        let session = store.create_session("openai", "m").await.unwrap();
        store
            .append_message(session, 0, &Message::user("please add a login form"))
            .await
            .unwrap();
        store
            .append_message(session, 1, &Message::assistant("done, see login.rs"))
            .await
            .unwrap();
        drop(store);

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            base_url: Some(format!("{}/v1", server.uri())),
            ..EngineConfig::default()
        });
        let mut events = engine.take_events().expect("receiver");

        // Resume first, so `Command::Compact` finds a resident slot to read
        // history from — the same precondition `Command::Send` needs.
        loop {
            match events.recv().await {
                Some(EngineEvent::HistoryLoaded {
                    session: loaded, ..
                }) => {
                    assert_eq!(loaded, session);
                    break;
                }
                Some(EngineEvent::Failed { message, .. }) => panic!("startup failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before startup finished"),
            }
        }

        engine.compact(session);

        let summary = loop {
            match events.recv().await {
                Some(EngineEvent::Compacted {
                    session: got,
                    summary,
                }) => {
                    assert_eq!(got, session);
                    break summary;
                }
                Some(EngineEvent::Failed { message, .. }) => panic!("compact failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before compaction finished"),
            }
        };
        assert_eq!(summary, "a tidy summary");

        let store = SessionStore::open(workspace.path())
            .await
            .expect("reopen store");
        let messages = store.load_messages(session).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[0].text(), "a tidy summary");
    }

    /// No history yet for the session — refused the same way a `Rollback`
    /// or a second `Send` is, via `Failed`, rather than making a pointless
    /// (and misleading) request to summarize nothing.
    #[tokio::test]
    async fn compacting_a_session_with_no_history_is_refused() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let session = SessionId::new();
        engine.compact(session);

        let message = loop {
            match events.recv().await {
                Some(EngineEvent::Failed {
                    session: Some(id),
                    message,
                }) if id == session => break message,
                Some(_) => {}
                None => panic!("engine closed before refusing the compaction"),
            }
        };
        assert!(message.contains("nothing to compact"));
    }

    /// `Command::ListModels` end to end: a mocked `GET /v1/models` and the
    /// resulting `EngineEvent::ModelsListed` — not tied to any session or
    /// saved `Profile`, unlike every other command tested around it.
    #[tokio::test]
    async fn listing_models_reports_what_the_server_has_loaded() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"data":[{"id":"qwen/qwen3.8-27b"},{"id":"gpt-oss-20b"}]}"#,
                "application/json",
            ))
            .mount(&server)
            .await;

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let base_url = format!("{}/v1", server.uri());
        engine.list_models(base_url.clone(), None);

        let (got_base_url, models) = loop {
            match events.recv().await {
                Some(EngineEvent::ModelsListed { base_url, models }) => break (base_url, models),
                Some(EngineEvent::Failed { message, .. }) => {
                    panic!("listing models failed: {message}")
                }
                Some(_) => {}
                None => panic!("engine closed before the model list arrived"),
            }
        };
        assert_eq!(got_base_url, base_url);
        assert_eq!(models, ["qwen/qwen3.8-27b", "gpt-oss-20b"]);
    }

    /// `Command::UseAdHocModel` activates a discovered model the same way
    /// `ActivateProfile` does — the header should pick it up via
    /// `AdHocModelActivated` — but must never touch `ConfigStore`: this is
    /// the LM Studio page's whole reason for existing as a page separate
    /// from "API configurations" (see that page's module docs). No mocked
    /// server is needed — `ProviderRegistry::build` only constructs a
    /// client, it never dials out.
    #[tokio::test]
    async fn using_an_adhoc_model_activates_it_without_touching_saved_profiles() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        engine.use_ad_hoc_model("http://localhost:1234/v1", None, "gpt-oss-20b".to_owned());

        let model = loop {
            match events.recv().await {
                Some(EngineEvent::AdHocModelActivated { model }) => break model,
                Some(EngineEvent::Failed { message, .. }) => {
                    panic!("ad-hoc activation failed: {message}")
                }
                Some(_) => {}
                None => panic!("engine closed before activation was reported"),
            }
        };
        assert_eq!(model, "gpt-oss-20b");

        // Never persisted: `profiles.json` isn't even created by this —
        // only a saved profile's CRUD (`save_profile`/`activate_profile`/
        // etc.) ever writes it.
        assert!(
            !config_home.path().join("profiles.json").exists(),
            "an ad-hoc pick must never touch ConfigStore"
        );
    }

    /// An unreachable server is reported the same way any other config-
    /// level failure is — `Failed { session: None, .. }`, not a panic or a
    /// silently empty list.
    #[tokio::test]
    async fn listing_models_from_an_unreachable_server_is_reported() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        // Nothing listens on this port — a real dial failure, not a mock.
        engine.list_models("http://127.0.0.1:1", None);

        let message = loop {
            match events.recv().await {
                Some(EngineEvent::Failed {
                    session: None,
                    message,
                }) => break message,
                Some(EngineEvent::ModelsListed { .. }) => {
                    panic!("an unreachable server must not report a model list")
                }
                Some(_) => {}
                None => panic!("engine closed before reporting the failure"),
            }
        };
        assert!(!message.is_empty());
    }

    #[tokio::test]
    async fn resuming_a_session_also_loads_its_file_changes() {
        // Seeded directly through `SessionStore`, the same pattern
        // `deleting_a_session_updates_the_list_..` uses to seed sessions —
        // no live model needed to prove the resume path wires file changes
        // through, only that `SessionStore::load_file_changes` gets called
        // and reported.
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");

        let store = SessionStore::open(workspace.path())
            .await
            .expect("open store");
        let session = store.create_session("openai", "m").await.unwrap();
        store
            .append_message(session, 0, &Message::user("hello"))
            .await
            .unwrap();
        let path = workspace.path().join("a.txt");
        store
            .record_file_change(
                session,
                0,
                &FileChange {
                    file_path: path.clone(),
                    old_content: None,
                    new_content: "hi".into(),
                    tool_name: "write_file",
                },
            )
            .await
            .unwrap();
        drop(store);

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        loop {
            match events.recv().await {
                Some(EngineEvent::HistoryLoaded {
                    session: loaded, ..
                }) => {
                    assert_eq!(loaded, session);
                    break;
                }
                Some(EngineEvent::Failed { message, .. }) => panic!("startup failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before startup finished"),
            }
        }
        // Right after `HistoryLoaded`, not just eventually — the same
        // ordering `state.rs`'s reducer relies on.
        let changes = match events.recv().await {
            Some(EngineEvent::FileChangesLoaded {
                session: loaded,
                changes,
            }) => {
                assert_eq!(loaded, session);
                changes
            }
            other => panic!("expected FileChangesLoaded right after HistoryLoaded, got {other:?}"),
        };

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change.file_path, path);
        assert_eq!(changes[0].change.new_content, "hi");
    }

    /// The `Plan` analog of `resuming_a_session_also_loads_its_file_
    /// changes` above — same seed-directly-through-`SessionStore`
    /// approach, no live model needed to prove the resume path wires the
    /// plan through.
    #[tokio::test]
    async fn resuming_a_session_also_loads_its_plan() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");

        let store = SessionStore::open(workspace.path())
            .await
            .expect("open store");
        let session = store.create_session("openai", "m").await.unwrap();
        store
            .append_message(session, 0, &Message::user("hello"))
            .await
            .unwrap();
        let plan = architect_core::Plan {
            goal: Some("Ship it".to_owned()),
            steps: vec![architect_core::PlanStep {
                description: "Write the code".to_owned(),
                status: architect_core::StepStatus::InProgress,
                substeps: vec![],
            }],
        };
        store.save_plan(session, &plan).await.unwrap();
        drop(store);

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        loop {
            match events.recv().await {
                Some(EngineEvent::HistoryLoaded {
                    session: loaded, ..
                }) => {
                    assert_eq!(loaded, session);
                    break;
                }
                Some(EngineEvent::Failed { message, .. }) => panic!("startup failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before startup finished"),
            }
        }
        // `FileChangesLoaded` always fires next (see the test above) —
        // drain it before asserting on `PlanLoaded` right after it.
        match events.recv().await {
            Some(EngineEvent::FileChangesLoaded { .. }) => {}
            other => panic!("expected FileChangesLoaded, got {other:?}"),
        }
        let loaded_plan = match events.recv().await {
            Some(EngineEvent::PlanLoaded {
                session: loaded,
                plan,
            }) => {
                assert_eq!(loaded, session);
                plan
            }
            other => panic!("expected PlanLoaded right after FileChangesLoaded, got {other:?}"),
        };

        assert_eq!(loaded_plan, Some(plan));
    }

    #[tokio::test]
    async fn rolling_back_restores_the_file_and_shrinks_the_list() {
        // Two edits to the same file, seeded directly through `SessionStore`
        // (no live model needed — rolling back is a DB/filesystem operation,
        // identical regardless of how the change got there).
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");

        let store = SessionStore::open(workspace.path())
            .await
            .expect("open store");
        let session = store.create_session("openai", "m").await.unwrap();
        let path = workspace.path().join("a.txt");

        store
            .append_message(session, 0, &Message::user("write it"))
            .await
            .unwrap();
        store
            .record_file_change(
                session,
                0,
                &FileChange {
                    file_path: path.clone(),
                    old_content: None,
                    new_content: "first".into(),
                    tool_name: "write_file",
                },
            )
            .await
            .unwrap();
        store
            .append_message(session, 1, &Message::user("edit it"))
            .await
            .unwrap();
        store
            .record_file_change(
                session,
                1,
                &FileChange {
                    file_path: path.clone(),
                    old_content: Some("first".into()),
                    new_content: "second".into(),
                    tool_name: "edit_file",
                },
            )
            .await
            .unwrap();
        // The DB records match a file that's really on disk as "second" —
        // what the two changes above claim actually happened.
        std::fs::write(&path, "second").unwrap();
        drop(store);

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        loop {
            match events.recv().await {
                Some(EngineEvent::HistoryLoaded { .. }) => break,
                Some(EngineEvent::Failed { message, .. }) => panic!("startup failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before startup finished"),
            }
        }
        loop {
            match events.recv().await {
                Some(EngineEvent::FileChangesLoaded { .. }) => break,
                Some(EngineEvent::Failed { message, .. }) => panic!("startup failed: {message}"),
                None => panic!("engine closed before startup finished"),
                Some(_) => {}
            }
        }

        // Roll back to right before the second edit (message_seq 1) —
        // undoing it and restoring the first edit's content.
        engine.rollback(session, 0);

        let changes = loop {
            match events.recv().await {
                Some(EngineEvent::FileChangesLoaded {
                    session: loaded,
                    changes,
                }) => {
                    assert_eq!(loaded, session);
                    break changes;
                }
                Some(EngineEvent::Failed { message, .. }) => panic!("rollback failed: {message}"),
                None => panic!("engine closed before rollback was reported"),
                Some(_) => {}
            }
        };

        assert_eq!(changes.len(), 1, "the second edit's row should be gone");
        assert_eq!(changes[0].change.new_content, "first");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
    }

    #[tokio::test]
    async fn rollback_is_refused_while_a_turn_is_running() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                // Never resolves within this test — just needs the turn to
                // still be "running" when `Command::Rollback` arrives.
                // `IterationStarted` (waited on below) fires before this
                // response is even awaited, so the delay never blocks the
                // test itself.
                ResponseTemplate::new(200).set_delay(Duration::from_secs(60)),
            )
            .mount(&server)
            .await;

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            base_url: Some(format!("{}/v1", server.uri())),
            ..EngineConfig::default()
        });
        let mut events = engine.take_events().expect("receiver");

        let session = SessionId::new();
        engine.send(session, "hello", Vec::new());
        // Wait for the turn to actually be running, not just sent — the
        // engine only starts treating it as "running" once `Send` has been
        // processed.
        loop {
            match events.recv().await {
                Some(EngineEvent::Agent {
                    event: AgentEvent::IterationStarted { .. },
                    ..
                }) => break,
                Some(EngineEvent::Failed { message, .. }) => panic!("turn failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before the turn started"),
            }
        }

        engine.rollback(session, 0);

        let message = loop {
            match events.recv().await {
                Some(EngineEvent::Failed {
                    session: Some(failed),
                    message,
                }) if failed == session => break message,
                Some(_) => {}
                None => panic!("engine closed before the rollback was refused"),
            }
        };
        assert!(message.contains("already running"));

        engine.cancel(session);
    }

    /// Build an SSE body from raw `data:` payloads, terminated the way the
    /// dialect terminates: an explicit `[DONE]`.
    fn sse(chunks: &[String]) -> String {
        let mut body: String = chunks
            .iter()
            .map(|chunk| format!("data: {chunk}\n\n"))
            .collect();
        body.push_str("data: [DONE]\n\n");
        body
    }

    /// A minimal but complete OpenAI-compatible streamed reply: one content
    /// delta, a `stop` finish reason, and a trailing usage-only chunk.
    fn sse_reply(id: &str, text: &str) -> String {
        sse(&[
            format!(
                r#"{{"id":"{id}","choices":[{{"delta":{{"role":"assistant","content":"{text}"}}}}]}}"#
            ),
            format!(r#"{{"id":"{id}","choices":[{{"delta":{{}},"finish_reason":"stop"}}]}}"#),
            format!(
                r#"{{"id":"{id}","choices":[],"usage":{{"prompt_tokens":5,"completion_tokens":2}}}}"#
            ),
        ])
    }

    /// Same shape as [`sse_reply`], but a single complete tool call instead
    /// of text — `arguments_json` is embedded as-is, so it must already be
    /// valid, escaped JSON-inside-a-JSON-string (no fragmenting across
    /// chunks, unlike `architect-llm`'s own wire tests, which is unnecessary
    /// complexity here since nothing in this file is testing the streaming
    /// parser itself).
    fn sse_tool_call(id: &str, call_id: &str, tool_name: &str, arguments_json: &str) -> String {
        let escaped_arguments = arguments_json.replace('\\', "\\\\").replace('"', "\\\"");
        sse(&[
            format!(
                r#"{{"id":"{id}","choices":[{{"delta":{{"tool_calls":[{{"index":0,"id":"{call_id}","function":{{"name":"{tool_name}","arguments":"{escaped_arguments}"}}}}]}}}}]}}"#
            ),
            format!(r#"{{"id":"{id}","choices":[{{"delta":{{}},"finish_reason":"tool_calls"}}]}}"#),
            format!(
                r#"{{"id":"{id}","choices":[],"usage":{{"prompt_tokens":5,"completion_tokens":2}}}}"#
            ),
        ])
    }

    /// End to end: `Command::Send`'s `images` both reach the provider (as
    /// this dialect's `image_url` part) and get persisted (as a
    /// `ContentBlock::Image`, base64-encoded) — the two edges `Attachment`'s
    /// own doc comment describes.
    #[tokio::test]
    async fn an_attached_image_is_sent_and_persisted() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_reply("c", "a cat"), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            base_url: Some(format!("{}/v1", server.uri())),
            ..EngineConfig::default()
        });
        let mut events = engine.take_events().expect("receiver");

        let session = SessionId::new();
        engine.send(
            session,
            "what is this?",
            vec![Attachment {
                media_type: "image/png".into(),
                bytes: b"hello".to_vec(),
            }],
        );

        loop {
            match events.recv().await {
                Some(EngineEvent::Agent {
                    event: AgentEvent::TurnCompleted { .. },
                    ..
                }) => break,
                Some(EngineEvent::Failed { message, .. }) => panic!("turn failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before the turn finished"),
            }
        }

        let requests = server.received_requests().await.expect("recorded requests");
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).expect("json body");
        // `messages[0]` is the system prompt `EngineConfig::default()` sets;
        // the user turn with the image is the one after it.
        let content = body["messages"][1]["content"]
            .as_array()
            .expect("array content once an image is attached");
        assert_eq!(content[0]["type"], serde_json::json!("image_url"));
        assert_eq!(
            content[0]["image_url"]["url"],
            serde_json::json!(format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode("hello")
            ))
        );

        let store = SessionStore::open(workspace.path())
            .await
            .expect("reopen store");
        let messages = store.load_messages(session).await.unwrap();
        let images: Vec<(String, String)> = messages[0]
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Image { media_type, data } => {
                    Some((media_type.clone(), data.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].0, "image/png");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&images[0].1)
                .unwrap(),
            b"hello"
        );
    }

    /// A complete `spawn_subagents` round trip, entirely hermetic (a mock
    /// server plays all three of a parent-calls-spawn_subagents-then-a-
    /// child-runs-then-the-parent-continues turn's model responses) — proves
    /// the child session is created with the right `parent_id`, shows up in
    /// `SessionsListed` promptly, and that its final text really does make
    /// it back into the parent's own tool result (proven indirectly: the
    /// parent's own final answer is mocked to only appear once it has *seen*
    /// the child's exact summary text in its own request body).
    #[tokio::test]
    async fn spawn_subagents_creates_a_child_session_and_reports_its_summary_back() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(workspace.path().join("docs")).expect("docs dir");
        let server = MockServer::start().await;

        // 1. The parent's first call: decides to call `spawn_subagents`.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains(
                "Please investigate the docs directory",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                sse_tool_call(
                    "p1",
                    "call_a",
                    "spawn_subagents",
                    r#"{"tasks":[{"prompt":"Look at the docs and summarize","path":"docs"}]}"#,
                ),
                "text/event-stream",
            ))
            // Bounded so this doesn't also (mis)match the parent's *second*
            // call below, whose body still contains this same original
            // prompt text as part of the conversation history.
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // 2. The child's own, isolated first call. Bounded for the same
        // reason as (1) above: the parent's *second* call also carries this
        // same text, now embedded inside its previous tool call's
        // arguments — without a limit this mock would just as happily
        // answer that request too. Also asserts the child's own tool
        // schema includes `search_docs` — proof the read-only doc tools
        // actually reached the sub-agent's `Investigation` registry, not
        // just that `read_only_doc_tools` computes the right list in
        // isolation.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("Look at the docs and summarize"))
            .and(body_string_contains("search_docs"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_reply("c1", "The docs describe X."), "text/event-stream"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // 3. The parent's second call, now holding the child's summary in
        // its own tool-result content — only matches once that text is
        // actually present, which is exactly what proves it made the trip.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("The docs describe X."))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                sse_reply("p2", "Investigation complete."),
                "text/event-stream",
            ))
            .mount(&server)
            .await;

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            base_url: Some(format!("{}/v1", server.uri())),
            ..EngineConfig::default()
        });
        let mut events = engine.take_events().expect("receiver");

        let parent = SessionId::new();
        engine.send(
            parent,
            "Please investigate the docs directory using spawn_subagents.",
            Vec::new(),
        );

        let mut child = None;
        let mut parent_turn_completed = false;
        while !parent_turn_completed || child.is_none() {
            match events.recv().await {
                Some(EngineEvent::SessionsListed(sessions)) => {
                    if let Some(found) = sessions
                        .iter()
                        .find(|session| session.parent_id == Some(parent))
                    {
                        child = Some(found.id);
                    }
                }
                Some(EngineEvent::Agent {
                    session,
                    event: AgentEvent::TurnCompleted { .. },
                }) if session == parent => parent_turn_completed = true,
                Some(EngineEvent::Failed { message, .. }) => panic!("turn failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before the round trip finished"),
            }
        }
        let child = child.expect("a child session should have been listed");

        // Give the worker's post-turn persistence a moment to run.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let store = SessionStore::open(workspace.path())
            .await
            .expect("reopen store");
        let sessions = store.list_sessions().await.unwrap();
        let child_row = sessions
            .iter()
            .find(|session| session.id == child)
            .expect("the child session should be persisted");
        assert_eq!(child_row.parent_id, Some(parent));

        let parent_messages = store.load_messages(parent).await.unwrap();
        assert!(
            parent_messages
                .last()
                .map(Message::text)
                .unwrap_or_default()
                .contains("Investigation complete"),
            "the parent's final answer should reflect having seen the child's summary"
        );
    }

    /// `write_plan` used to only reach the Inspector's Plan tab once the
    /// *entire* turn finished — `plan_rx` was drained in one batch after
    /// `run_turn` returned. Proves that regression stays fixed: a plan
    /// saved partway through a two-iteration turn shows up as its own
    /// `PlanUpdated` event before that turn's `TurnCompleted`, not bundled
    /// in afterward.
    #[tokio::test]
    async fn write_plan_reaches_the_inspector_before_the_turn_finishes() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let server = MockServer::start().await;

        // 1. Saves a plan, then asks for another iteration rather than
        // ending here — if `PlanUpdated` only ever arrived at the very end,
        // this second round-trip is what would swallow it.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("please plan first"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                sse_tool_call(
                    "p1",
                    "call_a",
                    "write_plan",
                    r#"{"steps":[{"description":"step one"}]}"#,
                ),
                "text/event-stream",
            ))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // 2. The turn's actual final answer.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("step one"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_reply("p2", "Done."), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            base_url: Some(format!("{}/v1", server.uri())),
            ..EngineConfig::default()
        });
        let mut events = engine.take_events().expect("receiver");

        let session = SessionId::new();
        engine.send(session, "please plan first, then continue.", Vec::new());

        let mut plan_seen_before_completion = false;
        let mut turn_completed = false;
        while !turn_completed {
            match events.recv().await.expect("engine closed mid-turn") {
                EngineEvent::PlanUpdated { .. } => plan_seen_before_completion = true,
                EngineEvent::Agent {
                    event: AgentEvent::TurnCompleted { .. },
                    ..
                } => turn_completed = true,
                EngineEvent::Failed { message, .. } => panic!("turn failed: {message}"),
                _ => {}
            }
        }

        assert!(
            plan_seen_before_completion,
            "expected PlanUpdated to arrive before the turn's TurnCompleted"
        );
    }

    /// The hermetic test above proves the CRUD plumbing; this proves
    /// activating a profile actually redirects real network calls, not just
    /// a `Provider` object in memory. Starts pointed at a port nothing
    /// listens on, saves and activates a profile pointed at the real local
    /// server, and only then sends — the turn must succeed, which it only
    /// can if the activated profile, not the broken startup config, is what
    /// `Command::Send` actually used.
    #[tokio::test]
    #[ignore = "requires a local OpenAI-compatible server"]
    async fn activating_a_saved_profile_is_used_for_the_next_turn() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            base_url: Some("http://localhost:1/v1".into()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let profile = Profile::new(
            "Real server",
            "openai",
            std::env::var("ARCHITECT_LIVE_MODEL").unwrap_or_else(|_| "qwen/qwen3.8-27b".into()),
        )
        .base_url(
            std::env::var("ARCHITECT_LIVE_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:1234/v1".into()),
        );
        engine.save_profile(profile.clone());
        engine.activate_profile(profile.id);

        loop {
            match events.recv().await {
                Some(EngineEvent::ProfilesListed { active, .. }) if active == Some(profile.id) => {
                    break;
                }
                Some(EngineEvent::Failed { message, .. }) => panic!("activation failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before activation was reported"),
            }
        }

        let session = SessionId::new();
        engine.send(session, "Say hello in exactly three words.", Vec::new());

        loop {
            match events.recv().await {
                Some(EngineEvent::Agent {
                    session: id,
                    event: AgentEvent::TurnCompleted { .. },
                }) if id == session => break,
                Some(EngineEvent::Failed { message, .. }) => panic!(
                    "the turn used the broken startup config, not the activated profile: {message}"
                ),
                Some(_) => {}
                None => panic!("engine closed before the turn finished"),
            }
        }
    }

    /// The centerpiece of the MCP client feature, against a real server and
    /// a real model: a saved, enabled MCP server's tool must actually show
    /// up in a turn's tool set and get called through it — not just
    /// discovered and left unused. `@modelcontextprotocol/server-everything`
    /// is the official demo/test server (self-installs via `npx` on first
    /// use); its `get-sum` tool is asked for by name so a model that might
    /// otherwise just answer from arithmetic knowledge has no reason not to
    /// call it.
    #[tokio::test]
    #[ignore = "requires a local OpenAI-compatible server with a tool-capable model, and npx"]
    async fn a_saved_mcp_servers_tool_is_available_and_gets_called_in_a_real_turn() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            system: "When asked to add two numbers, always use the get-sum tool rather than \
                     computing it yourself."
                .into(),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let server = architect_config::McpServerConfig::stdio("Everything", "npx")
            .args(["-y", "@modelcontextprotocol/server-everything"]);
        engine.save_mcp_server(server.clone());

        // No `Failed` must arrive before the server list settles — a real
        // connection failure (server not installed, `npx` missing) should
        // fail this test loudly here rather than time out later waiting for
        // a tool call that can never happen.
        loop {
            match events.recv().await {
                Some(EngineEvent::McpServersListed(servers)) if !servers.is_empty() => break,
                Some(EngineEvent::Failed { message, .. }) => {
                    panic!("the MCP server failed to connect: {message}")
                }
                Some(_) => {}
                None => panic!("engine closed before the server was saved"),
            }
        }

        let session = SessionId::new();
        engine.send(session, "What is 5 + 3? Use your tools.", Vec::new());

        let mut called_get_sum = false;
        loop {
            match events.recv().await {
                Some(EngineEvent::Agent {
                    session: id,
                    event: AgentEvent::ToolStarted { call },
                }) if id == session && call.name == "get-sum" => {
                    called_get_sum = true;
                }
                Some(EngineEvent::Agent {
                    session: id,
                    event: AgentEvent::TurnCompleted { .. },
                }) if id == session => break,
                Some(EngineEvent::Failed { message, .. }) => panic!("turn failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before the turn finished"),
            }
        }

        assert!(
            called_get_sum,
            "the model should have called the MCP server's get-sum tool"
        );
    }

    /// Process tools are always registered (no saved credential needed,
    /// unlike MCP/GitHub/Slack/Linear) — this proves `start_process`
    /// actually reaches a real turn's tool set, and that starting one is
    /// reported via a real `EngineEvent::Process`, not just built and
    /// forgotten. The hermetic tests in `architect_tools::process` already
    /// cover the registry/tool logic itself in isolation; this is the one
    /// thing only a real turn can prove — that `worker()`'s `process_rx`
    /// arm actually forwards what the registry sends.
    #[tokio::test]
    #[ignore = "requires a local OpenAI-compatible server with a tool-capable model"]
    async fn a_process_started_in_a_real_turn_reports_a_process_event() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            system: "When asked to start a background process, always use the start_process \
                     tool rather than run_command."
                .into(),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let session = SessionId::new();
        engine.send(
            session,
            "Start a background process running `echo hello` and tell me its id.",
            Vec::new(),
        );

        let mut saw_started = false;
        loop {
            match events.recv().await {
                Some(EngineEvent::Process(architect_tools::ProcessEvent::Started { .. })) => {
                    saw_started = true;
                }
                Some(EngineEvent::Agent {
                    session: id,
                    event: AgentEvent::TurnCompleted { .. },
                }) if id == session => break,
                Some(EngineEvent::Failed { message, .. }) => panic!("turn failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before the turn finished"),
            }
        }

        assert!(
            saw_started,
            "the model should have started a process, reported via EngineEvent::Process"
        );
    }

    /// The whole desktop pipeline except the pixels: engine thread, provider,
    /// agent loop, event channel, and the transcript reducer. Runs in a
    /// tempdir workspace, not the real repository — every engine now opens
    /// `.coder/` and a tool sandbox unconditionally, so a real workspace root
    /// would leave a stray `.coder/` behind in this project.
    #[tokio::test]
    #[ignore = "requires a local OpenAI-compatible server"]
    async fn a_real_turn_reaches_the_transcript() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");
        let mut transcript = Transcript::default();

        let session = SessionId::new();
        transcript.push_user(session, "Say hello in exactly three words.", Vec::new());
        engine.send(session, "Say hello in exactly three words.", Vec::new());

        while let Some(event) = events.recv().await {
            let finished = matches!(
                &event,
                EngineEvent::Agent {
                    event: AgentEvent::TurnCompleted { .. },
                    ..
                } | EngineEvent::Failed { .. }
            );
            transcript.apply(&event);
            if finished {
                break;
            }
        }

        let conversation = transcript
            .conversation(session)
            .expect("the session's own conversation");
        println!(
            "status: {:?}\nrows: {:#?}\nusage: {:?}",
            conversation.status, conversation.rows, conversation.usage
        );

        assert_eq!(
            conversation.status,
            crate::state::Status::Idle,
            "the turn should have completed cleanly"
        );
        assert!(
            conversation.rows.iter().any(
                |row| matches!(row, crate::state::Row::Assistant { text, .. } if !text.is_empty())
            ),
            "the assistant's streamed text should have landed in a row"
        );
        assert!(
            conversation.usage.output_tokens > 0,
            "usage should be recorded"
        );
    }

    /// A real turn that writes a file: the write must land on disk, and the
    /// turn, its messages, and the file change must all be durable in
    /// `.coder/sessions.db` afterward.
    #[tokio::test]
    #[ignore = "requires a local OpenAI-compatible server with a tool-capable model"]
    async fn a_real_write_is_persisted() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            system: "Use the write_file tool when asked to create a file. Then answer briefly."
                .into(),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let session = SessionId::new();
        engine.send(
            session,
            "Create a file named greeting.txt containing exactly: hello",
            Vec::new(),
        );

        // `FileChanged` is sent from the post-turn persistence step, which
        // runs after `TurnCompleted` has already gone out — order between
        // the two is not asserted, only that both eventually arrive, so a
        // single-event catch-all loop can't silently swallow whichever one
        // shows up first.
        let mut turn_completed = false;
        let mut file_changed = None;
        loop {
            match events.recv().await {
                Some(EngineEvent::Agent {
                    event: AgentEvent::TurnCompleted { .. },
                    ..
                }) => {
                    turn_completed = true;
                    if file_changed.is_some() {
                        break;
                    }
                }
                Some(EngineEvent::FileChanged { entry, .. }) => {
                    file_changed = Some(entry);
                    if turn_completed {
                        break;
                    }
                }
                Some(EngineEvent::Failed { message, .. }) => panic!("turn failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before the turn finished"),
            }
        }
        let file_changed = file_changed.expect("a FileChanged event during the turn");
        assert_eq!(file_changed.change.tool_name, "write_file");
        assert_eq!(file_changed.change.new_content.trim(), "hello");

        let written = std::fs::read_to_string(workspace.path().join("greeting.txt"))
            .expect("write_file should have created greeting.txt");
        assert_eq!(written.trim(), "hello");

        // Give the worker's post-turn persistence a moment to run; it happens
        // after `TurnCompleted` is sent, not before.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let db_path = workspace.path().join(".coder/sessions.db");
        let conn = rusqlite::Connection::open(&db_path).expect("sessions.db should exist");

        let sessions: i64 = conn
            .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sessions, 1);

        let messages: i64 = conn
            .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert!(
            messages >= 2,
            "at least the user message and one reply, got {messages}"
        );

        let (file_changes, tool_name): (i64, String) = conn
            .query_row(
                "SELECT count(*), max(tool_name) FROM file_changes",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(file_changes, 1);
        assert_eq!(tool_name, "write_file");
    }

    /// Closing the app and reopening it against the same workspace must not
    /// lose the conversation — this reproduces exactly that: a second, fully
    /// independent `Engine` against the same workspace root, with nothing
    /// carried over in memory from the first.
    #[tokio::test]
    #[ignore = "requires a local OpenAI-compatible server"]
    async fn reopening_the_app_resumes_the_last_session() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let config = || EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        };

        // First "session": send a message, wait for the reply, then drop the
        // engine — nothing survives this but what got written to disk.
        {
            let engine = Engine::start(config());
            let mut events = engine.take_events().expect("receiver");

            let session = SessionId::new();
            engine.send(
                session,
                "Remember this exact phrase: purple lighthouse.",
                Vec::new(),
            );

            loop {
                match events.recv().await {
                    Some(EngineEvent::Agent {
                        event: AgentEvent::TurnCompleted { .. },
                        ..
                    }) => break,
                    Some(EngineEvent::Failed { message, .. }) => panic!("turn failed: {message}"),
                    Some(_) => {}
                    None => panic!("engine closed before the turn finished"),
                }
            }
            // Give post-turn persistence a moment before the engine is dropped.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        // "Reopen": a brand new engine, same workspace, nothing shared.
        let engine = Engine::start(config());
        let mut events = engine.take_events().expect("receiver");

        let (session, history) = loop {
            match events.recv().await {
                Some(EngineEvent::HistoryLoaded { session, messages }) => {
                    break (session, messages);
                }
                Some(EngineEvent::Failed { message, .. }) => panic!("startup failed: {message}"),
                Some(_) => {}
                None => panic!("no history arrived before the engine closed"),
            }
        };

        assert!(
            history
                .iter()
                .any(|message| message.text().contains("purple lighthouse")),
            "the resumed history should contain the earlier turn: {history:?}"
        );

        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::HistoryLoaded {
            session,
            messages: history,
        });
        let conversation = transcript
            .conversation(session)
            .expect("resumed conversation");
        assert_eq!(
            conversation.status,
            crate::state::Status::Idle,
            "a resumed session must not look mid-turn"
        );
        assert!(!conversation.rows.is_empty());
        assert_eq!(transcript.active_session, Some(session));
    }

    /// Switching to an older session — the sidebar's `Command::LoadSession`
    /// path — must bring back *its own* content, not another session's, and
    /// must actually hit the store rather than silently no-op.
    ///
    /// This deliberately uses a second, fresh `Engine`, not a second send on
    /// the same one: `Command::LoadSession` no-ops when a session is already
    /// resident in the worker's own `sessions` map (by design — see
    /// `worker`'s doc comment — a session created via `Send` never leaves
    /// that map for the life of the engine, so re-fetching it would be both
    /// wasteful and risk clobbering live state). A single continuous engine
    /// can therefore never actually exercise the fetch path for a session it
    /// created itself; only a *different* engine instance, with an empty
    /// `sessions` map, can. (An earlier version of this test sent both
    /// messages through one engine and then waited on `HistoryLoaded` for a
    /// `LoadSession` that was a guaranteed no-op — it hung forever, which is
    /// what caught this in the first place.)
    #[tokio::test]
    #[ignore = "requires a local OpenAI-compatible server"]
    async fn switching_back_to_an_older_session_restores_its_own_history() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let config = || EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        };

        async fn drain_turn(events: &mut UnboundedReceiver<EngineEvent>) {
            loop {
                match events.recv().await {
                    Some(EngineEvent::Agent {
                        event: AgentEvent::TurnCompleted { .. },
                        ..
                    }) => return,
                    Some(EngineEvent::Failed { message, .. }) => panic!("turn failed: {message}"),
                    Some(_) => {}
                    None => panic!("engine closed before the turn finished"),
                }
            }
        }

        let first_session = SessionId::new();
        let second_session = SessionId::new();

        {
            let engine = Engine::start(config());
            let mut events = engine.take_events().expect("receiver");

            engine.send(
                first_session,
                "Remember this exact phrase: crimson lantern.",
                Vec::new(),
            );
            drain_turn(&mut events).await;
            engine.send(
                second_session,
                "Remember this exact phrase: golden anchor.",
                Vec::new(),
            );
            drain_turn(&mut events).await;
            // Give post-turn persistence a moment before the engine is dropped.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        // A brand new engine, same workspace, nothing shared — neither
        // session is resident in this one's `sessions` map yet.
        let engine = Engine::start(config());
        let mut events = engine.take_events().expect("receiver");

        // Startup resumes the most recently touched session on its own —
        // drain that before asking for the other one explicitly.
        loop {
            match events.recv().await {
                Some(EngineEvent::HistoryLoaded { .. }) => break,
                Some(EngineEvent::Failed { message, .. }) => panic!("startup failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before startup finished"),
            }
        }

        engine.load_session(first_session);
        let (session, history) = loop {
            match events.recv().await {
                Some(EngineEvent::HistoryLoaded { session, messages }) => {
                    break (session, messages);
                }
                Some(EngineEvent::Failed { message, .. }) => {
                    panic!("load_session failed: {message}")
                }
                _ => {}
            }
        };

        assert_eq!(session, first_session);
        assert!(
            history
                .iter()
                .any(|message| message.text().contains("crimson lantern")),
            "switching back should bring the first session's own content: {history:?}"
        );
        assert!(
            !history
                .iter()
                .any(|message| message.text().contains("golden anchor")),
            "the second session's content must not bleed into the first: {history:?}"
        );
    }

    /// The centerpiece of this feature, against a real model: two sessions
    /// sent to without waiting between them must both complete, and each
    /// must be persisted with only its own messages — proof concurrency
    /// holds up outside the mocked hermetic test too.
    #[tokio::test]
    #[ignore = "requires a local OpenAI-compatible server"]
    async fn two_sessions_sent_to_concurrently_both_complete_and_persist_separately() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let config_home = tempfile::tempdir().expect("tempdir");
        let engine = Engine::start(EngineConfig {
            workspace_root: workspace.path().to_owned(),
            config_dir: Some(config_home.path().to_owned()),
            ..EngineConfig::from_env()
        });
        let mut events = engine.take_events().expect("receiver");

        let a = SessionId::new();
        let b = SessionId::new();
        engine.send(a, "Remember this exact phrase: violet compass.", Vec::new());
        engine.send(b, "Remember this exact phrase: amber lantern.", Vec::new());

        let mut finished = std::collections::HashSet::new();
        while finished.len() < 2 {
            match events.recv().await {
                Some(EngineEvent::Agent {
                    session,
                    event: AgentEvent::TurnCompleted { .. },
                }) => {
                    finished.insert(session);
                }
                Some(EngineEvent::Failed { message, .. }) => panic!("turn failed: {message}"),
                Some(_) => {}
                None => panic!("engine closed before both turns finished"),
            }
        }
        assert_eq!(finished, [a, b].into_iter().collect());

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let db_path = workspace.path().join(".coder/sessions.db");
        let conn = rusqlite::Connection::open(&db_path).expect("sessions.db should exist");
        let sessions: i64 = conn
            .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sessions, 2);

        for (session, phrase) in [(a, "violet compass"), (b, "amber lantern")] {
            let mut statement = conn
                .prepare("SELECT content FROM messages WHERE session_id = ?1 ORDER BY seq")
                .unwrap();
            let contents: Vec<String> = statement
                .query_map([session.to_string()], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let joined = contents.join(" ");
            assert!(
                joined.contains(phrase),
                "session {session}'s own messages should contain {phrase:?}: {joined}"
            );
            let (other_session, other_phrase) = if session == a {
                (b, "amber lantern")
            } else {
                (a, "violet compass")
            };
            let _ = other_session;
            assert!(
                !joined.contains(other_phrase),
                "session {session}'s messages must not contain the other session's phrase: {joined}"
            );
        }
    }
}
