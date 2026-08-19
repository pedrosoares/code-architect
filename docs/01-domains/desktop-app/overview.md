---
id: domain.desktop-app
type: domain
title: Desktop App Domain
relations:
  related_rules:
  - rule.ui-core-seam
  - rule.cancellation
  related_flows:
  - flow.send-message
  - flow.session-lifecycle
  - flow.model-config
  - flow.subagents
  - flow.oauth-login
---

The desktop app (`apps/desktop`, binary `code-architect`) is the composition root: Freya window, panels, and the `Engine` actor that bridges the UI to the agent, tools, MCP, integrations, session store, and config store. No business logic lives here beyond orchestration.

## Entry point (`main.rs`)

No CLI arguments. Sequence:
1. `tracing_subscriber` with `EnvFilter::from_env("ARCHITECT_LOG")`.
2. `static ENGINE: OnceLock<Engine>` set to `Engine::start(EngineConfig::from_env())` **before** `launch(...)` — so `main` can call `Engine::shutdown()` after the window closes (kills tracked background processes; a dead parent's children are not cleaned up automatically by the OS).
3. `launch(LaunchConfig::new().with_window(...))` — window 1440×900, min 960×600, dark theme, app id `net.pedrosoares.code_architect`.

Workspace root comes from `ARCHITECT_WORKSPACE` or the **current working directory**.

## `EngineConfig`

Fields: `kind: String`, `base_url: Option<String>`, `api_key: Option<String>`, `model: String`, `system: String`, `workspace_root: PathBuf` (sandbox root, parent of `.coder/`), `config_dir: Option<PathBuf>` (`None` → `~/.config/code-architect/`).

- **Default**: `kind "openai"`, `base_url Some("http://localhost:1234/v1")` (LM Studio), `model "qwen/qwen3.8-27b"`, `system` (the "code-architect" base prompt: read-before-edit, smallest change, verify with build/tests, short replies — see the [Agent Loop domain](../agent-loop/overview.md) System prompts), `workspace_root = cwd`.
- **`from_env()`**: `ARCHITECT_PROVIDER` / `ARCHITECT_BASE_URL` / `ARCHITECT_API_KEY` (required for anthropic) / `ARCHITECT_MODEL` / `ARCHITECT_WORKSPACE` / `ARCHITECT_CONFIG_DIR` (empty strings = unset).

## The channel pair

```
Freya UI (main thread) ──Command──►  worker on a dedicated Tokio runtime thread
                       ◄─EngineEvent──
```

`Engine { commands: UnboundedSender<Command>, events: Arc<Mutex<Option<UnboundedReceiver<EngineEvent>>>> (one-shot take_events()), config, shutdown: UnboundedSender<sync::mpsc::Sender<()>> }`. The `shutdown` channel is a **std** channel nested in a tokio one, deliberately separate from `Command` — `Command` derives `PartialEq/Eq`, which sender types don't implement (same constraint keeps sub-agent requests out of `Command`).

## `Command` — the complete 21 variants

`Send { session: SessionId, text: String, images: Vec<Attachment> }` · `Cancel(SessionId)` · `LoadSession(SessionId)` · `DeleteSession(SessionId)` · `ListProfiles` · `SaveProfile(Profile)` · `DeleteProfile(Uuid)` · `ActivateProfile(Uuid)` · `DeactivateProfile` · `UseAdHocModel { base_url, api_key, model }` · `ListMcpServers` · `SaveMcpServer(McpServerConfig)` · `DeleteMcpServer(Uuid)` · `ListIntegrations` · `SaveIntegrations(IntegrationsConfig)` · `ListDocsConfig` · `SaveDocsConfig(DocsConfig)` · `StartOAuthLogin(OAuthProvider)` · `Rollback { session, up_to_seq: i64 }` · `Compact(SessionId)` · `ListModels { base_url, api_key: Option<String> }`.

`Attachment { media_type, bytes: Vec<u8> }` — raw bytes end to end; base64 is produced only in the worker.

## `EngineEvent` — the complete 18 variants

`Agent { session, event: AgentEvent }` · `Cancelled(session)` · `Failed { session: Option<SessionId>, message }` · `HistoryLoaded { session, messages }` · `SessionsListed(Vec<SessionSummary>)` · `SessionDeleted(session)` · `ProfilesListed { profiles, active: Option<Uuid> }` · `McpServersListed(Vec<McpServerConfig>)` · `FileChanged { session, entry: FileChangeEntry }` · `FileChangesLoaded { session, changes }` · `IntegrationsListed(IntegrationsConfig)` · `DocsConfigListed(DocsConfig)` · `Process(ProcessEvent)` · `PlanUpdated { session, plan }` · `PlanLoaded { session, plan: Option<Plan> }` · `Compacted { session, summary }` · `ModelsListed { base_url, models: Vec<String> }` · `AdHocModelActivated { model }`.

`Failed.session == None` is reserved for genuinely global failures (bad startup provider, persistence, MCP connect, OAuth, gh-CLI, doc-tools construction).

## Worker startup

Open `SessionStore` (best-effort; failure → one global `Failed "persistence unavailable: {error}"`, chat still works) → open `ConfigStore` (same) → load profiles + active profile (active profile **overrides** the env-derived provider config) → `ProviderRegistry::default().build(&provider_config)` (failure → one global `Failed`, `provider = None`, settings panel stays usable) → `rebuild_external_tools()` (MCP connections + GitHub/Slack/Linear + doc tools) → `ProcessRegistry::new()` + the always-on process tools and plan tools → **startup resume**: list sessions, load the most recent one's messages → `HistoryLoaded`, then `FileChangesLoaded`, then `PlanLoaded` (that ordering is relied on by `state.rs` and pinned by tests), then `SessionsListed`, `ProfilesListed`, `McpServersListed`, `IntegrationsListed`, `DocsConfigListed`.

Main loop: `tokio::select!` over **seven** inputs — `commands`, `task_rx` (finished turns), `compact_rx`, `subagent_rx`, `oauth_rx`, `process_rx` (forwarded as `EngineEvent::Process`), `shutdown_rx` (`kill_all()` then ack).

Per-session: `sessions: HashMap<SessionId, SessionSlot { history, persisted_len, plan }>` (lazily populated, **never replaced, only appended** — switching back to a mid-turn session shows live state, never a stale DB snapshot) and `running: HashMap<SessionId, CancellationToken>` (one concurrent turn per session, many across sessions).

## The turn pipeline (`Command::Send` → `TaskOutcome`)

1. **UI-minted `SessionId`**: a first-ever id → `store.create_session_with_id(session, &provider_config.kind, &model)` + title from the first message (whitespace-collapsed, 60 chars, `…`; `"Image"` for image-only). A fresh `SessionsListed` is emitted.
2. User message (text and/or images) is pushed into the slot and persisted (`append_message` for the unsaved suffix tracked by `persisted_len`).
3. No provider → `Failed { session: Some, "no working API configuration — open Settings to add or fix one" }`.
4. `mem::take` the history, insert a fresh `CancellationToken` into `running`, `spawn_turn` (its own `tokio::spawn`).
5. **`spawn_turn`** builds a **fresh per-turn tool registry**: `ToolRegistry::with_default_tools` (user turns) or `with_investigation_tools` (sub-agent turns), then registers the shared long-lived `external_tools` (MCP + integrations + docs), `process_tools`, `plan_tools` (empty for sub-agents), and configures the context with `with_recorder(ChannelRecorder(file_change_tx))`, `with_plan_recorder(ChannelPlanRecorder(plan_tx))`, `with_current_plan`, `with_sub_agent_spawner(EngineSubAgentSpawner { parent, subagent_tx, run_sequentially: architect_llm::is_local(&provider_config) })`. Registry build failure → `NoTools` + `tools_error` (turn still runs; later `Failed "tools unavailable: {message}"`).
6. `Agent::new(provider, tools, AgentConfig::new(model).system(system).reasoning(Reasoning::VISIBLE))`; system prompt = `config.system` + docs-protocol block **only when** doc tools are present (sub-agent turns use `SUBAGENT_SYSTEM`).
7. `Agent::run_turn(&mut history, &raw_tx, cancel)` with a local `AgentEvent` channel + a forwarder task re-sending each as `EngineEvent::Agent { session, event }` (the tagging is what lets one shared stream serve many sessions). A second forwarder drains `plan_rx` → `PlanUpdated` **live** mid-turn.
8. After the turn: await forwarders, `try_recv`-drain `file_change_rx`, send `TaskOutcome { session, history, file_changes, plan, tools_error, result: Result<TurnOutcome, AgentError> }`.
9. **`task_rx` arm**: remove from `running`; put history back, persist the new tail; each file change → `store.record_file_change(session, message_seq, &change)` with **`message_seq = slot.history.len() - 1`** (all changes attributed to the turn's last message — rollback granularity is per-turn) + a live `FileChanged` event; persist the plan (`save_plan`); resolve a waiting sub-agent's oneshot (success = child's last message text, or a classified failure); `tools_error` → `Failed`; `Ok` → nothing; `Cancelled` → `Cancelled(session)`; `Err(other)` → `Failed { session: Some, message: error.to_string() }`.

## Tool registration conditions

- **Always (per user turn)**: the 10 built-ins, the 3 process tools, the 2 plan tools.
- **GitHub** (when `resolve_github_token` → `Some`): saved `github_token`, or `gh auth token` subprocess when `github_use_gh_cli` (failure → global `Failed` with hints "is the GitHub CLI installed and on PATH?" / "run `gh auth login` first"). Eight read-only tools.
- **Slack** (when `slack_token` present): 3 tools. **Linear** (when `linear_api_key` present): 1 tool. **Docs** (when `DocsConfig.enabled`): 6 tools (sub-agents get only the 3 read-only ones).
- **MCP**: every *enabled* saved server's tools from live connections.

## Sub-agent machinery

`spawn_subagents` → `SubAgentSpawner::spawn(prompt, path)` → the engine's `EngineSubAgentSpawner` submits `SpawnSubAgentRequest { parent, prompt, path, reply: oneshot::Sender }` on its own `subagent_tx` channel. The worker's `subagent_rx` arm: containment-checks `path` with `ToolContext::resolve` **plus** requires it to be an existing directory (it becomes the child's whole workspace root); mints `child = SessionId::new()`; `store.create_child_session_with_id(child, parent, kind, model)`; wraps the prompt in a `scoped_prompt` preamble; persists it; re-broadcasts `SessionsListed`; parks the reply in `pending_subagent_replies[child]`; `spawn_turn` with `workspace_root = resolved`, `SUBAGENT_SYSTEM`, the Investigation registry, a fresh token, its own spawner (so grandchildren are possible). The child runs in the background as an ordinary session — its `Agent` events stream to the UI nested under its parent; when its `TaskOutcome` lands, the oneshot resolves and the parent's blocked tool call returns the child's final text. Sequential vs concurrent is `is_local(&provider_config)`.

## Cancellation, compaction, deletion

- `Command::Cancel` → `token.cancel()` (harmless if idle) → `AgentError::Cancelled` → `Cancelled(session)`.
- `Command::DeleteSession` cancels first (nothing left to persist the result against), hard-deletes (FK cascade), emits `SessionDeleted`; if the deleted session was active, the UI starts a fresh chat.
- `Command::Compact`: refused (via `Failed`) if a turn is running or history is empty. `spawn_compact` runs the history through `Agent::run_turn` with `NoTools` + `max_iterations(1)` (cancellation/error handling from the one place that gets them right); on success `slot.history = vec![Message::assistant(summary)]`, `store.replace_messages`, `Compacted { session, summary }`. Identity, file changes, and plan survive. `Status::Compacting` is set optimistically client-side and folds into `is_busy()`.

## MCP connection management

`reconnect_mcp` (called from `rebuild_external_tools`): for every enabled server — `Stdio { command, args, env }` → `architect_mcp::connect`, `Http { url, bearer_token }` → `connect_http` — best-effort per server (one failure → global `Failed`, the rest proceed). Connections are held in the worker and **dropped wholesale on every rebuild**. Rebuild triggers: startup, `SaveMcpServer`, `DeleteMcpServer`, `SaveIntegrations`, `SaveDocsConfig`, and after a successful OAuth login. Whole-list rebuild (not diff) is deliberate — server lists are small, and this isn't a hot path.

## OAuth login (`src/oauth.rs`)

RFC 8252 native-app **authorization code + PKCE** with a short-lived local loopback callback listener (no device flow). `run_login(provider)`: generate PKCE (verifier = 3×UUID v4 raw bytes → 48 → 64 base64url chars, within RFC 7636's 43–128; challenge = SHA-256 → base64url-no-pad, `S256`), `state` = UUID, `webbrowser::open(authorize_url)`, bind `127.0.0.1:53682` (fixed `REDIRECT_PORT`, 300s timeout), read one raw HTTP request, answer "You can close this tab…", parse `code`/`state`/`error`, verify `state` (mismatch → "possible CSRF"), `exchange_code` (POST form with `code_verifier`; GitHub adds `client_secret`; Linear adds `grant_type`). Scopes: GitHub `scope=repo`; Slack `user_scope=channels:history,groups:history,im:history,mpim:history` (PKCE Slack apps can only request user-token scopes — Login yields an `xoxp-` user token, not `xoxb-`); Linear `scope=read`. Token extraction: GitHub/Linear top-level `access_token`; Slack **nested** `authed_user.access_token` (explicitly not the bot token in the same response). Client IDs/secret are `env!` constants baked by `build.rs`. `apply_oauth_result` does a read-modify-write into `IntegrationsConfig` via `ConfigStore::set_integrations` — machine-global plaintext, the same tradeoff `Profile.api_key` makes — then rebuilds `external_tools` and re-sends `IntegrationsListed` (the refreshed list is the confirmation; no dedicated success event).

## Panels

- **`app.rs`** — root component: dark theme; `Transcript` as `State` context; `ScrollToToolCall(Option<String>)` cross-panel signal; the `Engine` handle (from the `ENGINE` static, or `Engine::start(EngineConfig::from_env())` for headless tests); the **event pump** — a once-only `use_hook` takes the event receiver and spawns `while let Some(e) = events.recv().await { transcript.write().apply(&e) }` — no polling, no timers.
- **`sessions.rs`** — left sidebar: "+ New Chat" (pure client-side `start_new_chat`, no engine round-trip), list of `SessionSummary` with sub-agent children indented under their parent (spawn order), busy dot per `conversation.status.is_busy()`, click-to-select (resident → `switch_to`, else `engine.load_session`), Delete per row.
- **`transcript.rs`** — center: pure renderer of `Row`s; auto-scroll on row change; `Row::User` (image thumbnails + text card), `Row::Assistant` (reasoning in a `Disclosure`, text in a card; trimmed-empty turns render `None`), `ToolRow` (status glyph `●/✓/✗`, click-to-expand output, **scroll-to-tool-call** via a per-row a11y id watching the `ScrollToToolCall` signal), `Row::Error`, `Row::Compacted`. Message cards are `SelectableText`, not markdown — the deliberate copy-paste tradeoff.
- **`composer.rs`** — bottom of center: `ComposerInput` (hand-rolled on `freya_edit` because Freya's `Input` is hard-coded single-line; wraps, auto-grows to 160px, then scrolls); Enter submits / Shift+Enter newline / Escape unfocuses; Send becomes **Stop** while busy; Attach → native `rfd` file picker (png/jpg/jpeg/gif/webp) → `Attachment { media_type, bytes }` (raw bytes, no base64 in the UI); placeholder "Type a message... (@ to reference files)" — `@`-mention handling is **not implemented**.
- **`inspector.rs`** — right panel, **five** tabs (`Files, Tools, Diff, Processes, Plan`): Files (deduped per path; click → Diff; per-file **Roll Back** → confirmation popup → `engine.rollback(session, entry.message_seq - 1)`); Tools (click → writes the call id into `ScrollToToolCall`); Diff (`similar::TextDiff` with syntax highlighting via freya's `code_editor` for rs/py/js/ts/json/bash, plain diff-colored text otherwise); Processes (global; status glyphs; click-to-expand live log); Plan (per-conversation; status glyphs; no interaction).
- **`settings.rs`** — popup, five sections: **Profiles** (list/add/edit/delete/activate/deactivate; a "Default" row for the engine's startup config with "Use" → `DeactivateProfile`); **LM Studio** (its own page — base URL + optional key held only in `use_state`, "never saved"; "Fetch models" → `list_models`; each row's "Use" → `UseAdHocModel`, which rebuilds the provider in place without touching `ConfigStore`); **MCP servers** (add/edit/enable/disable/delete; stdio command+args+env or HTTP url+bearer; new saves are `enabled: true`); **Integrations** (GitHub token + **Login** button + gh-CLI toggle — when on, the token input is hidden and `to_config` forces `github_token: None`; Slack token + Login; Linear key + Login; "✓ Connected" when saved; "Waiting for browser…" while pending); **Documentation** (enable toggle, default on; vault path, blank → workspace `docs/`; driver hard-fixed to `"obsidian"`).
- **`shell.rs`** — window chrome: header (model/provider chip — ad-hoc model is a second tier between a saved profile and the startup default; vision badge from `supports_vision`; Compact button; Settings button), status bar (workspace path, token totals, context fill or "unsized model", spend or "unpriced model"), the three resizable panels with collapse-to-strip toggles (the distinct-key remount trick).

## Engine tests

41 total (33 hermetic + 8 `#[ignore]`d live): config defaults/failure; **concurrent-sessions flagship** (a slow wiremock SSE for one session doesn't block a fast one); profile CRUD + activation; MCP save/list ordering (a failed connection reports *before* `McpServersListed`); integrations/docs config persistence; OAuth result application; exact tool-name registration per credential state; the docs-protocol system-prompt gating; `gh auth token` via fake `gh` scripts; process events into a real `Transcript`; session delete; compaction (including DB re-open); model listing + ad-hoc activation (asserts `profiles.json` is never created); file-changes/plan resume; rollback (restores on-disk content, refused while a turn is running); image attachment (asserts the `image_url` part in the recorded request body *and* the base64 in the reopened DB); **`spawn_subagents_creates_a_child_session_and_reports_its_summary_back`** (three bounded wiremock mocks play parent→child→parent); **`write_plan_reaches_the_inspector_before_the_turn_finishes`**. The live tests drive the whole pipeline end to end in tempdir workspaces.