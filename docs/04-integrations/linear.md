---
id: integration.linear
type: integration
title: Linear Integration
depends_on:
- domain.linear
relations:
  related_domains:
  - domain.linear
  related_flows:
  - flow.oauth-login
---

Linear integration: read-only access to tickets via the Linear GraphQL API (`reqwest`, single endpoint `https://api.linear.app/graphql`). One crate — `architect-linear` — fills the `Tool` extension point with **one** tool.

## Authentication

A saved `linear_api_key` in `IntegrationsConfig` (typed in or via the **Login** OAuth flow, `scope=read`), sent as the `Authorization: Bearer` header. No key → the tool is not registered.

## Tool

`linear_read_ticket` — one GraphQL query fetches the ticket's identifier, title, description, priority, state, assignee, up to 50 comments, and both `relations` and `inverseRelations` in a **single round-trip**.

- `id` accepts either the human identifier (`ENG-123`) or Linear's internal id.
- **Only `blocks` relations are reported.** Linear's relation connections carry every relation type (`related`, `similar`, `duplicate` too); the tool filters to `type == "blocks"`, reads the other issue from `issue` (inverse / "blocked by") or `relatedIssue` (direct / "blocks") depending on which connection it is.
- A ticket with no blockers omits both relation sections; empty comments render as "No comments.".

## Error handling

A null `issue` in the response becomes "no Linear ticket found for …" rather than a crash on missing fields.

## See also

- [Linear Domain](../linear/overview.md)
- [OAuth Login Flow](../../02-flows/oauth-login.md)