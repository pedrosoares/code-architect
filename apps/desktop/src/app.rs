//! Root component: theme, shared state, and the bridge to the agent.

use freya::components::use_init_theme;
use freya::prelude::*;

use crate::{
    ENGINE,
    engine::{Engine, EngineConfig},
    panels::ScrollToToolCall,
    shell::Shell,
    state::Transcript,
};

pub fn app() -> impl IntoElement {
    use_init_theme(architect_ui::theme::dark);

    // One signal for the whole conversation: the panels read different parts of
    // it, but they all re-render off the same stream of agent events.
    let transcript = use_state(Transcript::default);
    use_provide_context(|| transcript);

    // The Inspector's Tools tab and the Transcript panel are siblings with no
    // other shared state between them — this is the one signal that crosses
    // that gap, for "jump to this call."
    let scroll_to_tool_call = use_state(|| None::<String>);
    use_provide_context(|| ScrollToToolCall(scroll_to_tool_call));

    // `main()` sets `ENGINE` before `launch(...)` in the real binary, so
    // `shutdown()` can reach it after the window closes (see main.rs). A
    // headless test rendering `app::app` directly, without going through
    // `main()`, falls back to constructing its own — same as every other
    // test fixture's `Engine::start(EngineConfig::default())`.
    let engine = use_hook(|| match ENGINE.get() {
        Some(engine) => engine.clone(),
        None => Engine::start(EngineConfig::from_env()),
    });
    use_provide_context(|| engine.clone());

    // Drain the agent's events into the transcript for as long as the app runs.
    // The receiver is taken once; `use_hook` guarantees this body runs on the
    // first render only, so a re-render never starts a second drain.
    use_hook(move || {
        let Some(mut events) = engine.take_events() else {
            return;
        };
        let mut transcript = transcript;

        spawn(async move {
            while let Some(event) = events.recv().await {
                transcript.write().apply(&event);
            }
        });
    });

    Shell
}
