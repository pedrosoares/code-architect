---
id: domain.slack
type: domain
title: Slack Domain
relations:
  related_flows:
  - flow.oauth-login
  related_integrations:
  - integration.slack
---

The Slack domain is the read-only window into the team's channels: the agent can discover channels, read recent message history, and open full threads — the research path for "what did we decide in #channel-x". All access goes through the Slack Web API, one crate (`architect-slack`), three tools — see the [Slack Integration](../../04-integrations/slack.md) for endpoint details.

## Scope

- **Channel discovery**: `slack_list_channels` (public + private, archived excluded) — the ID/name lookup step that precedes any read.
- **History**: `slack_read_channel_history` — most recent messages, newest activity last, default 50 / max 200.
- **Threads**: `slack_read_thread` — parent + every reply in order, by permalink or channel + `thread_ts`. Thread-starting messages in history carry their reply count and the `thread_ts` to pass back.
- **No posting, no editing, no reactions** — read-only by design.

## Credential model

A saved `slack_token` (typed or via the Login OAuth flow). PKCE Slack apps can only request user-token scopes, so the token is `xoxp-` with the four `*:history` user scopes; a bot token is not what Login produces. No token → tools not registered.

## Design notes

- **Two-step reads are intentional**: history returns just enough (the `thread_ts`) to follow a thread on demand, keeping the default read cheap.
- **Slack's error envelope** (200 + `ok:false`) is unwrapped so the model sees `not_authed` / `channel_not_found` rather than a raw HTTP status.

## See also

- [Slack Integration](../../04-integrations/slack.md)
- [OAuth Login Flow](../../02-flows/oauth-login.md)