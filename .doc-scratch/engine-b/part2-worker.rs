async fn worker(
    config: EngineConfig,
    mut commands: UnboundedReceiver<Command>,
    events: UnboundedSender<EngineEvent>,
    mut shutdown_rx: UnboundedReceiver<std::sync::mpsc::Sender<()>>,
) {
    // Persistence is best-effort: a workspace that can't hold a `.coder/`
    // directory (read-only filesystem, permissions) still gets a working
    // chat, just not a saved one. Reported once, not on every message.
    let store = match SessionStore::open(&config.workspace_root).await {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::warn!(%error, "session persistence unavailable");
            let _ = events.send(EngineEvent::Failed {
                session: None,
                message: format!("persistence unavailable: {error}"),
            });
            None
        }
    };

    // Saved API configurations are best-effort too, and global to the
    // machine rather than this workspace — see `architect_config`'s docs for
    // why it's a separate store from `SessionStore` above.
    let config_store = match &config.config_dir {
        Some(dir) => ConfigStore::open(dir.join("profiles.json")).await,
        None => ConfigStore::open_default().await,
    };
    let config_store = match config_store {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::warn!(%error, "profile storage unavailable");
            None
        }
    };

    let mut profiles: Vec<Profile> = Vec::new();
    let mut active_profile: Option<Uuid> = None;
    // `config`'s own env/default-derived provider — unchanged from before
    // saved profiles existed. Kept around (immutable) so the settings
    // panel's "Default" row always has something to revert to via
    // `Command::DeactivateProfile`.
    let default_provider_config = ProviderConfig {
        kind: config.kind.clone(),
        base_url: config.base_url.clone(),
        api_key: config.api_key.clone(),
        extra_headers: Default::default(),
        model: Some(config.model.clone()),
    };
    let default_model = config.model.clone();

    let mut provider_config = default_provider_config.clone();
    let mut model = default_model.clone();

    if let Some(store) = &config_store {
        match store.list().await {
            Ok(list) => profiles = list,
            Err(error) => tracing::warn!(%error, "could not list saved configurations"),
        }
        match store.active().await {
            Ok(id) => active_profile = id,
            Err(error) => tracing::warn!(%error, "could not read the active configuration"),
        }
    }

    if let Some(id) = active_profile
        && let Some(profile) = profiles.iter().find(|profile| profile.id == id)
    {
        provider_config = ProviderConfig {
            kind: profile.kind.clone(),
            base_url: profile.base_url.clone(),
            api_key: profile.api_key.clone(),
            extra_headers: Default::default(),
            model: Some(profile.model.clone()),
        };
        model = profile.model.clone();
    }

    // Unlike tools/persistence, a bad provider has no usable fallback: every
    // `Send` would fail anyway. Rather than a permanent dead-end, this is
    // reported once and `provider` stays `None` — the settings panel is
    // still fully usable, so a bad env-derived default or a mistyped saved
    // profile can be fixed from the running app instead of only from
    // outside it.
    let mut provider: Option<Arc<dyn Provider>> =
        match ProviderRegistry::default().build(&provider_config) {
            Ok(provider) => Some(provider),
            Err(error) => {
                let _ = events.send(EngineEvent::Failed {
                    session: None,
                    message: error.to_string(),
                });
                None
            }
        };

    // MCP servers — and the GitHub/Slack/Linear integrations folded in
    // alongside them below — are long-lived and shared across every turn,
    // unlike the local built-in tools (which are rebuilt fresh per turn
    // purely so `FileChange`s can be tagged by session — irrelevant here,
    // none of these touch the workspace through `ToolContext` at all).
    // Connecting is best-effort per server: one server failing to connect
    // is reported and skipped, the rest still work.
    let (mut _mcp_connections, mut external_tools) =
        rebuild_external_tools(&config.workspace_root, &config_store, &events).await;

    let mut sessions: HashMap<SessionId, SessionSlot> = HashMap::new();
    let mut running: HashMap<SessionId, CancellationToken> = HashMap::new();
    let (task_tx, mut task_rx) = unbounded_channel::<TaskOutcome>();
    // Same "spawn it, never await it, react to the outcome later" shape as
    // `task_tx`/`task_rx` — a compaction is its own lightweight task rather
    // than a normal turn (no tools, no `TaskOutcome`-shaped file changes or
    // plan to merge back), so it gets its own channel rather than
    // shoehorning `CompactOutcome` into `TaskOutcome`'s fields.
    let (compact_tx, mut compact_rx) = unbounded_channel::<CompactOutcome>();
    // Same "spawn it, never await it, react to the outcome later" shape as
    // `task_tx`/`task_rx` above — a login involves a real human in a real
    // browser, so it can take anywhere from seconds to never completing at
    // all (a closed tab); it must not block the command loop either way.
    let (oauth_tx, mut oauth_rx) = unbounded_channel::<(OAuthProvider, Result<String, String>)>();
    // A `spawn_subagents` tool call's way of asking this same loop to start
    // a new session and wait for it — not a `Command` variant, for the same
    // reason `shutdown` above isn't: `Command` derives `PartialEq`/`Eq`,
    // which no sender type implements, and the `reply` below needs a real
    // `oneshot::Sender` a tool call can `.await` on for its result. `parent`
    // sessions map to the spawned child's id once the request is handled
    // below, so the reply can be resolved later, from `task_rx`'s arm, once
    // that child session's turn actually finishes.
    let (subagent_tx, mut subagent_rx) = unbounded_channel::<SpawnSubAgentRequest>();
    let mut pending_subagent_replies: HashMap<
        SessionId,
        tokio::sync::oneshot::Sender<Result<String, String>>,
    > = HashMap::new();

    // Long-running processes the agent starts (`start_process`/
    // `get_process_logs`/`stop_process`) — built once, unlike
    // `external_tools`: nothing here is driven by saved config, so there's
    // no equivalent of a "rebuild" trigger. See `architect_tools::process`'s
    // docs for why this can't live in `ToolContext` (rebuilt fresh every
    // turn) the way a per-turn tool's own state could.
    let (process_registry, mut process_rx) = architect_tools::ProcessRegistry::new();
    let process_registry = Arc::new(process_registry);
    let process_tools: Arc<Vec<Arc<dyn Tool>>> = Arc::new(vec![
        Arc::new(architect_tools::StartProcess {
            registry: process_registry.clone(),
        }),
        Arc::new(architect_tools::GetProcessLogs {
            registry: process_registry.clone(),
        }),
        Arc::new(architect_tools::StopProcess {
            registry: process_registry.clone(),
        }),
    ]);

    // `write_plan`/`read_plan` — always registered, same reasoning as
    // `process_tools`: no saved config or shared runtime state to build,
    // so unlike `process_tools` this needs no shared registry either.
    let plan_tools: Arc<Vec<Arc<dyn Tool>>> = Arc::new(vec![
        Arc::new(architect_tools::WritePlan),
        Arc::new(architect_tools::ReadPlan),
    ]);

    // Pick up the most recently touched session, if there is one — otherwise
    // a saved conversation would only ever be visible until the window closes,
    // which is the opposite of what persistence is for.
    if let Some(store) = &store {
        match store.list_sessions().await {
            Ok(sessions_list) => {
                if let Some(most_recent) = sessions_list.first() {
                    match store.load_messages(most_recent.id).await {
                        Ok(messages) => {
                            sessions.insert(
                                most_recent.id,
                                SessionSlot {
                                    history: messages.clone(),
                                    persisted_len: messages.len(),
                                    plan: None,
                                },
                            );
                            let _ = events.send(EngineEvent::HistoryLoaded {
                                session: most_recent.id,
                                messages,
                            });
                            match store.load_file_changes(most_recent.id).await {
                                Ok(changes) => {
                                    let _ = events.send(EngineEvent::FileChangesLoaded {
                                        session: most_recent.id,
                                        changes,
                                    });
                                }
                                Err(error) => tracing::warn!(
                                    %error,
                                    "could not load file changes for the most recent session"
                                ),
                            }
                            match store.load_plan(most_recent.id).await {
                                Ok(plan) => {
                                    if let Some(slot) = sessions.get_mut(&most_recent.id) {
                                        slot.plan = plan.clone();
                                    }
                                    let _ = events.send(EngineEvent::PlanLoaded {
                                        session: most_recent.id,
                                        plan,
                                    });
                                }
                                Err(error) => tracing::warn!(
                                    %error,
                                    "could not load the plan for the most recent session"
                                ),
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "could not load the most recent session")
                        }
                    }
                }
                let _ = events.send(EngineEvent::SessionsListed(sessions_list));
            }
            Err(error) => tracing::warn!(%error, "could not list saved sessions"),
        }
    }
    let _ = events.send(EngineEvent::ProfilesListed {
        profiles: profiles.clone(),
        active: active_profile,
    });
    send_mcp_servers_listed(&config_store, &events).await;
    send_integrations_listed(&config_store, &events).await;
    send_docs_config_listed(&config_store, &events).await;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command {
                    Command::Send { session, text, images } => {
                        if running.contains_key(&session) {
                            let _ = events.send(EngineEvent::Failed {
                                session: Some(session),
                                message: "a turn is already running for this session".into(),
                            });
                            continue;
                        }

                        let is_new_session = !sessions.contains_key(&session);
                        if is_new_session {
                            if let Some(store) = &store {
                                // An image-only message has no text to derive
                                // a title from — `title_from("")` would leave
                                // the session permanently untitled.
                                let title = if text.trim().is_empty() && !images.is_empty() {
                                    "Image".to_owned()
                                } else {
                                    title_from(&text)
                                };
                                if let Err(error) = store
                                    .create_session_with_id(session, &provider_config.kind, &model)
                                    .await
                                {
                                    tracing::warn!(%error, "could not create a session");
                                } else if let Err(error) = store.set_title(session, &title).await {
                                    tracing::warn!(%error, "could not title the session");
                                }
                            }
                            sessions.insert(session, SessionSlot::default());
                        }

                        let slot = sessions
                            .get_mut(&session)
                            .expect("just inserted above, or already present");
                        let content_images: Vec<ContentBlock> = images
                            .into_iter()
                            .map(|Attachment { media_type, bytes }| {
                                ContentBlock::image(
                                    media_type,
                                    base64::engine::general_purpose::STANDARD.encode(bytes),
                                )
                            })
                            .collect();
                        slot.history.push(if content_images.is_empty() {
                            Message::user(text)
                        } else {
                            Message::user_with_images(text, content_images)
                        });
                        persist_new_messages(
                            store.as_ref(),
                            session,
                            &slot.history,
                            &mut slot.persisted_len,
                        )
                        .await;

                        if is_new_session
                            && let Some(store) = &store
                        {
                            match store.list_sessions().await {
                                Ok(list) => {
                                    let _ = events.send(EngineEvent::SessionsListed(list));
                                }
                                Err(error) => tracing::warn!(%error, "could not list saved sessions"),
                            }
                        }

                        let Some(provider) = provider.clone() else {
                            // The user message above is still recorded — the
                            // same as any other failed turn — so nothing is
                            // silently lost once a working configuration is
                            // added.
                            let _ = events.send(EngineEvent::Failed {
                                session: Some(session),
                                message: "no working API configuration — open Settings to add or fix one".into(),
                            });
                            continue;
                        };

                        let history = std::mem::take(&mut slot.history);
                        let current_plan = slot.plan.clone();
                        let token = CancellationToken::new();
                        running.insert(session, token.clone());

                        spawn_turn(SpawnTurn {
                            session,
                            history,
                            provider,
                            workspace_root: config.workspace_root.clone(),
                            model: model.clone(),
                            system: system_prompt_for(&config.system, &external_tools),
                            cancel: token,
                            events: events.clone(),
                            task_tx: task_tx.clone(),
                            external_tools: external_tools.clone(),
                            process_tools: process_tools.clone(),
                            plan_tools: plan_tools.clone(),
                            current_plan,
                            sub_agent_spawner: Arc::new(EngineSubAgentSpawner {
                                parent: session,
                                subagent_tx: subagent_tx.clone(),
                                run_sequentially: architect_llm::is_local(&provider_config),
                            }),
                            tool_registry: ToolRegistryKind::Default,
                        });
                    }
                    Command::Cancel(session) => {
                        if let Some(token) = running.get(&session) {
                            token.cancel();
                        }
                    }
                    Command::LoadSession(id) => {
                        if sessions.contains_key(&id) {
                            // Already resident — currently running, or
                            // loaded earlier this app run. Re-fetching would
                            // clobber live state with a stale DB snapshot.
                            continue;
                        }
                        let Some(store) = &store else { continue };
                        match store.load_messages(id).await {
                            Ok(messages) => {
                                sessions.insert(
                                    id,
                                    SessionSlot {
                                        history: messages.clone(),
                                        persisted_len: messages.len(),
                                        plan: None,
                                    },
                                );
                                let _ = events.send(EngineEvent::HistoryLoaded {
                                    session: id,
                                    messages,
                                });
                                match store.load_file_changes(id).await {
                                    Ok(changes) => {
                                        let _ = events.send(EngineEvent::FileChangesLoaded {
                                            session: id,
                                            changes,
                                        });
                                    }
                                    Err(error) => tracing::warn!(
                                        %error,
                                        "could not load file changes for the session"
                                    ),
                                }
                                match store.load_plan(id).await {
                                    Ok(plan) => {
                                        if let Some(slot) = sessions.get_mut(&id) {
                                            slot.plan = plan.clone();
                                        }
                                        let _ = events.send(EngineEvent::PlanLoaded {
                                            session: id,
                                            plan,
                                        });
                                    }
                                    Err(error) => tracing::warn!(
                                        %error,
                                        "could not load the plan for the session"
                                    ),
                                }
                            }
                            Err(error) => tracing::warn!(%error, "could not load the session"),
                        }
                    }
                    Command::DeleteSession(id) => {
                        // A turn in flight for a deleted session has nothing
                        // left to persist against — stop it rather than let
                        // its eventual persistence calls fail against a
                        // session row that no longer exists.
                        if let Some(token) = running.get(&id) {
                            token.cancel();
                        }
                        let Some(store) = &store else { continue };
                        if let Err(error) = store.delete_session(id).await {
                            let _ = events.send(EngineEvent::Failed {
                                session: Some(id),
                                message: error.to_string(),
                            });
                            continue;
                        }
                        sessions.remove(&id);
                        let _ = events.send(EngineEvent::SessionDeleted(id));
                        match store.list_sessions().await {
                            Ok(list) => {
                                let _ = events.send(EngineEvent::SessionsListed(list));
                            }
                            Err(error) => tracing::warn!(%error, "could not list saved sessions"),
                        }
                    }
                    Command::ListProfiles => {
                        refresh_profiles(&config_store, &mut profiles, &mut active_profile, &events)
                            .await;
                    }
                    Command::SaveProfile(profile) => {
                        let Some(cs) = &config_store else {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: "profile storage unavailable".into(),
                            });
                            continue;
                        };
                        if let Err(error) = cs.upsert(profile).await {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: error.to_string(),
                            });
                            continue;
                        }
                        refresh_profiles(&config_store, &mut profiles, &mut active_profile, &events)
                            .await;
                    }
                    Command::DeleteProfile(id) => {
                        let Some(cs) = &config_store else { continue };
                        if let Err(error) = cs.remove(id).await {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: error.to_string(),
                            });
                            continue;
                        }
                        refresh_profiles(&config_store, &mut profiles, &mut active_profile, &events)
                            .await;
                    }
                    Command::ActivateProfile(id) => {
                        let Some(cs) = &config_store else { continue };
                        let Some(profile) = profiles.iter().find(|profile| profile.id == id).cloned()
                        else {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: "that configuration no longer exists".into(),
                            });
                            continue;
                        };
                        if let Err(error) = cs.set_active(id).await {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: error.to_string(),
                            });
                            continue;
                        }

                        let new_provider_config = ProviderConfig {
                            kind: profile.kind.clone(),
                            base_url: profile.base_url.clone(),
                            api_key: profile.api_key.clone(),
                            extra_headers: Default::default(),
                            model: Some(profile.model.clone()),
                        };

                        match ProviderRegistry::default().build(&new_provider_config) {
                            Ok(new_provider) => {
                                provider_config = new_provider_config;
                                model = profile.model.clone();
                                provider = Some(new_provider);
                                active_profile = Some(id);
                                let _ = events.send(EngineEvent::ProfilesListed {
                                    profiles: profiles.clone(),
                                    active: active_profile,
                                });
                            }
                            Err(error) => {
                                // Keep whatever was working before —
                                // switching to a broken configuration must
                                // not take down one that already worked.
                                let _ = events.send(EngineEvent::Failed {
                                    session: None,
                                    message: error.to_string(),
                                });
                            }
                        }
                    }
                    Command::DeactivateProfile => {
                        if let Some(cs) = &config_store
                            && let Err(error) = cs.clear_active().await
                        {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: error.to_string(),
                            });
                            continue;
                        }

                        match ProviderRegistry::default().build(&default_provider_config) {
                            Ok(new_provider) => {
                                provider_config = default_provider_config.clone();
                                model = default_model.clone();
                                provider = Some(new_provider);
                                active_profile = None;
                                let _ = events.send(EngineEvent::ProfilesListed {
                                    profiles: profiles.clone(),
                                    active: active_profile,
                                });
                            }
                            Err(error) => {
                                let _ = events.send(EngineEvent::Failed {
                                    session: None,
                                    message: error.to_string(),
                                });
                            }
                        }
                    }
                    Command::UseAdHocModel {
                        base_url,
                        api_key,
                        model: requested_model,
                    } => {
                        let new_provider_config = ProviderConfig {
                            kind: "openai".to_owned(), // the only dialect this page targets
                            base_url: Some(base_url),
                            api_key,
                            extra_headers: Default::default(),
                            model: Some(requested_model.clone()),
                        };

                        match ProviderRegistry::default().build(&new_provider_config) {
                            Ok(new_provider) => {
                                provider_config = new_provider_config;
                                model = requested_model.clone();
                                provider = Some(new_provider);
                                // An ad-hoc pick supersedes any saved
                                // profile that was active — nothing here
                                // touches `config_store`, this is never
                                // persisted.
                                active_profile = None;
                                let _ = events.send(EngineEvent::AdHocModelActivated {
                                    model: requested_model,
                                });
                            }
                            Err(error) => {
                                let _ = events.send(EngineEvent::Failed {
                                    session: None,
                                    message: error.to_string(),
                                });
                            }
                        }
                    }
                    Command::ListMcpServers => {
                        send_mcp_servers_listed(&config_store, &events).await;
                    }
                    Command::ListIntegrations => {
                        send_integrations_listed(&config_store, &events).await;
                    }
                    Command::SaveMcpServer(server) => {
                        let Some(cs) = &config_store else {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: "profile storage unavailable".into(),
                            });
                            continue;
                        };
                        if let Err(error) = cs.upsert_mcp_server(server).await {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: error.to_string(),
                            });
                            continue;
                        }
                        (_mcp_connections, external_tools) =
                            rebuild_external_tools(&config.workspace_root, &config_store, &events).await;
                        send_mcp_servers_listed(&config_store, &events).await;
                    }
                    Command::DeleteMcpServer(id) => {
                        let Some(cs) = &config_store else { continue };
                        if let Err(error) = cs.remove_mcp_server(id).await {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: error.to_string(),
                            });
                            continue;
                        }
                        (_mcp_connections, external_tools) =
                            rebuild_external_tools(&config.workspace_root, &config_store, &events).await;
                        send_mcp_servers_listed(&config_store, &events).await;
                    }
                    Command::SaveIntegrations(integrations) => {
                        let Some(cs) = &config_store else {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: "profile storage unavailable".into(),
                            });
                            continue;
                        };
                        if let Err(error) = cs.set_integrations(integrations).await {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: error.to_string(),
                            });
                            continue;
                        }
                        (_mcp_connections, external_tools) =
                            rebuild_external_tools(&config.workspace_root, &config_store, &events).await;
                        send_integrations_listed(&config_store, &events).await;
                    }
                    Command::ListDocsConfig => {
                        send_docs_config_listed(&config_store, &events).await;
                    }
                    Command::SaveDocsConfig(docs) => {
                        let Some(cs) = &config_store else {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: "profile storage unavailable".into(),
                            });
                            continue;
                        };
                        if let Err(error) = cs.set_docs(docs).await {
                            let _ = events.send(EngineEvent::Failed {
                                session: None,
                                message: error.to_string(),
                            });
                            continue;
                        }
                        (_mcp_connections, external_tools) =
                            rebuild_external_tools(&config.workspace_root, &config_store, &events).await;
                        send_docs_config_listed(&config_store, &events).await;
                    }
                    Command::StartOAuthLogin(provider) => {
                        let oauth_tx = oauth_tx.clone();
                        tokio::spawn(async move {
                            let result = oauth::run_login(provider).await;
                            let _ = oauth_tx.send((provider, result));
                        });
                    }
                    Command::Rollback { session, up_to_seq } => {
                        if running.contains_key(&session) {
                            let _ = events.send(EngineEvent::Failed {
                                session: Some(session),
                                message: "a turn is already running for this session".into(),
                            });
                            continue;
                        }
                        let Some(store) = &store else { continue };
                        if let Err(error) = store.reverse_to_point(session, up_to_seq).await {
                            let _ = events.send(EngineEvent::Failed {
                                session: Some(session),
                                message: error.to_string(),
                            });
                            continue;
                        }
                        // No dedicated "it worked" event — a fresh load is
                        // both the update and the confirmation.
                        match store.load_file_changes(session).await {
                            Ok(changes) => {
                                let _ = events.send(EngineEvent::FileChangesLoaded {
                                    session,
                                    changes,
                                });
                            }
                            Err(error) => tracing::warn!(
                                %error,
                                "could not reload file changes after rollback"
                            ),
                        }
                    }
                    Command::Compact(session) => {
                        if running.contains_key(&session) {
                            let _ = events.send(EngineEvent::Failed {
                                session: Some(session),
                                message: "a turn is already running for this session".into(),
                            });
                            continue;
                        }
                        let history = sessions.get(&session).map(|slot| slot.history.clone());
                        let Some(history) = history.filter(|history| !history.is_empty()) else {
                            let _ = events.send(EngineEvent::Failed {
                                session: Some(session),
                                message: "nothing to compact yet".into(),
                            });
                            continue;
                        };
                        let Some(provider) = provider.clone() else {
                            let _ = events.send(EngineEvent::Failed {
                                session: Some(session),
                                message:
                                    "no working API configuration — open Settings to add or fix one"
                                        .into(),
                            });
                            continue;
                        };

                        let token = CancellationToken::new();
                        running.insert(session, token.clone());
                        spawn_compact(
                            session,
                            history,
                            provider,
                            model.clone(),
                            token,
                            compact_tx.clone(),
                        );
                    }
                    Command::ListModels { base_url, api_key } => {
                        // No shared state to update afterward — unlike a
                        // turn or a compaction, nothing here needs the
                        // `task_rx`-style outcome channel; the fetch just
                        // reports straight back as a UI-facing event.
                        let events = events.clone();
                        tokio::spawn(async move {
                            match architect_llm::list_models(&base_url, api_key.as_deref()).await
                            {
                                Ok(models) => {
                                    let _ = events
                                        .send(EngineEvent::ModelsListed { base_url, models });
                                }
                                Err(error) => {
                                    let _ = events.send(EngineEvent::Failed {
                                        session: None,
                                        message: error.to_string(),
                                    });
                                }
                            }
                        });
                    }
                }
            }
            Some(outcome) = task_rx.recv() => {
                let TaskOutcome { session, history, file_changes, plan, tools_error, result } = outcome;
                running.remove(&session);

                if let Some(slot) = sessions.get_mut(&session) {
                    slot.history = history;
                    persist_new_messages(
                        store.as_ref(),
                        session,
                        &slot.history,
                        &mut slot.persisted_len,
                    )
                    .await;
                    if let Some(store) = &store {
                        // Attributed to the turn's last message rather than
                        // the exact iteration that produced each change:
                        // Rollback's granularity is per-turn, not per-tool-
                        // call — "undo everything after this point in the
                        // conversation," not "undo this one call."
                        let message_seq = slot.history.len().saturating_sub(1) as i64;
                        for change in file_changes {
                            if let Err(error) =
                                store.record_file_change(session, message_seq, &change).await
                            {
                                tracing::warn!(%error, "could not record a file change");
                            }
                            let _ = events.send(EngineEvent::FileChanged {
                                session,
                                entry: FileChangeEntry {
                                    message_seq,
                                    change,
                                },
                            });
                        }
                    }
                    if let Some(plan) = plan {
                        slot.plan = Some(plan.clone());
                        if let Some(store) = &store
                            && let Err(error) = store.save_plan(session, &plan).await
                        {
                            tracing::warn!(%error, "could not save the plan");
                        }
                        // No `events.send(PlanUpdated)` here: `spawn_turn`
                        // already forwarded this exact value live, the
                        // moment `write_plan` saved it — this is just the
                        // one point that also needs to persist it, now that
                        // the whole turn (and thus the final history) is known.
                    }

                    // If a `spawn_subagents` tool call is waiting on this
                    // session specifically (it's a sub-agent's own turn,
                    // not a normal one), resolve it now — success or
                    // failure, so that call never hangs forever.
                    if let Some(reply) = pending_subagent_replies.remove(&session) {
                        // `Ok` from `run_turn` only means the loop exited
                        // without erroring — it says nothing about whether
                        // the last message is an actual finished answer.
                        // Three ways it can be junk instead: the loop ran
                        // out of iterations mid-work, the model got cut off
                        // at its token limit (a `break`, not a caught
                        // error, so this wouldn't otherwise be flagged),
                        // or — degenerately — the last message just has no
                        // text at all. Any of those, reported back as a
                        // silent `Ok("")` or a narration fragment, would be
                        // indistinguishable from a real answer to whatever
                        // parent turn is waiting on it.
                        let outcome = match &result {
                            Ok(turn_outcome) if turn_outcome.hit_iteration_limit => Err(
                                "sub-agent hit its iteration limit before finishing".to_owned(),
                            ),
                            Ok(turn_outcome) if turn_outcome.stop_reason == StopReason::MaxTokens => {
                                Err("sub-agent's answer was cut off at the model's token limit"
                                    .to_owned())
                            }
                            Ok(_) => {
                                let text = slot.history.last().map(Message::text).unwrap_or_default();
                                if text.trim().is_empty() {
                                    Err("sub-agent finished without producing any text".to_owned())
                                } else {
                                    Ok(text)
                                }
                            }
                            Err(error) => Err(error.to_string()),
                        };
                        let _ = reply.send(outcome);
                    }
                }

                if let Some(message) = tools_error {
                    let _ = events.send(EngineEvent::Failed {
                        session: Some(session),
                        message: format!("tools unavailable: {message}"),
                    });
                }

                match result {
                    Ok(_) => {}
                    Err(AgentError::Cancelled) => {
                        let _ = events.send(EngineEvent::Cancelled(session));
                    }
                    Err(error) => {
                        let _ = events.send(EngineEvent::Failed {
                            session: Some(session),
                            message: error.to_string(),
                        });
                    }
                }
            }
            Some(outcome) = compact_rx.recv() => {
                let CompactOutcome { session, result } = outcome;
                running.remove(&session);

                match result {
                    Ok(summary) => {
                        let new_history = vec![Message::assistant(summary.clone())];
                        if let Some(slot) = sessions.get_mut(&session) {
                            slot.history = new_history.clone();
                            slot.persisted_len = new_history.len();
                        }
                        if let Some(store) = &store
                            && let Err(error) = store.replace_messages(session, &new_history).await
                        {
                            tracing::warn!(%error, "could not persist the compacted history");
                        }
                        let _ = events.send(EngineEvent::Compacted { session, summary });
                    }
                    Err(AgentError::Cancelled) => {
                        let _ = events.send(EngineEvent::Cancelled(session));
                    }
                    Err(error) => {
                        let _ = events.send(EngineEvent::Failed {
                            session: Some(session),
                            message: error.to_string(),
                        });
                    }
                }
            }
            Some(request) = subagent_rx.recv() => {
                let SpawnSubAgentRequest { parent, prompt, path, reply } = request;

                // Containment check every file-path-taking tool already goes
                // through (`ToolContext::resolve`), reused rather than
                // re-implemented for the escape-the-workspace case. But
                // `resolve` is written for `write_file` semantics — a path
                // that doesn't exist yet is fine, since only its parent
                // needs to live inside the root — and that's wrong here:
                // this path becomes a *whole turn's* workspace root, so
                // unlike a not-yet-written file, it must already exist and
                // be a directory, or the sub-agent gets scoped into a place
                // where every read-only tool just fails.
                let resolved_workspace_root = architect_tools::ToolContext::new(&config.workspace_root)
                    .map_err(|error| error.to_string())
                    .and_then(|ctx| ctx.resolve(&path));
                let resolved_workspace_root = match resolved_workspace_root {
                    Ok(resolved) if !resolved.is_dir() => {
                        let _ = reply.send(Err(format!(
                            "{path:?} is not an existing directory in the workspace"
                        )));
                        continue;
                    }
                    Ok(resolved) => resolved,
                    Err(error) => {
                        let _ = reply.send(Err(format!("{path:?} is not a usable path: {error}")));
                        continue;
                    }
                };

                let Some(provider) = provider.clone() else {
                    let _ = reply.send(Err(
                        "no working API configuration — open Settings to add or fix one".into(),
                    ));
                    continue;
                };

                let child = SessionId::new();
                if let Some(store) = &store {
                    if let Err(error) = store
                        .create_child_session_with_id(child, parent, &provider_config.kind, &model)
                        .await
                    {
                        tracing::warn!(%error, "could not create a sub-agent session");
                    } else if let Err(error) = store.set_title(child, &title_from(&prompt)).await {
                        tracing::warn!(%error, "could not title a sub-agent session");
                    }
                }

                sessions.insert(child, SessionSlot::default());
                let slot = sessions.get_mut(&child).expect("just inserted above");
                // The path the caller wrote in its own prompt (if it wrote
                // one at all) is almost always relative to the *parent's*
                // workspace root, not this sub-agent's — but this sub-agent's
                // tools are rooted at `path` itself, so from where it's
                // sitting that path doesn't exist. Spelling this out up
                // front is cheaper than letting the model discover it by
                // trial and error (or, worse, silently answer about the
                // wrong directory).
                let scoped_prompt = format!(
                    "Your tools for this task are scoped to `{path}` — that path already IS \
                     your workspace root here, not a subdirectory to navigate into. Refer to \
                     paths inside it directly (e.g. `src/lib.rs`, not `{path}/src/lib.rs`); \
                     anything outside it is out of scope for you.\n\n{prompt}"
                );
                slot.history.push(Message::user(scoped_prompt));
                persist_new_messages(store.as_ref(), child, &slot.history, &mut slot.persisted_len).await;

                if let Some(store) = &store {
                    match store.list_sessions().await {
                        Ok(list) => {
                            let _ = events.send(EngineEvent::SessionsListed(list));
                        }
                        Err(error) => tracing::warn!(%error, "could not list saved sessions"),
                    }
                }

                pending_subagent_replies.insert(child, reply);

                let slot = sessions.get_mut(&child).expect("just inserted above");
                let history = std::mem::take(&mut slot.history);
                let token = CancellationToken::new();
                running.insert(child, token.clone());

                spawn_turn(SpawnTurn {
                    session: child,
                    history,
                    provider,
                    workspace_root: resolved_workspace_root,
                    model: model.clone(),
                    system: SUBAGENT_SYSTEM.to_owned(),
                    cancel: token,
                    events: events.clone(),
                    task_tx: task_tx.clone(),
                    external_tools: Arc::new(read_only_doc_tools(&external_tools)),
                    process_tools: Arc::new(Vec::new()),
                    plan_tools: Arc::new(Vec::new()),
                    current_plan: None,
                    sub_agent_spawner: Arc::new(EngineSubAgentSpawner {
                        parent: child,
                        subagent_tx: subagent_tx.clone(),
                        run_sequentially: architect_llm::is_local(&provider_config),
                    }),
                    tool_registry: ToolRegistryKind::Investigation,
                });
            }
            Some((provider, result)) = oauth_rx.recv() => {
                // No dedicated "login succeeded" event — same as
                // `Command::Rollback`, the refreshed list sent by
                // `rebuild_external_tools`/`send_integrations_listed` is
                // the confirmation.
                if apply_oauth_result(&config_store, &events, provider, result).await {
                    (_mcp_connections, external_tools) =
                        rebuild_external_tools(&config.workspace_root, &config_store, &events).await;
                    send_integrations_listed(&config_store, &events).await;
                }
            }
            Some(event) = process_rx.recv() => {
                let _ = events.send(EngineEvent::Process(event));
            }
            Some(ack) = shutdown_rx.recv() => {
                // Best-effort, bounded — see `ProcessRegistry::kill_all`'s
                // own docs. `main.rs` blocks on `ack` briefly so the process
                // doesn't fully exit before this has a chance to run.
                process_registry.kill_all().await;
                let _ = ack.send(());
                break;
            }
        }
    }
}

