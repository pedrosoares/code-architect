---
id: rule.read-only-integrations
type: rule
title: Read-Only Integration Rule
---

**The GitHub, Slack, and Linear tools are read-only. That is a rule, not an implementation accident.**

## The rule

- **No tool in `architect-github`, `architect-slack`, or `architect-linear` writes, comments, merges, or posts anything.** Each is a thin `reqwest`-based HTTP client plus one or two `Tool` impls with compile-time-known names/descriptions/schemas.
- The OAuth scopes request exactly the read ceiling and nothing more: GitHub `scope=repo` (minimum for private repos — a no-scope token gets `404` rather than `403`, so this is the floor that makes private-repo reads work, not a privilege escalation), Slack the four `*:history` user-token scopes, Linear `scope=read`.
- The `repo` scope is justified purely by the API's 404-not-403 leaking behavior; the app never calls write endpoints.

## Why the crates are shaped this way

- One crate per external service, **no shared base crate** — three tools don't justify factoring out common HTTP-client scaffolding.
- None of the three depends on `architect-config` or `apps/desktop` — matching `architect-mcp`'s separation; `engine.rs` is the one place that knows a saved credential exists at all.
- Building a service's tools from its saved credential is **synchronous and infallible** for Slack and Linear (`integration_tools`); whatever goes wrong (a bad token, a 404) surfaces from the tool's own `call`, same as a built-in tool's errors.
- **GitHub is the one exception** — `github_use_gh_cli` makes `integration_tools` `async` and genuinely fallible (runs `gh auth token`; failure → global `Failed` rather than silently registering no tools). Slack/Linear are untouched by that.
- Unlike MCP, **none of the three needs a live connection** kept alive or reconnected — no long-lived state to rebuild.

## Where they sit

- Gated by `IntegrationsConfig` (`github_token` / `slack_token` / `linear_api_key`, one of each, machine-global, no enabled flag).
- Registered into `external_tools` by `rebuild_external_tools`, which also folds in MCP tools and doc tools — the one tool set `spawn_turn` registers per turn alongside the built-ins.
- Rebuilt at startup and after `SaveMcpServer`/`DeleteMcpServer`/`SaveIntegrations`/`SaveDocsConfig`/successful-OAuth.
- **Sub-agent turns do not get integration tools** — their registry is the read-only investigation set plus only the three read-only doc tools.