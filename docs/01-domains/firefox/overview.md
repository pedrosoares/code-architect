---
id: domain.firefox
type: domain
title: Browser Tools Domain (headless Firefox)
depends_on:
- domain.tools
- domain.desktop-app
relations:
  related_adrs:
  - adr.browser-tools
  related_flows:
  - flow.send-message
  related_integrations:
  - integration.firefox
---

The Browser Tools domain is the agent's window onto the **web**: the LLM can
open a URL, see what actually renders (screenshot), read the page's console logs,
and drive the page — fill a form, click a button, run JavaScript — the same loop
a human uses to verify a UI change, reproduce a frontend bug, or inspect a remote
page. It fills the `Tool` extension point with **seven** `firefox_*` tools, one
crate (`architect-firefox`) — see the [Browser Tools Integration](../../04-integrations/firefox.md)
for the geckodriver details.

This is the web-facing counterpart to two existing senses: [`screenshot`](../tools/overview.md)
only shows the *display* (nothing headless/CI), and [`run_command`](../tools/overview.md)
has no browser. Together they left a gap — "open this page, see what renders,
read the console, click around" — that this domain closes.

## Scope

- **Open & inspect**: `firefox_open` (start the session on first call, navigate to a URL), `firefox_screenshot` (PNG returned inline so the chat renders it, and saved to `.coder/screenshots/` for `view_image` to re-open).
- **Read**: `firefox_logs` (the page console + current title/URL), `firefox_eval` (run JS in the page, return the JSON value).
- **Interact**: `firefox_click` (first element matching a CSS selector), `firefox_fill` (set a value; `<select>` handled specially).
- **Tear down**: `firefox_close` (end the session, terminate geckodriver).

## Credential model

None — no API key or OAuth. The only precondition is that `geckodriver` and
`firefox` are installed on the machine (or `FIREFOX_GECKODRIVER` points at the
driver binary). There is nothing to configure in Settings and no Login flow:
unlike GitHub/Slack/Linear, the browser is always registered, and lazily
started on the first tool call rather than gated on a saved token.

## Design notes

- **Long-lived, engine-owned state.** A browser session outlives any single
  tool call and is shared across turns, so it is held by the engine worker (an
  `Arc<Browser>`) and passed to every turn as `browser_tools` — exactly the
  `ProcessRegistry` pattern, because `ToolContext` is rebuilt fresh each turn.
  It is **excluded from sub-agent turns** (empty `browser_tools`), preserving the
  "sub-agents are read-only investigation" invariant.
- **A single shared tab**, not a tab pool: one reasoner acts sequentially; a pool
  only adds ownership/cleanup complexity. An idle session stays up between turns
  (cheap) and is torn down by `firefox_close` or app shutdown.
- **The console is read with a JS hook, not the WebDriver log API** — geckodriver
  does not implement `GET /session/{id}/se/log`, so logs are captured by a small
  injected hook into a bounded `window.__caConsole` ring buffer. See the
  [ADR](../../06-decisions/0004-browser-tools.md).

## See also

- [Browser Tools Integration](../../04-integrations/firefox.md)
- [ADR: Browser Tools](../../06-decisions/0004-browser-tools.md)
- [Tools Domain](../tools/overview.md) — the `Tool` extension point these tools fill