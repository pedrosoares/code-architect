---
id: index.dependency-map
type: index
title: Dependency Map
---

A human-written map of cross-domain dependencies, for orientation. Each doc's own `depends_on` (visible via `list_docs`) is the current, generated version of this — prefer that when precision matters.

## The dependency rule, restated

Everything points at `architect-core` (and the agent/llm chain) — **nothing below imports something above it**, and the integration/tool/session/config crates are mutually independent. See [Architecture](../00-system/architecture.md).

## Domain → domain

```
  desktop-app ──► agent-loop, tools, conversation, file-changes, configuration
  agent-loop ───► conversation, llm-providers, tools
  conversation ─► llm-providers
  tools ────────► conversation
  file-changes ─► tools, rule.persistence
  plans ────────► tools, conversation
  configuration ► llm-providers
  docs-vault ───► tools
  github/slack/linear/mcp ──► (each an independent Tool provider)
  firefox ──────► tools, desktop-app (engine owns the Browser)
  ui-presentation ► system (no business domains)
```

## Doc → doc (the `depends_on` edges worth knowing)

- [Send a Message](../02-flows/send-message.md) depends on the desktop-app, agent-loop, tools, and conversation domains — it is the flow that stitches them together.
- [Roll Back File Changes](../02-flows/rollback.md) depends on [Session Lifecycle](../02-flows/session-lifecycle.md) and the file-changes domain.
- [Model & Provider Configuration](../02-flows/model-config.md) depends on the desktop-app, configuration, and llm-providers domains.
- [OAuth Login](../02-flows/oauth-login.md) depends on the desktop-app and configuration domains.
- Each integration doc (`integration.github`/`slack`/`linear`/`mcp`) depends on its matching domain doc.
- The system docs: [Overview](../00-system/overview.md) depends on [Architecture](../00-system/architecture.md), [Conventions](../00-system/conventions.md), and [Glossary](../00-system/glossary.md).

## Rules that bind across domains

- [UI ↔ Engine Seam](../03-business-rules/ui-core-seam.md) binds the desktop-app and ui-presentation domains.
- [Persistence Rules](../03-business-rules/persistence.md) binds conversation, file-changes, and plans.
- [Sandboxing & Shell Safety](../03-business-rules/sandboxing.md) and [Read-Only Integration Rule](../03-business-rules/read-only-integrations.md) bind tools and the integration domains.