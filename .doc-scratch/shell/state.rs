//! The conversation as the UI sees it.
//!
//! Each session gets its own [`Conversation`] — rows, status, usage, cost —
//! keyed by [`SessionId`] in [`Transcript::conversations`]. That is what lets
//! a turn keep streaming into one session's conversation while a different
//! one is on screen: [`Transcript::apply`] always routes an event into the
//! conversation it names, never into "whichever one is active," so switching
//! `active_session` back to it later shows exactly what accumulated while it
//! was off-screen — not a stale snapshot. [`Conversation`]'s own reducer
//! logic (`agent_event`, `stream_event`, ...) is a pure function of the
//! events — no IO, no Freya — which is what makes the streaming behavior
//! testable without a model or a window.

use std::collections::HashMap;

use architect_agent::AgentEvent;
use architect_config::{DocsConfig, IntegrationsConfig, McpServerConfig, Profile};
use architect_core::{ContentBlock, Cost, FileChangeEntry, Message, Plan, Role, ToolResult, Usage};
use architect_llm::StreamEvent;
use architect_session::{SessionId, SessionSummary};
use architect_tools::{ProcessEvent, ProcessStatus};
use base64::Engine as _;
use uuid::Uuid;

use crate::engine::{Attachment, EngineEvent};

/// One rendered row of the conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    User {
        text: String,
        /// Images attached via the composer's "Attach" button — rendered
        /// above `text`. Empty for every row before this feature existed
        /// or for a message that never had one.
        images: Vec<Attachment>,
    },
    /// One assistant turn. Reasoning and text stream in separately.
    Assistant {
        reasoning: String,
        text: String,
    },
    Tool {
        /// The provider's call id, used to match the result back.
        id: String,
        /// Position within the turn, which is all an input delta identifies.
        index: usize,
        name: String,
        arguments: String,
        status: ToolStatus,
        output: Option<String>,
    },
    Error {
        message: String,
    },
    /// The conversation's history was just replaced by this summary —
    /// `EngineEvent::Compacted`'s only effect on `rows`. Rendered distinctly
    /// from `Assistant`: it marks a break in the conversation, not another
    /// turn in it.
    Compacted {
        summary: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
}

/// What a session's turn is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Idle,
    /// Request sent, nothing streamed back yet.
    Waiting,
    Streaming,
    RunningTools,
    /// `Command::Compact` is summarizing this session — set optimistically
    /// by `Transcript::start_compacting` the moment the button is pressed,
    /// same as `push_user` flips to `Waiting` before the engine round-trip.
    Compacting,
    Failed,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Waiting => "thinking",
            Self::Streaming => "streaming",
            Self::RunningTools => "running tools",
            Self::Compacting => "compacting",
            Self::Failed => "error",
        }
    }

    /// Whether a turn is in flight — drives the Stop button, input locking,
    /// and the sidebar's busy indicator on a session that isn't on screen.
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            Self::Waiting | Self::Streaming | Self::RunningTools | Self::Compacting
        )
    }
}

/// One session's conversation: its rows, and the turn status/usage/cost that
/// go with them. Everything here is scoped to a single session — nothing
/// reads or writes another conversation's fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    pub rows: Vec<Row>,
    pub status: Status,
    /// Cumulative across the conversation, not just the last turn.
    pub usage: Usage,
    pub cost: Option<Cost>,
    /// Size of the context as of the most recent request — unlike `usage`
    /// above, replaced each turn rather than accumulated, since it reflects
    /// the current fill, not a running total. What the status bar's
    /// "context used" figure comes from. Reset to 0 by a successful
    /// `EngineEvent::Compacted`, since the whole point of compacting is
    /// that the next request starts from far less than this.
    pub context_tokens: u64,
    /// The active model's context window, if known — `None` for a
    /// local/self-hosted model, same as `cost` being `None` for one.
    pub context_window: Option<u64>,
    /// Every file change recorded against this conversation, chronological —
    /// what the Inspector panel's Files/Diff tabs show. Arrives via
    /// `EngineEvent::FileChanged` (live, appended) and
    /// `EngineEvent::FileChangesLoaded` (a fresh load, replaced outright),
    /// never through `from_history` — file changes have their own event.
    pub file_changes: Vec<FileChangeEntry>,
    /// This conversation's current plan, if `write_plan` has ever saved
    /// one — the Inspector's Plan tab. Arrives via `EngineEvent::
    /// PlanUpdated` (a new save, replacing whatever was here) and
    /// `EngineEvent::PlanLoaded` (a fresh load on resume/switch, which can
    /// itself be `None` if the session never wrote one).
    pub plan: Option<Plan>,
}

impl Conversation {
    /// Record what the user just sent, before the agent has said anything.
    fn push_user(&mut self, text: impl Into<String>, images: Vec<Attachment>) {
        self.rows.push(Row::User {
            text: text.into(),
            images,
        });
        self.status = Status::Waiting;
    }

    /// Rebuild from a session's saved message history.
    ///
    /// Rather than re-deriving row-construction rules a second time, this
    /// turns each stored message back into the same [`AgentEvent`]s live
    /// streaming would have produced, and feeds them through the same
    /// reducer methods unchanged — one set of rules, two sources.
    ///
    /// Token usage and cost are not stored per message, so a resumed
    /// conversation starts those counters at zero rather than showing a
    /// misleadingly partial total.
    fn from_history(messages: &[Message]) -> Self {
        let mut conversation = Self::default();

        for message in messages {
            let results: Vec<ToolResult> = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolResult(result) => Some(result.clone()),
                    _ => None,
                })
                .collect();

            if !results.is_empty() {
                for result in results {
                    conversation.agent_event(&AgentEvent::ToolFinished { result });
                }
                continue;
            }

