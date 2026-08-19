---
id: adr.no-markdown-rendering
type: adr
title: 'ADR: No Markdown Rendering'
---

# No Markdown Rendering in the UI

## Status

Accepted

## Context

The assistant's output is markdown. The obvious thing is to render it — headers, code blocks with syntax highlighting, tables, links. The tempting path was a markdown→egui converter or an embedded web view.

The problem is **copy-paste**. The single most common thing a user does with agent output is copy a code snippet, a file path, or a config value out of it. A rendered view has to reconstruct clean, un-styled text on copy (or the user gets markdown syntax or missing whitespace). An embedded web view makes that even harder and drags in a browser engine. Every "improvement" to the rendering was a regression for the copy path, which is the path people actually use.

## Decision

Assistant text (and reasoning) is shown **plain**, as `SelectableText`, with no markdown parsing:

- Text cards render the raw model output; the user selects and copies it exactly as written.
- Reasoning is the same, collapsed into a `Disclosure`.
- The Diff tab is the one place with real formatting — it uses Freya's `code_editor` for syntax-highlighted diffs (rs/py/js/ts/json/bash) and diff-colored plain text otherwise — because a diff's whole value is its structure, and it's a deliberate, bounded exception.
- Tool outputs expand to plain text in the transcript.

## Consequences

**Positive**
- Copy is always exact and trivial. No "why did my pasted code lose its indentation" class of bug.
- No markdown dependency in the UI, no web view, no rendering-escape issues.
- The transcript stays a pure renderer of `Row`s; there's no per-block layout state to manage.

**Negative / accepted costs**
- Output looks flat compared to a rendered chat app; long code blocks have no highlighting in the transcript.
- Tables and headers don't get visual structure.

**Enforcement / notes**
- Captured as a rule, [No Markdown Rendering](../../03-business-rules/no-markdown-rendering.md).
- The `code_editor` dependency exists **only** for the Diff tab; do not reach for it in the transcript.