/// What compaction is asked to preserve — the objective, decisions, and
/// state needed to keep working from the summary alone, nothing else.
const COMPACT_SYSTEM: &str = "You are compacting a coding-agent conversation to free up its \
context window. Reply with only the summary itself — no preamble, no headers, no offer to \
continue, no meta-commentary. The summary you write replaces this entire conversation's \
history from this point on.";

const COMPACT_INSTRUCTION: &str = "Summarize this conversation so that an assistant reading \
only your summary could continue the task with full context: the objective, key decisions \
and their rationale, files or resources touched and their current state, and specifically \
what remains to be done. Be concise, but do not omit anything necessary to continue \
correctly.";

/// What a sub-agent session — spawned by another session's `spawn_subagents`
/// tool call — is told about its own role. It starts with no history beyond
/// its own task prompt, so this is the only context it has for what it is
/// and why its final message matters.
const SUBAGENT_SYSTEM: &str = "You are a focused sub-agent, spawned by another agent to \
investigate one specific thing on its behalf. You have no memory of the larger conversation \
that spawned you — only the task prompt you were given. Your file/search tools are read-only \
and scoped to a specific path; if this project has a documentation knowledge base, you may \
also have read_doc/search_docs/list_docs — those are read-only too, but scoped to the whole \
knowledge base, not to your investigation path, since docs live outside any one crate. Give a \
clear, complete final answer: it is the only part of your work \
that reaches whoever asked you to look into this. If your scoped path turns out to be empty, \
missing, or every tool call against it fails, stop and report that verbatim as your final \
answer rather than guessing at its contents or reaching for a tool you were not given — you \
only have the tools actually offered to you in this conversation, never assume another one \
exists. If your final answer includes a count or total (lines of code, number of tests, items \
found), recompute it by explicitly summing the exact numbers you gathered rather than trusting \
a single mental tally — a wrong sum over correct data is a more common mistake here than \
missing or wrong data itself.";

