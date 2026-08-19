---
id: flow.oauth-login
type: flow
title: OAuth Login (GitHub / Slack / Linear)
depends_on:
- domain.desktop-app
- domain.configuration
relations:
  related_integrations:
  - integration.github
  - integration.slack
  - integration.linear
---

**Goal:** the user clicks "Login" next to the GitHub/Slack/Linear token field and gets a working credential saved — without pasting a token by hand.

## Prerequisite (one-time, per provider)

An OAuth App registered on each provider's site, with the callback `http://127.0.0.1:53682/callback`. The resulting client id (and, **GitHub only**, a client secret) are set as `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET`, `SLACK_CLIENT_ID`, `LINEAR_CLIENT_ID` and **baked into the binary at compile time** by `apps/desktop/build.rs` (`cargo:rustc-env`; `.env` locally via `dotenvy`, repo secrets in CI — GitHub Actions reserves the `GITHUB_*` prefix, so CI stores the two GitHub values as `OAUTH_GITHUB_*` and the workflow remaps). Until configured, Login fails fast with a message pointing at the README; the paste-a-token field keeps working regardless.

## Steps

1. **User clicks Login** → the button shows "Waiting for browser…"; `engine.start_oauth_login(provider)` → `Command::StartOAuthLogin(OAuthProvider)` → the worker spawns `oauth::run_login(provider)` on its own task (the UI is never blocked; `oauth_rx` carries the result back).
2. **`run_login` — RFC 8252 authorization code + PKCE**:
   - **PKCE**: verifier = 3×UUID v4 raw bytes → 48 bytes → 64 base64url chars (within RFC 7636's 43–128; uses `uuid`'s RNG to avoid a `rand` dependency); challenge = `SHA-256(verifier)` → base64url-no-pad (`S256`). `state` = a fresh UUID (CSRF guard).
   - **Browser**: `webbrowser::open(authorize_url)` with per-provider scopes —
     - **GitHub**: `scope=repo` (minimum that makes private-repo tools work — GitHub has no read-only scope for private repos; a no-scope token gets `404`, not `403`, on anything it can't see). GitHub's classic OAuth apps require a client secret in the exchange *even with PKCE*.
     - **Slack**: `user_scope=channels:history,groups:history,im:history,mpim:history` — PKCE-enabled Slack apps can **only** request user-token scopes, so Login yields an `xoxp-` user token, not an `xoxb-` bot token (`architect-slack` works the same either way).
     - **Linear**: `scope=read` (+ `grant_type=authorization_code` in the exchange).
   - **Callback**: bind `127.0.0.1:53682` (fixed `REDIRECT_PORT`, registered with all three providers), 300 s timeout, read **one raw HTTP request** (no HTTP framework), answer "You can close this tab…", parse `code`/`state`/`error` from the request line.
   - **Verify `state`** — mismatch → "possible CSRF" error.
   - **`exchange_code`** — reqwest POST form with `code_verifier` (GitHub adds `client_secret`; Linear adds `grant_type`).
   - **Token extraction** — GitHub/Linear: top-level `access_token`; Slack: **nested `authed_user.access_token`** (explicitly *not* the bot `xoxb-` token present in the same response).
3. **`oauth_rx` arm → `apply_oauth_result`** — a read-modify-write into `IntegrationsConfig` (`github_token` / `slack_token` / `linear_api_key`) via `ConfigStore::set_integrations` — machine-global, **plaintext** (the same tradeoff `Profile.api_key` already makes). Failure → global `Failed "{provider} login failed: {message}"`.
4. **On success** — no dedicated success event: the worker rebuilds `external_tools` (the new credential's tools join the next turn's registry) and re-sends `IntegrationsListed` — the refreshed list (button now "✓ Connected") is the confirmation.
5. **UI** — a `use_side_effect` in the settings panel reactively refreshes the corresponding field when `integrations` changes during a pending login.

## Rules and notes

- **The token field needs no setup at all** — pasting a token (`SaveIntegrations`) and OAuth land in the *same* `IntegrationsConfig` field; the first two credential paths are indistinguishable to `rebuild_external_tools`.
- **GitHub's third option** — the gh-CLI toggle: when `github_use_gh_cli` is on, `resolve_github_token` runs `gh auth token` (subprocess) each time tools are rebuilt instead of using a saved token; the token field and Login button disappear while it's on, and `to_config` forces `github_token: None` (a stale pasted token should never linger). `gh` missing/unauthenticated → GitHub's tools simply don't register and a global `Failed` explains why.
- **All integration tools are read-only** — the `repo`/`read`/history scopes are the ceiling; no tool writes, comments, merges, or posts.
- OAuth's network round-trip happens on `oauth_rx`, earlier and separately from the tool-building path — which is why Slack/Linear's tool construction stays "synchronous and infallible".
- 12 unit tests cover the PKCE vectors (including RFC 7636 Appendix B), verifier charset, per-provider URL contents, `user_scope`-only for Slack, `repo` scope for GitHub, callback parsing (including a real loopback `TcpStream` connection), and token extraction (including Slack's nested `authed_user`).