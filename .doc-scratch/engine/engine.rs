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
