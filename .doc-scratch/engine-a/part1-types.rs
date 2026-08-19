//! The bridge between the UI and the agent.
//!
//! Freya owns the main thread and runs its own executor; the agent needs a
//! Tokio runtime for HTTP. So the agent lives on a dedicated runtime thread and
//! the two sides talk only over channels:
//!
//! ```text
//!   UI (main thread) ──Command──►  worker (Tokio runtime)
//!                    ◄─EngineEvent──
//! ```
//!
//! Channels are unbounded in both directions: a busy UI must never stall the
//! model stream, and a queued command must never block a render.
//!
//! Nothing above this module knows a network exists, and nothing below it knows
//! a UI does — the same worker would serve a CLI or a socket daemon unchanged.
//!
//! Every session's turn runs as its own spawned task, not inline in the
//! command loop — see [`worker`]'s doc comment for why, and for how that's
//! what lets the UI use one session while another keeps streaming.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use architect_agent::{
    Agent, AgentConfig, AgentError, AgentEvent, NoTools, ToolExecutor, TurnOutcome,
};
use architect_config::{
    ConfigStore, DocsConfig, IntegrationsConfig, McpServerConfig, McpTransport, Profile,
};
use architect_core::{ContentBlock, FileChange, FileChangeEntry, Message, Plan, StopReason};
use architect_llm::{Provider, ProviderConfig, ProviderRegistry, Reasoning};
use architect_mcp::McpConnection;
use architect_session::{SessionId, SessionStore, SessionSummary};
use architect_tools::{ChannelPlanRecorder, ChannelRecorder, Tool, ToolRegistry};
use base64::Engine as _;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::oauth::{self, OAuthProvider};

/// One image attached to a user message — the composer's "Attach" button,
/// read straight off disk. Raw bytes, not base64, all the way from the
/// composer through `Command::Send` and into `state::Row::User`; base64 is
/// only ever produced right here in the worker, the one place a `Message`
/// actually gets built (see `Command::Send`'s handler), and only ever
/// decoded back in `state.rs`'s history-replay path when a resumed
/// session's images need reconstructing for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// e.g. `"image/png"`, `"image/jpeg"`.
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// What the UI asks the agent to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Send a message within a session. If the worker hasn't seen this id
    /// before, this is its first message — the UI already minted the id
    /// (see [`SessionId::new`]'s docs) before sending it, so a session row
    /// is created for it here rather than the id being handed back later.
    Send {
        session: SessionId,
        text: String,
        images: Vec<Attachment>,
    },
    /// Stop the turn in flight for a session. Harmless if none is running.
    Cancel(SessionId),
    /// Fetch a session's full history if it is not already resident in the
    /// worker's in-memory state — as when switching to a session that
    /// hasn't been touched yet this app run. A session with a turn in
    /// flight, or one already loaded earlier this run, is left alone: its
    /// live state must never be clobbered by a stale DB snapshot.
    LoadSession(SessionId),
    /// Permanently remove a saved session and everything recorded against
    /// it. If a turn is in flight for it, that turn is cancelled first —
    /// there is nothing left to persist its result against.
    DeleteSession(SessionId),
    /// Read the saved API configurations again — also sent unprompted after
    /// every command below, so the settings panel never has to poll.
    ListProfiles,
    /// Add a new configuration, or update an existing one (matched by id).
    SaveProfile(Profile),
    DeleteProfile(Uuid),
    /// Make this configuration the one live turns use, from now on. Rebuilds
    /// the provider for future turns; a turn already in flight keeps using
    /// whichever provider it started with.
    ActivateProfile(Uuid),
    /// Stop using any saved configuration — go back to the one this engine
    /// started with (env vars, or the built-in local-server default).
    DeactivateProfile,
    /// Make this OpenAI-compatible server/model the one live turns use,
    /// from now on — same immediate effect as `ActivateProfile`, but
    /// nothing is written to `ConfigStore`. The LM Studio settings page
    /// this comes from is deliberately unsaved: what a local server has
    /// loaded changes over time, so persisting a snapshot of it would only
    /// ever be stale the moment it's written.
    UseAdHocModel {
        base_url: String,
        api_key: Option<String>,
        model: String,
    },
    /// Read the saved MCP servers again — also sent unprompted after every
    /// command below.
    ListMcpServers,
    /// Add a new MCP server, or update an existing one (matched by id).
    /// Reconnects every enabled server afterward, so a name/command/args
    /// edit or an enabled-flag flip takes effect immediately.
    SaveMcpServer(McpServerConfig),
    /// Remove an MCP server and reconnect the remaining enabled ones.
    DeleteMcpServer(Uuid),
    /// Undo every file change recorded after `up_to_seq` for this session —
    /// restoring or deleting files on disk, per
    /// `SessionStore::reverse_to_point`. Refused (via `Failed`) while a turn
    /// is running for the session, the same as a second concurrent `Send`
    /// would be: a rollback must never race a live turn's own writes.
    /// Chat messages are never touched.
    Rollback {
        session: SessionId,
        up_to_seq: i64,
    },
    /// Read the saved GitHub/Slack/Linear credentials again — also sent
    /// unprompted after `SaveIntegrations`.
    ListIntegrations,
    /// Replace the saved integrations wholesale (there is only ever one of
    /// each credential) and rebuild the GitHub/Slack/Linear tools from it,
    /// same as `SaveMcpServer` rebuilds MCP's.
    SaveIntegrations(IntegrationsConfig),
    /// Read the saved documentation-driver config again — also sent
    /// unprompted after `SaveDocsConfig`.
    ListDocsConfig,
    /// Replace the saved documentation-driver config wholesale (there is
    /// only ever one) and rebuild the doc tools from it, same as
    /// `SaveIntegrations` rebuilds GitHub/Slack/Linear's.
    SaveDocsConfig(DocsConfig),
    /// Open the browser and run one OAuth login for a GitHub/Slack/Linear
    /// credential — see `oauth::run_login`. Returns immediately; the
    /// result arrives later on `oauth_rx`, same shape as `Command::Send`
    /// never awaiting the turn it spawns.
    StartOAuthLogin(OAuthProvider),
    /// Ask the model to summarize this session's conversation so far, then
    /// replace its message history with just that summary — frees up
    /// context the same way starting a new chat would, without losing the
    /// session's identity or its recorded file changes/plan. Refused (via
    /// `Failed`) while a turn is already running for the session, the same
    /// as `Rollback`; also refused if there is nothing to compact yet.
    Compact(SessionId),
    /// Fetch what models a running OpenAI-compatible server (LM Studio,
    /// vLLM, Ollama, ...) currently has loaded, via `GET {base_url}/models`
    /// — so a saved configuration's model can be picked from what's
    /// actually available instead of hand-typed. Not tied to any saved
    /// `Profile`: the settings form calls this against whatever base_url/
    /// api_key is currently filled in, before the profile is even saved.
    ListModels {
        base_url: String,
        api_key: Option<String>,
    },
}