            match message.role {
                Role::User => {
                    let images = message
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Image { media_type, data } => {
                                // A corrupt or truncated base64 blob (should
                                // never happen — this app is the only thing
                                // that writes this column) just drops that
                                // one image rather than failing the whole
                                // replay, the same tolerance malformed tool
                                // arguments already get.
                                base64::engine::general_purpose::STANDARD
                                    .decode(data)
                                    .ok()
                                    .map(|bytes| Attachment {
                                        media_type: media_type.clone(),
                                        bytes,
                                    })
                            }
                            _ => None,
                        })
                        .collect();
                    conversation.push_user(message.text(), images);
                }
                Role::Assistant => conversation.replay_assistant_message(message),
                Role::System | Role::Tool => {}
            }
        }

        conversation.drop_empty_assistant();
        // A resumed conversation is never mid-turn — whatever it was doing
        // when the app last closed, there is nothing in flight now.
        conversation.status = Status::Idle;
        conversation
    }

    fn replay_assistant_message(&mut self, message: &Message) {
        self.agent_event(&AgentEvent::IterationStarted { iteration: 0 });

        let mut tool_index = 0;
        for block in &message.content {
            let event = match block {
                ContentBlock::Reasoning { text, .. } if !text.is_empty() => {
                    StreamEvent::ReasoningDelta { text: text.clone() }
                }
                ContentBlock::Text { text } if !text.is_empty() => {
                    StreamEvent::TextDelta { text: text.clone() }
                }
                ContentBlock::ToolUse(call) => {
                    let index = tool_index;
                    tool_index += 1;
                    self.agent_event(&AgentEvent::Stream(StreamEvent::ToolCallStart {
                        index,
                        id: call.id.clone(),
                        name: call.name.clone(),
                    }));
                    StreamEvent::ToolCallEnd {
                        index,
                        call: call.clone(),
                    }
                }
                _ => continue,
            };
            self.agent_event(&AgentEvent::Stream(event));
        }
    }

    fn agent_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::IterationStarted { .. } => {
                // Each request gets its own assistant row, matching how the
                // model's turns actually interleave with tool calls. Empty ones
                // are dropped when the turn ends.
                self.rows.push(Row::Assistant {
                    reasoning: String::new(),
                    text: String::new(),
                });
                self.status = Status::Waiting;
            }
            AgentEvent::Stream(event) => self.stream_event(event),
            AgentEvent::ToolStarted { call } => {
                self.status = Status::RunningTools;
                if self.tool_by_id(&call.id).is_none() {
                    self.rows.push(Row::Tool {
                        id: call.id.clone(),
                        index: 0,
                        name: call.name.clone(),
                        arguments: pretty(&call.input),
                        status: ToolStatus::Running,
                        output: None,
                    });
                }
            }
            AgentEvent::ToolFinished { result } => {
                if let Some(Row::Tool { status, output, .. }) = self.tool_by_id(&result.tool_use_id)
                {
                    *status = if result.is_error {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Ok
                    };
                    *output = Some(result.content.clone());
                }
            }
            AgentEvent::TurnCompleted { outcome } => {
                self.drop_empty_assistant();
                self.usage += outcome.usage;
                self.cost = outcome.cost;
                self.context_tokens = outcome.context_tokens;
                self.context_window = outcome.context_window;
                self.status = Status::Idle;

                if outcome.hit_iteration_limit {
                    self.rows.push(Row::Error {
                        message:
                            "Stopped at the tool-call limit. Send another message to continue."
                                .into(),
                    });
                }
            }
        }
    }

    fn stream_event(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::TextDelta { text } => {
                self.status = Status::Streaming;
                self.assistant().1.push_str(text);
            }
            StreamEvent::ReasoningDelta { text } => {
                self.status = Status::Streaming;
                self.assistant().0.push_str(text);
            }
            StreamEvent::ToolCallStart { index, id, name } => {
                self.status = Status::RunningTools;
                self.rows.push(Row::Tool {
                    id: id.clone(),
                    index: *index,
                    name: name.clone(),
                    arguments: String::new(),
                    status: ToolStatus::Running,
                    output: None,
                });
            }
            StreamEvent::ToolCallInputDelta {
                index,
                partial_json,
            } => {
                // An input delta only carries the index, so the row is found by
                // that — the id may not have arrived yet.
                if let Some(Row::Tool { arguments, .. }) = self.tool_by_index(*index) {
                    arguments.push_str(partial_json);
                }
            }
            StreamEvent::ToolCallEnd { index, call } => {
                if let Some(Row::Tool {
                    id,
                    arguments,
                    name,
                    ..
                }) = self.tool_by_index(*index)
                {
                    // Replace the streamed fragments with the parsed value, so
                    // a half-written payload never sticks around on screen.
                    *arguments = pretty(&call.input);
                    *id = call.id.clone();
                    *name = call.name.clone();
                }
            }
            StreamEvent::MessageStart { .. } | StreamEvent::Finished { .. } => {}
        }
    }

    /// The reasoning and text of the current assistant row, creating one if the
    /// provider streamed content before any iteration was announced.
    fn assistant(&mut self) -> (&mut String, &mut String) {
        if !matches!(self.rows.last(), Some(Row::Assistant { .. })) {
            self.rows.push(Row::Assistant {
                reasoning: String::new(),
                text: String::new(),
            });
        }

        match self.rows.last_mut() {
            Some(Row::Assistant { reasoning, text }) => (reasoning, text),
            _ => unreachable!("an assistant row was just ensured"),
        }
    }

    fn tool_by_id(&mut self, wanted: &str) -> Option<&mut Row> {
        self.rows
            .iter_mut()
            .rev()
            .find(|row| matches!(row, Row::Tool { id, .. } if id == wanted))
    }

    /// The most recent still-running tool row at `index`. Indices repeat across
    /// iterations, so only the open one can match.
    fn tool_by_index(&mut self, wanted: usize) -> Option<&mut Row> {
        self.rows.iter_mut().rev().find(|row| {
            matches!(row, Row::Tool { index, status: ToolStatus::Running, .. } if *index == wanted)
        })
    }

    /// Remove an assistant row that never received content — a turn that went
    /// straight to tool calls would otherwise leave an empty bubble.
    ///
    /// Checking only the *last* row is not enough: that row is a tool row
    /// once the turn has made any tool calls at all, with the empty assistant
    /// row sitting just before them. A turn's own tool rows always follow its
    /// assistant row directly, so walking back past any trailing tool rows
    /// still lands on the one assistant row that might be empty — never on an
    /// earlier turn's.
    fn drop_empty_assistant(&mut self) {
        let Some(index) = self
            .rows
            .iter()
            .rposition(|row| !matches!(row, Row::Tool { .. }))
        else {
            return;
        };

        if let Row::Assistant { reasoning, text } = &self.rows[index]
            && reasoning.is_empty()
            && text.is_empty()
        {
            self.rows.remove(index);
        }
    }
}

fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

/// Every session's conversation, plus the sidebar's session list and the
/// settings panel's profile list — neither of which belongs to any one
/// conversation.
/// A process started with `start_process` — global, like `mcp_servers`
/// below, not owned by any one conversation (`architect_tools::Tool::call`
/// has no session id to tag it with even if that were wanted, and
/// conceptually a running dev server isn't "owned" by whichever chat
/// happened to start it, the same way a connected MCP server isn't).
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessSummary {
    pub id: String,
    pub command: String,
    pub status: ProcessStatus,
    /// Grows via `push_str` as `ProcessEvent::Output` chunks arrive — the
    /// same append-only-string-plus-streamed-deltas shape `Row::Assistant`'s
    /// `text` already uses for the model's streamed reply.
    pub log: String,
}