/// Appended to `config.system` when this turn's tools include the doc
/// tools (`doc_tools`/`rebuild_external_tools` — gated on `DocsConfig::
/// enabled`, on by default). `config.system` itself stays a plain, static
/// string (its long-standing shape); this is layered on per-turn instead of
/// baked into it, the same way `SUBAGENT_SYSTEM` is a separate string
/// entirely rather than a variant of the main one.
const DOCS_PROTOCOL: &str = "\n\nThis project has a documentation knowledge base (search_docs, \
read_doc, write_doc, edit_doc, list_docs, scaffold_docs). Before a non-trivial code change, \
look up the affected domain/flow, its rules, entities, and related flows/ADRs. After a change \
that alters behavior, rules, entities, or flows, update the corresponding docs — treat them as \
part of the change, not an afterthought.";

/// `base`, plus [`DOCS_PROTOCOL`] when `tools` includes the doc tools.
fn system_prompt_for(base: &str, tools: &[Arc<dyn Tool>]) -> String {
    if tools.iter().any(|tool| tool.name() == "search_docs") {
        format!("{base}{DOCS_PROTOCOL}")
    } else {
        base.to_owned()
    }
}

/// What a spawned compaction task reports back once it finishes.
struct CompactOutcome {
    session: SessionId,
    /// The summary text, or why summarizing failed — cancellation included,
    /// the same as a normal turn's `Err(AgentError::Cancelled)`.
    result: Result<String, AgentError>,
}