/// What the agent reports back.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// One step of a session's turn.
    Agent {
        session: SessionId,
        event: AgentEvent,
    },
    /// A session's turn stopped because the user asked it to.
    Cancelled(SessionId),
    /// Something failed. `session` is `Some` when it's attributable to one
    /// session's turn or operation — shown in that conversation specifically,
    /// wherever it's being viewed, not wherever happens to be on screen when
    /// the event arrives. `None` is a genuinely global failure (a bad
    /// startup provider config, profile storage) shown in whichever
    /// conversation is currently on screen.
    Failed {
        session: Option<SessionId>,
        message: String,
    },
    /// A session's full message history was loaded — on startup (resuming
    /// the most recent one) or after `Command::LoadSession`. Carries the
    /// session id so the UI knows which one is now active.
    HistoryLoaded {
        session: SessionId,
        messages: Vec<Message>,
    },
    /// The list of saved sessions changed (or was just read at startup) — how
    /// the sidebar knows what to show without polling the store itself.
    SessionsListed(Vec<SessionSummary>),
    /// A session was permanently deleted. Carries the id so the UI can clear
    /// its transcript if that session happened to be the active one — the
    /// same signal `SessionsListed` alone can't give, since a deleted id is
    /// exactly what's missing from that list.
    SessionDeleted(SessionId),
    /// The saved API configurations changed (or were just read at startup) —
    /// how the settings panel knows what to show without polling the store.
    ProfilesListed {
        profiles: Vec<Profile>,
        active: Option<Uuid>,
    },
    /// The saved MCP servers changed (or were just read at startup) — how
    /// the settings panel knows what to show without polling the store.
    /// Carries only the saved configs, not connection status: a server that
    /// fails to connect is reported once via `Failed`, not tracked here.
    McpServersListed(Vec<McpServerConfig>),
    /// One file changed during a live turn — sent alongside persisting it,
    /// not just after the fact.
    FileChanged {
        session: SessionId,
        entry: FileChangeEntry,
    },
    /// A session's full file-change history was loaded — sent right after
    /// `HistoryLoaded`, on startup resume and after `Command::LoadSession`;
    /// also sent after a successful `Command::Rollback`, in place of a
    /// dedicated "it worked" event — the shorter list *is* the
    /// confirmation.
    FileChangesLoaded {
        session: SessionId,
        changes: Vec<FileChangeEntry>,
    },
    /// The saved GitHub/Slack/Linear credentials changed (or were just read
    /// at startup) — how the settings panel knows what to show without
    /// polling the store. Carries the config as-is, tokens included — the
    /// same "plaintext, machine-local" tradeoff `Profile.api_key` already
    /// makes.
    IntegrationsListed(IntegrationsConfig),
    /// The saved documentation-driver config changed (or was just read at
    /// startup) — how the settings panel knows what to show without
    /// polling the store.
    DocsConfigListed(DocsConfig),
    /// A process this app started (via `start_process`) reported progress —
    /// it started, produced a line of output, or exited. Global, not tied
    /// to any one session — the same reasoning MCP connections and
    /// `external_tools` already get: a process isn't owned by whichever
    /// conversation happened to start it any more than a connected MCP
    /// server is.
    Process(architect_tools::ProcessEvent),
    /// A session's plan changed — `write_plan` saved a new one, sent from
    /// the same post-turn persistence step `FileChanged` already uses (see
    /// its own doc comment for why: only the last plan a turn wrote
    /// matters, so there's nothing to stream mid-turn the way `FileChanged`
    /// theoretically could).
    PlanUpdated { session: SessionId, plan: Plan },
    /// A session's saved plan was loaded — sent right after
    /// `FileChangesLoaded`, on startup resume and after `Command::
    /// LoadSession`. `None` means the session has never called
    /// `write_plan`, same as `Conversation.plan` starting out.
    PlanLoaded {
        session: SessionId,
        plan: Option<Plan>,
    },
    /// `Command::Compact` finished: this session's message history was
    /// replaced by a single summary, freeing up its context. Carries the
    /// summary text so the transcript can show what happened — the same
    /// role `messages` plays in `HistoryLoaded`, except there is only ever
    /// one message here, and it isn't fed through `Conversation::
    /// from_history`: a compacted conversation is collapsed, not replayed.
    Compacted { session: SessionId, summary: String },
    /// `Command::ListModels` finished. Carries `base_url` back so a stale
    /// response (the user changed the field mid-fetch) is at least
    /// identifiable — the same "attribute results to what asked for them"
    /// idea `FileChanged` carrying `session` already follows.
    ModelsListed {
        base_url: String,
        models: Vec<String>,
    },
    /// `Command::UseAdHocModel` finished: this model is now what live turns
    /// use, the same as `ProfilesListed { active: Some(id), .. }`'s effect
    /// but for a model that was never saved as a `Profile`. Mutually
    /// exclusive with a saved profile being active — `Transcript::apply`
    /// clears one whenever the other arrives.
    AdHocModelActivated { model: String },
}

