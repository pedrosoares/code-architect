# Code Architect

A modular code harness — a coding-agent workstation — written in Rust with a
[Freya](https://freyaui.dev) desktop UI.

## Status

Type a message and it streams back from a real model, with reasoning shown
separately, a Stop button that cancels the turn in flight, and the model
actually able to read and change files in the workspace: `read_file`,
`write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `run_command`. It can
also see: `view_image` to look at an image file already on disk, and
`screenshot` to capture the screen right now — the same vision path a
human's attached image already uses, just triggered by a tool call. Every
message and file change is durably saved to `.coder/sessions.db` in the
workspace as the conversation happens. The sidebar lists every saved session
and switches between them — including while another one is still streaming:
sessions run concurrently, so starting a turn in one, switching to a
different one and using it, and switching back at any time (even mid-stream)
all just work, and a small dot marks any session still working in the
background. Starting a new chat does not lose the old one. Each row also has
a Delete button that permanently removes that session and everything
recorded against it.

`run_command` blocks until its process exits — no good for a dev server or
a watcher. For those, three more tools manage a long-running process
directly: `start_process` (spawns it in the background and returns
immediately), `get_process_logs` (its accumulated stdout/stderr and current
status), and `stop_process` (kills it). The Inspector's Processes tab shows
every process the agent has started, running or finished, with a status dot
that turns into a checkmark or an X — click a row to expand its log inline,
live as it grows. Processes are global to the app, not tied to any one
session (same as MCP servers), and closing the window kills anything still
running rather than leaving it orphaned.

Two more tools let the agent lay out and track a plan before executing it:
`write_plan` saves a list of steps — each with a status (pending, in
progress, completed) and optionally its own sub-steps — replacing whatever
was saved before, and `read_plan` reads it back. The Inspector's Plan tab
shows the current plan live as it's written: a status glyph per step
(○/●/✓), sub-steps indented beneath. Unlike Processes, a plan belongs to
one session's task, not the whole app, and it's saved to `.coder/
sessions.db` — a resumed session picks its plan back up automatically.

The Settings button in the header opens a panel with four tabs. "API
configurations" adds, edits and removes saved API configurations — a name,
provider, optional base URL and key, and a model; picking one with "Use"
switches the running conversation to it immediately, without losing the
conversation in progress. "LM Studio" is a separate, dedicated page (its
own scrollable model list) for an OpenAI-compatible local server — LM
Studio, vLLM, Ollama, and the like: "Fetch models" lists whatever it
currently has loaded, and each row's "Use" button switches the running
conversation to that model immediately, the same way a saved
configuration's "Use" does. Nothing on this page is ever saved as a
configuration — what a local server has loaded changes over time, so a
saved snapshot of it would only ever be stale the moment it's written; the
page just remembers the Base URL/API key you typed for as long as the app
keeps running. "MCP servers" adds, edits, enables/disables and
removes MCP servers, over either transport: a locally spawned command (like
`npx`/`uvx`, with its args/env) or a remote server over streamable HTTP (a
URL and an optional Bearer token) — every enabled server's tools join the
same tool set `read_file`/`write_file`/etc. already sit in, reconnected
automatically whenever the saved list changes. "Integrations" holds a
GitHub token, a Slack token, and a Linear API key — each one, once set,
turns on that service's read-only tools: GitHub's eight cover a pull
request's summary, comments, diff, and commits, a plain issue and its
comments, and reading a repo's files/directories at any ref (`github_read_
pull_request`, `github_read_pull_request_comments`, `github_read_pull_
request_diff`, `github_read_pull_request_commits`, `github_read_issue`,
`github_read_issue_comments`, `github_read_file`, `github_list_directory`);
Slack's three (`slack_list_channels`, `slack_read_channel_history`, and
`slack_read_thread` — which also accepts a pasted Slack message/thread link
in place of a channel ID + timestamp); and Linear's `linear_read_ticket`
(which also reports the ticket's comments and what it's blocked
by/blocking, all in one call). Each field has a "Login" button
next to it that opens your system browser to sign in there instead of
pasting a token by hand — see "Connecting GitHub/Slack/Linear" below for the
one-time setup that button needs. All three are saved machine-wide
(`~/.config/code-architect/profiles.json`, not per-workspace).

The right-hand Inspector panel shows what the active conversation has
actually done. A Tools tab lists every tool call with its status; clicking
one scrolls the transcript to that exact call and expands its output. A
Files tab lists every file the conversation touched (including ones from a
resumed, previously saved session); clicking a file switches to a Diff tab
showing what changed in it — real syntax-highlighted color per token for a
handful of common languages (Rust, Python, JS/TS, JSON, Bash; anything else
still gets plain insertion/deletion-colored text), across every edit made to
it this conversation. Each file also has a Roll Back button that, after a
confirmation dialog explaining exactly what it does, undoes every file
change made after that point in the conversation — restoring or deleting
files on disk. Chat history itself is never touched by a rollback.

Both side panels — Sessions and Inspector — collapse to a narrow strip via
a button in their header, freeing up space for the Transcript in the
middle; a matching button on the strip restores them to their previous
size. The Transcript/Composer panel itself is not collapsible.

Transcript text — message bubbles, reasoning, a tool's expanded output,
errors — is real, click-drag-selectable text: select any of it and Ctrl+C
(Cmd+C on macOS) copies it, the same as any text field. That's a
deliberate tradeoff against showing rich markdown (bold, code blocks,
bullet lists) in message bubbles, which this app used to render but which
has no selection support at all — a message's raw markdown source shows
as plain text now, in exchange for actually being able to get it out of
the app.

The composer's "Attach" button opens a native file picker for an image
(PNG/JPEG/GIF/WebP); it rides along with the next message you send to
whichever model is active, for models that accept image input — a "vision"
badge next to the model chip in the header shows when the active model is
known to support it (currently the Claude 5 family), though attaching
never requires it: most real usage here is a local LM Studio model this
app has no published capability data for either way, so the button stays
available regardless and an incompatible model's own rejection, if any,
surfaces the normal way an error would.

The status bar shows how full the model's context window is, alongside the
running token/spend totals: `<tokens> / <window> context (<percent>%)` for a
model with a published context window, or "unsized model" for a local one
with no published ceiling to measure against. The "Compact" button next to
Settings asks the model to summarize the active conversation, then replaces
its history with just that summary — freeing up context the same way
starting a new chat would, without losing the session's identity, its saved
file changes, or its plan. The conversation keeps running afterward exactly
as before; only what gets sent as history shrinks.

## Prerequisites

Rust 1.96.0 (pinned in `rust-toolchain.toml`; rustup installs it automatically).

Freya renders with Skia. Skia itself downloads prebuilt binaries, so it does not
need to be compiled, but the final link needs a handful of system libraries. On
Fedora:

```sh
sudo dnf install freetype-devel fontconfig-devel libglvnd-devel wayland-devel
```

Those four cover `libfreetype`, `libfontconfig`, `libEGL` / `libGL` /
`libGLESv2` (all from `libglvnd-devel`) and `libwayland-egl`. Without them
`cargo build` compiles everything and then fails at link with
`unable to find library -l...`.

The first build takes several minutes; later builds are incremental.

## Running

```sh
cargo run -p desktop                      # opens the window
ARCHITECT_LOG=debug cargo run -p desktop  # with logging
cargo run -p desktop --features devtools  # with Freya's element inspector
```

By default it talks to a local OpenAI-compatible server at
`http://localhost:1234/v1` using `qwen/qwen3.8-27b` — start LM Studio (or
Ollama, vLLM, llama.cpp) and it works with no configuration. A saved,
activated configuration (see below) overrides these env vars once one exists.

