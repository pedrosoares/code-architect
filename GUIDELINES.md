# Code Architect — Project Guidelines

## The one rule

Dependencies point one way. Nothing below ever imports something above it.

```
apps/desktop ──► architect-ui ──► freya
     │
     ├────────► architect-tools ──► architect-agent ──► architect-llm
     │              ▲              └──────────────┬──────────► architect-core
     │              │
     │         architect-mcp ───────────────────────────────► rmcp
     │         architect-github ──────────────────────────────► reqwest
     │         architect-slack ───────────────────────────────► reqwest
     │         architect-linear ──────────────────────────────► reqwest
     │
     ├────────► architect-session ───────────────────────────(depends on nothing)
     └────────► architect-config ───────────────────────────(depends on nothing)
```

`architect-tools` depends on `architect-agent`, not the other way around: its
`ToolRegistry` *implements* `architect-agent`'s `ToolExecutor` trait, so the
agent defines the extension point and tools fills it. `architect-mcp` has the
same relationship to `architect-tools` — its `McpToolAdapter` implements
`Tool`, filling that extension point in turn, rather than `architect-tools`
knowing MCP exists at all. `architect-github`/`architect-slack`/
`architect-linear` fill the same extension point again, one crate per
external service, each a thin `reqwest`-based client plus one or two `Tool`
impls — no shared base crate between them, since three tools don't yet
justify factoring out common HTTP-client scaffolding. `architect-tools`,
`architect-mcp`, `architect-github`, `architect-slack`, `architect-linear`,
`architect-session` and `architect-config` do not depend on each other or on
`architect-llm`; they only share plain data (`architect-core`'s
`FileChange`/`ChangeRecorder`, or a `architect-config::Profile`'s fields
mirroring `architect_llm::ProviderConfig` by name — `architect-config::
McpServerConfig` is the same idea for `architect-mcp::connect`, and
`architect-config::IntegrationsConfig` for the three tool crates' bare
tokens/keys). `apps/desktop/src/engine.rs` is the one place that knows all
of these exist and bridges between them.

* `architect-core` never imports Freya.
* `architect-ui` never imports the agent, the LLM client or storage.
* `apps/desktop` holds composition and window setup — no business logic.

That is what keeps the harness modular: the same core can later run behind a
CLI or a socket daemon without the UI knowing, and the UI can be restyled or
replaced without touching the agent.

## Where things go

| Crate | Owns | Status |
|---|---|---|
| `architect-core` | Messages, content blocks, tool calls, stop reasons, usage, pricing, cache policy. Pure serde. | built |
| `architect-llm` | `Provider` trait, normalized `StreamEvent`, SSE decoding, OpenAI + Anthropic adapters, registry. | built |
| `architect-agent` | `ToolExecutor` trait and the tool-call loop; emits `AgentEvent`. | built |
| `architect-ui` | Design tokens, reusable widgets. No application state. | built |
| `architect-tools` | `Tool` trait, `ToolRegistry` (implements `ToolExecutor`), seven built-in tools. | built |
| `architect-mcp` | MCP **client**: `connect`/`connect_http` spawn a server over stdio or streamable HTTP and turn its tools into `Tool` impls. | built |
| `architect-github` | Read-only GitHub tools: a pull request, and its conversation/review comments/reviews merged. | built |
| `architect-slack` | Read-only Slack tools: a thread (parent message + every reply). | built |
| `architect-linear` | Read-only Linear tools: a ticket, its comments, and its blocking relations, in one call. | built |
| `architect-session` | `SessionStore`: `.coder/sessions.db`, messages, file changes, reverse-to-point. | built |
| `architect-config` | `ConfigStore`: `~/.config/code-architect/profiles.json`, saved API configurations, MCP servers, and GitHub/Slack/Linear credentials. | built |
| `apps/desktop` | Window, shell layout, panels, and the engine bridge to the agent, tools, MCP connections, GitHub/Slack/Linear clients, session store and config store. | wired |

Crates are created when there is code for them — no empty placeholders. LSP
and `read_docs`/`write_docs` are not built yet — see `## Tools` and
`## Persistence` below for what each pass added and what it deliberately
left out.

## Tools

`architect-tools::Tool` is the interface every capability implements: a name,
a description, a JSON-schema input, and an async `call` returning
`Result<ToolOutput, String>` — the error text is exactly what the model sees,
so there is nothing to translate. `ToolOutput` is `{ text: String, image:
Option<ToolResultImage> }`; every tool but `view_image`/`screenshot` (see
`## Image attachments (vision)` below) only ever sets `text` — `impl
From<String> for ToolOutput` is what lets those call sites stay a bare
`Ok(some_string.into())` instead of naming the struct everywhere. `ToolRegistry`
composes any number of `Tool`s into one `architect_agent::ToolExecutor`;
`ToolRegistry::with_default_tools` builds the ten per-turn built-ins
(`read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`,
`run_command`, `view_image`, `screenshot`, `spawn_subagents`) rooted at a
workspace. `ToolRegistry::with_investigation_tools` builds a second, smaller
registry — `read_file`, `list_dir`, `glob`, `grep`, `view_image` only — for
sub-agent turns (see `## Sub-agents` below); it deliberately excludes
`spawn_subagents` itself, which is the whole recursion guard. The three
long-running-process tools (`start_process`/`get_process_logs`/
`stop_process`) are registered alongside these but built once at the engine
layer instead, for reasons `## Long-running processes` below covers.

Every path-taking tool goes through `ToolContext::resolve`, the single place
that canonicalizes a model-supplied path and rejects one that escapes the
workspace root — added because V1 had that check copy-pasted into each file
tool separately. `run_command` is denylist-checked (`architect-tools::blocklist`,
ported from V1) before it runs — this is **not a sandbox**, only a best-effort
filter over the obvious destructive commands; a quoted or obfuscated command
bypasses it by design.