/// Where to send requests, as which model, and where its tools and
/// persistence are rooted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    pub kind: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: String,
    pub system: String,
    /// Sandbox root for file/command tools, and the parent of `.coder/`.
    pub workspace_root: PathBuf,
    /// Where saved API configurations (`architect_config::ConfigStore`) live.
    /// `None` uses the real, machine-global `~/.config/code-architect/` —
    /// tests point this at a tempdir so they never touch (or depend on) the
    /// developer's own saved profiles.
    pub config_dir: Option<PathBuf>,
}

impl Default for EngineConfig {
    /// A local OpenAI-compatible server, which is what a developer running this
    /// on their own machine most likely has.
    fn default() -> Self {
        Self {
            kind: "openai".into(),
            base_url: Some("http://localhost:1234/v1".into()),
            api_key: None,
            model: "qwen/qwen3.8-27b".into(),
            system: "You are a coding assistant. Be concise.".into(),
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            config_dir: None,
        }
    }
}

impl EngineConfig {
    /// Read configuration from the environment, falling back to the defaults.
    ///
    /// | Variable | Meaning |
    /// |---|---|
    /// | `ARCHITECT_PROVIDER` | `openai` (default) or `anthropic` |
    /// | `ARCHITECT_BASE_URL` | endpoint root, including any version segment |
    /// | `ARCHITECT_API_KEY` | required for `anthropic` |
    /// | `ARCHITECT_MODEL` | model id |
    /// | `ARCHITECT_WORKSPACE` | sandbox root; defaults to the current directory |
    /// | `ARCHITECT_CONFIG_DIR` | where saved API configurations live; defaults to `~/.config/code-architect/` |
    pub fn from_env() -> Self {
        let default = Self::default();
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());

