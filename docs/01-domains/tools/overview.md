---
id: domain.tools
type: domain
title: Tools Domain
depends_on:
- domain.conversation
relations:
  related_rules:
  - rule.sandboxing
  - rule.persistence
  related_flows:
  - flow.subagents
---

The tools domain (`architect-tools`) provides the agent's built-in capabilities: files, search, a shell, background processes, plans, and sub-agent spawning. Nothing here depends on where a turn or session is stored.

## The `Tool` trait

Deliberately shaped like MCP's `tools/call` so a future MCP client needs only a thin adapter:

```
fn name(&self) -> &'static str          // stable — part of the cached prompt prefix and every stored ToolCall
fn description(&self) -> &'static str
fn input_schema(&self) -> Value         // JSON Schema
fn mutates(&self) -> bool { false }     // documented, not yet enforced — a seam for a future permission prompt
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, String>
```

- **`ToolOutput { text: String, image: Option<ToolResultImage> }`** — `From<String>` lets text-only tools return bare strings. There is no `ToolError` type: the error string is exactly what the model sees to recover.
- **`ToolContext`** — `{ workspace_root (canonicalized), recorder: Arc<dyn ChangeRecorder>, plan_recorder: Arc<dyn PlanRecorder>, current_plan: Option<Plan>, sub_agent_spawner: Arc<dyn SubAgentSpawner> }` with builder methods. `resolve(path)` is the **single sandbox entry point**: joins against the root, canonicalizes the first existing ancestor, requires `starts_with(workspace_root)`, then re-attaches any nonexistent suffix (so `write_file` can create files). Rejects `../../etc/passwd`, absolute paths outside the root, and symlinks pointing out.
- **`ToolRegistry`** — `HashMap<&'static str, Arc<dyn Tool>>` + context; `register(Arc<dyn Tool>)` (fluent; duplicate names replace); implements `architect_agent::ToolExecutor`. An unknown tool name yields `ToolResult::error(id, "no tool named {:?} is available")` — never a panic.
  - `with_default_tools(root)` — the ten per-turn built-ins.
  - `with_investigation_tools(root)` — exactly five read-only tools for sub-agent turns: `read_file`, `list_dir`, `glob`, `grep`, `view_image`. Deliberately excludes `spawn_subagents` (the whole recursion guard) and any credential tool.
- **Recorders** — `ChannelRecorder(UnboundedSender<FileChange>)` and `ChannelPlanRecorder(UnboundedSender<Plan>)`; newtypes because the orphan rule blocks implementing the core traits directly on tokio senders. Dropped channels silently drop events.

## The built-in tools

| Tool | Mutates | Parameters | Semantics |
|---|---|---|---|
| `read_file` | no | `path` (req), `offset`, `limit` | Line-numbered output (1-based numbers, 0-based offset). Binary sniff over first 8,000 bytes (any NUL/control char → "appears to be a binary file"). |
| `write_file` | yes | `path`, `content` | Read-before-write for the change record (`NotFound` → `old_content: None`, the creation sentinel); creates missing parents; returns `"Wrote {path}"`. |
| `edit_file` | yes | `path`, `old_string`, `new_string`, `replace_all?` | Exact-substring replace; ambiguous match → error unless `replace_all`. Falls back to curly-quote→straight normalization (positions preserved 1:1). Returns a unified diff (radius 3). |
| `list_dir` | no | `path?` (default `.`) | Immediate entries, name-sorted, directories suffixed `/`. Does not follow symlinks for `is_dir`. |
| `glob` | no | `pattern` (req) | Rooted at the workspace; expands one `{a,b}` brace group; newest-modified first. No matches → success with "no files match". |
| `grep` | no | `pattern` (req), `path?`, `glob?`, `output_mode? (content\|files_with_matches\|count)`, `-i?`, `-C/-A/-B?`, `head_limit?` | Regex; `ignore` crate walker (`.gitignore` respected even without a `.git` dir, hidden files included); caps: 500 files, 20,000 chars (tail-truncated). |
| `run_command` | yes | `command` (req), `timeout? (s)`, `description?` (accepted, discarded) | Denylist-checked first; `bash -c` with cwd = workspace root; 30s default / 120s max timeout; 100,000-byte output cap (head-kept); nonzero exit → `Err`. |
| `view_image` | no | `path` (req) | Returns the image as a tool result (png/jpg/gif/webp → base64). |
| `screenshot` | no | — | Captures the primary display to PNG via `xcap`; headless → clean `Err`, never a panic. |
| `spawn_subagents` | yes | `tasks` (req, 1..=8), each `{ prompt, path }` | See the sub-agents flow. Sequential vs concurrent is decided by `spawner.run_sequentially()` (local provider → sequential), not the model. |
| `start_process` / `get_process_logs` / `stop_process` | — | `command`+`description?` / `id` / `id` | **Not in `with_default_tools`** — the engine constructs one shared `Arc<ProcessRegistry>` and registers these separately, because `ToolContext` is rebuilt fresh per turn and the registry must survive from one call to a much later one. |
| `firefox_open` / `firefox_logs` / `firefox_click` / `firefox_fill` / `firefox_eval` / `firefox_screenshot` / `firefox_close` | yes | `url` / `clear?` / `selector` / `selector`+`value` / `script` / — / — | The seven browser tools (`architect-firefox`) follow the **same engine-owned pattern** as the process tools: built once in the worker, chained into every main turn, excluded from sub-agent turns. See the [Browser Tools Integration](../../04-integrations/firefox.md). |
| `write_plan` / `read_plan` | — | plan JSON / — | Always registered by the engine (`plan_tools`). |