**Adding a tool**: implement `Tool` and call `registry.register(Arc::new(..))`.
This is also exactly how MCP tools join the same tool set — see `## MCP
client` below; no trait changes were needed for it, matching the "shaped like
`tools/call` from day one" design this trait started with.

A tool that mutates files reports it through `architect_core::ChangeRecorder`
(`ctx.record_change(..)`), not through its `ToolResult` — that trait is what
`architect-session` listens on, bridged by a plain channel the engine owns
(`architect_tools::ChannelRecorder`), so `architect-tools` never depends on
`architect-session` to report a change. MCP tools don't call `record_change`
at all — an MCP server's own filesystem, if it touches one, is outside this
workspace's sandbox and not this app's to track.

## Sub-agents

`spawn_subagents` lets a turn fan investigation work out to focused
sub-agents — e.g. one per crate — each a genuine, first-class child session:
saved to `sessions.db` with `parent_id` set, listed in `SessionsListed`
alongside every other session, shown nested under its parent in the
sidebar, resumable like any other. Nothing about how a sub-agent's own turn
runs is special-cased — its request-triggered handler in `apps/desktop/src/
engine.rs` builds the exact same `SpawnTurn`/`spawn_turn` call a normal
`Command::Send` does (own `ToolRegistry`, own `Agent`, its `AgentEvent`s
streamed to the real UI the same way), just seeded with only the task
prompt (no shared history with its parent) and rooted at whatever sub-path
the task named, and using `with_investigation_tools` instead of
`with_default_tools` for its registry.