/// Ask the model to summarize `history`, on its own task so the command
/// loop never blocks on it — same "spawn it, react later" shape as
/// [`spawn_turn`], but far lighter: no tool registry, no file-change/plan
/// channels, since a compaction call makes exactly one request and does
/// nothing but talk.
///
/// Runs through `Agent::run_turn` (with `NoTools` and `max_iterations(1)`)
/// rather than a raw `Provider::complete` call, so cancellation, error
/// handling, and the request shape all come from the one place that
/// already gets them right, instead of a second hand-rolled copy.
fn spawn_compact(
    session: SessionId,
    history: Vec<Message>,
    provider: Arc<dyn Provider>,
    model: String,
    cancel: CancellationToken,
    compact_tx: UnboundedSender<CompactOutcome>,
) {
    tokio::spawn(async move {
        let mut messages = history;
        messages.push(Message::user(COMPACT_INSTRUCTION));

        let agent = Agent::new(
            provider,
            Arc::new(NoTools),
            AgentConfig::new(model)
                .system(COMPACT_SYSTEM)
                .max_iterations(1),
        );

        // Compaction has no row/turn of its own to stream deltas into, so
        // its events are never forwarded to the UI — only the assembled
        // reply, once the whole thing is done, matters here.
        let (local_tx, _local_rx) = unbounded_channel::<AgentEvent>();
        let result = agent
            .run_turn(&mut messages, &local_tx, cancel)
            .await
            .map(|_| messages.last().map(Message::text).unwrap_or_default());

        let _ = compact_tx.send(CompactOutcome { session, result });
    });
}