        let kind = var("ARCHITECT_PROVIDER").unwrap_or(default.kind);
        // A hosted provider has its own canonical URL; only override it when
        // asked, so pointing at Anthropic needs no base URL at all.
        let base_url = var("ARCHITECT_BASE_URL")
            .or_else(|| (kind == "openai").then(|| default.base_url.clone().unwrap_or_default()));

        Self {
            kind,
            base_url,
            api_key: var("ARCHITECT_API_KEY"),
            model: var("ARCHITECT_MODEL").unwrap_or(default.model),
            system: default.system,
            workspace_root: var("ARCHITECT_WORKSPACE")
                .map(PathBuf::from)
                .unwrap_or(default.workspace_root),
            config_dir: var("ARCHITECT_CONFIG_DIR").map(PathBuf::from),
        }
    }

    /// Short label for the header, e.g. `qwen/qwen3.8-27b`.
    pub fn label(&self) -> &str {
        &self.model
    }
}

/// Handle to the running agent.
#[derive(Clone)]
pub struct Engine {
    commands: UnboundedSender<Command>,
    events: Arc<std::sync::Mutex<Option<UnboundedReceiver<EngineEvent>>>>,
    config: EngineConfig,
    /// A *std*, non-async channel — `Engine::shutdown` is a plain
    /// synchronous method callable from `main()`, which isn't inside any
    /// async runtime itself (it just blocks on `launch(...)`); this is how
    /// it bridges into the worker's tokio one. Separate from `commands`
    /// (rather than a new `Command` variant) since `Command` derives
    /// `PartialEq`/`Eq`, which no sender type implements.
    shutdown: UnboundedSender<std::sync::mpsc::Sender<()>>,
}

impl Engine {
    /// Start the worker thread. Returns immediately.
    pub fn start(config: EngineConfig) -> Self {
        let (command_tx, command_rx) = unbounded_channel();
        let (event_tx, event_rx) = unbounded_channel();
        let (shutdown_tx, shutdown_rx) = unbounded_channel();
        let worker_config = config.clone();

        // A dedicated runtime rather than Freya's executor: reqwest needs a
        // Tokio reactor, and model streaming must not share a thread with
        // rendering.
        std::thread::Builder::new()
            .name("architect-agent".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = event_tx.send(EngineEvent::Failed {
                            session: None,
                            message: format!("could not start the agent runtime: {error}"),
                        });
                        return;
                    }
                };