The seam a tool needs to reach back into the engine at all —
`architect_tools::SubAgentSpawner`, a `ToolContext` field mirroring
`ChangeRecorder`/`PlanRecorder`'s shape (trait in `architect-tools`, since
unlike those two this one is unavoidably `async`; the real implementation
lives in `apps/desktop`, so `architect-tools` never depends on `Engine`).
`spawn_subagents`'s `call()` reads `ctx.sub_agent_spawner()` and either
awaits each task in a loop or runs them all via `futures_util::join_all`,
based on `SubAgentSpawner::run_sequentially()` — true when
`architect_llm::is_local` says the active provider is a loopback server
(LM Studio, Ollama, vLLM generally can't usefully serve overlapping
requests), false for a real hosted API. The engine-side `spawn()` submits a
request on its own dedicated channel (not a `Command` variant — `Command`
derives `PartialEq`/`Eq`, which no `oneshot::Sender` implements, the same
reason `shutdown` isn't a `Command` either) and awaits a `oneshot` the
worker resolves once that child session's `TaskOutcome` arrives — the
child's own last message, or its error, either way unblocking the tool call
that's waiting on it.

Real-world testing surfaced two harness-level gaps worth knowing about: a
`path` that doesn't exist (or names a file, not a directory) used to pass
the same containment check `write_file` uses — which tolerates a
not-yet-created path — and quietly spawn a child into an unusable scope;
the engine's `SpawnSubAgent` handler now additionally requires the resolved
path to already exist and be a directory, failing fast instead. Separately,
a child that hits its iteration limit, gets cut off at the model's token
limit, or ends on an empty message used to come back as a silent `Ok("")`
or a narration fragment, indistinguishable from a real answer — the
collector in `engine.rs`'s `task_rx` arm now classifies each of those as a
failure instead.

One residual, model-level (not harness) failure mode: a sub-agent's
*enumeration* (a list of files, tests, exports) is reliable, but a
self-reported *count or total* derived from that list can still be a plain
arithmetic slip even when the underlying data was correct — confirmed
non-deterministic by re-running the identical prompt against unchanged
ground truth. `SUBAGENT_SYSTEM` now tells sub-agents to recompute totals
explicitly rather than trust a single mental tally, but a parent orchestrating
`spawn_subagents` calls that depend on an exact number is still better off
re-deriving it mechanically (e.g. re-running the count command itself)
rather than trusting a sub-agent's self-reported total outright.

## Long-running processes

`start_process`/`get_process_logs`/`stop_process` (`architect-tools::
tools::process`) are for anything not meant to exit — `run_command` blocks
until the process does, which is useless for a dev server. The three tools
share an `Arc<ProcessRegistry>` (`architect-tools::process`) — a
`Mutex<HashMap<id, Entry>>` plus one `tokio::spawn`ed supervisor task per
process that exclusively owns the real `Child` handle (reading both output
streams and reacting to a `Notify`-based kill signal via `tokio::select!`
until the process actually exits). Nothing outside that task ever touches
the `Child` directly — the shared map only holds the *observable* state
(command, status, capped log), which is what avoids holding a lock across
an indefinite `.wait()`/`.kill()` await.

**Killing the whole tree, not just the tracked PID**: the spawned command
is `bash -c "{command}"`, so a real command like `npm run dev` forks a
grandchild (`npm` → `node`) that a plain `Child::kill`/`start_kill` — a
single-PID `SIGKILL` — never reaches; the shell wrapper dies, the actual
long-running process is silently orphaned and keeps running (a real bug
this shipped with initially). `start()` spawns each child into its own
process group via `.process_group(0)` specifically so `kill_tree`
(`#[cfg(unix)]`, `libc::kill(-pid, SIGKILL)`) can signal the entire group
at once — the standard Unix idiom for "kill this and everything it
spawned." Both `stop()` and `kill_all()` go through the same `Notify` →
`kill_tree` path, so this fix covers app-shutdown cleanup too, not just an
explicit `stop_process` call. `process::tests::
stopping_a_process_also_kills_the_children_it_spawned` is the regression
test — it backgrounds a real child inside the tracked shell and asserts
that child's pid is actually gone afterward (`kill -0`), not just that the
registry's own status flipped.

This can't live in `ToolContext` or be rebuilt per-turn like the seven
built-ins: `ToolRegistry::with_default_tools` (and the `ToolContext` inside
it) is reconstructed fresh every turn (`engine.rs`'s `spawn_turn`), so
nothing stored there survives from a `start_process` call to a later
`get_process_logs` call. `ProcessRegistry` is instead built once in
`worker()`, alongside `_mcp_connections`/`external_tools`, wrapped in the
three tool structs, and registered into every turn via a new
`process_tools: Arc<Vec<Arc<dyn Tool>>>` field on `SpawnTurn` — a sibling
to `external_tools`, not folded into it, since its lifecycle is different
(built once, never rebuilt — nothing here is driven by saved config the
way MCP/integrations are).

`architect-tools` has no knowledge of `EngineEvent` or any UI (the
dependency rule below forbids it) — `ProcessRegistry::new()` returns its
own `UnboundedReceiver<ProcessEvent>`, and `worker()`'s `tokio::select!`
loop forwards each one as `EngineEvent::Process(event)`, the same
wrap-and-forward shape `EngineEvent::Agent { session, event }` already
uses for `AgentEvent` — minus a session tag, since processes are global
(`Transcript.processes: Vec<ProcessSummary>`, not nested in
`Conversation`), the same reasoning MCP connections/`external_tools`
already get: a process isn't owned by whichever conversation happened to
start it any more than a connected MCP server is. `Transcript::
apply_process_event` finds-or-creates by id and either pushes a new entry
(`Started`), `push_str`s into its `log` (`Output`, capped — the same
policy `ProcessRegistry`'s own log applies, kept in both places so the UI
field can never out-accumulate what the registry intended to retain), or
flips `status` in place (`Exited`) — the exact growing-string/
status-flipped-on-a-terminal-event shape `Row::Assistant`/`Row::Tool`
already use for streamed replies and tool calls, applied here instead.

Closing the window kills every tracked process rather than leaving it
orphaned (nothing on this OS does that automatically for a dead parent's
children) — `main()` constructs `Engine` itself now (a `static ENGINE:
OnceLock<Engine>`, read by `app::app()`'s `use_hook` instead of
constructing its own) specifically so it has a handle to call
`Engine::shutdown()` after `launch(...)` returns. That method bridges into
the worker's tokio channel via a *plain* `std::sync::mpsc` round trip
(`main()` isn't inside any async runtime itself), landing on a dedicated
channel — not a new `Command` variant, since `Command` derives
`PartialEq`/`Eq` and no sender type implements either.

## Plan tool

`write_plan`/`read_plan` (`architect-tools::tools::plan`) let the agent
lay out and update a multi-step plan — `architect_core::Plan { goal:
Option<String>, steps: Vec<PlanStep> }`, each `PlanStep` carrying a
`StepStatus` (`Pending`/`InProgress`/`Completed`) and an optional
`Vec<PlanSubstep>`, one level of nesting only. `write_plan`'s input *is*
`Plan` directly (it already derives `Deserialize`) — no separate `Input`
struct, unlike every other tool.

**A snapshot, not a log**: `write_plan` replaces the whole plan every
call, including status flips on earlier steps — the same shape a real
todo-list tool (e.g. Claude Code's own) already uses. This is what makes
persistence simple: `architect-session`'s `plans` table is one row per
session, upserted (`ON CONFLICT(session_id) DO UPDATE`), not appended to
like `file_changes` — there is exactly one current `Plan` per session, not
a history of edits.

**`read_plan` without a persistence dependency**: `architect-tools` can't
depend on `architect-session` (the dependency rule below forbids it), so
`read_plan` can't fetch a plan itself. Instead `ToolContext` gains
`current_plan: Option<Plan>` (+ `with_current_plan`, alongside the
existing `plan_recorder`/`with_plan_recorder` mirroring `ChangeRecorder`'s
own shape) — the engine hands each turn's `ToolContext` whatever plan was
already known for that session *before* the turn started, the same
"handed in, not fetched" shape `workspace_root` already has. Consequence
worth knowing: a `write_plan` earlier in the *same* turn won't show up in
a `read_plan` later in that turn — the model already knows what it just
wrote, so this only matters for a fresh turn or a resumed session.

**Wiring mirrors `FileChange` throughout** `engine.rs`: a per-turn
`plan_tx`/`plan_rx` channel next to `file_change_tx`/`_rx` (only the last
value drained matters, unlike file changes' `Vec`); `TaskOutcome.plan:
Option<Plan>`; persisted + broadcast as `EngineEvent::PlanUpdated` from
the same post-turn step `FileChanged` already uses; `EngineEvent::
PlanLoaded` sent right after `FileChangesLoaded` on startup resume and
`Command::LoadSession` (not `Command::Rollback` — rollback undoes file
changes, not the plan, so there's nothing to reload there).
`SessionSlot.plan`/`Conversation.plan` are the engine-side and UI-side
copies respectively, the same split `history`/rows already have.
`write_plan`/`read_plan` are always registered (`plan_tools`, built once,
unconditionally — no saved credential or shared runtime state to
construct, simpler than `process_tools`' shared registry), the same
reasoning `process_tools` already established for "always-on app
capability, not gated behind config."

The Inspector's Plan tab is per-conversation (`active_conversation()`,
like Files/Tools/Diff), unlike the Processes tab's global scope — a plan
belongs to one session's task. Its rows have no click/expand interaction,
unlike Processes' log view: a step's description already is its whole
content.

## Context tracking and compaction

**`usage` vs `context_tokens`**: `Conversation.usage` (and `TurnOutcome.
usage`) is a running *sum* across every request a turn or a session ever
made — right for cost, wrong for "how full is the context window right
now," since a turn with several tool round-trips resends the whole growing
history on each one, so summing double- (triple-, ...) counts it. Each
individual response's own `usage.input_tokens` (+ cache read/write) is
already the size of *that* request's whole prompt — a provider reports the
total, not a delta — so the *last* response in a turn is what the next
request will start from. `agent.rs` tracks this separately as `latest_
usage`, replaced (not `+=`) each iteration, and reports it on `TurnOutcome`
as `context_tokens` alongside `context_window: Option<u64>` (`pricing::
context_window_for`, a table living next to `pricing_for` in `architect-
core::pricing` with the same "unknown, don't guess" `None` for local
models). `Conversation.context_tokens`/`context_window` mirror this
one-for-one, replaced on every `TurnCompleted` the same way `cost` already
is (unlike cumulative `usage`).

**`Command::Compact`** asks the model to summarize a session's history,
then replaces it outright with just that summary — freeing context the way
starting a new chat would, without losing the session's identity or its
recorded file changes/plan (those live in their own tables, untouched).
It's spawned on its own lightweight task (`spawn_compact`/
`CompactOutcome`/`compact_tx`/`compact_rx`, the same "never block the
command loop" shape `spawn_turn`/`TaskOutcome`/`task_tx` already
established) but is deliberately *not* a normal turn: it runs through
`Agent::run_turn` with `NoTools` and `max_iterations(1)` rather than a
hand-rolled `Provider::complete` call, getting cancellation and error
handling from the one place that already gets them right, with none of a
real turn's tool registry, file-change channel, or plan channel — nothing
here needs any of that. Persistence is `SessionStore::replace_messages`
(`DELETE` then re-insert at `seq` 0..), a genuine wholesale replace unlike
`append_message`, matching the same "no undo" tradeoff `delete_session`
already makes. `Status::Compacting` is set optimistically client-side
(`Transcript::start_compacting`, the same shape `push_user` already uses
for `Waiting`) before the engine round-trip, and folds into `is_busy()`,
so the Composer's Stop button (and `Command::Cancel`, via the same
`running` map every other in-flight op is tracked in) work on it for free.
`Row::Compacted` is the transcript's record of it: the *only* row left
after a compaction, not appended alongside what was there.

## MCP client

`architect-mcp::connect(command, args, env)` spawns a local MCP server over
stdio (`rmcp::transport::TokioChildProcess`), completes the handshake with
the no-op `ClientHandler` blanket impl on `()` (no sampling or roots support
needed for a tool-only client), and discovers every tool the server exposes
via `Peer::list_all_tools` (paginates internally). Each discovered
`rmcp::model::Tool` becomes an `McpToolAdapter` — `architect-tools::Tool`'s
`name`/`description` must be `&'static str`, but an MCP server's tool names
are only known once connected, so they're leaked once per tool at connect
time (`Box::leak`; a fixed, one-time cost, not a growing one) rather than
reshaping the trait for one implementer. `Tool::call` forwards through a
cloned `Peer<RoleClient>` (`Clone + Send + Sync`, so one connection's handle
is shared safely across concurrent turns) and folds `CallToolResult` back
into `Result<String, String>` — its text content blocks joined,
`is_error == Some(true)` mapped to `Err`.

Saved server configs (`architect_config::McpServerConfig` — name, command,
args, env, `enabled`) live in the same `ConfigStore`/`profiles.json` as API
profiles; unlike a profile, more than one server can be `enabled` at once,
since each just contributes its own tools rather than competing to be "the"
active one. `apps/desktop/src/engine.rs`'s `worker()` connects to every
enabled server once at startup and again after any `SaveMcpServer`/
`DeleteMcpServer` (`reconnect_mcp` — rebuilds the whole connection list from
scratch rather than diffing it; server lists are small and this isn't a hot
path), and registers every connected tool into each turn's own
`ToolRegistry` alongside the seven built-ins (`spawn_turn`, the same
`ToolRegistry::register` extension point, not a new composition mechanism).
Unlike the built-ins, MCP connections are **not** rebuilt per turn — they're
long-lived and shared, captured once when a turn is spawned, the same way
the active provider is. A server that fails to connect is reported once via
the same global-failure path a bad provider config uses
(`EngineEvent::Failed { session: None, .. }`) and skipped; the rest still
connect.

Both stdio and streamable-HTTP transports are implemented —
`architect_mcp::connect_http(url, bearer_token)` is the constructor stdio's
own doc comment once described as the natural extension point, added next
to `connect` without changing anything downstream of transport
construction: the handshake, tool discovery, and `McpToolAdapter` wiring are
identical either way. `McpTransport` (`architect-config`) is a tagged enum
(`Stdio { command, args, env }` / `Http { url, bearer_token }`) that
`engine.rs`'s `reconnect_mcp` dispatches on per saved server.

## GitHub, Slack, and Linear tools

Three more crates fill the same `Tool` extension point MCP does, one per
external service, each read-only — no tool here writes, comments, merges,
or posts anything. Each is a thin `reqwest`-based HTTP client plus one or
two `Tool` impls with compile-time-known names/descriptions/schemas (unlike
`McpToolAdapter`, nothing here needs `Box::leak` — a remote MCP server's
tool names are only known at connect time, these are known at compile
time). None of the three depends on `architect-config` or `apps/desktop`,
matching `architect-mcp`'s own separation: `apps/desktop/src/engine.rs` is
still the one place that knows a saved credential exists at all.

- `architect-github::tools(token)` — eight tools, all sharing one
  `GitHubClient` (`Authorization: Bearer {token}`; list endpoints paginate
  via the `Link: rel="next"` response header): `github_read_pull_request`
  (title, description, author, status, base/head branches, diff stats);
  `github_read_pull_request_comments` (issue/conversation comments, inline
  review comments, and review summaries — three distinct GitHub REST
  endpoints, merged into one chronological list the way a human sees them
  combined in GitHub's own PR view); `github_read_pull_request_diff` (each
  changed file's status and unified-diff patch, straight from GitHub's
  `/pulls/{n}/files`); `github_read_pull_request_commits` (SHA, author,
  date, and message per commit); `github_read_issue` and
  `github_read_issue_comments` (a plain issue — GitHub's issues API also
  answers for PR numbers, so `github_read_issue` flags it when that's what
  happened and points at the richer PR tool instead); `github_read_file`
  and `github_list_directory` (GitHub's contents API returns an object for
  a file, an array for a directory — same request, and each tool errors
  with a pointer to the other one if given the wrong kind of path). Shared
  formatting helpers (`author`/`created_at`/`format_issue_comment`) live in
  `format.rs` since the issue-comments shape is identical whether it's
  reached from a PR or a plain issue. `GitHubClient::build_url`/`get_url`
  exist only for the contents API, whose arbitrary file-path/`ref` inputs
  need real percent-encoding (via `reqwest::Url`, not a new dependency) —
  every other endpoint here interpolates owner/repo/numeric ids into a path
  string directly, which is safe as-is.
- `architect-slack::tools(token)` — three tools, sharing one `SlackClient`.
  Slack's Web API is RPC-style and always answers HTTP 200; failure is a
  body-level `ok: false` + `error` code, not a status code — the one
  service here where that's true. `slack_list_channels` and
  `slack_read_thread` paginate exhaustively via
  `response_metadata.next_cursor`; `slack_read_channel_history` takes one
  page only (bounded by a `limit` input, default 50/max 200), since
  channel history is unbounded and this tool is for browsing recent
  activity, not exhaustive export. `slack_list_channels` (`conversations.
  list`) surfaces each channel's id/name/privacy/membership so an agent can
  find a channel before reading it. `slack_read_channel_history`
  (`conversations.history`) marks any message that started a thread with
  its `reply_count`/`thread_ts`, which is what `slack_read_thread` needs —
  Slack has no dedicated "list threads" endpoint, so browsing history is
  how a thread gets discovered. `slack_read_thread` (`conversations.
  replies`) also accepts a pasted Slack message/thread permalink (`link`)
  as an alternative to `channel`+`thread_ts`; `thread.rs`'s
  `parse_slack_link` decodes the permalink's `p<digits>` segment into a
  timestamp, or reads `?thread_ts=` when the link points at a reply rather
  than a thread's parent. Shared formatting (`format_message`) lives in
  `format.rs`, the same precedent `architect-github`'s own `format.rs` set.
- `architect-linear::tools(api_key)` — `linear_read_ticket` (title,
  description, state, assignee, comments, and blocking relations, all in
  one GraphQL query — Linear's API fetches it all in a single round-trip,
  and blocking relationships are `IssueRelation`s of type `"blocks"` found
  under `Issue.relations`/`Issue.inverseRelations`, not a distinct
  `blocked_by` enum value). Auth: `Authorization: {api_key}`, deliberately
  **no** `Bearer` prefix — confirmed from Linear's own docs, a real point
  of confusion worth knowing before "fixing" it.

Unlike MCP, none of these three needs a live connection kept alive or
reconnected — building a service's tools from its saved credential is
synchronous and infallible for Slack and Linear (`engine.rs`'s
`integration_tools`); whatever goes wrong (a bad token, a 404) surfaces
from the tool's own `call`, same as a built-in tool's own errors do. Saved
credentials (`architect_config::IntegrationsConfig` — `github_token`/
`slack_token`/`linear_api_key`, each a bare `Option<String>`, no id/enabled
flag since there's only ever one of each) live in the same `ConfigStore`/
`profiles.json` as profiles and MCP servers; `rebuild_external_tools`
combines MCP's tools with these three services' into the one tool set
`spawn_turn` registers, rebuilt at startup and after any
`SaveMcpServer`/`DeleteMcpServer`/`SaveIntegrations` command.

GitHub is the one exception to "synchronous and infallible": alongside
`github_token`, `IntegrationsConfig.github_use_gh_cli: bool` opts into
resolving a token by running `gh auth token` (`engine.rs`'s
`resolve_github_token`/`run_gh_auth_token`) instead of reading a saved
one — for someone who already has the GitHub CLI authenticated locally,
skipping both the manual-paste and OAuth-app-registration paths entirely.
This makes `integration_tools` itself `async` (its only caller,
`rebuild_external_tools`, already was) and genuinely fallible for GitHub
specifically — `gh` not installed, or not logged in — reported via a
global `EngineEvent::Failed` rather than silently registering no GitHub
tools. Slack and Linear's paths are completely untouched by this.

A credential can arrive three ways: pasted by hand (`SaveIntegrations`, as
above), via `apps/desktop/src/oauth.rs`'s browser-redirect login
(`Command::StartOAuthLogin` → `oauth::run_login` → `apply_oauth_result`
writes the resulting token into the same `IntegrationsConfig` field a paste
would), or — GitHub only — via the gh-CLI toggle just described. The first
two land in the same place before `rebuild_external_tools` ever runs, so
Slack/Linear's "synchronous and infallible" framing above is unaffected —
OAuth's own network round-trip happens earlier and separately, on
`oauth_rx`, not on the tool-building path.

## Persistence

`architect-session::SessionStore` opens (or creates) `<workspace_root>/.coder/`
and `sessions.db` inside it — the same location V1 used. The schema is new,
not ported: V1 stored a message as OpenAI-shaped columns
(`content`/`reasoning_content`/`tool_call_id`/`tool_calls`); V2's `Message` is
provider-neutral, so one `messages.content` column holds
`serde_json::to_string(&Vec<ContentBlock>)` and round-trips through the same
serde derives the rest of the codebase already uses. `file_changes` mirrors
V1's table closely, including its `old_content: NULL` sentinel for "this
change created the file" — the same signal `reverse_to_point` (ported from
V1's undo) uses to delete on rollback instead of restoring.

Every public method on `SessionStore` is `async` over blocking `rusqlite`,
wrapped in `tokio::task::spawn_blocking` — V1 called blocking SQLite straight
from async Axum handlers, which blocks the executor thread it runs on; this
doesn't.

File paths are stored **relative to the workspace root**, not the absolute
path a tool resolved — so the database stays meaningful if the workspace is
later moved or copied to another machine.

The sidebar (`apps/desktop/src/panels/sessions.rs`) lists every saved session
via `EngineEvent::SessionsListed`. Clicking a row switches to it two possible
ways, decided client-side: if `transcript.conversations.contains_key(&id)` —
already resident, either running in the background right now or loaded
earlier this app run — it's a pure local `Transcript::switch_to`, no engine
call, no risk of a stale DB read clobbering live state. Otherwise it's
`Engine::load_session` → `Command::LoadSession` → `EngineEvent::HistoryLoaded`
— the same event `Conversation::from_history` already used to resume on
startup, so switching to a not-yet-resident session and resuming on launch
are one code path, not two. A busy row (`conversation.status.is_busy()`)
shows a small dot next to its title — the visible proof a session left
running in the background is actually still working. A new session is
titled from the user's first message (`engine::title_from`, truncated to 60
chars) the moment it's created, since every session otherwise starts with
`sessions.title = ''`.

Each row's Delete button calls `Engine::delete_session` →
`Command::DeleteSession` → `SessionStore::delete_session`, a hard delete —
`messages`/`file_changes` cascade via the schema's foreign keys, which only
take effect because `SessionStore::open` sets `PRAGMA foreign_keys = ON`
per connection (SQLite has this off by default; the `ON DELETE CASCADE` in
`schema.rs` is otherwise declared but inert). If a turn is in flight for the
deleted session, it's cancelled first — nothing is left to persist its
result against once the row is gone. If the deleted session was the active
one, `Transcript::apply` reacts to `EngineEvent::SessionDeleted` by starting
a fresh new chat, the same blank state "+ New Chat" produces —
`SessionsListed` alone can't carry that signal, since a deleted id is
exactly what's missing from that list. Deleting an unknown or already-gone
session id is not an error, matching plain SQL `DELETE` semantics.

The Inspector panel's Files tab (`apps/desktop/src/panels/inspector.rs`) has
a Roll Back button per file, calling `reverse_to_point` through
`Command::Rollback` after a confirmation dialog — scoped to files only,
never messages: a rollback restores/deletes files on disk but leaves
`messages` untouched, and is refused (`EngineEvent::Failed`) while a turn is
running for that session, the same guard `Command::Send` uses against a
second concurrent turn.

## Saved API configurations

`architect-config::ConfigStore` is global to the machine
(`~/.config/code-architect/profiles.json`, or `$XDG_CONFIG_HOME` if set), not
per-workspace like `SessionStore` — an API key is set up once and reused
across every project, so it deliberately does not live under `.coder/`. A
`Profile`'s fields mirror `architect_llm::ProviderConfig` by name (`kind`,
`base_url`, `api_key`, `model`) plus a display `name`; `architect-config`
holds no `architect-llm` dependency, so `apps/desktop/src/engine.rs` is the
only place that translates one into the other, the same shape as its
`ChangeRecorder` bridge between `architect-tools` and `architect-session`.

The settings panel (`apps/desktop/src/panels/settings.rs`, opened from the
header's Settings button) only ever talks to the store through
`Command::{ListProfiles,SaveProfile,DeleteProfile,ActivateProfile}` and
`EngineEvent::ProfilesListed` — never directly, the same rule every other
panel follows. `Command::ActivateProfile` rebuilds the running `Provider` and
`Agent` inside `worker()` in place and keeps the conversation in progress: only
which API answers the *next* turn changes, nothing is reset. If the very
first provider build (from `EngineConfig`'s env/default values) fails, the
worker no longer treats that as fatal — `agent` stays `None`, `Command::Send`
reports a friendly error instead of running, and the settings panel stays
fully usable so a bad startup configuration can be fixed from the running app
instead of only from outside it.

API keys are stored in plaintext JSON, matching V1's precedent for
`~/.config/code_architect/config.db`; nothing in this codebase encrypts them
at rest — `IntegrationsConfig`'s GitHub/Slack/Linear credentials follow the
same convention. Unlike `Profile`/`McpServerConfig` (lists, upserted by id),
`IntegrationsConfig` is a single record with `set_integrations` replacing it
wholesale — there's exactly one of each credential, not a named collection
of them.

## Adding a provider

Implement `architect_llm::Provider` — one method, `stream`; the non-streaming
path is derived from it — and register a factory:

```rust
registry.register("my-api", |config: &ProviderConfig| { ... });
```

Do not add a kind for an OpenAI-compatible server. LM Studio, DeepSeek,
OpenRouter, vLLM and Ollama are `kind: "openai"` with a different `base_url`.

Each adapter owns its own history serialization, because the dialects genuinely
disagree: Anthropic takes a turn's tool results batched into one user message,
OpenAI takes one message per result. Normalizing that away would break one of
them.

## Discovering models from an OpenAI-compatible server

`architect_llm::list_models(base_url, api_key)` (`crates/architect-llm/src/
providers/openai.rs`) fetches `GET {base_url}/models` — the endpoint LM
Studio, vLLM, Ollama, and the rest of the OpenAI-compatible family all
report their loaded/available models on, in the same `{"data": [{"id":
...}]}` shape. It is a bare function, not a `Provider`/`OpenAiProvider`
method: its only caller (the "LM Studio" settings tab's "Fetch models"
button) has a candidate `base_url`/`api_key` typed into a form, not a
constructed provider or even a `Profile` — the whole point of this page,
see below. It reuses `http::send_retrying`/`MAX_RETRIES`, the same retry/
error-classification every chat request already gets, so a rate limit or
a 5xx here behaves exactly like it would mid-turn.

`apps/desktop/src/engine.rs`'s `Command::ListModels` is the one command in
`worker()` that skips the `*_tx`/`*_rx` outcome-channel pattern `spawn_turn`/
`spawn_compact` use: it is spawned straight from the command handler and
sends `EngineEvent::ModelsListed` (or `Failed`) directly, because nothing
in the worker's own state — `sessions`, `store`, `provider_config` — needs
updating afterward, only a UI-facing event goes out. `Transcript.
discovered_models` holds the last fetch's result, replaced wholesale each
time, the same "last one wins" shape `profiles`/`mcp_servers` get for
their own `*Listed` events.

"LM Studio" is its own page in Settings (`apps/desktop/src/panels/
settings.rs`'s `Section::LmStudio`/`lmstudio_popup`), not folded into "API
configurations" — deliberately, and not just for layout reasons: what a
local server has loaded changes over time, so persisting a discovered
model as a saved `Profile` would only ever be a stale snapshot the moment
it's written. (An earlier version of this feature did exactly that, with
a "Save one config per model" bulk button; it was removed for this reason
— see this feature's git history if ever tempted to bring persistence
back.) Instead, each discovered model's "Use" button calls `Engine::
use_ad_hoc_model`, which sends `Command::UseAdHocModel { base_url,
api_key, model }`. Its worker handler rebuilds the live `provider` in
place exactly the way `Command::ActivateProfile`'s handler does — same
`ProviderRegistry::build` call, same in-flight-conversation-preserving
effect — but skips every `ConfigStore` step entirely; nothing is written
to `profiles.json`. It reports back with `EngineEvent::AdHocModelActivated
{ model }`, which `Transcript::apply` folds into `active_adhoc_model:
Option<String>` — mutually exclusive with `active_profile` (activating an
ad-hoc model clears the saved-profile field and vice versa, via one line
in each event's `apply` arm) — and `shell.rs`'s `Header` checks it as a
second tier between a saved profile and the engine's own startup default
when deciding what model/kind label to show.

## Image attachments (vision)

`ContentBlock::Image { media_type, data }` (`crates/architect-core/src/
message.rs`) is the wire/persistence shape — `data` is *always* base64 by
the time a value reaches this layer, the same way `ToolResult.content` is
always a plain string. A human attaching one is still the only way an image
enters a *user* turn, which is why the constructor lives on `Message` as
`user_with_images` rather than anywhere near the assistant-turn path — but
the model can now also *request to see* an existing image itself, via the
`view_image` (reads a file off disk) and `screenshot` (captures the screen)
tools: either one sets `ToolOutput.image`, which `ToolRegistry::execute`
carries into a new `ToolResult.image: Option<ToolResultImage>` field
(`ToolResultImage` is the same `{ media_type, data }` shape as
`ContentBlock::Image`, kept as its own type since a tool result should only
ever carry an image, never any other content block). The model still never
*generates* image pixels — it only ever sees ones that already exist,
whether attached by a human or read/captured by a tool.

Raw bytes, not base64, are the representation everywhere *except* right at
that core layer: `apps/desktop/src/engine.rs`'s `Attachment { media_type,
bytes }` is what the composer reads off disk, what `Command::Send` carries,
and what `state::Row::User.images` renders from. Base64 only happens
twice — encoded once in `engine.rs`'s `Command::Send` handler, the one
place a `Message` actually gets built, and decoded once in `state.rs`'s
`Conversation::from_history`, when a resumed session's saved images need
turning back into bytes for display. Neither happens on every render,
which matters: `ImageViewer` (from `freya-components`, unused anywhere
else in this codebase before this) decodes asset bytes asynchronously on
its own, so repeated encode/decode work on a hot render path would be
wasted work at best.

Provider serialization is asymmetric, and that asymmetry is real, not an
oversight: Anthropic's `serialize_content` (`crates/architect-llm/src/
providers/anthropic.rs`) already emits a content array per message, so an
`Image` block is one additive match arm. The OpenAI-compatible adapter's
`serialize_messages` (`.../providers/openai.rs`) emits `content` as a
plain string for every message today; it only switches to the dialect's
array-of-parts shape (`image_url` parts plus an optional trailing `text`
part) for a message that actually carries an image, leaving the far more
common text-only path untouched.

A tool result's image serializes differently per provider for the same
reason a user-attached one already did, plus one more constraint: Anthropic
nests it *inside* that specific `tool_result` block's own content array
(`serialize_content`'s `ContentBlock::ToolResult` arm) — the only
unambiguous place for it when several tool calls run in parallel in one
turn and only one of them returns an image; Anthropic's own computer-use
tool returns screenshots the same way. The OpenAI-compatible dialect has no
way to put an image inside a `tool`-role message at all, so
`serialize_messages` folds a result's image into the same trailing
`image_url` message a `ContentBlock::Image` already produces — the model
still sees it, just not attributed to a specific `tool_call_id` (this
dialect has no way to express that attribution either).

Whether the active model *actually* supports vision is informational only
— `architect_core::pricing::supports_vision` (same lookup-table shape as
`pricing_for`/`context_window_for`, `false` for anything not in the
table) drives a badge next to the model chip, never a gate on the Attach
button or on `view_image`/`screenshot`. Most real usage of this app is a
local LM Studio model this table has no data for either way, so treating
"not in the table" as "definitely
can't" would block the app's own primary audience far more often than it
would ever protect anyone from a real error.

Video is explicitly out of scope: neither Anthropic's Messages API nor the
OpenAI chat-completions dialect accept raw video as input, so "supporting"
it would mean this app doing its own client-side frame extraction (e.g.
shelling out to `ffmpeg`) — ruled out as a separate, much larger feature
rather than folded into this one; revisit only if it turns out to be
genuinely needed.

## UI <-> core seam

Implemented in `apps/desktop/src/engine.rs`. The UI and the agent communicate
**only** through a channel pair:

```
Freya UI (main thread) ──Command──►  worker on a Tokio runtime thread
                       ◄─EngineEvent──
```

Freya owns the main thread and runs its own executor; reqwest needs a Tokio
reactor. So the agent gets a dedicated runtime thread, and both channels are
unbounded — a busy UI must never stall the model stream.

Every session's turn runs as its own `tokio::spawn`ed task inside `worker()`,
not inline in its command loop — that loop only ever does quick, non-blocking
work (look up or create a session's slot, persist the user's message, spawn
the turn) before going back to `select!`ing on `commands` and a second,
internal `task_tx`/`task_rx` pair a finished turn reports back on. That is
what lets a `Command::Send` or `Command::LoadSession` for one session be
processed immediately while another session's turn is still streaming — the
loop is never stuck `.await`ing a turn the way an earlier, single-session
version of this file was. Each session's history lives in its own
`SessionSlot` inside a `sessions: HashMap<SessionId, SessionSlot>`, taken out
via `mem::take` for the duration of its spawned task and merged back in from
`TaskOutcome` when it reports done — never touched by anything else in the
meantime, which is why switching back to a still-running session's
conversation shows it live rather than a stale snapshot.

`Agent::run_turn` needs a concrete `&UnboundedSender<E>` (`E: From<
AgentEvent>`), so it can't be handed a session-tagging wrapper directly. Each
spawned turn instead uses a small *local* `AgentEvent` channel (`E =
AgentEvent` trivially satisfies the bound) and runs a forwarder task
alongside `run_turn` that tags each event with the session before re-sending
it as `EngineEvent::Agent { session, event }` on the real outer channel. Do
not add a second channel for errors on the *outer* channel — a failure would
then be able to overtake deltas emitted before it — `Cancelled`/`Failed`
carry a session tag (`Failed`'s is `Option<SessionId>`; `None` is a genuinely
global failure, shown wherever the UI currently is) precisely so they can
share it.

Tools are built fresh per turn, not once at startup — see `spawn_turn` — so
that two sessions' concurrent `write_file`/`edit_file` calls report
`FileChange`s each tagged for the session that made them, instead of both
funneling into one shared, session-blind recorder.

A session's id is minted client-side (`architect_session::SessionId::new()`,
`pub` rather than `pub(crate)` for exactly this) the moment "+ New Chat" is
clicked, before either side of the channel does any work for it — see
`Transcript::start_new_chat` in `apps/desktop/src/state.rs`. That is what
lets the worker key everything by session from the first message, and is why
there is no `Command::Reset` any more: starting a new chat needs no engine
round-trip at all until something is actually sent into it.

The UI folds events into state with `Transcript::apply`
(`apps/desktop/src/state.rs`), a pure function with no IO and no Freya types,
which is what makes the streaming behavior testable without a model or a
window. `Transcript` keeps one `Conversation` (rows, status, usage, cost) per
`SessionId` in a `HashMap`, not one flat set of fields — `apply` always
routes an event into the conversation it names, never into "whichever one is
active," so a session's conversation keeps accumulating whether or not it's
the one on screen. No component ever calls the agent directly; they send a
`Command`.

## Freya conventions

Freya 0.4 uses a **builder API — there is no `rsx!` macro**. Examples written
for 0.3 will not compile.

* One component per file; the file is named after the component.
* Reusable UI is a `#[derive(PartialEq)] struct` implementing `Component`, so it
  gets its own hook scope. Small stateless fragments can be plain
  `fn x() -> impl IntoElement`.
* Style with tokens from `architect_ui::theme`, never with literal colors.
* Component-wide styling belongs in `architect_ui::theme::dark` via
  `Theme::set` and the component's `*ThemePartial` — not restyled at call sites.
* Build dynamic lists by folding with `.child(..)` in a loop.
* `ResizablePanel`'s `initial_size` is read once, at mount — changing it on
  a later render of the same instance does nothing; the rendered width
  only changes via drag-resize or via mount/unmount. To make a panel's
  size actually change at runtime (e.g. collapsing a sidebar to a strip),
  render two branches with different content and give each a distinct
  `.key(...)` (`KeyExt`) — the differing key forces Freya to unmount the
  old instance and mount a genuinely new one, correctly re-triggering
  `ResizableContext`'s panel registration with the new size. See
  `shell.rs`'s Sessions/Inspector collapse toggles.
* `SelectableText` (`freya::components`) handles its own pointer-down —
  including an explicit `e.stop_propagation()` — to start a drag-select.
  Never wrap it around, or place it inside, anything that is itself a
  click target (a row with an `.on_press` toggle, a button): the press
  never reaches that handler. `transcript.rs`'s `ToolRow` is the pattern
  to copy — the collapsed header (a click target, toggles the row open)
  stays plain `label()`s; only the expanded output block below it (never
  a click target) is `SelectableText`.
* Verifying selection/copy in a headless `freya_testing` test: drive it
  through real events (`click_cursor` to focus, then `send_event` with a
  `PlatformEvent::Keyboard` carrying `Modifiers::ctrl_or_meta()` for
  Ctrl+A/Ctrl+C) and check the result with a screenshot — `SelectableText`
  paints a highlight over whatever's selected, so a PNG after Ctrl+A is a
  direct check that selection happened. `freya_clipboard::Clipboard::get()`
  cannot be called from a bare `#[test]` body to also assert Ctrl+C's
  result: it calls `consume_root_context()` internally, which panics
  ("trying to access Freya's current context outside of it") anywhere
  that isn't inside the component tree's own render/hook call — this
  harness has no supported way to reach that from outside.

## Errors

`thiserror` enums in libraries, `anyhow` at the binary edge. No `unwrap` outside
tests and `main`.

## Checks before committing

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace
```