/// Which `ToolRegistry` constructor [`spawn_turn`] should build this turn's
/// tools with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolRegistryKind {
    /// The normal nine-tool (plus `spawn_subagents` itself) registry every
    /// user-started turn gets.
    Default,
    /// The read-only, `spawn_subagents`-excluding registry a sub-agent's own
    /// turn runs with — see `ToolRegistry::with_investigation_tools`'s docs
    /// for why.
    Investigation,
}

/// Arguments for [`spawn_turn`] — a plain struct rather than a long parameter
/// list, since every field is required and several share a type (`String`).
struct SpawnTurn {
    session: SessionId,
    history: Vec<Message>,
    provider: Arc<dyn Provider>,
    workspace_root: PathBuf,
    model: String,
    system: String,
    cancel: CancellationToken,
    events: UnboundedSender<EngineEvent>,
    task_tx: UnboundedSender<TaskOutcome>,
    /// Every tool discovered from a connected MCP server, plus every
    /// GitHub/Slack/Linear tool built from a saved credential — shared and
    /// long-lived (unlike the local built-ins below) — registered into this
    /// turn's own `ToolRegistry` alongside them.
    external_tools: Arc<Vec<Arc<dyn Tool>>>,
    /// `start_process`/`get_process_logs`/`stop_process` — always
    /// registered, unlike `external_tools`, since nothing here needs a
    /// saved credential. A separate field rather than folded into
    /// `external_tools` because its lifecycle is different: built once at
    /// worker startup and never rebuilt, where `external_tools` is rebuilt
    /// whenever saved MCP servers or integrations change.
    process_tools: Arc<Vec<Arc<dyn Tool>>>,
    /// `write_plan`/`read_plan` — always registered, same reasoning as
    /// `process_tools`.
    plan_tools: Arc<Vec<Arc<dyn Tool>>>,
    /// This session's plan as of the start of the turn — what `read_plan`
    /// answers with via `ToolContext::current_plan`. See `architect_tools::
    /// tools::plan`'s docs for why a `write_plan` earlier in *this* turn
    /// won't be reflected back.
    current_plan: Option<Plan>,
    /// What this turn's own `spawn_subagents` calls (if its tool registry
    /// includes that tool at all — see `tool_registry`) start and await.
    sub_agent_spawner: Arc<dyn architect_tools::SubAgentSpawner>,
    tool_registry: ToolRegistryKind,
}

