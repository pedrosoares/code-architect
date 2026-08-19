//! End-to-end against a real local model.
//!
//! Ignored by default so the normal suite stays hermetic. Run with a local
//! OpenAI-compatible server (LM Studio, Ollama, vLLM, llama.cpp) listening:
//!
//! ```sh
//! cargo test -p architect-agent --test live_lmstudio -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` matters: these tests run against whatever model the
//! server has loaded, and two of them racing makes the server swap models
//! mid-request ("Model is unloaded").
//!
//! Override with `ARCHITECT_LIVE_BASE_URL` and `ARCHITECT_LIVE_MODEL`.

use std::sync::{Arc, Mutex};

use architect_agent::{Agent, AgentConfig, AgentEvent, NoTools, ToolExecutor};
use architect_core::{Message, StopReason, ToolCall, ToolResult, ToolSchema};
use architect_llm::{ProviderConfig, ProviderRegistry};
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn base_url() -> String {
    std::env::var("ARCHITECT_LIVE_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:1234/v1".to_owned())
}

fn model() -> String {
    // Any tool-capable local model works; this one is verified to load and to
    // emit `reasoning_content`.
    std::env::var("ARCHITECT_LIVE_MODEL").unwrap_or_else(|_| "qwen/qwen3.8-27b".to_owned())
}

fn provider() -> Arc<dyn architect_llm::Provider> {
    ProviderRegistry::default()
        .build(&ProviderConfig::new("openai").base_url(base_url()))
        .expect("the openai kind is built in")
}

/// Reports a fixed temperature, and records that it was actually called.
struct Weather {
    calls: Mutex<Vec<ToolCall>>,
}

#[async_trait]
impl ToolExecutor for Weather {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "get_weather".into(),
            description: "Get the current weather for a city.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"city": {"type": "string", "description": "City name"}},
                "required": ["city"],
            }),
        }]
    }

    async fn execute(&self, call: ToolCall, _cancel: CancellationToken) -> ToolResult {
        self.calls.lock().unwrap().push(call.clone());
        ToolResult::ok(call.id, "17°C and raining")
    }
}

#[tokio::test]
#[ignore = "requires a local OpenAI-compatible server"]
async fn streams_a_real_completion() {
    let agent = Agent::new(
        provider(),
        Arc::new(NoTools),
        AgentConfig::new(model()).system("Answer in one short sentence."),
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut history = vec![Message::user("Name the capital of France.")];

    let outcome = agent
        .run_turn(&mut history, &tx, CancellationToken::new())
        .await
        .expect("the local server should answer");

    let mut text = String::new();
    while let Ok(event) = rx.try_recv() {
        if let Some(delta) = architect_agent::agent::text_of(&event) {
            text.push_str(delta);
        }
    }

    println!(
        "model: {}\nreply: {text}\nusage: {:?}",
        model(),
        outcome.usage
    );

    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert!(!text.is_empty(), "the model streamed no text");
    assert_eq!(
        history.last().expect("a reply").text(),
        text,
        "streamed text must match the assembled message"
    );
}

#[tokio::test]
#[ignore = "requires a local OpenAI-compatible server with a tool-capable model"]
async fn runs_a_real_tool_call_round_trip() {
    let tools = Arc::new(Weather {
        calls: Mutex::new(Vec::new()),
    });
    let agent = Agent::new(
        provider(),
        tools.clone(),
        AgentConfig::new(model())
            .system("Use the get_weather tool when asked about weather. Then answer briefly.")
            .max_iterations(4),
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut history = vec![Message::user("What is the weather in Paris right now?")];

    let outcome = agent
        .run_turn(&mut history, &tx, CancellationToken::new())
        .await
        .expect("the local server should answer");

    let mut started = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, AgentEvent::ToolStarted { .. }) {
            started += 1;
        }
    }

    let calls = tools.calls.lock().unwrap();
    println!(
        "model: {}\niterations: {} tool calls: {} outcome: {:?}\nfinal: {}",
        model(),
        outcome.iterations,
        calls.len(),
        outcome.stop_reason,
        history.last().expect("a reply").text()
    );

    // A local model that ignores tools is a model limitation, not a bug in the
    // loop — say which it is rather than silently passing.
    assert!(
        !calls.is_empty(),
        "{} never called the tool; the loop is fine but this model is not usable for tool use",
        model()
    );
    assert_eq!(
        started,
        calls.len(),
        "every executed call should have been announced"
    );
    assert_eq!(calls[0].name, "get_weather");
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert!(
        outcome.iterations >= 2,
        "a tool round trip needs at least two requests"
    );
}
