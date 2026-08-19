---
id: integration.firefox
type: integration
title: Browser Tools Integration (headless Firefox via geckodriver)
depends_on:
- domain.firefox
relations:
  related_flows:
  - flow.send-message
  related_adrs:
  - adr.browser-tools
  related_domains:
  - domain.firefox
---

Browser Tools integration: the LLM drives a **headless Firefox** through
`geckodriver` (the WebDriver protocol over `127.0.0.1:<port>`, spoken with the
existing `reqwest` client). One crate — `architect-firefox` — fills the `Tool`
extension point with **seven** tools. See the [Browser Tools Domain](../firefox/overview.md)
for the "why" and the [ADR](../../06-decisions/0004-browser-tools.md) for the
decision record.

## Transport

- `geckodriver` is spawned as a child process (lazily, on the first browser tool
  call — not at app startup) and talks HTTP at `http://127.0.0.1:<port>`.
- The `port` is deterministic from the PID (`14_949 + pid % 2000`), so concurrent
  test runs/app instances don't collide and nothing is port-scanned.
- One WebDriver **session** (one tab) is created on demand and re-used; `Browser`
  is `Send + Sync` (a `reqwest::Client` + a `tokio::sync::Mutex` around the
  driver/session handles) so it is trivially shared as `Arc<Browser>`.
- Requires `geckodriver` and `firefox` on the machine (or `FIREFOX_GECKODRIVER`
  set). No credential, no Settings entry, no OAuth — always registered.

## Tools

| Tool | WebDriver / mechanism | Notes |
|---|---|---|
| `firefox_open` | `POST /session` (if needed) + `POST /url` | Starts the session on first call; re-uses a live one. Re-injects the console hook. Returns title + URL. |
| `firefox_logs` | read the injected JS console ring buffer | `clear` drains it. Page console only (see below), not geckodriver's internal logs. |
| `firefox_click` | `POST /element/{id}/click` | First element matching the CSS selector. |
| `firefox_fill` | `POST /element/{id}/clear` + `/value` — **or** in-page for `<select>` | Text inputs: clear-then-type (fires the page's input events). `<select>`: matched by option `value` then label, `change` event fired — the W3C select-option endpoint is broken on geckodriver 0.37. |
| `firefox_eval` | `POST /session/{id}/execute/sync` | Bare expressions/IIFEs are wrapped in `return (…);`; multi-statement or keyword-led scripts are left to their own `return` (geckodriver runs the script as a function body). Args arrive as `arguments[0..]`. |
| `firefox_screenshot` | `GET /screenshot` (base64 PNG) | Returned **inline** (the chat renders it, the model sees it directly — same as `screenshot`/`view_image`) **and** saved under `.coder/screenshots/firefox-N.png`; the path is in the text so `view_image` can re-open it. |
| `firefox_close` | `DELETE /session` + kill geckodriver | Idempotent — a no-op if nothing was started. |

## Logging (the non-obvious part)

Geckodriver **does not implement** the W3C log endpoint (`GET /session/{id}/se/log`
returns *method not allowed*) and page `console.*` does not leak to its stderr.
So `firefox_logs` reads a **JavaScript console hook** — wrapping
`console.log/info/warn/error/debug` plus `window.onerror`/`unhandledrejection` —
injected via `execute/sync` after every navigation into a bounded
`window.__caConsole` ring buffer (capped at 200 entries). It is re-injected after
each `firefox_open`, since a new page wipes `window`. This is version-independent
(it only relies on `execute/sync`) and captures exactly what a human sees in
DevTools.

## Dispatch wiring (how the LLM reaches these)

- The engine worker builds `Browser` + `architect_firefox::tools(browser)` **once
  at startup** (alongside `process_registry`) and passes them to every turn as
  `browser_tools: Arc<Vec<Arc<dyn Tool>>>`.
- Each turn's `ToolRegistry` **chains them in** (like `process_tools`/`plan_tools`),
  so `ToolRegistry::execute` routes a model `ToolCall` named `firefox_*` to the
  right tool by name — the same extension point as the ten built-ins and the
  GitHub/Slack/Linear tools.
- A `BROWSER_PROTOCOL` block is appended to the system prompt **only when** the
  turn's tools include `firefox_open` — the model is told the browser exists and
  how to use it, but the protocol never leaks into turns that don't have the
  tools.
- **Sub-agent turns pass an empty `browser_tools`** (empty `process_tools` too) —
  sub-agents are read-only investigation and never get the browser.
- Lifecycle events (`BrowserEvent`: `DriverStarted`/`SessionStarted`/`Navigated`/
  `Closed`/`Failed`) flow out on the engine's event channel as
  `EngineEvent::Browser`, which the UI folds into a single global **Browser
  summary** (the Inspector's Browser tab). Shutdown calls `browser.close().await`
  next to `process_registry.kill_all().await`.

## See also

- [Browser Tools Domain](../firefox/overview.md)
- [ADR: Browser Tools](../../06-decisions/0004-browser-tools.md)
- [Send a Message (the Turn Pipeline)](../../02-flows/send-message.md)