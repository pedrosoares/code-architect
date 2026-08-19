---
id: adr.ui-core-seam
type: adr
title: 'ADR: UI Core Seam'
---

# UI Core Seam: `Command` / `EngineEvent` as the only UI↔engine bridge

## Status

Accepted

## Context

The desktop app is a Freya (egui) UI and a long-running async worker (the engine). Early on it was tempting for the UI to hold `Arc`s to the engine's internals — the provider registry, the MCP connection list, the SQLite pool — and call methods on them directly. That creates a two-way coupling: the UI must know the worker's types and lifetimes, the worker must keep them alive for the UI, and any refactor ripples across the process boundary. It also makes the UI hard to test (you'd need a live worker) and makes shutdown order fragile (the UI and worker both holding each other's state).

## Decision

The UI and the engine communicate **only** through a pair of unbounded channels:

- **`Command`** (UI → worker): 21 variants covering every user action — send/cancel/rollback/compact, profile/MCP/integration/docs config, OAuth login, ad-hoc model activation, session load/delete.
- **`EngineEvent`** (worker → UI): 18 variants — the agent's live stream (`Agent`), plus listings, file-change records, process events, plan updates, compaction, and the `Failed`/`Cancelled` outcomes.

Everything else is an implementation detail on one side or the other. The worker is a single `select!` loop over its inputs; the UI is a **pure reducer** — `Transcript::apply(&EngineEvent)` folds events into `Transcript`/`Conversation` state, and the panels render that state. No panel holds an engine handle or calls into the worker.

## Consequences

**Positive**
- The UI is testable without a worker: feed it a sequence of `EngineEvent`s and assert on the `Transcript`.
- The worker is testable without a UI: send `Command`s, collect `EngineEvent`s (the engine's integration tests do exactly this).
- Shutdown is trivial and one-directional: the UI owns the `Engine` handle; dropping it cancels the worker, and the channels close.
- The two sides can be refactored independently as long as the 39-variant contract holds.

**Negative / accepted costs**
- The channel contract is a large surface (21 + 18 variants) and must be kept in sync with the docs ([Engine Events & Commands](../01-domains/desktop-app/engine-events.md)).
- "Fire and forget" UX (e.g. a config save) has to route its confirmation back through a listing event — there is no dedicated success event; the updated list *is* the confirmation.
- One-shot state (the event receiver) is consumed by the UI's event pump via `take_events()`; a second consumer would get nothing.

**Enforcement**
- A dedicated rule, [UI Core Seam](../../03-business-rules/ui-core-seam.md), marks any panel holding an engine handle or calling worker internals a violation.
- The engine's tests construct the engine headless (no Freya) and drive it purely through the channels, which is only possible because the seam is total.