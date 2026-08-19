//! Code Architect — desktop entry point.
//!
//! This binary owns window setup and composition only. Application logic
//! belongs in the `architect-*` crates; see GUIDELINES.md.

mod app;
mod engine;
mod oauth;
mod panels;
mod shell;
mod state;

use std::sync::OnceLock;

use engine::{Engine, EngineConfig};
use freya::prelude::*;
use tracing_subscriber::EnvFilter;

/// Constructed here, not inside `app::app()`'s `use_hook` — `main()` needs
/// its own handle to reach `Engine::shutdown()` after `launch(...)` returns
/// (the window closed), and there is no path from inside the Freya
/// component tree back out to `main()` otherwise. `app::app()` reads this
/// instead of constructing its own `Engine`.
static ENGINE: OnceLock<Engine> = OnceLock::new();

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_env("ARCHITECT_LOG"))
        .init();

    if ENGINE.set(Engine::start(EngineConfig::from_env())).is_err() {
        panic!("ENGINE is set exactly once, before launch()");
    }

    launch(
        LaunchConfig::new().with_window(
            WindowConfig::new(app::app)
                .with_title("Code Architect")
                .with_size(1440.0, 900.0)
                .with_min_size(960.0, 600.0)
                .with_background(architect_ui::theme::BACKGROUND)
                .with_app_id("net.pedrosoares.code_architect"),
        ),
    );

    // The window closed — kill anything `start_process` left running rather
    // than leaving it orphaned (this OS doesn't clean up child processes of
    // a dead parent on its own).
    if let Some(engine) = ENGINE.get() {
        engine.shutdown();
    }
}