/// Belt-and-suspenders cap matching `architect_tools::process`'s own —
/// keeping one here too means this field can never out-accumulate what the
/// registry ever intended to retain, even if the two caps ever drifted.
const MAX_PROCESS_LOG: usize = 200_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub conversations: HashMap<SessionId, Conversation>,
    /// Which conversation is currently displayed. Always `Some` after
    /// construction — see `Default`'s impl — so there is always somewhere
    /// for a global failure to land even before any real session exists.
    pub active_session: Option<SessionId>,
    /// Every saved session, newest first — the sidebar's list. Untouched by
    /// switching or starting a new chat: neither adds or removes a saved
    /// session, only changes which conversation is on screen.
    pub sessions: Vec<SessionSummary>,
    /// Every saved API configuration — the settings panel's list.
    pub profiles: Vec<Profile>,
    /// Which saved configuration live turns currently use, if any is active.
    pub active_profile: Option<Uuid>,
    /// The model an ad-hoc `Command::UseAdHocModel` pick (the LM Studio
    /// settings page) currently has live turns pointed at, if any — never
    /// backed by a saved `Profile`. Mutually exclusive with
    /// `active_profile`: activating a saved profile (or reverting to the
    /// engine's own default) clears this, and picking an ad-hoc model
    /// clears `active_profile`.
    pub active_adhoc_model: Option<String>,
    /// Every saved MCP server — the settings panel's list.
    pub mcp_servers: Vec<McpServerConfig>,
    /// The saved GitHub/Slack/Linear credentials — the settings panel's
    /// Integrations form. Unlike `profiles`/`mcp_servers`, not a list: there
    /// is exactly one of each.
    pub integrations: IntegrationsConfig,
    /// The saved documentation-driver config — the settings panel's
    /// Documentation form. Same "exactly one, not a list" shape as
    /// `integrations`.
    pub docs: DocsConfig,
    /// Every process started with `start_process` this app run, running or
    /// finished — the Inspector's Processes tab list. Global; see
    /// `ProcessSummary`'s own doc comment for why.
    pub processes: Vec<ProcessSummary>,
    /// The last model list `Command::ListModels` fetched — the settings
    /// form's "Fetch models" result. Replaced wholesale on every fetch,
    /// the same "last one wins" shape `profiles`/`mcp_servers` already
    /// have for their own `*Listed` events; not correlated against the
    /// form's current base URL beyond what's needed to render — a stale
    /// list from a since-edited base URL just offers the wrong
    /// suggestions, it can't corrupt anything, since clicking one only
    /// ever fills a text field.
    pub discovered_models: Vec<String>,
}

impl Default for Transcript {
    fn default() -> Self {
        let mut transcript = Self {
            conversations: HashMap::new(),
            active_session: None,
            sessions: Vec::new(),
            profiles: Vec::new(),
            active_profile: None,
            active_adhoc_model: None,
            mcp_servers: Vec::new(),
            integrations: IntegrationsConfig::default(),
            docs: DocsConfig::default(),
            processes: Vec::new(),
            discovered_models: Vec::new(),
        };
        transcript.start_new_chat();
        transcript
    }
}

impl Transcript {
    /// Mint a fresh session id, give it an empty conversation, and make it
    /// the active one — "+ New Chat". No engine round-trip: nothing durable
    /// exists for this id until the first message is actually sent into it.
    pub fn start_new_chat(&mut self) -> SessionId {
        let session = SessionId::new();
        self.conversations.insert(session, Conversation::default());
        self.active_session = Some(session);
        session
    }

    /// Change which conversation is displayed. A pure local view change, no
    /// fetch — callers check `conversations.contains_key` first; a session
    /// that isn't resident yet should arrive via `EngineEvent::HistoryLoaded`
    /// instead, which both populates and activates it.
    pub fn switch_to(&mut self, session: SessionId) {
        self.active_session = Some(session);
    }

    pub fn push_user(
        &mut self,
        session: SessionId,
        text: impl Into<String>,
        images: Vec<Attachment>,
    ) {
        self.conversations
            .entry(session)
            .or_default()
            .push_user(text, images);
    }

    /// Flip a session to `Compacting` the moment "Compact" is pressed —
    /// before the engine round-trip, the same optimistic-update shape
    /// `push_user` already uses for `Waiting`.
    pub fn start_compacting(&mut self, session: SessionId) {
        self.conversations.entry(session).or_default().status = Status::Compacting;
    }

    pub fn conversation(&self, session: SessionId) -> Option<&Conversation> {
        self.conversations.get(&session)
    }

    pub fn active_conversation(&self) -> Option<&Conversation> {
        self.active_session
            .and_then(|session| self.conversations.get(&session))
    }

    pub fn apply(&mut self, event: &EngineEvent) {
        match event {
            EngineEvent::Agent { session, event } => {
                self.conversations
                    .entry(*session)
                    .or_default()
                    .agent_event(event);
            }
            EngineEvent::Cancelled(session) => {
                let conversation = self.conversations.entry(*session).or_default();
                conversation.drop_empty_assistant();
                conversation.status = Status::Idle;
            }
            EngineEvent::Failed { session, message } => {
                // A session-specific failure lands in its own conversation,
                // regardless of what's on screen; a global one (no session)
                // lands in whichever conversation *is* on screen —
                // `active_session` is always `Some` by the time any event
                // can arrive, so there is always somewhere for it to go.
                let target = session.unwrap_or_else(|| {
                    self.active_session
                        .expect("Transcript::default always starts one conversation")
                });
                let conversation = self.conversations.entry(target).or_default();
                conversation.drop_empty_assistant();
                conversation.rows.push(Row::Error {
                    message: message.clone(),
                });
                conversation.status = Status::Failed;
            }
            EngineEvent::HistoryLoaded { session, messages } => {
                self.conversations
                    .insert(*session, Conversation::from_history(messages));
                self.active_session = Some(*session);
            }
            EngineEvent::SessionsListed(sessions) => self.sessions = sessions.clone(),
            EngineEvent::SessionDeleted(session) => {
                self.conversations.remove(session);
                if self.active_session == Some(*session) {
                    // Nothing left to show for what was on screen — the
                    // same blank state "+ New Chat" produces.
                    self.start_new_chat();
                }
            }
            EngineEvent::ProfilesListed { profiles, active } => {
                self.profiles = profiles.clone();
                self.active_profile = *active;
                // Broadcast by both `ActivateProfile` and `DeactivateProfile`
                // — either way, whatever ad-hoc model was active is not
                // anymore.
                self.active_adhoc_model = None;
            }
            EngineEvent::McpServersListed(servers) => self.mcp_servers = servers.clone(),
            EngineEvent::IntegrationsListed(integrations) => {
                self.integrations = integrations.clone();
            }
            EngineEvent::DocsConfigListed(docs) => {
                self.docs = docs.clone();
            }
            EngineEvent::FileChanged { session, entry } => {
                self.conversations
                    .entry(*session)
                    .or_default()
                    .file_changes
                    .push(entry.clone());
            }
            EngineEvent::FileChangesLoaded { session, changes } => {
                self.conversations.entry(*session).or_default().file_changes = changes.clone();
            }
            EngineEvent::Process(event) => self.apply_process_event(event),
            EngineEvent::PlanUpdated { session, plan } => {
                self.conversations.entry(*session).or_default().plan = Some(plan.clone());
            }
            EngineEvent::PlanLoaded { session, plan } => {
                self.conversations.entry(*session).or_default().plan = plan.clone();
            }
            EngineEvent::Compacted { session, summary } => {
                let conversation = self.conversations.entry(*session).or_default();
                conversation.rows = vec![Row::Compacted {
                    summary: summary.clone(),
                }];
                conversation.status = Status::Idle;
                // The old context is gone — the next turn's own usage
                // report will set this back to a real figure.
                conversation.context_tokens = 0;
            }
            EngineEvent::ModelsListed { models, .. } => {
                self.discovered_models = models.clone();
            }
            EngineEvent::AdHocModelActivated { model } => {
                self.active_profile = None;
                self.active_adhoc_model = Some(model.clone());
            }
        }
    }

