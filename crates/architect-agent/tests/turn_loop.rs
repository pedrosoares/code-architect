//! The tool-call loop, driven by a scripted provider and fake tools.
//!
//! No HTTP: these pin the loop's behavior independently of any wire format.

use std::sync::{Arc, Mutex};

use architect_agent::{Agent, AgentConfig, AgentError, AgentEvent, ToolExecutor};
use architect_core::{
    ContentBlock, Message, Role, StopReason, ToolCall, ToolResult, ToolSchema, Usage,
};
use architect_llm::{ChatRequest, EventStream, LlmError, Provider, StreamEvent};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::json;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio_util::sync::CancellationToken;

/// Replays one scripted event list per request, recording what it was sent.
struct ScriptedProvider {
    turns: Mutex<Vec<Vec<StreamEvent>>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedProvider {
    fn new(turns: Vec<Vec<StreamEvent>>) -> Arc<Self> {
        Arc::new(Self {
            turns: Mutex::new(turns),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &'static str {
        "scripted"
    }

    async fn stream(
        &self,
        request: ChatRequest,
        _cancel: CancellationToken,
    ) -> Result<EventStream, LlmError> {
        self.requests.lock().unwrap().push(request);

        let mut turns = self.turns.lock().unwrap();
        let events = if turns.is_empty() {
            Vec::new()
        } else {
            turns.remove(0)
        };

        Ok(futures_util::stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

/// Echoes its arguments back, or fails on demand.
struct EchoTools {
    fail: bool,
    /// Ids the executor was asked to run, in order.
    seen: Mutex<Vec<String>>,
    /// Return a wrong id, to prove the loop repairs it.
    forge_id: bool,
}

impl EchoTools {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            fail: false,
            seen: Mutex::new(Vec::new()),
            forge_id: false,
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            fail: true,
            seen: Mutex::new(Vec::new()),
            forge_id: false,
        })
    }

    fn with_forged_ids() -> Arc<Self> {
        Arc::new(Self {
            fail: false,
            seen: Mutex::new(Vec::new()),
            forge_id: true,
        })
    }
}

#[async_trait]
impl ToolExecutor for EchoTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "echo".into(),
            description: "Echo the input".into(),
            input_schema: json!({"type": "object"}),
        }]
    }

    async fn execute(&self, call: ToolCall, _cancel: CancellationToken) -> ToolResult {
        self.seen.lock().unwrap().push(call.id.clone());

        let id = if self.forge_id {
            "wrong-id".to_owned()
        } else {
            call.id
        };

        if self.fail {
            ToolResult::error(id, "disk on fire")
        } else {
            ToolResult::ok(id, call.input.to_string())
        }
    }
}

fn tool_turn(id: &str, name: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta {
            text: "working".into(),
        },
        StreamEvent::ToolCallEnd {
            index: 0,
            call: ToolCall {
                id: id.into(),
                name: name.into(),
                input: json!({"a": 1}),
            },
        },
        StreamEvent::Finished {
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Usage::default()
            },
        },
    ]
}

fn final_turn(text: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta { text: text.into() },
        StreamEvent::Finished {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 20,
                output_tokens: 7,
                ..Usage::default()
            },
        },
    ]
}

fn drain(mut events: UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        out.push(event);
    }
    out
}

#[tokio::test]
async fn loops_until_the_model_stops_asking_for_tools() {
    let provider = ScriptedProvider::new(vec![tool_turn("call-1", "echo"), final_turn("done")]);
    let tools = EchoTools::new();
    let agent = Agent::new(
        provider.clone(),
        tools.clone(),
        AgentConfig::new("test-model"),
    );

    let (tx, rx) = mpsc::unbounded_channel();
    let mut history = vec![Message::user("do the thing")];

    let outcome = agent
        .run_turn(&mut history, &tx, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert_eq!(outcome.iterations, 2);
    assert!(!outcome.hit_iteration_limit);
    // Usage is summed across every request in the turn.
    assert_eq!(outcome.usage.input_tokens, 30);
    assert_eq!(outcome.usage.output_tokens, 12);
    // An unpriced model reports no cost rather than $0.00.
    assert!(outcome.cost.is_none());

    // user -> assistant(tool call) -> tool results -> assistant(final)
    assert_eq!(history.len(), 4);
    assert_eq!(history[1].role, Role::Assistant);
    assert_eq!(history[2].role, Role::User);
    assert_eq!(history[3].text(), "done");

    // The second request saw the whole conversation so far.
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages.len(), 3);
    assert_eq!(
        requests[1].tools.len(),
        1,
        "tools are offered on every iteration"
    );

    let events = drain(rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolStarted { call } if call.id == "call-1"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolFinished { .. }))
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::TurnCompleted { .. })
    ));
}

