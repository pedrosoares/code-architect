---
id: index.home
type: index
title: Documentation Home
---

The front door to the Code Architect documentation. Start here, then follow the link that matches what you're doing.

## What this is

[Code Architect](../00-system/overview.md) is a modular code harness — a Rust + Freya desktop workstation where a real model streams back answers and acts on your workspace through a set of tools, with everything durably saved per-workspace.

## Where to go

**I'm new here**
- [System Overview](../00-system/overview.md) — what it does, what the UI shows, what's deliberately not built.
- [Architecture](../00-system/architecture.md) — the one dependency rule, the runtime topology, the turn pipeline, storage layout, extension points.
- [Glossary](../00-system/glossary.md) — the vocabulary, defined.
- [Conventions](../00-system/conventions.md) — how the pieces fit together and the house style.

**I'm about to change something**
- [System Invariants](../05-data/invariants.md) — the rules that must always stay true (concurrency, persistence, sandboxing, credentials).
- [Business Rules](../03-business-rules/) — the named rules: [UI ↔ Engine Seam](../03-business-rules/ui-core-seam.md), [Persistence](../03-business-rules/persistence.md), [Sandboxing & Shell Safety](../03-business-rules/sandboxing.md), [Read-Only Integrations](../03-business-rules/read-only-integrations.md), [Cancellation & Single-Flight](../03-business-rules/cancellation.md), [Usage & Cost](../03-business-rules/usage-and-cost.md).
- [Architecture ADRs](../06-decisions/) — [0001 UI Core Seam](../06-decisions/0001-ui-core-seam.md), [0002 No Markdown Rendering](../06-decisions/0002-no-markdown-rendering.md), [0003 Plaintext Credential Storage](../06-decisions/0003-plaintext-credential-storage.md), [0004 Browser Tools](../06-decisions/0004-browser-tools.md).

**I want to understand a behavior**
- [Flow Map](../99-index/flow-map.md) — the six flows: [Send a Message](../02-flows/send-message.md), [Session Lifecycle](../02-flows/session-lifecycle.md), [Model & Provider Config](../02-flows/model-config.md), [Sub-agents](../02-flows/subagents.md), [Roll Back File Changes](../02-flows/rollback.md), [OAuth Login](../02-flows/oauth-login.md).

**I want to understand a piece**
- [Domain Map](../99-index/domain-map.md) — the domains, grouped: core agent (agent-loop, conversation, llm-providers, tools, file-changes, plans), desktop app (desktop-app + panels + engine-events, ui-presentation), integrations (github, slack, linear, mcp, docs-vault, firefox).
- [Entities](../05-data/entities.md) — the persisted and runtime types, and where each is documented.
- [Entity Relationships](../05-data/relationships.md) — how those types hang together.
- [Dependency Map](dependency-map.md) — the cross-domain edges.

## The shape of the vault

```
00-system/      overview · architecture · conventions · glossary
01-domains/     one folder per domain (the "what")
02-flows/       one doc per behavior end-to-end (the "how")
03-business-rules/  the named invariants (the "must always hold")
04-integrations/    external-service specifics
05-data/        entities · relationships · invariants
06-decisions/   ADRs — why we built it this way
99-index/       this home + the domain/flow/dependency maps
```

Every doc carries an `id`, a `type`, `depends_on`, and `relations` in its frontmatter — that graph is the real structure. `list_docs` and `search_docs` are the generated, always-current view of it; the maps here are human-written orientation on top.

## How to keep it current

When a change alters a behavior, rule, entity, or flow, update the matching doc as part of the change — not after it. The doc's `id`/`path`/`type` are its stable identity; the body is the living part. If a domain grows past what one overview can hold, split it into `entities.md`, `rules.md`, `integrations.md` under that domain's folder and re-link from the overview.