    /// Finds-or-creates a `ProcessSummary` by id: `Started` pushes a new
    /// entry, `Output` appends to its `log` (capped, dropping the oldest
    /// content first — the same policy `architect_tools::process` applies
    /// to its own copy), `Exited` sets `status` in place. The exact
    /// growing-string/status-flipped-on-a-terminal-event shape
    /// `Row::Assistant`/`Row::Tool` already use, applied to this field
    /// instead of a transcript row.
    fn apply_process_event(&mut self, event: &ProcessEvent) {
        match event {
            ProcessEvent::Started { id, command } => {
                self.processes.push(ProcessSummary {
                    id: id.clone(),
                    command: command.clone(),
                    status: ProcessStatus::Running,
                    log: String::new(),
                });
            }
            ProcessEvent::Output { id, chunk } => {
                if let Some(process) = self.processes.iter_mut().find(|p| &p.id == id) {
                    process.log.push_str(chunk);
                    process.log.push('\n');
                    if process.log.len() > MAX_PROCESS_LOG {
                        let excess = process.log.len() - MAX_PROCESS_LOG;
                        let cut = process
                            .log
                            .char_indices()
                            .map(|(i, _)| i)
                            .find(|&i| i >= excess)
                            .unwrap_or(process.log.len());
                        process.log.drain(..cut);
                    }
                }
            }
            ProcessEvent::Exited { id, status } => {
                if let Some(process) = self.processes.iter_mut().find(|p| &p.id == id) {
                    process.status = status.clone();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use architect_agent::TurnOutcome;
    use architect_core::{
        FileChange, PlanStep, StepStatus, StopReason, ToolCall, ToolResult, Usage,
    };
    use serde_json::json;

    use super::*;

    fn stream(event: StreamEvent) -> EngineEvent {
        EngineEvent::Agent {
            session: session(),
            event: AgentEvent::Stream(event),
        }
    }

    fn agent(event: AgentEvent) -> EngineEvent {
        EngineEvent::Agent {
            session: session(),
            event,
        }
    }

    fn apply(transcript: &mut Transcript, events: Vec<EngineEvent>) {
        for event in events {
            transcript.apply(&event);
        }
    }

    /// A fixed id for tests that only ever deal with one session — real
    /// code always mints one via `SessionId::new()` or gets one from an
    /// event, but a fixed one keeps single-session tests readable.
    fn session_id(seed: u8) -> SessionId {
        format!("00000000-0000-0000-0000-00000000000{seed}")
            .parse()
            .unwrap()
    }

    fn session() -> SessionId {
        session_id(0)
    }

    fn conv(transcript: &Transcript) -> &Conversation {
        transcript
            .conversation(session())
            .expect("the fixed test session's conversation")
    }

    #[test]
    fn a_fresh_transcript_starts_with_one_active_empty_conversation() {
        let transcript = Transcript::default();

        let session = transcript.active_session.expect("always Some");
        let conversation = transcript
            .active_conversation()
            .expect("the just-started conversation");
        assert!(conversation.rows.is_empty());
        assert_eq!(transcript.conversations.len(), 1);
        assert!(transcript.conversations.contains_key(&session));
    }

    #[test]
    fn starting_a_new_chat_does_not_remove_the_previous_one() {
        let mut transcript = Transcript::default();
        let first = transcript.active_session.unwrap();

        let second = transcript.start_new_chat();

        assert_ne!(first, second);
        assert_eq!(transcript.active_session, Some(second));
        assert_eq!(transcript.conversations.len(), 2);
        assert!(transcript.conversations.contains_key(&first));
    }

    #[test]
    fn streams_text_into_one_assistant_row() {
        let mut transcript = Transcript::default();
        transcript.push_user(session(), "hello", Vec::new());

        apply(
            &mut transcript,
            vec![
                agent(AgentEvent::IterationStarted { iteration: 0 }),
                stream(StreamEvent::TextDelta { text: "Hel".into() }),
                stream(StreamEvent::TextDelta { text: "lo!".into() }),
            ],
        );

        let conversation = conv(&transcript);
        assert_eq!(conversation.status, Status::Streaming);
        assert_eq!(conversation.rows.len(), 2);
        assert_eq!(
            conversation.rows[1],
            Row::Assistant {
                reasoning: String::new(),
                text: "Hello!".into()
            }
        );
    }

    #[test]
    fn keeps_reasoning_separate_from_the_answer() {
        let mut transcript = Transcript::default();

        apply(
            &mut transcript,
            vec![
                agent(AgentEvent::IterationStarted { iteration: 0 }),
                stream(StreamEvent::ReasoningDelta {
                    text: "thinking".into(),
                }),
                stream(StreamEvent::TextDelta {
                    text: "answer".into(),
                }),
            ],
        );

        assert_eq!(
            conv(&transcript).rows[0],
            Row::Assistant {
                reasoning: "thinking".into(),
                text: "answer".into()
            }
        );
    }

    #[test]
    fn builds_a_tool_row_from_streamed_fragments() {
        let mut transcript = Transcript::default();

        apply(
            &mut transcript,
            vec![
                agent(AgentEvent::IterationStarted { iteration: 0 }),
                stream(StreamEvent::ToolCallStart {
                    index: 0,
                    id: "call_a".into(),
                    name: "read_file".into(),
                }),
                stream(StreamEvent::ToolCallInputDelta {
                    index: 0,
                    partial_json: "{\"path\"".into(),
                }),
                stream(StreamEvent::ToolCallEnd {
                    index: 0,
                    call: ToolCall {
                        id: "call_a".into(),
                        name: "read_file".into(),
                        input: json!({"path": "a.rs"}),
                    },
                }),
                agent(AgentEvent::ToolFinished {
                    result: ToolResult::ok("call_a", "fn main() {}"),
                }),
            ],
        );

        // The empty assistant row is still open; the tool row follows it.
        match &conv(&transcript).rows[1] {
            Row::Tool {
                name,
                arguments,
                status,
                output,
                ..
            } => {
                assert_eq!(name, "read_file");
                // The half-streamed fragment is replaced by the parsed value.
                assert_eq!(arguments, r#"{"path":"a.rs"}"#);
                assert_eq!(*status, ToolStatus::Ok);
                assert_eq!(output.as_deref(), Some("fn main() {}"));
            }
            other => panic!("expected a tool row, got {other:?}"),
        }
    }

    #[test]
    fn marks_a_failed_tool() {
        let mut transcript = Transcript::default();

        apply(
            &mut transcript,
            vec![
                agent(AgentEvent::ToolStarted {
                    call: ToolCall {
                        id: "x".into(),
                        name: "run".into(),
                        input: json!({}),
                    },
                }),
                agent(AgentEvent::ToolFinished {
                    result: ToolResult::error("x", "boom"),
                }),
            ],
        );

        match &conv(&transcript).rows[0] {
            Row::Tool { status, output, .. } => {
                assert_eq!(*status, ToolStatus::Failed);
                assert_eq!(output.as_deref(), Some("boom"));
            }
            other => panic!("expected a tool row, got {other:?}"),
        }
    }

    #[test]
    fn drops_the_empty_assistant_row_when_a_turn_ends() {
        let mut transcript = Transcript::default();
        let outcome = TurnOutcome {
            stop_reason: StopReason::EndTurn,
            iterations: 1,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 2,
                ..Usage::default()
            },
            cost: None,
            context_tokens: 12,
            context_window: Some(200_000),
            hit_iteration_limit: false,
        };

        apply(
            &mut transcript,
            vec![
                agent(AgentEvent::IterationStarted { iteration: 0 }),
                agent(AgentEvent::TurnCompleted { outcome }),
            ],
        );

        let conversation = conv(&transcript);
        assert!(
            conversation.rows.is_empty(),
            "an empty bubble must not survive"
        );
        assert_eq!(conversation.status, Status::Idle);
        assert_eq!(conversation.usage.input_tokens, 10);
        assert_eq!(conversation.context_tokens, 12);
        assert_eq!(conversation.context_window, Some(200_000));
    }

    #[test]
    fn a_turn_that_goes_straight_to_a_tool_call_leaves_no_empty_bubble() {
        // The empty assistant row is no longer the *last* row once a tool row
        // follows it — checking only `.last()` missed exactly this case.
        let mut transcript = Transcript::default();
        let outcome = TurnOutcome {
            stop_reason: StopReason::EndTurn,
            iterations: 1,
            usage: Usage::default(),
            cost: None,
            context_tokens: 0,
            context_window: None,
            hit_iteration_limit: false,
        };

        apply(
            &mut transcript,
            vec![
                agent(AgentEvent::IterationStarted { iteration: 0 }),
                stream(StreamEvent::ToolCallStart {
                    index: 0,
                    id: "call_a".into(),
                    name: "list_dir".into(),
                }),
                stream(StreamEvent::ToolCallEnd {
                    index: 0,
                    call: ToolCall {
                        id: "call_a".into(),
                        name: "list_dir".into(),
                        input: json!({}),
                    },
                }),
                agent(AgentEvent::ToolFinished {
                    result: ToolResult::ok("call_a", "a.rs\nb.rs"),
                }),
                agent(AgentEvent::TurnCompleted { outcome }),
            ],
        );

        let conversation = conv(&transcript);
        assert_eq!(
            conversation.rows.len(),
            1,
            "only the tool row should remain: {:?}",
            conversation.rows
        );
        assert!(matches!(&conversation.rows[0], Row::Tool { .. }));
    }

    #[test]
    fn a_global_failure_surfaces_in_whatever_is_on_screen() {
        let mut transcript = Transcript::default();
        let active = transcript.active_session.unwrap();

        transcript.apply(&EngineEvent::Failed {
            session: None,
            message: "connection refused".into(),
        });

        let conversation = transcript.conversation(active).unwrap();
        assert_eq!(conversation.status, Status::Failed);
        assert_eq!(
            conversation.rows[0],
            Row::Error {
                message: "connection refused".into()
            }
        );
    }

    #[test]
    fn a_session_specific_failure_lands_in_its_own_conversation_not_the_one_on_screen() {
        let mut transcript = Transcript::default();
        let background = SessionId::new();
        transcript
            .conversations
            .insert(background, Conversation::default());
        let foreground = transcript.active_session.unwrap();
        assert_ne!(foreground, background);

        transcript.apply(&EngineEvent::Failed {
            session: Some(background),
            message: "background turn failed".into(),
        });

        assert!(
            transcript.conversation(foreground).unwrap().rows.is_empty(),
            "the conversation on screen must not see another session's error"
        );
        assert_eq!(
            transcript.conversation(background).unwrap().status,
            Status::Failed
        );
    }

    #[test]
    fn cancelling_returns_to_idle_and_keeps_partial_text() {
        let mut transcript = Transcript::default();

        apply(
            &mut transcript,
            vec![
                agent(AgentEvent::IterationStarted { iteration: 0 }),
                stream(StreamEvent::TextDelta {
                    text: "half a th".into(),
                }),
                EngineEvent::Cancelled(session()),
            ],
        );

        let conversation = conv(&transcript);
        assert_eq!(conversation.status, Status::Idle);
        assert_eq!(
            conversation.rows[0],
            Row::Assistant {
                reasoning: String::new(),
                text: "half a th".into()
            }
        );
    }

    #[test]
    fn a_second_iteration_starts_a_new_assistant_row() {
        let mut transcript = Transcript::default();

        apply(
            &mut transcript,
            vec![
                agent(AgentEvent::IterationStarted { iteration: 0 }),
                stream(StreamEvent::TextDelta {
                    text: "first".into(),
                }),
                agent(AgentEvent::IterationStarted { iteration: 1 }),
                stream(StreamEvent::TextDelta {
                    text: "second".into(),
                }),
            ],
        );

        let conversation = conv(&transcript);
        assert_eq!(conversation.rows.len(), 2);
        assert_eq!(
            conversation.rows[1],
            Row::Assistant {
                reasoning: String::new(),
                text: "second".into()
            }
        );
    }

    #[test]
    fn loading_history_replays_a_saved_conversation_into_the_same_rows_live_streaming_would_produce()
     {
        let history = vec![
            Message::user("read the config"),
            Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::reasoning("I should look at the file first"),
                    ContentBlock::text("Reading it now."),
                    ContentBlock::ToolUse(ToolCall {
                        id: "call_a".into(),
                        name: "read_file".into(),
                        input: json!({"path": "config.toml"}),
                    }),
                ],
            ),
            Message::tool_results([ToolResult::ok("call_a", "port = 8080")]),
            Message::assistant("The config sets the port to 8080."),
        ];

        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::HistoryLoaded {
            session: session(),
            messages: history,
        });

        let conversation = conv(&transcript);
        assert_eq!(conversation.status, Status::Idle);
        assert_eq!(conversation.rows.len(), 4);
        assert_eq!(
            conversation.rows[0],
            Row::User {
                text: "read the config".into(),
                images: Vec::new(),
            }
        );
        assert_eq!(
            conversation.rows[1],
            Row::Assistant {
                reasoning: "I should look at the file first".into(),
                text: "Reading it now.".into()
            }
        );
        match &conversation.rows[2] {
            Row::Tool {
                id,
                name,
                status,
                output,
                ..
            } => {
                assert_eq!(id, "call_a");
                assert_eq!(name, "read_file");
                assert_eq!(*status, ToolStatus::Ok);
                assert_eq!(output.as_deref(), Some("port = 8080"));
            }
            other => panic!("expected a tool row, got {other:?}"),
        }
        assert_eq!(
            conversation.rows[3],
            Row::Assistant {
                reasoning: String::new(),
                text: "The config sets the port to 8080.".into()
            }
        );
    }

    #[test]
    fn loading_history_decodes_an_attached_image() {
        let history = vec![Message::user_with_images(
            "what is this?",
            [ContentBlock::image("image/png", "aGVsbG8=")], // base64 of "hello"
        )];

        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::HistoryLoaded {
            session: session(),
            messages: history,
        });

        assert_eq!(
            conv(&transcript).rows[0],
            Row::User {
                text: "what is this?".into(),
                images: vec![Attachment {
                    media_type: "image/png".into(),
                    bytes: b"hello".to_vec(),
                }],
            }
        );
    }

