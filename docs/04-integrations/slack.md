---
id: integration.slack
type: integration
title: Slack Integration
depends_on:
- domain.slack
relations:
  related_domains:
  - domain.slack
  related_flows:
  - flow.oauth-login
---

Slack integration: read-only access to workspace channels and threads via the Slack Web API (`reqwest`, base `https://slack.com/api`). One crate — `architect-slack` — fills the `Tool` extension point with **three** tools.

## Authentication

A saved `slack_token` in `IntegrationsConfig` (typed in or obtained via the **Login** OAuth flow). PKCE Slack apps can only request *user-token* scopes, so Login yields an `xoxp-` user token (not an `xoxb-` bot token), with `user_scope=channels:history,groups:history,im:history,mpim:history`. No token → the tools are not registered.

## Tools

| Tool | Endpoint | Notes |
|---|---|---|
| `slack_list_channels` | `GET /conversations.list` | Public and private channels, excluding archived; ID, name, privacy/membership — the lookup step before reading history |
| `slack_read_channel_history` | `GET /conversations.history` | Most recent messages, newest activity last; default limit 50, max 200; thread-starting messages carry their reply count and `thread_ts` |
| `slack_read_thread` | `GET /conversations.replies` | Parent message + every reply, in order; accepts either a pasted permalink or channel + `thread_ts` |

## Error handling

Slack returns 200 with an `ok: false` envelope on most failures; the client reports the `error` field (e.g. `not_authed`, `channel_not_found`) as the tool error instead of a raw status code.

## See also

- [Slack Domain](../slack/overview.md)
- [OAuth Login Flow](../../02-flows/oauth-login.md)