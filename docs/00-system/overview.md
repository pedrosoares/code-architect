---
id: system.overview
type: system
title: System Overview
depends_on:
- system.architecture
- system.conventions
- system.glossary
---

Code Architect is a **modular code harness** — a coding-agent workstation — written in Rust with a [Freya](https://freyaui.dev) desktop UI. The binary is `code-architect` (package `desktop`, `apps/desktop`).

## What it does

You type a message and it streams back from a real model — with reasoning shown separately and a Stop button that cancels a turn in flight — while the model can actually act on your workspace:

- **Read and change files**: `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `run_command`.
- **See things**: attach images to your own messages; the model can also *request to see* via `view_image` (an image file on disk) and `screenshot` (captures the screen) — the same vision path a human's attached image uses, triggered by a tool call.
- **Run long processes**: `start_process` / `get_process_logs` / `stop_process` manage dev servers and watchers that `run_command` (which blocks) can't.
- **Plan**: `write_plan` / `read_plan` let the agent lay out and track a multi-step plan, shown live in the Inspector.
- **Fan out**: `spawn_subagents` runs focused sub-agent sessions (e.g. one per crate) in parallel, each a real first-class child session.
- **Integrate**: read-only tools for GitHub, Slack, and Linear (when a credential is saved), plus tools from any enabled MCP server (stdio or streamable HTTP).
- **Document**: an Obsidian-style markdown knowledge base in `<workspace>/docs` with its own six tools (`write_doc`, `edit_doc`, `read_doc`, `search_docs`, `list_docs`, `scaffold_docs`).

Every message and file change is durably saved to `.coder/sessions.db` in the workspace as the conversation happens. Sessions run **concurrently** — start a turn in one, switch to another, use it, switch back mid-stream; a dot marks any session still working in the background.

## What the UI shows

Three resizable panels plus a header and status bar:

- **Sessions** (left) — every saved session, sub-agent sessions nested under their parent, busy indicators, Delete per row.
- **Transcript + Composer** (center) — streaming rows (user, assistant with reasoning, tool calls, errors, compaction markers), selectable text, image attachments, Enter/Shift+Enter, a Stop button while busy.
- **Inspector** (right) — Files (with Diff and per-file Roll Back), Tools, Processes (live logs), Plan.
- **Header** — model/provider chips, vision badge, status, Compact, Settings (API profiles, LM Studio ad-hoc models, MCP servers, Integrations, Documentation).
- **Status bar** — workspace path, token totals, context-window fill, spend.

## Status and scope

The harness is **built and wired**: streaming, tools, persistence, sessions, config, MCP, integrations, plans, processes, sub-agents, and the doc vault all work end to end. Deliberately not built yet: LSP tools, `@`-file-mention handling in the composer (the placeholder text is aspirational), video attachments, Notion/Jira doc drivers, and any write-side integration tools (all GitHub/Slack/Linear tools are read-only).

## Where things live

```
apps/
  desktop/            the Freya binary → `code-architect`
crates/
  architect-core/     shared types: messages, tool calls, usage, pricing, file changes, plans
  architect-llm/      provider trait + OpenAI and Anthropic adapters
  architect-agent/    the tool-call loop
  architect-tools/    built-in tools (files, search, shell, processes, plan, sub-agents)
  architect-mcp/      MCP client — stdio or streamable HTTP, adapts its tools
  architect-github/   read-only GitHub tools
  architect-slack/    read-only Slack tools
  architect-linear/   read-only Linear tools
  architect-docs/     documentation knowledge base + doc tools
  architect-session/  .coder/sessions.db — messages, file changes, reverse-to-point
  architect-config/   ~/.config/code-architect/profiles.json — profiles, MCP, integrations, docs
  architect-ui/       design tokens + reusable widgets (depends on freya only)
```

See [Architecture](#) for the dependency rules and [Conventions](#) for how the pieces fit together.