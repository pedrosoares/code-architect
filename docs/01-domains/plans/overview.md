---
id: domain.plans
type: domain
title: Plans Domain
depends_on:
- domain.tools
- domain.conversation
relations:
  related_flows:
  - flow.send-message
---

A plan is the agent's current multi-step todo list for a session's task — a **snapshot, not a log**.

## The data model (`architect-core::plan`)

- `Plan { goal: Option<String>, steps: Vec<PlanStep> }` — `goal` is a one-line summary distinct from any step's description; a plan can be just a step list.
- `PlanStep { description, status: StepStatus (default Pending), substeps: Vec<PlanSubstep> (default empty) }`.
- `PlanSubstep { description, status: StepStatus (default Pending) }` — **one level deep only**, by design.
- `StepStatus`: `Pending | InProgress | Completed` (serde: `pending`/`in_progress`/`completed`).
- `PlanRecorder` (trait, `Send + Sync`, single `record(Plan)` method) — the persistence-side sink, kept separate from what the model sees; `NoPlanRecorder` for tools used outside a session.

## Tools

- **`write_plan`** — input deserializes directly into `Plan` (no wrapper struct, unlike every other tool). Full-replace semantics: each call records the entire plan, including status flips on earlier steps — the same shape as other coding agents' todo tools. Returns the formatted plan (goal line; `n. [glyph] description` with blank/`~`/`x` glyphs; substeps indented `n.m.`; empty → "(no steps)").
- **`read_plan`** — no parameters; returns the formatted `ctx.current_plan()` or "No plan saved yet for this session." The plan is **handed in at turn start** via `ToolContext::with_current_plan` (the engine passes whatever was known before the turn) — so a `write_plan` earlier in the same turn isn't visible to a later `read_plan` in that turn. This only matters for fresh turns or resumed sessions; the model already knows what it just wrote.

## Wiring

- Per-turn `plan_tx`/`plan_rx` channel next to the file-change channels; only the **last** value drained matters (unlike file changes' `Vec`).
- `EngineEvent::PlanUpdated { session, plan }` is emitted **live** as `write_plan` runs mid-turn (a documented regression fix: plans used to appear only after the whole turn).
- `TaskOutcome.plan: Option<Plan>` — the last one is persisted via `store.save_plan` (upsert) from the same post-turn step.
- `EngineEvent::PlanLoaded { session, plan: Option<Plan> }` is sent right after `FileChangesLoaded` on startup resume and `Command::LoadSession` — but **not** after `Command::Rollback`, because rollback undoes file changes, not the plan.
- `write_plan`/`read_plan` are always registered (`plan_tools`, built once, unconditionally — an always-on app capability, not gated behind config).

## Storage

`plans` table: one row per session — `session_id` PK (FK cascade), `data` (the whole `Plan` as JSON), `updated_at`. `save_plan` is an upsert (`ON CONFLICT(session_id) DO UPDATE`). There is exactly one current `Plan` per session, not a history of edits — which is what makes persistence trivial compared to `file_changes`.

## UI

The Inspector's **Plan tab** is per-conversation (like Files/Tools/Diff, unlike the global Processes tab) — a plan belongs to one session's task. Rows: goal, then steps with status glyphs (○ pending / ● in progress / ✓ completed) and indented substeps. No click/expand interaction — a step's description is its whole content. A resumed session picks its plan back up automatically.