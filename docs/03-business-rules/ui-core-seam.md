---
id: rule.ui-core-seam
type: rule
title: UI ↔ Engine Seam Rules
---

**The only contract between the Freya UI and the rest of the system is a channel pair. Everything else is convention enforced by that shape.**

## The channel pair

```
Freya UI (main thread) ──Command──►  worker on a dedicated Tokio runtime thread
                       ◄─EngineEvent──
```

- Freya owns the main thread and runs its own executor; `reqwest` needs a Tokio reactor. So the agent gets a dedicated runtime thread, and **both channels are unbounded** — a busy UI must never stall the model stream, and a queued command must never block a render.
- `Engine` is a `static OnceLock<Engine>` set by `main()` before `launch(...)`, read by `app::app()` via `use_hook` — the only reason being that `main` needs a handle to call `Engine::shutdown()` after the window closes (there is no path from inside the Freya tree back to `main`).
- The event pump in `app.rs` is a once-only `use_hook` that `take_events()` (one-shot: the receiver is `.take()`n from an `Arc<Mutex<Option<...>>>`, so re-renders never start a second drain) and spawns `while let Some(e) = events.recv().await { transcript.write().apply(&e) }`. No polling, no timers.

## The rules

1. **No component ever calls the agent directly; they send a `Command`.** Settings, sessions, composer, inspector — all four. The engine is the only thing that touches the provider, the stores, or the tool crates.
2. **`Transcript::apply` is a pure function** — no IO, no Freya types — which is what makes the streaming behavior testable without a model or a window.
3. **`apply` routes every event into the conversation it names by session**, never into "whichever one is active" — a session's conversation keeps accumulating whether or not it's the one on screen.
4. **Session ids are minted client-side** (`SessionId::new()` at "+ New Chat") before either side of the channel does any work — starting a new chat needs no engine round-trip until something is actually sent.
5. **One concurrent turn per session** is enforced in the worker's `running` map, not in the UI — the UI's busy indicator is a projection, not a lock.
6. **Optimistic client-side flips** exist only where the round-trip would visibly lag: `push_user` → `Waiting`; `start_compacting` → `Compacting`. Both are corrected by the real events.
7. **Do not add a second channel for errors on the outer pair** — a failure would then be able to overtake deltas emitted before it. `Cancelled`/`Failed` carry a session tag (`Failed`'s is `Option<SessionId>`; `None` = genuinely global) precisely so they can share the stream.
8. **Tools are built fresh per turn** (per-turn `FileChange`/plan channels) so two sessions' concurrent `write_file`/`edit_file` calls report changes each tagged for the session that made them — a single shared tool instance couldn't tell them apart.
9. **`Command` derives `PartialEq/Eq`** — which is why `shutdown` and `SpawnSubAgentRequest` (both carrying senders) cannot be `Command` variants; they ride their own channels.
10. **Cross-panel signals are newtype-wrapped `State` contexts**, not engine commands: `ScrollToToolCall(Option<String>)` (Inspector→Transcript; the Transcript resets it to `None` on use so repeat clicks work), `SessionsCollapsed`, `InspectorCollapsed` (owned by the `Shell`).

## The same shape repeats

Turns, compactions, OAuth logins, and model-listing all follow **"spawn it, never await it, react to the outcome later"** (`spawn_turn`/`TaskOutcome`/`task_rx`, `spawn_compact`/`CompactOutcome`/`compact_rx`, `run_login`/`oauth_rx`, `ListModels` direct). `ListModels` is the one that skips the outcome channel entirely — nothing in worker state needs updating afterward.