                runtime.block_on(worker(worker_config, command_rx, event_tx, shutdown_rx));
            })
            .expect("spawning the agent thread");

        Self {
            commands: command_tx,
            events: Arc::new(std::sync::Mutex::new(Some(event_rx))),
            config,
            shutdown: shutdown_tx,
        }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Take the event receiver. Returns `None` after the first call — there is
    /// one consumer, the UI's drain loop.
    pub fn take_events(&self) -> Option<UnboundedReceiver<EngineEvent>> {
        self.events.lock().expect("event lock").take()
    }

    pub fn send(&self, session: SessionId, text: impl Into<String>, images: Vec<Attachment>) {
        let _ = self.commands.send(Command::Send {
            session,
            text: text.into(),
            images,
        });
    }

    /// Stop the turn in flight for a session. Harmless when nothing is
    /// running for it.
    pub fn cancel(&self, session: SessionId) {
        let _ = self.commands.send(Command::Cancel(session));
    }

    /// Switch to a previously saved session. A no-op on the worker side if
    /// it's already resident (currently running, or loaded earlier this
    /// app run) — the UI checks that itself before calling this at all.
    pub fn load_session(&self, session: SessionId) {
        let _ = self.commands.send(Command::LoadSession(session));
    }

    /// Permanently delete a saved session.
    pub fn delete_session(&self, session: SessionId) {
        let _ = self.commands.send(Command::DeleteSession(session));
    }

    /// Undo every file change recorded after `up_to_seq` for this session —
    /// restoring or deleting files on disk. Chat messages are untouched.
    pub fn rollback(&self, session: SessionId, up_to_seq: i64) {
        let _ = self.commands.send(Command::Rollback { session, up_to_seq });
    }

    /// Summarize this session's conversation and replace its history with
    /// the summary — see `Command::Compact`'s docs.
    pub fn compact(&self, session: SessionId) {
        let _ = self.commands.send(Command::Compact(session));
    }

    /// Fetch the model list from an OpenAI-compatible server at `base_url`
    /// — see `Command::ListModels`'s docs.
    pub fn list_models(&self, base_url: impl Into<String>, api_key: Option<String>) {
        let _ = self.commands.send(Command::ListModels {
            base_url: base_url.into(),
            api_key,
        });
    }

    /// Read the saved API configurations again — the settings panel calls
    /// this on open so it never shows data another window might have
    /// changed since the engine last broadcast it.
    pub fn list_profiles(&self) {
        let _ = self.commands.send(Command::ListProfiles);
    }

    /// Add a new saved API configuration, or update an existing one (matched
    /// by `profile.id`).
    pub fn save_profile(&self, profile: Profile) {
        let _ = self.commands.send(Command::SaveProfile(profile));
    }

    pub fn delete_profile(&self, id: Uuid) {
        let _ = self.commands.send(Command::DeleteProfile(id));
    }

    /// Make a saved configuration the one live turns use, from now on.
    pub fn activate_profile(&self, id: Uuid) {
        let _ = self.commands.send(Command::ActivateProfile(id));
    }

    /// Stop using any saved configuration — go back to the one this engine
    /// started with.
    pub fn deactivate_profile(&self) {
        let _ = self.commands.send(Command::DeactivateProfile);
    }

    /// Make this OpenAI-compatible model the one live turns use, from now
    /// on — see `Command::UseAdHocModel`'s docs for why this never touches
    /// saved configurations.
    pub fn use_ad_hoc_model(
        &self,
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) {
        let _ = self.commands.send(Command::UseAdHocModel {
            base_url: base_url.into(),
            api_key,
            model: model.into(),
        });
    }

    /// Read the saved MCP servers again — the settings panel calls this on
    /// open so it never shows data another window might have changed.
    pub fn list_mcp_servers(&self) {
        let _ = self.commands.send(Command::ListMcpServers);
    }

    /// Add a new MCP server, or update an existing one (matched by id).
    pub fn save_mcp_server(&self, server: McpServerConfig) {
        let _ = self.commands.send(Command::SaveMcpServer(server));
    }

    pub fn delete_mcp_server(&self, id: Uuid) {
        let _ = self.commands.send(Command::DeleteMcpServer(id));
    }

    /// Read the saved GitHub/Slack/Linear credentials again — the settings
    /// panel calls this on open, same reason `list_mcp_servers` does.
    pub fn list_integrations(&self) {
        let _ = self.commands.send(Command::ListIntegrations);
    }

    /// Replace the saved GitHub/Slack/Linear credentials wholesale.
    pub fn save_integrations(&self, integrations: IntegrationsConfig) {
        let _ = self.commands.send(Command::SaveIntegrations(integrations));
    }

    /// Open the browser and run one OAuth login for `provider`. The
    /// resulting token is saved the same way `save_integrations` saves a
    /// pasted one — watch for the next `IntegrationsListed`/`Failed`.
    pub fn start_oauth_login(&self, provider: OAuthProvider) {
        let _ = self.commands.send(Command::StartOAuthLogin(provider));
    }

    /// Read the saved documentation-driver config again — the settings
    /// panel calls this on open, same reason `list_integrations` does.
    pub fn list_docs_config(&self) {
        let _ = self.commands.send(Command::ListDocsConfig);
    }

    /// Replace the saved documentation-driver config wholesale.
    pub fn save_docs_config(&self, docs: DocsConfig) {
        let _ = self.commands.send(Command::SaveDocsConfig(docs));
    }

    /// Kills every process this engine started and waits, briefly and
    /// boundedly, for confirmation — call once, after the window closes.
    /// If the worker is already gone (channel closed), this is a no-op:
    /// nothing left to shut down.
    pub fn shutdown(&self) {
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        if self.shutdown.send(ack_tx).is_ok() {
            let _ = ack_rx.recv_timeout(std::time::Duration::from_secs(3));
        }
    }
}