| Variable | Meaning |
|---|---|
| `ARCHITECT_PROVIDER` | `openai` (default) or `anthropic` |
| `ARCHITECT_BASE_URL` | endpoint root, including any version segment |
| `ARCHITECT_API_KEY` | required for `anthropic` |
| `ARCHITECT_MODEL` | model id |
| `ARCHITECT_CONFIG_DIR` | where saved API configurations live; defaults to `~/.config/code-architect/` |

```sh
# Anthropic
ARCHITECT_PROVIDER=anthropic ARCHITECT_API_KEY=sk-... ARCHITECT_MODEL=claude-opus-5 \
  cargo run -p desktop

# any OpenAI-compatible host
ARCHITECT_BASE_URL=https://openrouter.ai/api/v1 ARCHITECT_API_KEY=... \
  ARCHITECT_MODEL=anthropic/claude-opus-5 cargo run -p desktop
```

A misconfigured provider does not crash the window — the error appears in the
transcript.

## Connecting GitHub/Slack/Linear

Each provider's Integrations tab has both a token field (paste one in by
hand) and a "Login" button (opens your browser to sign in, then redirects
back to the app on `http://127.0.0.1:53682/callback`). The token field works
with no setup. The Login button needs a one-time OAuth App registration —
this app can't register those itself, since that's an action only you can
take on each provider's own site — and the resulting client id (and, for
GitHub only, a client secret) set as four environment variables:
`GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET`, `SLACK_CLIENT_ID`,
`LINEAR_CLIENT_ID`. These are baked into the binary at *compile* time (not
read at runtime — see `apps/desktop/build.rs`'s doc comment for why), so a
downloaded build just works with no setup, the same tradeoff GitHub
Desktop makes for its own embedded OAuth secret:

- **Locally**: `cp .env.example .env`, fill in whichever values you have,
  then `cargo build`/`cargo run` — `.env` is gitignored, so nothing here
  ever gets committed. Values are picked up fresh on the next build
  whenever `.env` changes.
- **In CI** (`.github/workflows/ci.yml`): the same four names come from
  GitHub Actions repo secrets instead. **GitHub Actions won't allow a repo
  secret named `GITHUB_*`** (a reserved prefix), so the two GitHub values
  are stored under different names and remapped in the workflow: add
  repo secrets named `OAUTH_GITHUB_CLIENT_ID` and
  `OAUTH_GITHUB_CLIENT_SECRET` (not `GITHUB_CLIENT_ID`/
  `GITHUB_CLIENT_SECRET` — GitHub will refuse to create those), plus
  `SLACK_CLIENT_ID` and `LINEAR_CLIENT_ID` as-is, under Settings → Secrets
  and variables → Actions.

Until these are set, clicking Login fails fast with a message pointing
back here; the paste-a-token field keeps working regardless.

GitHub has a third option, no OAuth app or token needed at all: the "Use
gh CLI instead of a token" toggle on the GitHub row. If you already have
[the GitHub CLI](https://cli.github.com) installed and logged in
(`gh auth login`), turning this on and saving makes GitHub's tools resolve
a token by running `gh auth token` each time they're rebuilt, instead of
using a saved one — the token field and Login button disappear while it's
on. If `gh` isn't installed or isn't logged in, GitHub's tools simply
won't register and a message explaining why appears wherever failures
normally surface in this app.

- **GitHub**: Settings → Developer settings → OAuth Apps → New OAuth App.
  Authorization callback URL: `http://127.0.0.1:53682/callback`. Copy the
  Client ID into `GITHUB_CLIENT_ID`, then generate and copy a Client Secret
  into `GITHUB_CLIENT_SECRET` — GitHub requires a secret in the token
  exchange even with PKCE, so unlike Slack and Linear this one can't be
  secret-free. No scope needs configuring on the OAuth App itself — Login
  requests the `repo` scope directly (GitHub's classic OAuth apps have no
  read-only scope for private repos, so this is the minimum that makes
  private-repo tools work; a token with no scope at all gets a `404`, not
  a `403`, on anything it can't see, since GitHub avoids confirming a
  private repo even exists to a token without access).
- **Slack**: api.slack.com/apps → Create New App → From scratch. Under
  OAuth & Permissions: enable PKCE, add the redirect URL
  `http://127.0.0.1:53682/callback`, and add `channels:history`,
  `groups:history`, `im:history`, `mpim:history` under **User Token
  Scopes** specifically (not Bot Token Scopes — PKCE-enabled Slack apps
  can only request user-token scopes, so Login yields a `xoxp-` user
  token rather than a `xoxb-` bot token; `architect-slack` works the same
  either way). Install the app to your own workspace. Copy the Client ID
  into `SLACK_CLIENT_ID` — no secret needed.
- **Linear**: Workspace Settings → API → OAuth Applications → Create new
  OAuth Application. Redirect URI: `http://127.0.0.1:53682/callback`. Copy
  the Client ID into `LINEAR_CLIENT_ID` — no secret needed.

## Layout

```
apps/
  desktop/            the Freya binary → `code-architect`
crates/
  architect-core/     shared types: messages, tool calls, usage, pricing, file changes, plans
  architect-llm/      provider trait + OpenAI and Anthropic adapters
  architect-agent/    the tool-call loop
  architect-tools/    read_file, write_file, edit_file, list_dir, glob, grep, run_command,
                      view_image, screenshot, start_process/get_process_logs/stop_process,
                      write_plan/read_plan
  architect-mcp/      MCP client — connects over stdio or streamable HTTP, adapts its tools
  architect-github/   read-only GitHub tools — pull requests (summary, comments, diff,
                      commits), issues (and their comments), and repo file/directory reads
  architect-slack/    read-only Slack tools — list channels, browse a channel's history,
                      and read a thread (by channel+timestamp or a pasted Slack link)
  architect-linear/   read-only Linear tools — a ticket, its comments, and its blockers
  architect-session/  .coder/sessions.db — messages, file changes, reverse-to-point
  architect-config/   ~/.config/code-architect/profiles.json — saved API configs, MCP servers, integrations
  architect-ui/       design tokens + reusable widgets (depends on freya only)
```

Dependency rules, the tool interface, and the persistence schema are documented
in [GUIDELINES.md](GUIDELINES.md).

## Where things live in your workspace

Running the app against a directory creates `<workspace>/.coder/sessions.db` —
the same location V1 used. File tools (`write_file`, `edit_file`, `run_command`,
`start_process`) are sandboxed to that workspace root and cannot read or
write outside it.

## Using another provider

Anything speaking the OpenAI chat-completions dialect needs no new code, only a
different `base_url`:

```rust
let provider = ProviderRegistry::default().build(
    &ProviderConfig::new("openai").base_url("http://localhost:1234/v1"),
)?;
```

That covers LM Studio, DeepSeek, OpenRouter, vLLM and Ollama. Anthropic is
`ProviderConfig::new("anthropic").api_key(..)`. A genuinely different API
implements `Provider` and registers a factory under its own kind.

## Checks

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace     # type-checks without linking, so it needs no system libs
cargo test --workspace
```

The suite is hermetic — the provider tests replay recorded SSE streams against a
mock server, the tools run against a tempdir workspace, and `architect-session`
runs against a tempdir `.coder/` — no keys, no network, no writes outside a
tempdir. There are also live tests, ignored by default, that hit a real local
model:

```sh
cargo test -p architect-agent --test live_lmstudio -- --ignored --nocapture --test-threads=1
cargo test -p desktop -- --ignored --nocapture --test-threads=1
cargo test -p architect-mcp -- --ignored --nocapture
```

The first exercises the provider and the loop. The second drives the whole
desktop pipeline end to end in a tempdir workspace — engine thread, agent,
tool calls, event channel, transcript reducer, and `.coder/sessions.db` — one
test streams a plain reply, another has the model call `write_file`, another
saves a real MCP server and has the model call one of its tools, another has
the model call `start_process` and checks a real `EngineEvent::Process`
comes back, and checks each landed where it should (a file on disk, rows in
`sessions.db`, a `ToolStarted` event with that tool's name). Everything but
the pixels. They
default to `http://localhost:1234/v1` and `qwen/qwen3.8-27b`; override with
`ARCHITECT_LIVE_BASE_URL` / `ARCHITECT_LIVE_MODEL` (or the `ARCHITECT_*` vars
above for the desktop tests — `ARCHITECT_WORKSPACE` is set internally by the
tests themselves to a tempdir, so running them never touches this repository).
The third, `architect-mcp`'s own live test, and the desktop MCP test, both
spawn `@modelcontextprotocol/server-everything` via `npx` — no manual setup
beyond having Node/npm, `npx` installs it on first run.

## CI

`.github/workflows/ci.yml` runs on every push to `main` and every pull
request, on a Linux and a macOS runner: the same `fmt --check`/`clippy -D
warnings`/`cargo test --workspace` as above, then a `--release` build of
the desktop binary, uploaded as a downloadable workflow artifact
(`code-architect-linux`/`code-architect-macos`) from the run's Summary
page. It's build validation, not a public release — nothing gets published
automatically. A pull request from a fork won't have the four OAuth repo
secrets available (GitHub's standard security model), so those builds
still succeed with Login simply unconfigured, same as a contributor's
machine with no `.env`. See "Connecting GitHub/Slack/Linear" above for
which repo secrets to set for Login to actually work in a downloaded
artifact.
