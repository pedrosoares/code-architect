---
id: adr.browser-tools
type: adr
title: 'ADR: Browser Tools (headless Firefox via geckodriver)'
relations:
  related_integrations:
  - integration.firefox
  related_domains:
  - domain.firefox
---

# Browser Tools: the LLM drives a headless Firefox via geckodriver

## Status

Accepted

## Context

The agent already "sees" the world two ways: it can capture the *real* screen
([`screenshot`](../01-domains/tools/overview.md)) and it can run *real* commands
([`run_command`](../01-domains/tools/overview.md)). But neither reaches the web:
`screenshot` only shows whatever the user has on their display (nothing in a
headless/CI environment), and `run_command` has no browser. There was no way to
"open this URL, see what renders, read the console, and click around" — exactly
the loop a human does to verify a web UI change, debug a frontend bug, or inspect
a remote page.

A browser tool is inherently different from the other tools in two ways:

1. **State is long-lived and out-of-band.** The other tools are stateless per
   call, or their state (a running process) lives in a registry the tool itself
   manages (`ProcessRegistry`). A browser is a *second OS process* — `geckodriver`
   — plus a *third* it spawns (`firefox`) — whose state (the open session, the
   loaded page, the captured console) outlives any single tool call and is shared
   across calls and across turns. That state belongs to the **engine worker**,
   which is the only place that outlives a turn.
2. **It talks to a server over HTTP** (the WebDriver protocol), not to a local
   file or a shell. So the tool needs a small HTTP client, and the crate that
   owns it can't be async-runtime-free like `architect-core`.

The driver choice is settled by the environment: Firefox is already installed
and `geckodriver` is a single static binary (no WebDriver protocol library to
compile, no `selenium` dependency tree). The `screenshot` tool's
`ToolResultImage` mechanism already gives a clean path for a *page* screenshot to
reach the model as an image, exactly the way a screen capture does.

## Decision

**A new crate, `architect-firefox`, owns the browser.** It is the one place that
knows geckodriver exists. It exposes:

- **`Browser`** — `Send + Sync`, held as `Arc` in the engine worker. It owns the
  geckodriver child process, lazily starts it (first browser tool call, not
  app startup), creates a single WebDriver session, and talks to it over
  `127.0.0.1:<port>` using the existing `reqwest` dependency. `port: u16` is
  chosen deterministically from the PID (no port-scanning) so concurrent test
  runs don't collide.
- **`BrowserEvent`** — a small `Debug` enum (`DriverStarted { port }`,
  `SessionStarted`, `Navigated { url, title }`, `Closed`, `Failed { message }`)
  with a `kind()` string, mirroring `ProcessEvent` so the UI can render a browser
  panel the same way it renders the process panel.
- **Seven tools**, all `Send + Sync`, all holding an `Arc<Browser>` (so they can
  be registered on every turn like `process_tools`):
  | tool | what it does |
  |------|--------------|
  | `firefox_open` | start the session (first call) and navigate to a URL |
  | `firefox_logs` | read the page console (see "Logging" below) + page title/URL |
  | `firefox_click` | click the first element matching a CSS selector |
  | `firefox_fill` | set a value on the first element matching a CSS selector |
  | `firefox_eval` | run a JavaScript expression and return its result |
  | `firefox_screenshot` | capture the page as a PNG and return the image **inline** (so the chat renders it and the model sees it directly), *and* save it under `.coder/screenshots/` (the path is also returned, so `view_image` can re-open it) |
  | `firefox_close` | end the session and terminate geckodriver |

**The browser is owned by the engine, not by `ToolContext`.** `ToolContext` is
rebuilt fresh on every turn (and handed to sub-agents and compaction), so it
can't hold the long-lived session — the exact reason `ProcessRegistry` can't
live there either. Instead, `Browser` + the seven tools are built once at worker
startup (alongside `process_registry`) and passed through `SpawnTurn` as a
`browser_tools: Arc<Vec<Arc<dyn Tool>>>` field, registered into the Default tool
registry on every turn, and **deliberately excluded from sub-agent turns** (they
pass an empty `browser_tools`, the same as `process_tools`). Shutdown calls
`browser.close().await` right next to `process_registry.kill_all().await`.

**The session is a single, shared tab.** Not a tab pool: the LLM is a single
reasoner acting sequentially, and a pool adds lifecycle complexity (who owns
which tab, how to clean up an orphan) for no benefit. `firefox_open` reuses the
existing session if one is live and only starts geckodriver when none is — so an
idle browser stays up between turns (cheap: one process, no work) but is torn
down on `firefox_close` or app shutdown.

## Logging: why a JS hook, not the WebDriver log API

