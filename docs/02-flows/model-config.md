---
id: flow.model-config
type: flow
title: Model & Provider Configuration (Profiles, Ad-hoc, Compaction)
depends_on:
- domain.desktop-app
- domain.configuration
- domain.llm-providers
---

**Goal:** pick which API and model answer the next turn — from saved profiles, the engine's startup config, or an ad-hoc local model — without losing the conversation in progress.

## Saved API configurations (Profiles)

- Settings → "API configurations": add/edit/delete `Profile { id, name, kind (openai|anthropic), base_url?, api_key?, model }`. All through `Command::{ListProfiles, SaveProfile, DeleteProfile, ActivateProfile, DeactivateProfile}` ↔ `ProfilesListed { profiles, active }` — the panel never touches the store directly.
- **Activate ("Use")**: the worker rebuilds the running `Provider` **in place** (same `ProviderRegistry::build` call) — only which API answers the *next* turn changes; nothing is reset, the in-flight conversation keeps streaming. A bad activation keeps the previously-working provider.
- **Deactivate ("Use" on the "Default" row)**: `clear_active` — the engine falls back to its startup (env-derived) provider config; the profile is untouched.
- An active profile **overrides** the env-derived `ProviderConfig` from startup on.
- If the very first provider build fails at startup, it's not fatal: `provider = None`, `Send` reports "no working API configuration — open Settings to add or fix one", settings stays usable — a bad startup config can be fixed from the running app.

## Ad-hoc models (LM Studio page)

"LM Studio" is a **deliberately separate settings page** (not folded into profiles):

- Base URL + optional API key are held **only in `use_state`** — "never saved — what a local server has loaded changes over time, so a saved snapshot would only ever be stale the moment it's written". (An earlier version persisted a profile per model; it was removed for this reason.)
- **"Fetch models"** → `engine.list_models(base_url, api_key)` (defaults to `http://localhost:1234/v1` if empty) → `architect_llm::list_models` does `GET {base_url}/models` (reusing `http::send_retrying` — a 429/5xx here behaves exactly like mid-turn) → `ModelsListed { base_url, models }` → `Transcript.discovered_models` (replaced wholesale; matched by `base_url`).
- **Per-row "Use"** → `Command::UseAdHocModel { base_url, api_key, model }` → the worker rebuilds the live provider in place exactly the way `ActivateProfile` does but **skips every `ConfigStore` step** — nothing is written to `profiles.json` → `AdHocModelActivated { model }` → `Transcript.active_adhoc_model` (mutually exclusive with `active_profile` — one line in each event's `apply` arm). The header's model chip shows the ad-hoc model as a second tier between a saved profile and the startup default.

## Provider kind rules

- **Do not add a kind for an OpenAI-compatible server.** LM Studio, DeepSeek, OpenRouter, vLLM, Ollama are `kind: "openai"` with a different `base_url`.
- Anthropic requires an `api_key` (missing → `LlmError::Config` at build).
- A genuinely different API implements `Provider` and registers a factory via `ProviderRegistry::register("my-api", |config| ..)`.

## Compaction (the other context lever)

The "Compact" button asks the model to summarize the active conversation, then replaces its history with just that summary — freeing context the way a new chat would, without losing the session's identity, its saved file changes, or its plan.

1. `Transcript::start_compacting` sets `Status::Compacting` optimistically (folds into `is_busy()` — the Stop button and `Command::Cancel` work on it for free, via the same `running` map).
2. `Command::Compact(session)` — refused (via `Failed`) if a turn is already running ("a turn is already running for this session") or history is empty ("nothing to compact yet").
3. `spawn_compact`: build `history + Message::user(COMPACT_INSTRUCTION)`, run through `Agent::new(provider, NoTools, AgentConfig::new(model).system(COMPACT_SYSTEM).max_iterations(1))` via the normal `run_turn` — cancellation and error handling from the one place that already gets them right, with none of a real turn's tool registry/file-change/plan channels. Events are deliberately not forwarded (local channel, receiver dropped). Prompts: `COMPACT_SYSTEM` ("Reply with only the summary itself — no preamble, no headers…"), `COMPACT_INSTRUCTION` (summarize objective, key decisions + rationale, files/resources and their state, what remains).
4. On success: `slot.history = vec![Message::assistant(summary)]`, `persisted_len` reset, `store.replace_messages(session, &new_history)` (DELETE then re-insert — a genuine wholesale replace), `Compacted { session, summary }`.
5. `Transcript::apply` replaces **all rows** with the single `Row::Compacted` (the *only* row left — it marks a break, not another turn) and resets `context_tokens` to 0. The conversation keeps running afterward exactly as before; only what gets sent as history shrinks.

## Notes

- `ListModels` is the one command that skips the `*_tx`/`*_rx` outcome-channel pattern — spawned straight from the handler; nothing in worker state needs updating afterward.
- Vision: the "vision" badge next to the model chip comes from `pricing::supports_vision` (informational only — it never gates the Attach button or `view_image`/`screenshot`).