## File-mutating tools and change recording

`write_file` and `edit_file` build a `FileChange { file_path (absolute resolved), old_content, new_content, tool_name: "write_file"|"edit_file" }` and call `ctx.record_change(..)` — forwarded to the injected `ChangeRecorder` (`ChannelRecorder` in the app). The user-facing text uses the model-supplied relative path; the recorded change uses the absolute path. MCP tools never call `record_change` — an MCP server's own filesystem is outside this workspace's sandbox.

## The shell: `run_command` denylist

Explicitly **not a sandbox** — no seccomp, container, or chroot. The command is split on `\n`, `;`, `&&`, `||`, `|` (not quote-aware, a known accepted limitation); per statement the first token (path-stripped) is checked:

- Unconditional: `su`, `sudo` → "is never allowed".
- `BLOCKED_COMMANDS`: `dd, mkfs, format, shutdown, reboot, poweroff, halt, kill, pkill, chown, passwd, wget, rmdir, del`.
- Pairs: `rm -rf/-fr/--recursive`, `chmod 0`, `chmod 777`.
- Patterns: `^rm -[rfR]`, `^chmod 0|777`, `^curl -o /`, `^mv … /dev/null`, fork bombs, writes to raw block devices.

Quoted/obfuscated commands bypass it by design — it stops the obvious, unintentional mistake, not a deliberate attempt. Note the app's own process killing goes through the registry's internal `libc::kill`, so the model can never kill arbitrary processes via the shell.

## Background processes (`ProcessRegistry`)

- Built once in the engine's worker, `Arc`-shared into the three tools. `new() -> (Self, UnboundedReceiver<ProcessEvent>)` — the event receiver is the engine's to forward.
- `start` spawns `bash -c` with cwd = workspace root, piped output, `kill_on_drop(true)`, and **`process_group(0)`** (Unix) so the whole tree is killable. Returns `proc-{n}` immediately.
- One `tokio::spawn`ed supervisor task per process exclusively owns the `Child`: `tokio::select!` over stdout/stderr lines, a `Notify`-based kill signal, and `child.wait()`. Nothing else touches the `Child` — the shared map holds only observable state (command, status, capped log), which avoids holding a lock across an indefinite await.
- **Killing the whole tree**: `kill_tree` on Unix is `libc::kill(-pid, SIGKILL)` — a process-group-wide signal. A single-PID kill would leave `npm run dev`'s real `node` grandchild orphaned (the original bug this design addresses). Both `stop()` and `kill_all()` (app shutdown, 2s grace) go through this path.
- `ProcessStatus`: `Running | Exited(i32) | Stopped | Failed(String)`. `ProcessEvent`: `Started | Output | Exited` — forwarded by the engine as `EngineEvent::Process` (no session tag — processes are app-global).
- Log cap: 200,000 bytes per process, drained from the **front** (oldest dropped) — the opposite direction of `run_command`'s tail truncation.
- `stop()` on an already-exited process is an `Ok` with an explanatory message, not an error.

## Plans (`write_plan` / `read_plan`)

- `write_plan`'s input deserializes **directly into `architect_core::Plan`** (no wrapper struct). Behavior is **full-replace snapshot** — the whole plan is forwarded to the `PlanRecorder`, no diffing. Returns the formatted plan (goal line, `n. [glyph] description`, substeps indented; glyphs blank/`~`/`x`).
- `read_plan` takes no parameters and returns the formatted `ctx.current_plan()`, or "No plan saved yet for this session." The current plan is handed in **at turn start** — a known accepted limitation: a `write_plan` earlier in the *same* turn isn't visible to a later `read_plan` in that turn.

## Sub-agent spawner seam

`SubAgentSpawner` (async trait, in `architect-tools` since it's unavoidably async and `architect-core` stays runtime-free): `run_sequentially() -> bool` and `spawn(prompt, path) -> Result<String, String>`. `NoSubAgentSpawner` reports "sub-agents are not available in this context". The real implementation lives in `apps/desktop` — see the sub-agents flow.

## Test coverage

72 inline tests, no `tests/` dir: sandbox escape (relative + absolute), the exact 10/5 tool-name sets, unknown-tool reporting, process start/stop/kill-tree (including a regression test proving a grandchild is actually dead), denylist (recursive rm, sudo, fork bomb, compound statements, quoted-bypass), per-tool behavior (curly-quote fallback, binary stubbing, gitignore respect, timeout), plan formatting, and sequential-vs-concurrent spawning via in-flight counters. Known gaps: no symlink-escape test despite `resolve`'s docs promising it; `grep`'s `head_limit` truncates the *file list*, not matching lines as the schema says; byte-slicing truncation in `cap()`/`grep` could panic on a multibyte char straddling the cap.