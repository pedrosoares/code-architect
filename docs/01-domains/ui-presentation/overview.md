---
id: domain.ui-presentation
type: domain
title: UI Presentation Domain (architect-ui)
status: active
depends_on:
- system.architecture
- system.conventions
relations:
  related_domains:
  - domain.desktop-app
  related_rules:
  - rule.ui-core-seam
---

The shared presentation layer — design tokens and reusable widgets — that every panel in `apps/desktop` builds on. The crate is `architect-ui`; it depends on **`freya` and nothing else**, never on the agent, the LLM client, or session storage. Keeping that edge one-way is the crate's entire reason for existing: it is what lets the same widgets be reused by a second window, a settings app, or a future headless-client shell without dragging in any business logic. See [Architecture](#) for the dependency rule this crate is the UI-side half of, and [Conventions](#) for the Freya house style it enforces.

## The one-way dependency

`architect-ui` sits at the top of the UI side of the dependency graph:

```
apps/desktop ──► architect-ui ──► freya
```

- It exports **no** type that the worker or any other crate needs; only `apps/desktop`'s panels import it.
- It **cannot** import `architect-agent`, `architect-llm`, `architect-session`, or `architect-config`. If a widget ever needs a value that lives in one of those, the panel lifts it and passes it in as a field — the widget itself never reaches across the seam (the same rule as [UI ↔ Engine Seam](#): components don't call the engine, they take data).
- There are **no tests in this crate** — it is pure presentation. Layout is verified where it matters by `apps/desktop`'s `freya_testing` PNG renders.

## Two layers: tokens and widgets

The crate is split into `theme` (design tokens + the app `Theme`) and `widgets` (reusable components). Everything a panel renders is composed from one or the other.

### `theme` — tokens and the `Theme`

Two layers of the same palette, for two different call sites:

- **Raw tokens** — `Color` / metric `const`s, used when styling a `rect()` directly in panel code.
- **`dark()`** — a `Theme` handed to `use_init_theme`, which restyles every *built-in* Freya component (buttons, inputs, scrollbars, …) against the semantic `COLORS` sheet.

The palette is ported from Code Architect v1: **warm near-black surfaces with a single amber accent.**

**Surfaces**

| Token | RGB | Used for |
|---|---|---|
| `BACKGROUND` | `13, 11, 10` | Window background — the darkest surface, behind the transcript |
| `SURFACE` | `22, 18, 14` | Panels: sidebar, header, status bar, composer |
| `SURFACE_RAISED` | `36, 28, 20` | Raised elements: message cards, tool rows, chips, session rows |
| `BORDER` | `51, 41, 29` | Hairlines between panels and around raised elements |

**Text**

| Token | RGB | Used for |
|---|---|---|
| `TEXT` | `237, 230, 220` | Primary text |
| `TEXT_DIM` | `160, 143, 121` | Secondary: timestamps, captions, tool arguments, empty states |

**Accents & status**

| Token | RGB | Used for |
|---|---|---|
| `ACCENT` | `235, 182, 90` | The amber accent: filled buttons, active chips, selection |
| `ON_ACCENT` | `26, 20, 9` | Text/icons drawn *on top of* `ACCENT` |
| `ACCENT_SOFT` | `58, 45, 26` | Amber wash behind selected rows |
| `SUCCESS` | `122, 184, 135` | Completed tool calls, applied edits, passing checks |
| `ERROR` | `224, 110, 105` | Failed tool calls, surfaced errors |
| `SUCCESS_SOFT` | `30, 46, 34` | Green wash behind an **inserted** diff line — `SUCCESS` as a background |
| `ERROR_SOFT` | `48, 28, 27` | Red wash behind a **deleted** diff line — `ERROR` as a background |

`*_SOFT` tokens are the deliberate background-form of a status color, the same relationship `ACCENT_SOFT` has to `ACCENT` — a signal you can sit text on without it fighting the glyphs.

**Metrics**

| Token | Value | Meaning |
|---|---|---|
| `SPACE_SM` | `6.0` | Gap between related elements inside a group |
| `SPACE_MD` | `12.0` | Standard padding inside panels and rows |
| `HEADER_HEIGHT` | `56.0` | Height of the window header |
| `STATUS_HEIGHT` | `26.0` | Height of the status bar |
| `AVATAR_SIZE` | `28.0` | Diameter of a transcript avatar |
| `FONT_BODY` | `14.0` | Body text size |
| `FONT_SMALL` | `12.0` | Caption / metadata text size |
| `FONT_MONO` | `"monospace"` | Tool names and arguments |

**The `COLORS` sheet and `dark()`**

`COLORS: ColorsSheet` is the *semantic* palette — the names every built-in component resolves against. `filled_button` takes its background from `primary` (= `ACCENT`) and its text from `text_inverse` (= `ON_ACCENT`); `Input` borders come from `border` / `border_focus`. Because components reference these by name, **restyling a component is a matter of changing this sheet, not touching call sites.**

`dark()` starts from Freya's `dark_theme()` (so every built-in component has a theme registered), swaps in `COLORS`, then applies one component-level override via `Theme::set`:

- `"input_layout"` → a `CornerRadius` of `22` and inner margins of `10, 16` — the **composer input is a pill**, as in v1.

Any further per-component override belongs here (`Theme::set` with that component's `*ThemePreference`, keys `"button"`, `"filled_button"`, `"input"`, `"scroll_bar"`, …), **not** at each call site.

### `widgets` — presentation-only components

Widgets take what they render as fields and **own no application state** (the one exception, `Disclosure`, holds its own open/closed flag — a local UI affordance, not app state). Each is a `#[derive(PartialEq)] struct` implementing `Component`; the public set is re-exported from `widgets::mod`.

| Widget | What it renders | Builder API |
|---|---|---|
| `Avatar` | The round initial badge that opens a transcript row — a 28px (`AVATAR_SIZE`) circle on `SURFACE_RAISED` with a `BORDER` outline and a bold `FONT_SMALL` initial | `Avatar::new("A")`, `.tint(color)` (initial color + badge outline tint; defaults to `TEXT_DIM`) |
| `Disclosure` | A collapsible section introduced by a small triangle (`▸` closed / `▾` open) — used for the agent's `reasoning` blocks | `Disclosure::new(label)`, `.content(element)`, `.open(bool)` (whether it starts expanded) |
| `PanelHeader` | The title bar at the top of a panel — 34px tall, a bold `FONT_SMALL` `TEXT_DIM` title left-aligned | `PanelHeader::new("Sessions")`, `.trailing(element)` (content pinned to the right edge, e.g. an action button) |
| `StatusChip` | A pill in the header — model name, mode, connection status | `StatusChip::new(text)`, `.active(bool)` (fills with `ACCENT` + `ON_ACCENT` text; inactive is `SURFACE_RAISED` + `TEXT_DIM`) |
| `divider()` | A 1px full-width horizontal rule in `BORDER` | plain `fn -> impl IntoElement`, not a `Component` |

Naming note: the header pill is **`StatusChip`, not `Chip`**, deliberately — to stay clear of Freya's own built-in `Chip` component.

## Rules and conventions

These come from [Conventions](#) and are enforced by this crate's shape:

- **Style with tokens from `architect_ui::theme`, never literal colors.** A panel reaches for `theme::ACCENT`, not `Color::from_rgb(235, 182, 90)`. This is why a palette change is one-file, not app-wide.
- **Component-wide styling lives in `theme::dark()` via `Theme::set`, not at call sites.** If you find yourself restyling a built-in component inline, move that override into `dark()`.
- **Reusable UI is a `#[derive(PartialEq)] struct` implementing `Component`** (its own hook scope); a small stateless fragment can be a plain `fn x() -> impl IntoElement` (as `divider()` is).
- **Widgets own no application state.** Data flows in as fields from the panel; the panel is where `Command`s and `Transcript` state live. A widget never talks to the engine.
- **Freya 0.4 builder API — no `rsx!` macro.** Dynamic lists are built by folding with `.child(..)` in a loop.

## Where it is used

`apps/desktop`'s panels import it for every shared visual: `transcript.rs` (the `Avatar` that opens each row, `Disclosure` for reasoning, message cards on `SURFACE_RAISED`), `sessions.rs` / `inspector.rs` / `settings.rs` (`PanelHeader`, `StatusChip`, `divider`), and `shell.rs` (the header `StatusChip`s, the status bar, `HEADER_HEIGHT`/`STATUS_HEIGHT`). The `Theme` from `theme::dark()` is installed once at the root in `app.rs` via `use_init_theme`, so the whole tree inherits the palette.