#[tokio::test]
async fn batches_parallel_tool_results_into_one_message() {
    let parallel = vec![
        StreamEvent::ToolCallEnd {
            index: 0,
            call: ToolCall {
                id: "a".into(),
                name: "echo".into(),
                input: json!({"n": 1}),
            },
        },
        StreamEvent::ToolCallEnd {
            index: 1,
            call: ToolCall {
                id: "b".into(),
                name: "echo".into(),
                input: json!({"n": 2}),
            },
        },
        StreamEvent::Finished {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        },
    ];

    let provider = ScriptedProvider::new(vec![parallel, final_turn("ok")]);
    let tools = EchoTools::new();
    let agent = Agent::new(provider, tools.clone(), AgentConfig::new("test-model"));

    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let mut history = vec![Message::user("two things")];

    agent
        .run_turn(&mut history, &tx, CancellationToken::new())
        .await
        .unwrap();

    // Both results land in a single message, in call order.
    let results = &history[2];
    assert_eq!(results.content.len(), 2);
    let ids: Vec<&str> = results
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::ToolResult(result) => result.tool_use_id.as_str(),
            other => panic!("expected a tool result, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(*tools.seen.lock().unwrap(), ["a", "b"]);
}

#[tokio::test]
async fn a_failing_tool_returns_an_error_result_instead_of_aborting() {
    let provider =
        ScriptedProvider::new(vec![tool_turn("call-1", "echo"), final_turn("recovered")]);
    let agent = Agent::new(
        provider,
        EchoTools::failing(),
        AgentConfig::new("test-model"),
    );

    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let mut history = vec![Message::user("go")];

    let outcome = agent
        .run_turn(&mut history, &tx, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    match &history[2].content[0] {
        ContentBlock::ToolResult(result) => {
            assert!(result.is_error);
            assert_eq!(result.content, "disk on fire");
        }
        other => panic!("expected a tool result, got {other:?}"),
    }
}

#[tokio::test]
async fn a_mismatched_result_id_is_repaired() {
    let provider = ScriptedProvider::new(vec![tool_turn("call-1", "echo"), final_turn("ok")]);
    let agent = Agent::new(
        provider,
        EchoTools::with_forged_ids(),
        AgentConfig::new("test-model"),
    );

    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let mut history = vec![Message::user("go")];

    agent
        .run_turn(&mut history, &tx, CancellationToken::new())
        .await
        .unwrap();

    match &history[2].content[0] {
        // Left alone, this id would leave the model waiting forever.
        ContentBlock::ToolResult(result) => assert_eq!(result.tool_use_id, "call-1"),
        other => panic!("expected a tool result, got {other:?}"),
    }
}

#[tokio::test]
async fn stops_at_the_iteration_limit_with_the_conversation_intact() {
    let provider = ScriptedProvider::new(vec![
        tool_turn("a", "echo"),
        tool_turn("b", "echo"),
        tool_turn("c", "echo"),
    ]);
    let agent = Agent::new(
        provider,
        EchoTools::new(),
        AgentConfig::new("test-model").max_iterations(2),
    );

    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let mut history = vec![Message::user("forever")];

    let outcome = agent
        .run_turn(&mut history, &tx, CancellationToken::new())
        .await
        .unwrap();

    assert!(outcome.hit_iteration_limit);
    assert_eq!(outcome.iterations, 2);
    assert_eq!(outcome.stop_reason, StopReason::ToolUse);
    // Every assistant turn and result batch is still recorded.
    assert_eq!(history.len(), 5);
}

#[tokio::test]
async fn cancelling_ends_the_turn_and_keeps_the_history() {
    let provider = ScriptedProvider::new(vec![tool_turn("call-1", "echo"), final_turn("never")]);
    let agent = Agent::new(provider, EchoTools::new(), AgentConfig::new("test-model"));

    let cancel = CancellationToken::new();
    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let mut history = vec![Message::user("go")];

    // Cancelled after the first reply is streamed and its tools have run.
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        token.cancel();
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let error = agent.run_turn(&mut history, &tx, cancel).await.unwrap_err();

    assert!(matches!(error, AgentError::Cancelled), "got {error:?}");
    assert_eq!(history.len(), 1, "nothing was appended after cancellation");
}

#[tokio::test]
async fn a_pause_turn_resumes_without_injecting_a_message() {
    let paused = vec![StreamEvent::Finished {
        stop_reason: StopReason::PauseTurn,
        usage: Usage::default(),
    }];
    let provider = ScriptedProvider::new(vec![paused, final_turn("resumed")]);
    let agent = Agent::new(
        provider.clone(),
        EchoTools::new(),
        AgentConfig::new("test-model"),
    );

    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let mut history = vec![Message::user("search the web")];

    let outcome = agent
        .run_turn(&mut history, &tx, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    // The resumed request carries the history as-is: no synthetic "continue".
    let requests = provider.requests();
    assert_eq!(requests[1].messages.len(), 2);
    assert!(
        requests[1]
            .messages
            .iter()
            .all(|message| message.text() != "continue")
    );
}