/// Per-session in-memory state the worker keeps for as long as the app runs.
/// Populated lazily — either by `Command::Send`'s first message for a
/// brand-new id, or by `Command::LoadSession` fetching an existing one from
/// disk — and never replaced afterward, only appended to. That's what makes
/// switching back to a session mid-turn show it live rather than a stale
/// snapshot: its `history` is never overwritten, only grown, regardless of
/// whether anyone was looking at it while that happened.
#[derive(Default)]
struct SessionSlot {
    history: Vec<Message>,
    /// How many leading entries of `history` are already durably saved.
    persisted_len: usize,
    /// This session's currently known plan, if `write_plan` has ever saved
    /// one — what the next turn's `ToolContext::current_plan` is fed from.
    plan: Option<Plan>,
}

/// What a spawned turn task reports back once `run_turn` returns.
struct TaskOutcome {
    session: SessionId,
    /// The full history the task ran with, including whatever the turn
    /// appended — moved back into the session's slot to replace what was
    /// taken out of it when the task was spawned.
    history: Vec<Message>,
    /// Already tagged with `session` by construction — each task builds its
    /// own tools with a recorder local to itself, so two concurrent turns'
    /// changes are never ambiguous about which session produced them.
    file_changes: Vec<FileChange>,
    /// The last plan `write_plan` saved this turn, if any — unlike
    /// `file_changes`, only the last one matters (a plan write replaces,
    /// it doesn't append), so this is `Option<Plan>` not a `Vec`.
    plan: Option<Plan>,
    tools_error: Option<String>,
    result: Result<TurnOutcome, AgentError>,
}

/// A `spawn_subagents` tool call's request to start a new child session and
/// wait for it — see `subagent_tx`/`subagent_rx`'s doc comment in
/// [`worker`] for why this rides its own channel instead of being a
/// `Command` variant.
struct SpawnSubAgentRequest {
    parent: SessionId,
    prompt: String,
    /// Relative to the workspace root — resolved and containment-checked
    /// against it before a session is ever created for this request.
    path: String,
    reply: tokio::sync::oneshot::Sender<Result<String, String>>,
}

/// What `spawn_subagents` (`crates/architect-tools/src/tools/
/// spawn_subagents.rs`) actually calls — submits a [`SpawnSubAgentRequest`]
/// on `subagent_tx` and awaits the reply the worker loop resolves once that
/// child session's turn finishes. One of these is built fresh per turn (in
/// `Command::Send`'s handler, alongside the recorders `spawn_turn` also
/// wires in), so `run_sequentially` always reflects *that turn's* active
/// provider, not a stale snapshot from whenever the engine started.
struct EngineSubAgentSpawner {
    parent: SessionId,
    subagent_tx: UnboundedSender<SpawnSubAgentRequest>,
    run_sequentially: bool,
}

#[async_trait::async_trait]
impl architect_tools::SubAgentSpawner for EngineSubAgentSpawner {
    fn run_sequentially(&self) -> bool {
        self.run_sequentially
    }

    async fn spawn(&self, prompt: String, path: String) -> Result<String, String> {
        let (reply, reply_rx) = tokio::sync::oneshot::channel();
        self.subagent_tx
            .send(SpawnSubAgentRequest {
                parent: self.parent,
                prompt,
                path,
                reply,
            })
            .map_err(|_| "the agent has shut down".to_owned())?;

        reply_rx
            .await
            .map_err(|_| "the sub-agent did not report back".to_owned())?
    }
}

/// Owns every session's conversation and runs each one's turn as an
/// independent task, so a turn in flight for one session never blocks
/// anything — including a `Send` or `LoadSession` — for another.
///
/// The command loop (`tokio::select!` over `commands` and `task_rx`) only
/// ever does quick, non-blocking work itself: look up or create a session's
/// slot, push and persist the user's message, then `tokio::spawn` the actual
/// turn and immediately go back to waiting. This is the fix for the old
/// design, where `Command::Send`'s handler `.await`ed `agent.run_turn(..)`
/// *inline*, which meant nothing else in the queue — not even switching to a
/// different session — could be processed until that turn finished.
/// `task_rx` is how a finished turn's result (history, file changes, outcome)
/// gets back into the slot it was taken out of.
