---
id: domain.desktop-app-panels
type: domain
title: Desktop App Panels
---

The desktop app's UI layer (`apps/desktop/src/ui/`): five panels plus the root component, all rendering pure state — the only bridge to the engine is the `Command`/`EngineEvent` channel pair described in [Desktop App Domain](overview.md). The state that gets rendered lives in `ui/state.rs` (`Transcript` + `Conversation`), folded by the pure `Transcript::apply(&EngineEvent)`.

## `app.rs` — root component

- Dark theme; `Transcript` published as Freya `State` context so every panel reads the same source.
- `ScrollToToolCall(Option<String>)` — a cross-panel signal: the Tools panel writes a tool-call id, the Transcript's matching row scrolls itself into view (each row has a per-row a11y id that watches the signal).
- Owns the `Engine` handle: the `ENGINE` `OnceLock` static in production, or a locally started engine in headless tests.
- **The event pump**: a once-only `use_hook` takes the engine's event receiver (one-shot `take_events()`) and spawns `while let Some(e) = events.recv().await { transcript.write().apply(&e) }`. No polling, no timers — the UI updates exactly as fast as events arrive.

## `sessions.rs` — left sidebar

- "+ New Chat" is **pure client-side** (`start_new_chat` mints a new `SessionId` locally; the engine only learns about it on the first `Send`). No round-trip.
- One row per `SessionSummary`; **sub-agent children are indented under their parent** (in spawn order), so the tree of a multi-agent investigation is visible at a glance.
- A busy dot per conversation (`status.is_busy()`); click-to-select (resident session → `switch_to`, otherwise `engine.load_session`); per-row Delete.

## `transcript.rs` — center panel

A **pure renderer of `Row`s** — no logic, no engine access. Auto-scrolls on row change. Row kinds:
- `User` — image thumbnails + a text card.
- `Assistant` — reasoning (if any) inside a `Disclosure`, text in a card; a turn with only whitespace renders `None`.
- `ToolRow` — status glyph `●/✓/✗`, click-to-expand the output, and the scroll-to-tool-call behavior. When the tool produced an image (a `view_image` result, which is how a `firefox_screenshot` reaches the model), the image renders alongside the text — the row is expandable if it has *any* content (text or image).
- `Error`, `Compacted`.

Message cards are `SelectableText`, **not markdown** — a deliberate copy-paste tradeoff, see [ADR: no markdown rendering](../../06-decisions/0002-no-markdown-rendering.md).

## `composer.rs` — bottom of center

- `ComposerInput` is **hand-rolled on `freya_edit`** because Freya's `Input` is hard-coded single-line: it wraps, auto-grows to 160px, then scrolls internally.
- Enter submits / Shift+Enter newline / Escape unfocuses. Send becomes **Stop** while the conversation is busy (sends `Cancel`).
- Attach → native `rfd` file picker (png/jpg/jpeg/gif/webp) → `Attachment { media_type, bytes }` — raw bytes end to end; base64 is produced only in the worker.
- Placeholder "Type a message... (@ to reference files)" — the `@`-mention handling is **not implemented** (the placeholder is aspirational).

## `inspector.rs` — right panel, five tabs

- **Files** — deduped per path; click → the Diff tab; per-file **Roll Back** opens a confirmation popup and then sends `Rollback { session, up_to_seq: entry.message_seq - 1 }`.
- **Tools** — click a call → writes its id into `ScrollToToolCall`.
- **Diff** — `similar::TextDiff`; syntax highlighting via Freya's `code_editor` for rs/py/js/ts/json/bash, plain diff-colored text otherwise.
- **Processes** — global (not per-session); status glyphs; click-to-expand the live log.
- **Plan** — per-conversation; status glyphs; read-only.

## `settings.rs` — popup, five sections

- **Profiles** — list/add/edit/delete/activate/deactivate, plus a "Default" row for the engine's startup config with a "Use" that deactivates the active profile.
- **LM Studio** — its own page: base URL + optional key held **only in `use_state`, never saved**; "Fetch models" → `list_models`; each row's "Use" → `UseAdHocModel` (rebuilds the provider in place without touching `ConfigStore`).
- **MCP servers** — add/edit/enable/disable/delete; stdio (command+args+env) or HTTP (url+bearer); new saves are `enabled: true`.
- **Integrations** — GitHub (token + **Login** + gh-CLI toggle — when the toggle is on the token input is hidden and the config forces `github_token: None`), Slack (token + Login), Linear (key + Login); "✓ Connected" when saved, "Waiting for browser…" while an OAuth login is pending.
- **Documentation** — enable toggle (default on); vault path (blank → workspace `docs/`); driver hard-fixed to `"obsidian"`.

## `shell.rs` — window chrome

- Header: model/provider chip (an ad-hoc model sits as a second tier between a saved profile and the startup default), a vision badge from `supports_vision`, the Compact button, the Settings button.
- Status bar: workspace path, token totals, context fill or "unsized model", spend or "unpriced model".
- The three resizable panels with collapse-to-strip toggles (the distinct-key remount trick).

## See also

- [Desktop App Domain](overview.md)
- [Engine Events & Commands](engine-events.md)