/// Builds a fresh [`Agent`] — with its own per-turn tools, so this session's
/// `write_file`/`edit_file` calls report `FileChange`s tagged for it alone; a
/// single shared tool instance couldn't tell two concurrent turns' changes
/// apart — and runs one turn to completion on its own task, reporting the
/// result back on `task_tx`.
///
/// Events stream out as they happen, not just at the end: `Agent::run_turn`
/// needs a concrete `UnboundedSender<E>`, so this hands it a *local* channel
/// of `AgentEvent` (`E = AgentEvent` trivially satisfies `E: From<AgentEvent>`
/// via the reflexive blanket impl) and runs a small forwarder alongside it
/// that tags each one with `session` before re-sending on the real outer
/// `events` channel.
fn spawn_turn(turn: SpawnTurn) {
    let SpawnTurn {
        session,
        mut history,
        provider,
        workspace_root,
        model,
        system,
        cancel,
        events,
        task_tx,
        external_tools,
        process_tools,
        plan_tools,
        current_plan,
        sub_agent_spawner,
        tool_registry,
    } = turn;

    tokio::spawn(async move {
        let (file_change_tx, mut file_change_rx) = unbounded_channel::<FileChange>();
        let (plan_tx, mut plan_rx) = unbounded_channel::<Plan>();
        let built_registry = match tool_registry {
            ToolRegistryKind::Default => ToolRegistry::with_default_tools(&workspace_root),
            // No `external_tools`/`process_tools`/`plan_tools` merged in —
            // a sub-agent's turn only ever gets the read-only set this
            // constructor registers, deliberately excluding `spawn_subagents`
            // itself (the recursion guard) and anything requiring a saved
            // credential.
            ToolRegistryKind::Investigation => {
                ToolRegistry::with_investigation_tools(&workspace_root)
            }
        };
        let (tools, tools_error): (Arc<dyn ToolExecutor>, Option<String>) = match built_registry {
            Ok(mut registry) => {
                // The existing extension point, not a new composition
                // mechanism — `ToolRegistry::register` already takes any
                // `Arc<dyn Tool>`, which is exactly what an MCP adapter or
                // a GitHub/Slack/Linear tool is. Empty for a sub-agent's
                // turn (`SpawnTurn`'s caller passes empty `Arc<Vec<_>>`s for
                // all three there), so this loop is a no-op in that case.
                for tool in external_tools
                    .iter()
                    .chain(process_tools.iter())
                    .chain(plan_tools.iter())
                {
                    registry.register(tool.clone());
                }
                let registry = registry
                    .with_recorder(Arc::new(ChannelRecorder(file_change_tx)))
                    .with_plan_recorder(Arc::new(ChannelPlanRecorder(plan_tx)))
                    .with_current_plan(current_plan)
                    .with_sub_agent_spawner(sub_agent_spawner);
                (Arc::new(registry), None)
            }
            Err(error) => {
                // No usable sandbox root for this turn — should not
                // happen in practice (the workspace root is the
                // process's own directory), but the agent can still
                // hold a conversation without tools rather than not run
                // the turn at all.
                tracing::warn!(%error, "tools unavailable");
                (Arc::new(NoTools), Some(error.to_string()))
            }
        };

        let agent = Agent::new(
            provider,
            tools,
            AgentConfig::new(model)
                .system(system)
                .reasoning(Reasoning::VISIBLE),
        );

        let (raw_tx, mut raw_rx) = unbounded_channel::<AgentEvent>();
        let forward_events = events.clone();
        let forward = tokio::spawn(async move {
            while let Some(event) = raw_rx.recv().await {
                let _ = forward_events.send(EngineEvent::Agent { session, event });
            }
        });

        // A `write_plan` call deep inside a long turn (several tool-call
        // iterations, possibly a slow `spawn_subagents` wait) used to be
        // invisible in the Inspector's Plan tab until the *entire* turn
        // finished — the plan only got read out of `plan_rx` in a batch
        // after `run_turn` returned. Forwarding each one live, the same way
        // `raw_rx` above already does for `AgentEvent`s, means a plan shows
        // up the moment it's saved rather than only once the whole turn is
        // done. `latest_plan` still tracks the last one seen so the final
        // `TaskOutcome` below (used for persistence) doesn't need its own
        // second read of the channel.
        let latest_plan: Arc<Mutex<Option<Plan>>> = Arc::new(Mutex::new(None));
        let plan_forward_events = events;
        let plan_forward_latest = latest_plan.clone();
        let plan_forward = tokio::spawn(async move {
            while let Some(plan) = plan_rx.recv().await {
                *plan_forward_latest.lock().expect("not poisoned") = Some(plan.clone());
                let _ = plan_forward_events.send(EngineEvent::PlanUpdated { session, plan });
            }
        });

        let result = agent.run_turn(&mut history, &raw_tx, cancel).await;
        // Closes the channel so the forwarder's loop ends once it has
        // drained whatever was already sent — must happen before awaiting
        // it below, or this would deadlock waiting on itself.
        drop(raw_tx);
        let _ = forward.await;

        // Same reasoning as `raw_tx` above: `agent` is the last thing
        // holding the tool registry, which is the last thing holding
        // `plan_tx` — dropping it closes the channel so `plan_forward`'s
        // loop ends rather than waiting forever.
        drop(agent);
        let _ = plan_forward.await;

        let mut file_changes = Vec::new();
        while let Ok(change) = file_change_rx.try_recv() {
            file_changes.push(change);
        }

        // The already-live-forwarded value, not a second read of the
        // channel — `plan_rx` was fully drained by `plan_forward` above.
        let plan = latest_plan.lock().expect("not poisoned").clone();

        let _ = task_tx.send(TaskOutcome {
            session,
            history,
            file_changes,
            plan,
            tools_error,
            result,
        });
    });
}

