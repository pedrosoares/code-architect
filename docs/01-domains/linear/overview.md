---
id: domain.linear
type: domain
title: Linear Domain
relations:
  related_flows:
  - flow.oauth-login
  related_integrations:
  - integration.linear
---

The Linear domain is the read-only window into the tracker: the agent can pull a ticket's full context — description, state, priority, assignee, comments, and its blockers — in one call, so "read the ticket before you start" costs a single tool call. Access goes through the Linear GraphQL API, one crate (`architect-linear`), one tool — see the [Linear Integration](../../04-integrations/linear.md) for details.

## Scope

- **Ticket read**: `linear_read_ticket` fetches identifier, title, description, priority, state, assignee, up to 50 comments, and relations in a single GraphQL round-trip.
- **Blockers only**: of Linear's relation types, only `blocks` is surfaced (both directions: what the ticket blocks, and what blocks it — "blocked by"). `related`/`similar`/`duplicate` are fetched but filtered out.
- **No writes**: no create, no state change, no comment.

## Credential model

A saved `linear_api_key` (typed or via the Login OAuth flow, `scope=read`) as a Bearer token. No key → tool not registered.

## Design notes

- **One round-trip for the whole ticket** is the core tradeoff: comments and both relation connections are requested up front because the agent's typical need is "understand the full context", and the payload stays small (≤50 comments).
- The relation connection carries both the direct (`issue`) and inverse (`relatedIssue`) shape; the tool normalizes both so the output reads the same either way.

## See also

- [Linear Integration](../../04-integrations/linear.md)
- [OAuth Login Flow](../../02-flows/oauth-login.md)