The W3C WebDriver spec has `GET /session/{id}/se/log` for reading browser
console logs. **Geckodriver does not implement it.** I verified this two ways
against the exact driver version in use (geckodriver 0.37.1 → the `webdriver`
0.54.0 crate it depends on): the crate's router has no log route, and the
endpoint returns `HTTP method not allowed`. And page `console.*` does **not**
leak into geckodriver's stderr (the `console.warn` lines that *do* appear there
are Firefox's own internal processes, not page output — confirmed by navigating
to a page that logs a unique marker and finding it nowhere).

So `firefox_logs` reads logs the only reliable, version-independent way:
**a small JavaScript console hook** (wrapping `console.log/info/warn/error/debug`
plus `window.onerror`/`unhandledrejection`) injected via `execute/sync` after
each navigation, writing into a bounded `window.__caConsole` ring buffer (capped
at 200 entries so a chatty page can't grow it unboundedly). `firefox_logs` then
reads that buffer back with a second `execute/sync`. This is robust across
geckodriver versions (it only relies on `execute/sync`, which is core W3C) and
captures exactly what a human sees in DevTools. The hook is re-injected after
every `firefox_open`/navigation, since a new page wipes `window`.

## Consequences

**Positive**
- The LLM gets a full browser loop — open, see (the screenshot is returned
  **inline**, so the model sees it directly with no extra `view_image`
  round-trip), read logs, click, fill, eval — with no new dependency
  (reuses `reqwest`, `base64`, `serde_json`, `async-trait`, all already in the
  workspace).
- The screenshot rides the same `ToolResult.image` path the `screenshot` and
  `view_image` tools already use, and is *also* saved to `.coder/screenshots/`
  as a real, re-viewable artifact the model or the human can re-open later.
- The image surfaces in the **transcript** for the human: the tool row renders
  it (the same `ToolRow.image` path a `screenshot`/`view_image` result uses),
  so the person watching the agent sees exactly what the model acted on — not
  only the model — including on session resume. A row that produced an image
  **auto-opens** when the image lands (the image arrives after the row is
  mounted, so it's driven by a one-shot effect, not the initial state), showing
  the full-size image immediately instead of hiding it behind a click; the
  user can still collapse it, and text-only rows stay collapsed by default.
- Lifecycle is owned in exactly one place (the engine worker), matching how
  `ProcessRegistry` is owned, so shutdown is one more line next to `kill_all`.
- Sub-agents and compaction never get the browser (empty `browser_tools`),
  preserving the "sub-agents are read-only investigation" invariant.
- A clean seam: `architect-firefox` has no knowledge of the agent, the engine,
  or the UI. The engine just holds an `Arc<Browser>` and registers its tools.

**Negative / accepted costs**
- The screenshot's inline base64 is a multi-MB entry in the persisted message
  history, re-sent to the provider on every subsequent iteration of the turn (and
  on resume replay). That is the accepted cost of the model and the human both
  seeing it directly — the exact cost the existing `screenshot` and `view_image`
  tools already pay — and it's why the PNG is *also* saved to a file (so
  `view_image` can re-open a small path instead of the inlined blob if the
  history ever needs trimming).
- Two extra processes (geckodriver + firefox) are live for the app's lifetime
  once the browser is first used; `firefox_close` and app shutdown are the only
  ways they go. An idle-but-open session is a deliberate trade (a tab you can
  keep working in) over a strict open-on-use/close-on-idle policy.
- Console logs are *page* logs only (the JS hook). Firefox's *internal* logs
  (the `glean`/`RSLoader` lines in geckodriver's stderr) are not surfaced —
  they're not something a debugging LLM needs, and geckodriver doesn't expose
  them through a stable API.
- `firefox_eval` can run arbitrary JS in the page context — same trust level as
  `run_command` running arbitrary bash. It is not sandboxed, and it must say so
  in its description.

**Enforcement**
- `Browser` is `Send + Sync` (a plain `reqwest::Client` + a `tokio::sync::Mutex`
  around session state), so it is trivially shareable as `Arc<Browser>` and
  `clippy`'s `arc_with_non_send_sync` will catch a regression.
- The engine's tests construct the engine headless (no Freya) and drive it
  through `Command`/`EngineEvent` — the browser tools are registered like any
  other `process_tools`-style tool, so they're covered by the same seam.

## Verification

This design was verified against the real stack in the build environment before
implementation: geckodriver 0.37.1 was installed and a headless Firefox session
was created; navigate, `get title`, element-find, click, `execute/sync`, and
`screenshot` (base64 PNG) all succeeded; and the console-hook logging path
(inject hook → click a button that logs → read the buffer back) was confirmed to
capture both `console.log` and `console.error`.

The shipped crate carries a live end-to-end test
(`tests/live.rs::drives_a_real_headless_firefox_end_to_end`, `#[ignore]d` because
it spawns real `geckodriver` + `firefox`) that drives all seven tools against a
real headless Firefox — including the `<select>` fill and IIFE/multi-statement
eval paths that were the original bugs.