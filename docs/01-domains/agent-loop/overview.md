---
id: domain.agent-loop
type: domain
title: Agent Loop Domain
depends_on:
- domain.conversation
- domain.llm-providers
- domain.tools
relations:
  related_flows:
  - flow.send-message
  - flow.subagents
---

The agent domain (`architect-agent`) owns the tool-call loop — the heart of a turn.

## Public API

- **`ToolExecutor`** (trait) — the extension point tools fill:
  - `fn schemas(&self) -> Vec<ToolSchema>`
  - `async fn execute(&self, call: ToolCall, cancel: CancellationToken) -> ToolResult`
  - `ToolRegistry` (`architect-tools`) is the canonical implementation. `NoTools` is the empty implementation (used by compaction).
- **`Agent`** — `Agent::new(provider: Arc<dyn Provider>, tools: Arc<dyn ToolExecutor>, config: AgentConfig)`.
- **`AgentConfig`** — `new(model)` plus builders: `system(impl Into<String>)`, `reasoning(Reasoning)`, `max_iterations(usize)`.
- **`AgentEvent`** — what `run_turn` streams:
  - `IterationStarted { iteration }`
  - `Stream(StreamEvent)` — raw provider events forwarded as they arrive (text deltas, reasoning deltas, tool-call start/input-delta/end)
  - `ToolFinished { result: ToolResult }`
  - `TurnCompleted(TurnOutcome)`
- **`TurnOutcome`** — final message, `usage` (cumulative for the turn), `context_tokens` (the **last** response's input size), `context_window: Option<u64>` (from `pricing::context_window_for`), `stop_reason`, `hit_iteration_limit: bool`.
- **`AgentError`** — `Cancelled`, plus LLM/tool errors.
- **`run_turn(&mut history: Vec<Message>, tx: &UnboundedSender<E>, cancel: CancellationToken)`** with `E: From<AgentEvent>` — the entry point; `history` is the conversation so far (system prompt handled by the provider layer), mutated in place with each new message.

## The loop

1. Build a `ChatRequest` from `history` (system prompt hoisted to the top-level `system` field; tools from `ToolExecutor::schemas()`), send it via `Provider::stream`.
2. Forward every `StreamEvent` to `tx` as `Stream(..)` — this is how the UI shows tokens live.
3. Collect the reply (assembly order: reasoning, text, tool calls; a stream ending without `Finished` falls back to `EndTurn`).
4. Append the assistant message to `history`.
5. If `stop_reason.wants_continuation()` (i.e. `ToolUse` or `PauseTurn`) **and** iterations remain:
   - Execute the turn's tool calls (via `ToolExecutor::execute`), emitting `ToolFinished` per result.
   - Append the results as one `Message::tool_results(...)` (single user message — Anthropic's batching requirement; the OpenAI adapter unpacks per-result at serialization).
   - `IterationStarted { iteration + 1 }`, go to 1.
6. Otherwise finish with `TurnCompleted`.

Cancellation: the `CancellationToken` is checked at stream level (`LlmError::Cancelled`); a cancelled turn surfaces as `AgentError::Cancelled`. Note: `ToolRegistry::execute` accepts the token but **ignores it** — a tool already running runs to completion.

## Invariants and notes

- One assistant message per iteration; tool results always ride in the next user message.
- `Refusal` and `StopSequence` end the turn normally — they are outcomes, not errors.
- `hit_iteration_limit` (true when the cap was reached mid-tool-loop) is reported on `TurnOutcome`; the engine uses it to classify sub-agent failures.
- The agent knows nothing about sessions, persistence, or which turn it is in — the engine supplies `history` and re-supplies it each turn.
- The desktop engine uses `Reasoning::VISIBLE` (adaptive, summarized display, high effort) for user turns; compaction uses a single iteration with `NoTools`.

## System prompts

- **User turns**: `EngineConfig.system` (default: "You are code-architect, an agent that does software-engineering work inside one workspace. Read the code before changing it; make the smallest change that fulfills the request; verify with the tools you are given (run the build and tests) and report the result honestly. Keep replies short: no narration of tool calls and no recap of the work in the final answer — state only what changed and any caveats."), plus a docs-protocol block appended **only** when doc tools are registered (instructing the model to look up affected domains/rules/entities before non-trivial changes and update docs after behavior-affecting changes).
- **Sub-agent turns**: the separate `SUBAGENT_SYSTEM` constant (scoped-prompt preamble + instructions to recompute totals explicitly rather than trust a single mental tally).