/// Re-read the saved configurations and broadcast them — the common tail of
/// every profile command.
async fn refresh_profiles(
    config_store: &Option<ConfigStore>,
    profiles: &mut Vec<Profile>,
    active_profile: &mut Option<Uuid>,
    events: &UnboundedSender<EngineEvent>,
) {
    let Some(config_store) = config_store else {
        return;
    };

    match config_store.list().await {
        Ok(list) => *profiles = list,
        Err(error) => {
            tracing::warn!(%error, "could not list saved configurations");
            return;
        }
    }
    match config_store.active().await {
        Ok(id) => *active_profile = id,
        Err(error) => tracing::warn!(%error, "could not read the active configuration"),
    }

    let _ = events.send(EngineEvent::ProfilesListed {
        profiles: profiles.clone(),
        active: *active_profile,
    });
}

/// Reads the saved MCP servers and connects to every enabled one, best
/// effort — a server that fails to connect is logged and reported via the
/// global-failure path (the same channel a bad startup provider config
/// already uses), the rest still connect. Rebuilding the whole list from
/// scratch rather than diffing it is deliberate: server lists are small and
/// this only runs on startup or after an explicit CRUD command, not on any
/// hot path.
async fn reconnect_mcp(
    config_store: &Option<ConfigStore>,
    events: &UnboundedSender<EngineEvent>,
) -> (Vec<McpConnection>, Arc<Vec<Arc<dyn Tool>>>) {
    let Some(config_store) = config_store else {
        return (Vec::new(), Arc::new(Vec::new()));
    };

    let servers = match config_store.list_mcp_servers().await {
        Ok(servers) => servers,
        Err(error) => {
            tracing::warn!(%error, "could not list saved MCP servers");
            return (Vec::new(), Arc::new(Vec::new()));
        }
    };

    let mut connections = Vec::new();
    let mut tools = Vec::new();

    for server in servers.into_iter().filter(|server| server.enabled) {
        let connected = match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                architect_mcp::connect(command, args, env).await
            }
            McpTransport::Http { url, bearer_token } => {
                architect_mcp::connect_http(url, bearer_token.as_deref()).await
            }
        };
        match connected {
            Ok(connection) => {
                tools.extend(connection.tools.iter().cloned());
                connections.push(connection);
            }
            Err(error) => {
                tracing::warn!(server = %server.name, %error, "MCP server failed to connect");
                let _ = events.send(EngineEvent::Failed {
                    session: None,
                    message: format!("MCP server {:?} failed to connect: {error}", server.name),
                });
            }
        }
    }

    (connections, Arc::new(tools))
}

/// `reconnect_mcp` plus the GitHub/Slack/Linear tools built from whatever
/// integrations are currently saved, plus the doc tools built from whatever
/// documentation-driver config is currently saved — the one place these
/// independent tool sources are combined into what `spawn_turn` actually
/// registers. Called everywhere `reconnect_mcp` used to be called alone: at
/// startup, and after any command that changes an MCP server, an
/// integration's credential, or the docs config.
async fn rebuild_external_tools(
    workspace_root: &Path,
    config_store: &Option<ConfigStore>,
    events: &UnboundedSender<EngineEvent>,
) -> (Vec<McpConnection>, Arc<Vec<Arc<dyn Tool>>>) {
    let (connections, mcp_tools) = reconnect_mcp(config_store, events).await;

    let mut tools = (*mcp_tools).clone();
    if let Some(config_store) = config_store {
        match config_store.integrations().await {
            Ok(integrations) => tools.extend(integration_tools(&integrations, events).await),
            Err(error) => tracing::warn!(%error, "could not read saved integrations"),
        }
        match config_store.docs().await {
            Ok(docs) => tools.extend(doc_tools(workspace_root, &docs, events)),
            Err(error) => tracing::warn!(%error, "could not read saved docs config"),
        }
    }

    (connections, Arc::new(tools))
}

/// The doc tools for whatever documentation-driver config is currently
/// saved. Unlike `integration_tools`, `enabled` alone gates this — Obsidian
/// needs no credential, so there is no "absent token means no tools"
/// signal to key off of the way GitHub/Slack/Linear can.
fn doc_tools(
    workspace_root: &Path,
    config: &DocsConfig,
    events: &UnboundedSender<EngineEvent>,
) -> Vec<Arc<dyn Tool>> {
    if !config.enabled {
        return Vec::new();
    }

    let vault_path = config
        .vault_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("docs"));

    match architect_docs::obsidian::ObsidianDriver::new(vault_path) {
        Ok(driver) => architect_docs::tools(Arc::new(driver)),
        Err(error) => {
            let _ = events.send(EngineEvent::Failed {
                session: None,
                message: format!("documentation tools unavailable: {error}"),
            });
            Vec::new()
        }
    }
}

