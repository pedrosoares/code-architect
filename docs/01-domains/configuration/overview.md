---
id: domain.configuration
type: domain
title: Configuration Domain
depends_on:
- domain.llm-providers
relations:
  related_flows:
  - flow.model-config
  - flow.oauth-login
---

The configuration domain (`architect-config`) stores machine-global app configuration — API provider profiles, MCP server connections, integration credentials, and documentation settings — in a single JSON file.

## Why a separate store

One `ConfigStore` (`~/.config/code-architect/profiles.json`), **not per-workspace** like `SessionStore`: an API key or an MCP server is set up once and reused across every project, not scoped to a single `.coder/` directory. The crate deliberately holds no `architect-llm` or `rmcp` dependency — a `Profile`'s fields mirror `ProviderConfig`'s shape, `McpServerConfig` is plain data mirroring `connect`'s parameters, and the desktop engine is the one place that bridges between the pairs.

## The types

- **`Profile`** — `{ id: Uuid, name (sidebar display, not sent to the provider), kind ("openai"|"anthropic"), base_url: Option, api_key: Option, model: String }`. `Profile::new(name, kind, model)` plus `base_url(..)`/`api_key(..)` builders. Field names deliberately mirror `ProviderConfig` so the engine's translation is a straight field copy.
- **`McpServerConfig`** — `{ id, name, enabled: bool, transport: McpTransport }`. Multiple servers can be `enabled` at once (each contributes its own tools; they don't compete for "the" active one, unlike profiles). Constructors `stdio(name, command)` / `http(name, url)` (both `enabled: true`); builders `args(..)` (no-op unless Stdio) and `bearer_token(..)` (no-op unless Http). There is no builder for the `env` map.
- **`McpTransport`** — internally tagged: `Stdio { command, args: Vec, env: HashMap }` or `Http { url, bearer_token: Option }`.
- **`IntegrationsConfig`** — a singleton (no id, no enabled flag): `{ github_token: Option, github_use_gh_cli: bool, slack_token: Option, linear_api_key: Option }`. `github_use_gh_cli` means GitHub tools resolve a token by running `gh auth token` instead of using `github_token`. An absent credential simply means that integration's tools aren't registered.
- **`DocsConfig`** — a singleton: `{ enabled: bool (default TRUE), driver: String (default "obsidian"), vault_path: Option (None → <workspace_root>/docs) }`. Obsidian needs no credential; `enabled` alone decides whether the doc tools are registered.

## Storage

One file: `~/.config/code-architect/profiles.json` (or `$XDG_CONFIG_HOME/code-architect/profiles.json`). A single `Document { active: Option<Uuid>, profiles, mcp_servers, integrations, docs }` serialized pretty — profiles, MCP servers, integrations, docs, and the active pointer all live in that one file (the name is legacy of an earlier profiles-only shape). The file is created lazily on first write; missing on read → empty defaults. **Corrupt JSON is a hard error** on every subsequent read/write (`ConfigError::Serialize`), not a silent reset. An `Arc<Mutex<()>>` serializes read-modify-write per store instance.

## The store API (all async)

`open_default` / `open(path)` (the test seam) · `list` / `upsert` / `remove` / `set_active` (must exist → `UnknownProfile`) / `clear_active` (profile untouched — the settings panel's "Default" row) / `active` · `list_mcp_servers` / `upsert_mcp_server` / `remove_mcp_server` · `integrations` / `set_integrations` (whole-object replace) · `docs` / `set_docs` (whole-object replace). `remove` clears `active` if it removed the active profile.

`ConfigError`: `Io { path, source }` | `Serialize(serde_json::Error)` | `NoConfigDir` | `TaskPanicked(String)` | `UnknownProfile(Uuid)`.

## How the UI consumes it

The settings panel talks to the store only through `Command::{ListProfiles, SaveProfile, DeleteProfile, ActivateProfile, DeactivateProfile, ListMcpServers, SaveMcpServer, DeleteMcpServer, ListIntegrations, SaveIntegrations, ListDocsConfig, SaveDocsConfig}` and the matching `*Listed` events — never directly, the same rule every panel follows.

- **Activating a profile** rebuilds the running `Provider` in place inside the worker and keeps the conversation in progress — only which API answers the *next* turn changes. A bad activation keeps the previously-working provider ("switching to a broken configuration must not take down one that already worked").
- If the very first provider build (from env/default values) fails, it is not fatal: `provider` stays `None`, `Send` reports "no working API configuration — open Settings to add or fix one", and the settings panel stays fully usable.
- MCP servers and integrations/doc config trigger a full **rebuild of external tools** on save/delete (see the desktop app domain).

## Credentials at rest

All tokens — `api_key`, MCP `bearer_token`, GitHub/Slack/Linear — are stored **plaintext JSON**; nothing in this codebase encrypts them at rest (documented tradeoff, matching V1's precedent). OAuth client secrets are a different matter: those are compiled into the binary at build time (see the desktop app domain and ADR).