    #[test]
    fn loading_history_skips_an_image_with_corrupt_base64_rather_than_failing_the_replay() {
        let history = vec![Message::user_with_images(
            "broken",
            [ContentBlock::image("image/png", "not valid base64!!")],
        )];

        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::HistoryLoaded {
            session: session(),
            messages: history,
        });

        assert_eq!(
            conv(&transcript).rows[0],
            Row::User {
                text: "broken".into(),
                images: Vec::new(),
            },
            "a corrupt image must not fail the whole replay"
        );
    }

    #[test]
    fn push_user_carries_images_into_the_row() {
        let mut transcript = Transcript::default();
        transcript.push_user(
            session(),
            "look at this",
            vec![Attachment {
                media_type: "image/png".into(),
                bytes: vec![1, 2, 3],
            }],
        );

        assert_eq!(
            conv(&transcript).rows[0],
            Row::User {
                text: "look at this".into(),
                images: vec![Attachment {
                    media_type: "image/png".into(),
                    bytes: vec![1, 2, 3],
                }],
            }
        );
    }

    #[test]
    fn loading_history_resets_usage_rather_than_showing_a_partial_total() {
        // Usage isn't stored per message, so a resumed conversation starts at
        // zero rather than a misleadingly partial figure.
        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::HistoryLoaded {
            session: session(),
            messages: vec![Message::user("hi")],
        });

        let conversation = conv(&transcript);
        assert_eq!(conversation.usage, Usage::default());
        assert_eq!(conversation.cost, None);
    }

    #[test]
    fn a_history_ending_on_an_unanswered_user_message_still_comes_back_idle() {
        // If the app closed mid-turn, the resumed session must not look like
        // it is still waiting on a reply that will never arrive.
        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::HistoryLoaded {
            session: session(),
            messages: vec![Message::user("are you there?")],
        });

        let conversation = conv(&transcript);
        assert_eq!(conversation.status, Status::Idle);
        assert_eq!(conversation.rows.len(), 1);
    }

    #[test]
    fn replaying_an_assistant_turn_with_parallel_tool_calls_keeps_them_distinct() {
        let history = vec![Message::new(
            Role::Assistant,
            vec![
                ContentBlock::ToolUse(ToolCall {
                    id: "a".into(),
                    name: "read_file".into(),
                    input: json!({}),
                }),
                ContentBlock::ToolUse(ToolCall {
                    id: "b".into(),
                    name: "list_dir".into(),
                    input: json!({}),
                }),
            ],
        )];

        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::HistoryLoaded {
            session: session(),
            messages: history,
        });

        // Two distinct running tool rows, not one overwriting the other via a
        // colliding index.
        let conversation = conv(&transcript);
        assert_eq!(conversation.rows.len(), 2);
        assert!(matches!(&conversation.rows[0], Row::Tool { id, .. } if id == "a"));
        assert!(matches!(&conversation.rows[1], Row::Tool { id, .. } if id == "b"));
    }

    fn summary(seed: u8) -> SessionSummary {
        SessionSummary {
            id: session_id(seed),
            title: format!("session {seed}"),
            provider_kind: "openai".into(),
            model: "m".into(),
            parent_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn sessions_listed_updates_the_sidebar_list() {
        let mut transcript = Transcript::default();

        transcript.apply(&EngineEvent::SessionsListed(vec![summary(1), summary(2)]));

        assert_eq!(transcript.sessions.len(), 2);
    }

    #[test]
    fn plan_updated_sets_the_active_conversations_plan() {
        let mut transcript = Transcript::default();
        let plan = Plan {
            goal: Some("Ship it".to_owned()),
            steps: vec![],
        };

        transcript.apply(&EngineEvent::PlanUpdated {
            session: session(),
            plan: plan.clone(),
        });

        assert_eq!(conv(&transcript).plan, Some(plan));
    }

    #[test]
    fn a_later_plan_updated_replaces_the_earlier_one() {
        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::PlanUpdated {
            session: session(),
            plan: Plan {
                goal: Some("First draft".to_owned()),
                steps: vec![],
            },
        });

        transcript.apply(&EngineEvent::PlanUpdated {
            session: session(),
            plan: Plan {
                goal: Some("Revised".to_owned()),
                steps: vec![],
            },
        });

        assert_eq!(
            conv(&transcript)
                .plan
                .as_ref()
                .and_then(|p| p.goal.as_deref()),
            Some("Revised")
        );
    }

    #[test]
    fn plan_loaded_with_none_leaves_the_conversation_with_no_plan() {
        let mut transcript = Transcript::default();

        transcript.apply(&EngineEvent::PlanLoaded {
            session: session(),
            plan: None,
        });

        assert_eq!(conv(&transcript).plan, None);
    }

    #[test]
    fn plan_loaded_with_some_sets_the_conversations_plan() {
        let mut transcript = Transcript::default();
        let plan = Plan {
            goal: None,
            steps: vec![PlanStep {
                description: "Step one".to_owned(),
                status: StepStatus::Completed,
                substeps: vec![],
            }],
        };

        transcript.apply(&EngineEvent::PlanLoaded {
            session: session(),
            plan: Some(plan.clone()),
        });

        assert_eq!(conv(&transcript).plan, Some(plan));
    }

    #[test]
    fn a_started_process_appears_running_with_an_empty_log() {
        let mut transcript = Transcript::default();

        transcript.apply(&EngineEvent::Process(ProcessEvent::Started {
            id: "proc-1".into(),
            command: "npm run dev".into(),
        }));

        assert_eq!(transcript.processes.len(), 1);
        let process = &transcript.processes[0];
        assert_eq!(process.command, "npm run dev");
        assert_eq!(process.status, ProcessStatus::Running);
        assert_eq!(process.log, "");
    }

    #[test]
    fn output_chunks_accumulate_into_the_processs_log() {
        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::Process(ProcessEvent::Started {
            id: "proc-1".into(),
            command: "npm run dev".into(),
        }));

        transcript.apply(&EngineEvent::Process(ProcessEvent::Output {
            id: "proc-1".into(),
            chunk: "listening on :3000".into(),
        }));
        transcript.apply(&EngineEvent::Process(ProcessEvent::Output {
            id: "proc-1".into(),
            chunk: "ready".into(),
        }));

        assert_eq!(transcript.processes[0].log, "listening on :3000\nready\n");
    }

    #[test]
    fn exiting_flips_status_in_place_without_touching_the_log() {
        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::Process(ProcessEvent::Started {
            id: "proc-1".into(),
            command: "npm run dev".into(),
        }));
        transcript.apply(&EngineEvent::Process(ProcessEvent::Output {
            id: "proc-1".into(),
            chunk: "bye".into(),
        }));

        transcript.apply(&EngineEvent::Process(ProcessEvent::Exited {
            id: "proc-1".into(),
            status: ProcessStatus::Exited(0),
        }));

        let process = &transcript.processes[0];
        assert_eq!(process.status, ProcessStatus::Exited(0));
        assert_eq!(process.log, "bye\n");
    }

    #[test]
    fn an_event_for_an_unknown_process_id_is_ignored_not_panicked() {
        let mut transcript = Transcript::default();

        transcript.apply(&EngineEvent::Process(ProcessEvent::Output {
            id: "proc-unknown".into(),
            chunk: "orphaned".into(),
        }));

        assert!(transcript.processes.is_empty());
    }

    #[test]
    fn a_second_processs_events_do_not_affect_the_first() {
        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::Process(ProcessEvent::Started {
            id: "proc-1".into(),
            command: "a".into(),
        }));
        transcript.apply(&EngineEvent::Process(ProcessEvent::Started {
            id: "proc-2".into(),
            command: "b".into(),
        }));

        transcript.apply(&EngineEvent::Process(ProcessEvent::Output {
            id: "proc-2".into(),
            chunk: "only for proc-2".into(),
        }));

        assert_eq!(transcript.processes.len(), 2);
        assert_eq!(transcript.processes[0].log, "");
        assert_eq!(transcript.processes[1].log, "only for proc-2\n");
    }

    #[test]
    fn a_log_over_the_cap_drops_the_oldest_content_first() {
        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::Process(ProcessEvent::Started {
            id: "proc-1".into(),
            command: "noisy".into(),
        }));

        transcript.apply(&EngineEvent::Process(ProcessEvent::Output {
            id: "proc-1".into(),
            chunk: "a".repeat(MAX_PROCESS_LOG),
        }));
        transcript.apply(&EngineEvent::Process(ProcessEvent::Output {
            id: "proc-1".into(),
            chunk: "newest".into(),
        }));

        let log = &transcript.processes[0].log;
        assert!(
            log.len() <= MAX_PROCESS_LOG,
            "log should stay capped, got {} bytes",
            log.len()
        );
        assert!(
            log.ends_with("newest\n"),
            "the most recent output should survive, got: {log}"
        );
    }

    #[test]
    fn loading_history_preserves_the_session_and_profile_lists() {
        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::SessionsListed(vec![summary(1), summary(2)]));
        transcript.apply(&EngineEvent::ProfilesListed {
            profiles: vec![],
            active: None,
        });

        transcript.apply(&EngineEvent::HistoryLoaded {
            session: session_id(1),
            messages: vec![Message::user("hello")],
        });

        // Populating one conversation must not wipe the sidebar's list of
        // every other session, nor the settings panel's profile list.
        assert_eq!(transcript.sessions.len(), 2);
        assert_eq!(transcript.active_session, Some(session_id(1)));
    }

    #[test]
    fn a_background_conversation_keeps_accumulating_while_a_different_one_is_on_screen() {
        // The core proof for concurrent sessions at the state layer: events
        // tagged for a session that is *not* currently active must still be
        // folded into that session's own conversation, not dropped or
        // misapplied to whatever's on screen.
        let mut transcript = Transcript::default();
        let foreground = transcript.active_session.unwrap();
        let background = transcript.start_new_chat();
        // Switch back to `foreground` — `background`'s turn keeps "running"
        // (in spirit) after this.
        transcript.switch_to(foreground);

        transcript.apply(&EngineEvent::Agent {
            session: background,
            event: AgentEvent::IterationStarted { iteration: 0 },
        });
        transcript.apply(&EngineEvent::Agent {
            session: background,
            event: AgentEvent::Stream(StreamEvent::TextDelta {
                text: "still going".into(),
            }),
        });

        assert!(
            transcript.conversation(foreground).unwrap().rows.is_empty(),
            "the foreground conversation must be untouched"
        );
        assert_eq!(
            transcript.conversation(background).unwrap().rows[0],
            Row::Assistant {
                reasoning: String::new(),
                text: "still going".into(),
            }
        );

        // Switching back shows exactly what accumulated, live — not a stale
        // snapshot, because it was never replaced, only appended to.
        transcript.switch_to(background);
        assert_eq!(
            transcript.active_conversation().unwrap().rows[0],
            Row::Assistant {
                reasoning: String::new(),
                text: "still going".into(),
            }
        );
    }

    #[test]
    fn file_changed_lands_in_its_own_conversation_not_whatevers_active() {
        // Same rule `a_background_conversation_keeps_accumulating_..` proves
        // for streamed rows: a session-tagged event always routes to that
        // session's own conversation, never to whatever's on screen.
        let mut transcript = Transcript::default();
        let foreground = transcript.active_session.unwrap();
        let background = transcript.start_new_chat();
        transcript.switch_to(foreground);

        let entry = FileChangeEntry {
            message_seq: 0,
            change: FileChange {
                file_path: "a.txt".into(),
                old_content: None,
                new_content: "hello".into(),
                tool_name: "write_file",
            },
        };
        transcript.apply(&EngineEvent::FileChanged {
            session: background,
            entry: entry.clone(),
        });

        assert!(
            transcript
                .conversation(foreground)
                .unwrap()
                .file_changes
                .is_empty(),
            "the foreground conversation must be untouched"
        );
        assert_eq!(
            transcript.conversation(background).unwrap().file_changes,
            vec![entry]
        );
    }

    #[test]
    fn file_changes_loaded_replaces_rather_than_appends() {
        let mut transcript = Transcript::default();
        let id = transcript.active_session.unwrap();

        transcript.apply(&EngineEvent::FileChanged {
            session: id,
            entry: FileChangeEntry {
                message_seq: 0,
                change: FileChange {
                    file_path: "live.txt".into(),
                    old_content: None,
                    new_content: "from this run".into(),
                    tool_name: "write_file",
                },
            },
        });

        let loaded_changes = vec![FileChangeEntry {
            message_seq: 1,
            change: FileChange {
                file_path: "loaded.txt".into(),
                old_content: Some("old".into()),
                new_content: "new".into(),
                tool_name: "edit_file",
            },
        }];
        transcript.apply(&EngineEvent::FileChangesLoaded {
            session: id,
            changes: loaded_changes.clone(),
        });

        assert_eq!(
            transcript.conversation(id).unwrap().file_changes,
            loaded_changes,
            "a fresh load replaces whatever was there, it does not append to it"
        );
    }

    #[test]
    fn compacted_replaces_rows_with_a_summary_and_resets_context() {
        let mut transcript = Transcript::default();

        transcript.push_user(session(), "please add a login form", Vec::new());
        transcript.start_compacting(session());
        assert_eq!(conv(&transcript).status, Status::Compacting);

        transcript.apply(&EngineEvent::Compacted {
            session: session(),
            summary: "added a login form; see login.rs".into(),
        });

        let conversation = conv(&transcript);
        assert_eq!(
            conversation.rows,
            vec![Row::Compacted {
                summary: "added a login form; see login.rs".into()
            }],
            "the old rows must be gone, not appended to"
        );
        assert_eq!(conversation.status, Status::Idle);
        assert_eq!(conversation.context_tokens, 0);
    }

    #[test]
    fn history_loaded_then_file_changes_loaded_leaves_both_intact() {
        // `FileChangesLoaded` is always sent right after `HistoryLoaded` for
        // the same session — `HistoryLoaded` fully replaces that
        // conversation, so this proves the file changes sent right after
        // survive that replace rather than landing on a conversation that
        // gets thrown away a moment later.
        let mut transcript = Transcript::default();
        let id = session_id(1);

        transcript.apply(&EngineEvent::HistoryLoaded {
            session: id,
            messages: vec![Message::user("hello")],
        });
        let changes = vec![FileChangeEntry {
            message_seq: 0,
            change: FileChange {
                file_path: "a.txt".into(),
                old_content: None,
                new_content: "hi".into(),
                tool_name: "write_file",
            },
        }];
        transcript.apply(&EngineEvent::FileChangesLoaded {
            session: id,
            changes: changes.clone(),
        });

        let conversation = transcript.conversation(id).unwrap();
        assert_eq!(
            conversation.rows[0],
            Row::User {
                text: "hello".into(),
                images: Vec::new(),
            }
        );
        assert_eq!(conversation.file_changes, changes);
    }

    #[test]
    fn deleting_the_active_session_starts_a_fresh_new_chat() {
        let mut transcript = Transcript::default();
        let deleted = transcript.active_session.unwrap();
        transcript.push_user(deleted, "hello", Vec::new());

        transcript.apply(&EngineEvent::SessionDeleted(deleted));

        assert!(
            !transcript.conversations.contains_key(&deleted),
            "the deleted session's conversation must not linger"
        );
        let active = transcript.active_session.expect("still always Some");
        assert_ne!(active, deleted);
        assert!(transcript.active_conversation().unwrap().rows.is_empty());
    }

    #[test]
    fn deleting_a_session_that_is_not_active_leaves_the_screen_alone() {
        let mut transcript = Transcript::default();
        let active = transcript.active_session.unwrap();
        transcript.push_user(active, "hello", Vec::new());
        let other = SessionId::new();
        transcript
            .conversations
            .insert(other, Conversation::default());

        transcript.apply(&EngineEvent::SessionDeleted(other));

        assert_eq!(transcript.active_session, Some(active));
        assert_eq!(transcript.conversation(active).unwrap().rows.len(), 1);
        assert!(!transcript.conversations.contains_key(&other));
    }

    #[test]
    fn profiles_listed_updates_the_settings_list() {
        let mut transcript = Transcript::default();

        transcript.apply(&EngineEvent::ProfilesListed {
            profiles: vec![profile(1), profile(2)],
            active: Some(profile(2).id),
        });

        assert_eq!(transcript.profiles.len(), 2);
        assert_eq!(transcript.active_profile, Some(profile(2).id));
    }

    #[test]
    fn models_listed_replaces_the_discovered_list() {
        let mut transcript = Transcript::default();

        transcript.apply(&EngineEvent::ModelsListed {
            base_url: "http://localhost:1234/v1".into(),
            models: vec!["qwen/qwen3.8-27b".into(), "gpt-oss-20b".into()],
        });
        assert_eq!(
            transcript.discovered_models,
            ["qwen/qwen3.8-27b", "gpt-oss-20b"]
        );

        // A second fetch replaces, it does not append.
        transcript.apply(&EngineEvent::ModelsListed {
            base_url: "http://localhost:1234/v1".into(),
            models: vec!["deepseek-v4-flash".into()],
        });
        assert_eq!(transcript.discovered_models, ["deepseek-v4-flash"]);
    }

    #[test]
    fn adhoc_model_activated_sets_it_and_clears_any_saved_profile() {
        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::ProfilesListed {
            profiles: vec![profile(1)],
            active: Some(profile(1).id),
        });

        transcript.apply(&EngineEvent::AdHocModelActivated {
            model: "gpt-oss-20b".into(),
        });

        assert_eq!(transcript.active_adhoc_model, Some("gpt-oss-20b".into()));
        // Mutually exclusive with a saved profile — the LM Studio page's
        // pick supersedes whatever was active before.
        assert_eq!(transcript.active_profile, None);
    }

    #[test]
    fn profiles_listed_clears_any_active_adhoc_model() {
        let mut transcript = Transcript::default();
        transcript.apply(&EngineEvent::AdHocModelActivated {
            model: "gpt-oss-20b".into(),
        });
        assert_eq!(transcript.active_adhoc_model, Some("gpt-oss-20b".into()));

        // Broadcast by both `ActivateProfile` and `DeactivateProfile` —
        // either way, whatever ad-hoc model was active is not anymore.
        transcript.apply(&EngineEvent::ProfilesListed {
            profiles: vec![profile(1)],
            active: Some(profile(1).id),
        });

        assert_eq!(transcript.active_adhoc_model, None);
        assert_eq!(transcript.active_profile, Some(profile(1).id));
    }

    fn profile(seed: u8) -> Profile {
        Profile {
            id: format!("00000000-0000-0000-0000-00000000000{seed}")
                .parse()
                .unwrap(),
            name: format!("profile {seed}"),
            kind: "openai".into(),
            base_url: None,
            api_key: None,
            model: "m".into(),
        }
    }
}