/// The read-only subset (`read_doc`/`search_docs`/`list_docs`) of whatever
/// `external_tools` currently holds — what a sub-agent's `Investigation`
/// registry gets handed, same conservative-default reasoning as excluding
/// `write_file`/`run_command`/`spawn_subagents` from that registry in the
/// first place. Filters `external_tools` itself rather than calling
/// `doc_tools` again so a sub-agent only ever sees exactly what the parent
/// turn's own registry has (same driver instance, same enabled/disabled
/// state) — never a second, independently-built one.
fn read_only_doc_tools(external_tools: &[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>> {
    external_tools
        .iter()
        .filter(|tool| matches!(tool.name(), "read_doc" | "search_docs" | "list_docs"))
        .cloned()
        .collect()
}

/// The GitHub/Slack/Linear tools for whichever credentials are actually
/// configured. Slack and Linear stay exactly as before — a saved key is
/// synchronous and infallible to turn into a tool set, same as a built-in
/// tool's own errors surfacing from its own `call`. GitHub is the one
/// exception when `github_use_gh_cli` is set: resolving a token means
/// running `gh auth token` as a subprocess, which can fail (gh not
/// installed, not logged in) — that failure is reported via `Failed`
/// rather than silently registering no GitHub tools with no explanation.
async fn integration_tools(
    config: &IntegrationsConfig,
    events: &UnboundedSender<EngineEvent>,
) -> Vec<Arc<dyn Tool>> {
    let mut tools = Vec::new();

    match resolve_github_token(config).await {
        Ok(Some(token)) => tools.extend(architect_github::tools(token)),
        Ok(None) => {}
        Err(message) => {
            let _ = events.send(EngineEvent::Failed {
                session: None,
                message: format!("GitHub tools unavailable: {message}"),
            });
        }
    }

    if let Some(token) = &config.slack_token {
        tools.extend(architect_slack::tools(token));
    }
    if let Some(key) = &config.linear_api_key {
        tools.extend(architect_linear::tools(key));
    }
    tools
}

/// The saved token as-is, or — when `github_use_gh_cli` is set — whatever
/// `gh auth token` prints, so someone already logged into the GitHub CLI
/// doesn't need to paste a token or register an OAuth app at all.
async fn resolve_github_token(config: &IntegrationsConfig) -> Result<Option<String>, String> {
    if !config.github_use_gh_cli {
        return Ok(config.github_token.clone());
    }
    run_gh_auth_token("gh").await
}

/// Split out from `resolve_github_token` so tests can point it at a fake
/// `gh` script instead of relying on `$PATH` (which would mean mutating
/// process-wide env in a suite that runs tests in parallel).
async fn run_gh_auth_token(gh_command: &str) -> Result<Option<String>, String> {
    let output = tokio::process::Command::new(gh_command)
        .args(["auth", "token"])
        .output()
        .await
        .map_err(|error| {
            format!("could not run \"gh\" ({error}) — is the GitHub CLI installed and on PATH?")
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "\"gh auth token\" failed: {} — run \"gh auth login\" first",
            stderr.trim()
        ));
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if token.is_empty() {
        return Err("\"gh auth token\" printed no token".to_owned());
    }
    Ok(Some(token))
}

async fn send_mcp_servers_listed(
    config_store: &Option<ConfigStore>,
    events: &UnboundedSender<EngineEvent>,
) {
    let Some(config_store) = config_store else {
        return;
    };
    match config_store.list_mcp_servers().await {
        Ok(servers) => {
            let _ = events.send(EngineEvent::McpServersListed(servers));
        }
        Err(error) => tracing::warn!(%error, "could not list saved MCP servers"),
    }
}

async fn send_integrations_listed(
    config_store: &Option<ConfigStore>,
    events: &UnboundedSender<EngineEvent>,
) {
    let Some(config_store) = config_store else {
        return;
    };
    match config_store.integrations().await {
        Ok(integrations) => {
            let _ = events.send(EngineEvent::IntegrationsListed(integrations));
        }
        Err(error) => tracing::warn!(%error, "could not read saved integrations"),
    }
}

async fn send_docs_config_listed(
    config_store: &Option<ConfigStore>,
    events: &UnboundedSender<EngineEvent>,
) {
    let Some(config_store) = config_store else {
        return;
    };
    match config_store.docs().await {
        Ok(docs) => {
            let _ = events.send(EngineEvent::DocsConfigListed(docs));
        }
        Err(error) => tracing::warn!(%error, "could not read saved docs config"),
    }
}

/// Applies one completed OAuth login: on success, a read-modify-write that
/// saves the token into the matching `IntegrationsConfig` field, the same
/// as `Command::SaveIntegrations` does for a pasted one; on failure,
/// reports it the same global-failure path a bad MCP server connection
/// already uses. Returns whether the save succeeded, so the caller knows
/// whether `external_tools` needs rebuilding — pulled out of the
/// `oauth_rx` arm as its own function so it's testable without a real
/// browser or network (see `engine::tests`).
async fn apply_oauth_result(
    config_store: &Option<ConfigStore>,
    events: &UnboundedSender<EngineEvent>,
    provider: OAuthProvider,
    result: Result<String, String>,
) -> bool {
    let token = match result {
        Ok(token) => token,
        Err(message) => {
            let _ = events.send(EngineEvent::Failed {
                session: None,
                message: format!("{provider} login failed: {message}"),
            });
            return false;
        }
    };

    let Some(cs) = config_store else {
        let _ = events.send(EngineEvent::Failed {
            session: None,
            message: "profile storage unavailable".into(),
        });
        return false;
    };

    let mut integrations = match cs.integrations().await {
        Ok(integrations) => integrations,
        Err(error) => {
            let _ = events.send(EngineEvent::Failed {
                session: None,
                message: error.to_string(),
            });
            return false;
        }
    };
    match provider {
        OAuthProvider::GitHub => integrations.github_token = Some(token),
        OAuthProvider::Slack => integrations.slack_token = Some(token),
        OAuthProvider::Linear => integrations.linear_api_key = Some(token),
    }

    if let Err(error) = cs.set_integrations(integrations).await {
        let _ = events.send(EngineEvent::Failed {
            session: None,
            message: error.to_string(),
        });
        return false;
    }

    true
}

/// A one-line label for the sidebar, derived from the user's first message.
const MAX_TITLE_LEN: usize = 60;

fn title_from(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");

    match collapsed.char_indices().nth(MAX_TITLE_LEN) {
        Some((cut, _)) => format!("{}…", &collapsed[..cut]),
        None => collapsed,
    }
}

/// Persist whatever is new in `history` since the last call. A no-op when
/// there is no store — chat still works, it just isn't saved.
async fn persist_new_messages(
    store: Option<&SessionStore>,
    session: SessionId,
    history: &[Message],
    persisted_len: &mut usize,
) {
    let Some(store) = store else {
        return;
    };

    for (offset, message) in history[*persisted_len..].iter().enumerate() {
        let seq = (*persisted_len + offset) as i64;
        if let Err(error) = store.append_message(session, seq, message).await {
            tracing::warn!(%error, "could not persist a message");
        }
    }

    *persisted_len = history.len();
}

#[cfg(test)]
