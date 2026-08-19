//! Window chrome and the three-panel layout.
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │ CA  Code Architect            [chips]        │
//! ├──────────┬───────────────────┬───────────────┤
//! │ Sessions │ Transcript        │ Inspector     │
//! │          ├───────────────────┤               │
//! │          │ Composer          │               │
//! ├──────────┴───────────────────┴───────────────┤
//! │ status bar                                   │
//! └──────────────────────────────────────────────┘
//! ```

use architect_ui::{
    theme,
    widgets::{StatusChip, divider},
};
use freya::components::{Button, PanelSize, ResizableContainer, ResizablePanel};
use freya::prelude::*;

use crate::{
    engine::Engine,
    panels::{
        Composer, InspectorCollapsed, InspectorPanel, SessionsCollapsed, SessionsPanel,
        SettingsPanel, TranscriptPanel,
    },
    state::Transcript,
};

/// Width of a collapsed sidebar — just enough for its re-expand button, not
/// a sizing mode a user could ever reach by dragging (min sizes on the
/// expanded panels stay well above this).
const COLLAPSED_WIDTH: f32 = 32.0;

#[derive(PartialEq)]
pub struct Shell;

impl Component for Shell {
    fn render(&self) -> impl IntoElement {
        // `Shell` owns this — it's the one place that decides panel
        // layout — and provides it down to `SessionsPanel`/`InspectorPanel`
        // so each panel's own header button can flip its own flag, the
        // same newtype-around-`State`-context shape `ScrollToToolCall`
        // already established for a cross-panel signal.
        let mut sessions_collapsed = use_state(|| false);
        use_provide_context(|| SessionsCollapsed(sessions_collapsed));
        let mut inspector_collapsed = use_state(|| false);
        use_provide_context(|| InspectorCollapsed(inspector_collapsed));

        rect()
            .expanded()
            .vertical()
            // `Size::fill` takes everything left at its position, starving later
            // siblings — the status bar would be pushed off-screen. Flex content
            // lets the middle absorb only the leftover.
            .content(Content::Flex)
            .background(theme::BACKGROUND)
            .color(theme::TEXT)
            .font_size(theme::FONT_BODY)
            .child(Header)
            .child(divider())
            .child(
                // Takes whatever the header and status bar leave behind.
                rect().width(Size::fill()).height(Size::flex(1.0)).child(
                    ResizableContainer::new()
                        .direction(Direction::Horizontal)
                        .panel(if *sessions_collapsed.read() {
                            // A distinct `.key` from the expanded variant
                            // below forces a real remount when toggling —
                            // `ResizablePanel`'s own sizing is otherwise
                            // only read once, at mount (`use_hook`), so
                            // just changing `initial_size` on the same
                            // instance would not actually resize it.
                            ResizablePanel::new(PanelSize::px(COLLAPSED_WIDTH))
                                .key("sessions-collapsed")
                                .child(collapsed_strip("\u{203A}", move |_| {
                                    sessions_collapsed.set(false)
                                }))
                        } else {
                            ResizablePanel::new(PanelSize::percent(20.0))
                                .key("sessions-expanded")
                                .child(SessionsPanel)
                        })
                        .panel(
                            ResizablePanel::new(PanelSize::percent(55.0)).child(
                                // Transcript takes the slack; the composer keeps
                                // its natural height at the bottom.
                                rect()
                                    .expanded()
                                    .vertical()
                                    .content(Content::Flex)
                                    .child(
                                        rect()
                                            .width(Size::fill())
                                            .height(Size::flex(1.0))
                                            .child(TranscriptPanel),
                                    )
                                    .child(divider())
                                    .child(Composer),
                            ),
                        )
                        .panel(if *inspector_collapsed.read() {
                            ResizablePanel::new(PanelSize::px(COLLAPSED_WIDTH))
                                .key("inspector-collapsed")
                                .child(collapsed_strip("\u{2039}", move |_| {
                                    inspector_collapsed.set(false)
                                }))
                        } else {
                            ResizablePanel::new(PanelSize::percent(25.0))
                                .key("inspector-expanded")
                                .child(InspectorPanel)
                        }),
                ),
            )
            .child(divider())
            .child(StatusBar)
    }
}

/// A collapsed panel's whole content: one button, centered, that re-expands
/// it. No label — at 32px wide there's room for nothing else. `glyph`
/// points toward the content the panel would reveal — `›` for the left
/// sidebar (expands rightward, into the window), `‹` for the right one
/// (expands leftward).
fn collapsed_strip(
    glyph: &'static str,
    on_expand: impl FnMut(Event<PressEventData>) + 'static,
) -> impl IntoElement {
    rect().expanded().center().background(theme::SURFACE).child(
        Button::new()
            .outline()
            .on_press(on_expand)
            .child(label().text(glyph).font_size(theme::FONT_SMALL)),
    )
}

#[derive(PartialEq)]
struct Header;

impl Component for Header {
    fn render(&self) -> impl IntoElement {
        let engine = consume_context::<Engine>();
        let mut transcript = consume_context::<State<Transcript>>();
        let status = transcript
            .read()
            .active_conversation()
            .map(|conversation| conversation.status)
            .unwrap_or_default();
        let mut show_settings = use_state(|| false);

        rect()
            .width(Size::fill())
            .height(Size::px(theme::HEADER_HEIGHT))
            .horizontal()
            .cross_align(Alignment::Center)
            .main_align(Alignment::SpaceBetween)
            .padding(Gaps::new_symmetric(0.0, theme::SPACE_MD))
            .background(theme::SURFACE)
            .child(
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(10.0)
                    .child(Logo)
                    .child(
                        rect()
                            .vertical()
                            .child(
                                label()
                                    .text("Code Architect")
                                    .font_size(16.0)
                                    .font_weight(FontWeight::BOLD),
                            )
                            .child(
                                label()
                                    .text("SYSTEM DESIGNER")
                                    .color(theme::TEXT_DIM)
                                    .font_size(10.0),
                            ),
                    ),
            )
            .child({
                // A saved, activated configuration overrides the label the
                // engine started with; an ad-hoc LM Studio pick (never
                // saved) overrides it the same way but is checked second,
                // since the two are mutually exclusive and a saved profile
                // is the more deliberate choice when somehow both are set.
                let (model_label, kind_label) = {
                    let transcript = transcript.read();
                    match transcript
                        .active_profile
                        .and_then(|id| transcript.profiles.iter().find(|p| p.id == id))
                    {
                        Some(profile) => (profile.model.clone(), profile.kind.clone()),
                        None => match &transcript.active_adhoc_model {
                            Some(model) => (model.clone(), "openai".to_owned()),
                            None => (
                                engine.config().label().to_owned(),
                                engine.config().kind.clone(),
                            ),
                        },
                    }
                };

                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(theme::SPACE_SM)
                    .child(StatusChip::new(model_label.clone()))
                    // Informational only — never gates the Attach button:
                    // most real usage here is local models this table has
                    // no data for at all, so "absent" means "unknown," not
                    // "confirmed no."
                    .maybe_child(
                        architect_core::pricing::supports_vision(&model_label)
                            .then(|| StatusChip::new("vision").active(true)),
                    )
                    .child(StatusChip::new(kind_label))
                    // Lights up whenever a turn is in flight.
                    .child(StatusChip::new(status.label()).active(status.is_busy()))
                    .child({
                        // Nothing to summarize yet, and a turn already in
                        // flight would just be refused server-side anyway —
                        // disabled here instead of waiting for that `Failed`
                        // round-trip.
                        let can_compact = !status.is_busy()
                            && transcript
                                .read()
                                .active_conversation()
                                .is_some_and(|conversation| !conversation.rows.is_empty());
                        let engine = engine.clone();
                        Button::new()
                            .outline()
                            .enabled(can_compact)
                            .on_press(move |_| {
                                // Not `if let Some(session) = transcript.read()...`:
                                // that keeps the read guard alive for the
                                // whole block (Rust's temporary-scope rules
                                // for an `if let` scrutinee), so the
                                // `.write()` right below it would panic on
                                // a double borrow.
                                let session = transcript.read().active_session;
                                if let Some(session) = session {
                                    transcript.write().start_compacting(session);
                                    engine.compact(session);
                                }
                            })
                            .child("Compact")
                    })
                    .child({
                        let engine = engine.clone();
                        Button::new()
                            .outline()
                            .on_press(move |_| {
                                engine.list_profiles();
                                engine.list_mcp_servers();
                                engine.list_integrations();
                                engine.list_docs_config();
                                show_settings.set(true);
                            })
                            .child("Settings")
                    })
            })
            .child(SettingsPanel {
                show: show_settings,
            })
    }
}

/// The amber initials badge.
#[derive(PartialEq)]
struct Logo;

impl Component for Logo {
    fn render(&self) -> impl IntoElement {
        rect()
            .width(Size::px(32.0))
            .height(Size::px(32.0))
            .center()
            .rounded_lg()
            .background(theme::SURFACE_RAISED)
            .border(Border::new().fill(theme::ACCENT).width(1.0))
            .child(
                label()
                    .text("CA")
                    .color(theme::ACCENT)
                    .font_size(theme::FONT_SMALL)
                    .font_weight(FontWeight::BOLD),
            )
    }
}

/// The directory the harness was launched in — the workspace it will act on
/// once tools exist.
fn workspace() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown workspace".to_owned())
}

#[derive(PartialEq)]
struct StatusBar;

impl Component for StatusBar {
    fn render(&self) -> impl IntoElement {
        let transcript = consume_context::<State<Transcript>>();
        let (usage, cost, context_tokens, context_window) = {
            let transcript = transcript.read();
            match transcript.active_conversation() {
                Some(conversation) => (
                    conversation.usage,
                    conversation.cost,
                    conversation.context_tokens,
                    conversation.context_window,
                ),
                None => Default::default(),
            }
        };

        let tokens = format!("{} in / {} out", usage.input_tokens, usage.output_tokens);
        // Local models have no published price, so no figure is shown rather
        // than a misleading $0.00.
        let spend = match cost {
            Some(cost) => format!("${:.4}", cost.total),
            None => "unpriced model".to_owned(),
        };
        // Same "unknown, don't guess" treatment as `spend` above: a local
        // model's context window isn't published anywhere `context_window_
        // for` could read it from, so a percentage of an unknown ceiling
        // isn't shown either.
        let context = match context_window {
            Some(window) => {
                let percent = (context_tokens as f64 / window.max(1) as f64 * 100.0).round();
                format!("{context_tokens} / {window} context ({percent:.0}%)")
            }
            None => "unsized model".to_owned(),
        };

        rect()
            .width(Size::fill())
            .height(Size::px(theme::STATUS_HEIGHT))
            .horizontal()
            .cross_align(Alignment::Center)
            .main_align(Alignment::SpaceBetween)
            .padding(Gaps::new_symmetric(0.0, theme::SPACE_MD))
            .background(theme::SURFACE)
            .color(theme::TEXT_DIM)
            .font_size(theme::FONT_SMALL)
            .child(label().text(workspace()))
            .child(
                rect()
                    .horizontal()
                    .spacing(theme::SPACE_MD)
                    .child(label().text(tokens))
                    .child(label().text(context))
                    .child(label().text(spend)),
            )
    }
}

#[cfg(test)]
mod tests {
    use architect_core::{
        FileChange, FileChangeEntry, Plan, PlanStep, PlanSubstep, StepStatus, ToolCall, ToolResult,
    };
    use architect_llm::StreamEvent;
    use architect_tools::ProcessStatus;
    use freya::components::use_init_theme;
    use freya_testing::TestingRunner;
    use freya_testing::prelude::{Code, KeyboardEventName, PlatformEvent};
    use serde_json::json;

    use super::*;
    use crate::{
        engine::{Attachment, EngineConfig, EngineEvent},
        panels::ScrollToToolCall,
        state::{BrowserSummary, ProcessSummary, Transcript},
    };

    /// Render the shell headlessly and write a PNG for review.
    ///
    /// Layout bugs — a panel eating the space its siblings needed — are
    /// invisible to the compiler and to unit tests on state. These render the
    /// real tree at a real window size; the test fails on a layout panic, and
    /// the artifact is there to be looked at.
    #[test]
    fn renders_the_empty_shell() {
        let (mut test, ()) =
            TestingRunner::new(crate::app::app, (1440.0, 900.0).into(), |_| (), 1.0);

        test.sync_and_update();
        test.render_to_file("/tmp/architect-shell.png");
    }

    /// A message longer than one line wraps and grows the composer instead
    /// of scrolling sideways out of view — the box has no vertical growth at
    /// all with a stock `freya::components::Input` (hardcoded to one line),
    /// which is what `ComposerInput` in `panels::composer` replaces it with.
    #[test]
    fn long_composer_text_wraps_instead_of_scrolling_sideways() {
        let (mut test, ()) =
            TestingRunner::new(crate::app::app, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // Inside the composer's message box — see `/tmp/architect-shell.png`
        // for the coordinates this assumes.
        test.click_cursor((687.0, 843.0));
        test.sync_and_update();
        test.write_text(
            "this is a very long message with lots of words that should not fit on one line \
             and keeps going and going and going and going and going and going and going \
             and going and going and going and going and going and going and going and going \
             and going and going and going and going and going and going and going and going \
             and going and going and going and going and going and going and going and going",
        );
        test.sync_and_update();
        test.render_to_file("/tmp/architect-shell-composer-long-text.png");
    }

    /// Enter still sends the message — `ComposerInput` replaced
    /// `freya::components::Input`, but the composer's Enter-to-send
    /// behavior has to keep working. Verified visually: the box goes back
    /// to showing its placeholder (proof the draft was cleared, which only
    /// happens after a real `submit()`), and a new "ping" bubble appears at
    /// the bottom of the transcript.
    #[test]
    fn pressing_enter_sends_the_message() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // Inside the composer's message box — see
        // `renders_a_populated_transcript`'s PNG for the coordinates this
        // assumes (the composer's own size doesn't depend on transcript
        // content, so these match the empty shell's too).
        test.click_cursor((687.0, 843.0));
        test.sync_and_update();
        test.write_text("ping");
        test.sync_and_update();
        test.press_key(Key::Named(NamedKey::Enter));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-composer-enter-sends.png");
    }

    /// Shift+Enter inserts a newline instead of sending — the one behavior
    /// `ComposerInput` had to add on top of `Input`'s original single-line
    /// Enter-always-sends handling, now that the box supports multiple
    /// lines at all. Verified visually: both lines are still sitting in the
    /// box (not cleared), and the transcript hasn't grown a new bubble.
    #[test]
    fn shift_enter_inserts_a_newline_without_sending() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        test.click_cursor((687.0, 843.0));
        test.sync_and_update();
        test.write_text("line one");
        test.sync_and_update();
        test.send_event(PlatformEvent::Keyboard {
            name: KeyboardEventName::KeyDown,
            key: Key::Named(NamedKey::Enter),
            code: Code::Enter,
            modifiers: Modifiers::SHIFT,
        });
        test.sync_and_update();
        test.write_text("line two");
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-composer-shift-enter.png");
    }

    /// Clicking the Sessions header's collapse button shrinks it to a
    /// narrow strip with just a re-expand button — no session list, no
    /// "+ New Chat".
    #[test]
    fn collapsing_the_sessions_panel_shrinks_it_to_a_strip() {
        let (mut test, ()) =
            TestingRunner::new(crate::app::app, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // The "‹" button in the Sessions header — see
        // `/tmp/architect-shell.png` for the coordinates this assumes.
        test.click_cursor((259.0, 128.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-sessions-collapsed.png");
    }

    /// Clicking a collapsed Sessions panel's own button expands it back —
    /// round-tripping through both states with the same panel identity.
    #[test]
    fn expanding_a_collapsed_sessions_panel_restores_it() {
        let (mut test, ()) =
            TestingRunner::new(crate::app::app, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();
        test.click_cursor((259.0, 128.0)); // collapse
        test.sync_and_update();

        // The collapsed strip's own button — centered in its 32px-wide
        // column, vertically centered in the content area (900 window,
        // minus the 57px header and 27px status bar, halved: 465).
        test.click_cursor((16.0, 465.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-sessions-re-expanded.png");
    }

    /// The Inspector panel's own collapse/expand round trip.
    #[test]
    fn collapsing_and_expanding_the_inspector_panel() {
        let (mut test, ()) =
            TestingRunner::new(crate::app::app, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // The "›" button in the Inspector header — see
        // `/tmp/architect-shell.png` for the coordinates this assumes.
        test.click_cursor((1413.0, 75.0));
        test.sync_and_update();
        test.render_to_file("/tmp/architect-shell-inspector-collapsed.png");

        test.click_cursor((1424.0, 465.0)); // the collapsed strip's own button
        test.sync_and_update();
        test.render_to_file("/tmp/architect-shell-inspector-re-expanded.png");
    }

    /// The same, with one of every row kind — the states a live turn produces.
    #[test]
    fn renders_a_populated_transcript() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);

        test.sync_and_update();
        test.render_to_file("/tmp/architect-shell-populated.png");
    }

    /// Ctrl+A inside a message bubble selects its text — exercises Freya's
    /// own built-in `SelectableText` select-all path end to end through the
    /// real event system (click to focus, then a real `Modifiers::CONTROL`
    /// keydown), not app-specific logic, since this app only swapped which
    /// text primitive renders the bubble. Verified visually: `SelectableText`
    /// paints a highlight over whatever is selected, so a screenshot after
    /// Ctrl+A is a direct check that selection actually happened, not just
    /// that the keydown didn't panic.
    ///
    /// Copying (Ctrl+C) is not asserted here: `Clipboard::get()` needs to
    /// run from inside Freya's own reactive context (`consume_root_context`
    /// panics called from a bare `#[test]` body), which this headless
    /// harness has no supported way to reach from outside the component
    /// tree — the same "OS clipboard, only reachable from within Freya's
    /// context" observation the plan for this feature flagged as a risk
    /// going in.
    #[test]
    fn selecting_message_text_highlights_it() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // Inside the assistant's "Summary" bubble — see
        // `renders_a_populated_transcript`'s PNG for the coordinates this
        // assumes. A click both focuses the `SelectableText` and is what
        // select-all needs to be routed to it.
        test.click_cursor((500.0, 450.0));
        test.sync_and_update();

        test.send_event(PlatformEvent::Keyboard {
            name: KeyboardEventName::KeyDown,
            key: Key::Character("a".into()),
            code: Code::Unidentified,
            modifiers: Modifiers::ctrl_or_meta(),
        });
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-text-selected.png");
    }

    /// The status bar's context-window figure for a known, sized model —
    /// proof the percentage/format string itself reads sensibly, not just
    /// that the underlying fields get set correctly (already covered by
    /// `state::tests::drops_the_empty_assistant_row_when_a_turn_ends`).
    #[test]
    fn renders_the_context_window_used() {
        let (mut test, ()) =
            TestingRunner::new(with_context_usage, (1440.0, 900.0).into(), |_| (), 1.0);

        test.sync_and_update();
        test.render_to_file("/tmp/architect-shell-context-usage.png");
    }

    /// A compacted conversation's whole transcript is the one summary
    /// row — the old rows must actually be gone from what's rendered, not
    /// just from `Conversation.rows` in isolation (already covered by
    /// `state::tests::compacted_replaces_rows_with_a_summary_and_resets_
    /// context`).
    #[test]
    fn renders_a_compacted_conversation() {
        let (mut test, ()) = TestingRunner::new(compacted, (1440.0, 900.0).into(), |_| (), 1.0);

        test.sync_and_update();
        test.render_to_file("/tmp/architect-shell-compacted.png");
    }

    /// Clicking "Compact" flips the session to `Compacting` immediately,
    /// before any engine round-trip — the same optimistic-update shape
    /// `push_user` already gets for `Waiting`. No live model needed: this
    /// only proves the click reaches `Transcript::start_compacting`, not
    /// that a real summary comes back.
    #[test]
    fn clicking_compact_shows_the_compacting_status() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // The "Compact" button, top-right of the header, just left of
        // "Settings" — see `renders_a_populated_transcript`'s PNG for the
        // coordinates this assumes.
        test.click_cursor((891.0, 28.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-compacting.png");
    }

    /// Clicking a collapsed tool row's header must actually reveal its
    /// output — a compiler can confirm `on_press` is wired to *something*,
    /// not that it flips the right piece of state.
    #[test]
    fn clicking_a_tool_row_expands_its_output() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // The `read_file` row's header, from the same seeded transcript
        // `renders_a_populated_transcript` renders — see that PNG for the
        // coordinates this assumes.
        test.click_cursor((400.0, 215.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-tool-expanded.png");
    }

    /// Clicking a call in the Inspector's Tools tab must scroll the
    /// Transcript to it and reveal its output — not just compile against
    /// `on_press`. Uses a transcript long enough that the target call
    /// starts off-screen, so the "scroll" half of this is actually
    /// exercised, not just the "expand" half `clicking_a_tool_row_
    /// expands_its_output` already covers.
    #[test]
    fn clicking_a_tool_in_the_inspector_scrolls_the_transcript_to_it() {
        let (mut test, ()) = TestingRunner::new(
            populated_with_many_tool_calls,
            (1440.0, 900.0).into(),
            |_| (),
            1.0,
        );
        test.sync_and_update();

        // The Inspector's "Tools" tab segment — see
        // `/tmp/architect-inspector-tools.png` for the layout this assumes.
        test.click_cursor((1192.0, 107.0));
        test.sync_and_update();
        test.render_to_file("/tmp/architect-shell-many-tools-before.png");

        // The 18th call (`file_17`) — still visible in the Inspector's own
        // list without scrolling it, but off-screen in the Transcript at
        // the start of this test (which only shows up to `file_15`,
        // partially, per the "before" PNG above).
        test.click_cursor((1260.0, 816.0));
        test.sync_and_update();
        // The scroll-into-view is driven by an accessibility focus request,
        // handled asynchronously relative to the click itself — give it a
        // moment to settle before rendering, the same way `settings.rs`'s
        // popup-open tests already do for its own animated state changes.
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );

        test.render_to_file("/tmp/architect-shell-scrolled-to-tool.png");
    }

    /// The gear button must actually open the settings popup with the
    /// engine's real profile list, not just compile against `on_press`.
    #[test]
    fn clicking_settings_opens_the_configuration_panel() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // The "Settings" button, top-right of the header — see
        // `renders_a_populated_transcript`'s PNG for the coordinates.
        test.click_cursor((974.0, 28.0));
        test.sync_and_update();
        // The popup animates open; let it settle before rendering.
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );

        test.render_to_file("/tmp/architect-shell-settings.png");
    }

    /// The add/edit form — a distinct render path from the list view above,
    /// with its own layout (labeled inputs, a provider dropdown).
    #[test]
    fn opening_the_add_configuration_form_renders_its_fields() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        test.click_cursor((951.0, 591.0)); // + Add, from the settings PNG above
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-settings-form.png");
    }

    /// Clicking "Fetch models" with an empty Base URL field must fill that
    /// field in with the same default it just queried — regression test
    /// for a real bug: the button used to resolve an empty field to
    /// `http://localhost:1234/v1` for the fetch itself but never wrote
    /// that back, so "Use" on a discovered model would activate it against
    /// no `base_url` at all. `ProviderRegistry` treats a missing
    /// `base_url` for `kind: "openai"` as "use the real OpenAI API," so
    /// that activation silently talked to api.openai.com instead of the
    /// local server it was actually fetched from — a 401 "no API key" the
    /// first time anyone tried to use it.
    #[test]
    fn fetching_models_fills_in_the_base_url_it_assumed() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        test.click_cursor((652.0, 308.0)); // LM Studio tab
        test.sync_and_update();

        // "Fetch models", Base URL left empty — see
        // `/tmp/architect-shell-lmstudio.png` for the coordinates this
        // assumes.
        test.click_cursor((515.0, 522.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-fetch-fills-base-url.png");
    }

    /// The discovered-models list on the LM Studio page — proof
    /// `Transcript.discovered_models` actually reaches that page and
    /// renders as rows with "Use" buttons, not just that the field
    /// exists. The fetch itself (`Command::ListModels` →
    /// `EngineEvent::ModelsListed`) is covered hermetically in
    /// `engine.rs`'s own tests, against a mocked server; this only needs
    /// to prove the UI wiring once the list has arrived. Clicking "Use"
    /// itself (`Command::UseAdHocModel`) is a fire-and-forget engine
    /// command this headless harness never pumps a response back for (no
    /// test here does), so this only proves the click doesn't panic —
    /// `renders_the_active_adhoc_model` below covers what the page looks
    /// like once that response *has* arrived.
    #[test]
    fn renders_the_discovered_models_list() {
        let (mut test, ()) =
            TestingRunner::new(with_discovered_models, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        test.click_cursor((652.0, 308.0)); // LM Studio tab
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-discovered-models.png");

        // "gpt-oss-20b"'s "Use" button, the second discovered-model row —
        // see the PNG above for the coordinates this assumes. The popup
        // grows (and re-centers) once the discovered-models list has
        // rows to show, so this is taller/higher than the empty-list
        // layout the other LM Studio tests assume.
        test.click_cursor((945.0, 491.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-model-picked.png");
    }

    /// A `Row::User` carrying an attached image renders it above the text
    /// bubble — proof `state::Row::User.images` actually reaches the
    /// transcript, not just that the field exists.
    #[test]
    fn renders_a_message_with_an_attached_image() {
        let (mut test, ()) =
            TestingRunner::new(with_an_image_attached, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();
        // The image decodes asynchronously (`ImageViewer` spawns a decode
        // task); give it a moment to settle before rendering, the same way
        // `settings.rs`'s popup-open tests already do for their own
        // animated state changes.
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );

        test.render_to_file("/tmp/architect-shell-image-attached.png");
    }

    /// The header's "vision" badge appears for a known vision-capable
    /// model and is absent for the default local-model fixture — informational
    /// only, never gating the Attach button (already proven by the
    /// discovered-models tests above, none of which needed a vision-capable
    /// model to attach anything).
    #[test]
    fn renders_the_vision_badge_for_a_known_model() {
        let (mut test, ()) = TestingRunner::new(
            with_a_vision_capable_model,
            (1440.0, 900.0).into(),
            |_| (),
            1.0,
        );
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-vision-badge.png");
    }

    /// Once `Transcript.active_adhoc_model` names one of the discovered
    /// models (what a completed `Command::UseAdHocModel` round-trip
    /// leaves behind — `renders_the_discovered_models_list` above can
    /// only click "Use", never see its effect, since this harness never
    /// pumps engine events back in), that row shows "Active" instead of
    /// a "Use" button, and the header reflects it — same as a saved
    /// profile's "Active" state, but for a model that was never saved as
    /// one.
    #[test]
    fn renders_the_active_adhoc_model() {
        let (mut test, ()) =
            TestingRunner::new(with_active_adhoc_model, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        test.click_cursor((652.0, 308.0)); // LM Studio tab
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-adhoc-active.png");
    }

    /// The user's explicit requirement for this page: a model list too
    /// long to fit must scroll inside the popup, not grow the popup off
    /// the bottom of the screen. Proven by seeding far more discovered
    /// models than the fixed-height list can show at once and confirming
    /// the popup (and its "Close" button) still fit on screen.
    #[test]
    fn many_discovered_models_scroll_instead_of_growing_the_popup() {
        let (mut test, ()) = TestingRunner::new(
            with_many_discovered_models,
            (1440.0, 900.0).into(),
            |_| (),
            1.0,
        );
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        test.click_cursor((652.0, 308.0)); // LM Studio tab
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-adhoc-scroll.png");
    }

    /// The MCP servers tab — a distinct section from the profiles list
    /// above, with its own enabled/disabled rows.
    #[test]
    fn switching_to_the_mcp_servers_tab_renders_the_saved_servers() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        // The "MCP servers" tab, third of four atop the popup — see
        // `/tmp/architect-shell-settings.png` for the layout this assumes.
        test.click_cursor((758.0, 308.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-mcp-servers.png");
    }

    /// The MCP server add/edit form — args and env are plain `Input`s that
    /// get parsed back into `Vec`/`HashMap` on save.
    #[test]
    fn opening_the_add_mcp_server_form_renders_its_fields() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        test.click_cursor((758.0, 308.0)); // MCP servers tab
        test.sync_and_update();
        test.click_cursor((951.0, 560.0)); // + Add, from the MCP servers PNG above
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-mcp-server-form.png");
    }

    /// Switching the MCP server form's transport selector to "Remote server
    /// (HTTP)" must swap which fields are shown — Command/Arguments/
    /// Environment (stdio) for URL/Bearer token (http), not both or
    /// neither.
    #[test]
    fn switching_mcp_transport_swaps_the_visible_fields() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        test.click_cursor((758.0, 308.0)); // MCP servers tab
        test.sync_and_update();
        test.click_cursor((951.0, 560.0)); // + Add
        test.sync_and_update();

        // The "Transport" dropdown — see
        // `/tmp/architect-shell-mcp-server-form.png` for the layout this
        // assumes.
        test.click_cursor((604.0, 392.0));
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        test.render_to_file("/tmp/architect-shell-mcp-transport-open.png");

        // "Remote server (HTTP)", from the dropdown PNG above.
        test.click_cursor((590.0, 464.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-mcp-transport-http.png");
    }

    /// The Integrations tab — no list, no add/edit `Mode`, just the flat
    /// GitHub/Slack/Linear credential form, always pre-loaded with whatever
    /// is currently saved.
    #[test]
    fn switching_to_the_integrations_tab_renders_the_credential_form() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        // The "Integrations" tab, last of four atop the popup — see
        // `/tmp/architect-shell-settings.png` for the layout this assumes.
        test.click_cursor((911.0, 308.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-integrations.png");
    }

    /// Clicking "Login" must relabel that provider's button immediately —
    /// a compiler can confirm `on_press` is wired to *something*, not that
    /// it flips the right piece of state (and not some other provider's).
    #[test]
    fn clicking_login_shows_a_waiting_state_for_that_provider_only() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        test.click_cursor((911.0, 308.0)); // Integrations tab
        test.sync_and_update();

        // The GitHub row's "Login" button — see
        // `/tmp/architect-shell-integrations.png` for the coordinates this
        // assumes.
        test.click_cursor((951.0, 420.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-integrations-pending.png");
    }

    /// A provider with a saved token — whether it arrived via Login or a
    /// manual paste+Save — shows a checkmark in place of its Login button;
    /// nothing left to click once already connected.
    #[test]
    fn a_connected_provider_shows_a_checkmark_instead_of_login() {
        let (mut test, ()) = TestingRunner::new(
            populated_with_a_connected_integration,
            (1440.0, 900.0).into(),
            |_| (),
            1.0,
        );
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        test.click_cursor((911.0, 308.0)); // Integrations tab
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-integrations-connected.png");
    }

    /// Toggling "Use gh CLI instead of a token" swaps the GitHub token
    /// input + Login button out for a plain status line — nothing left to
    /// paste or click while gh-CLI mode is on.
    #[test]
    fn toggling_gh_cli_mode_hides_the_github_token_field() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        test.click_cursor((974.0, 28.0)); // Settings
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );
        test.click_cursor((911.0, 308.0)); // Integrations tab
        test.sync_and_update();

        // "Use gh CLI instead of a token" — see
        // `/tmp/architect-shell-integrations.png` for the coordinates this
        // assumes.
        test.click_cursor((571.0, 460.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-shell-integrations-gh-cli.png");
    }

    /// The sidebar's real session list, with a mix of active/inactive and a
    /// title long enough to need truncation — the states
    /// `renders_a_populated_transcript`'s seed doesn't otherwise exercise.
    #[test]
    fn renders_the_session_list() {
        let (mut test, ()) =
            TestingRunner::new(populated_with_sessions, (1440.0, 900.0).into(), |_| (), 1.0);

        test.sync_and_update();
        test.render_to_file("/tmp/architect-shell-sessions.png");
    }

    /// Two sub-agent sessions, spawned by `spawn_subagents`, render nested
    /// directly beneath their parent — indented, with a connector glyph, in
    /// spawn order — rather than mixed into the top-level list.
    #[test]
    fn renders_nested_subagent_sessions_under_their_parent() {
        let (mut test, ()) = TestingRunner::new(
            populated_with_nested_subagent_sessions,
            (1440.0, 900.0).into(),
            |_| (),
            1.0,
        );

        test.sync_and_update();
        test.render_to_file("/tmp/architect-shell-nested-sessions.png");
    }

    /// A session other than the one on screen, still busy — the sidebar's
    /// visible proof a background turn is still running while the user
    /// looks at something else. `renders_the_session_list`'s seed is always
    /// idle (its turn already failed), so this is a distinct render path.
    #[test]
    fn renders_a_busy_background_session_in_the_sidebar() {
        let (mut test, ()) = TestingRunner::new(
            populated_with_a_busy_background_session,
            (1440.0, 900.0).into(),
            |_| (),
            1.0,
        );

        test.sync_and_update();
        test.render_to_file("/tmp/architect-shell-busy-session.png");
    }

    /// The Inspector's Files tab is selected by default and lists every
    /// path `seeded()` touched, each tagged with the tool that touched it.
    #[test]
    fn renders_the_inspector_files_tab() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);

        test.sync_and_update();
        test.render_to_file("/tmp/architect-inspector-files.png");
    }

    /// Switching to the Tools tab shows a compact, read-only overview of
    /// every tool call — a distinct render path from the same rows'
    /// collapsible detail view in the transcript.
    #[test]
    fn renders_the_inspector_tools_tab() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // The "Tools" segment, in the Inspector's tab bar — see
        // `/tmp/architect-inspector-files.png` for the layout this assumes.
        test.click_cursor((1192.0, 107.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-inspector-tools.png");
    }

    /// The Processes tab: a running process and a finished one, each with
    /// a distinct status glyph, and no collapsed detail until clicked.
    #[test]
    fn renders_the_inspector_processes_tab() {
        let (mut test, ()) = TestingRunner::new(
            populated_with_processes,
            (1440.0, 900.0).into(),
            |_| (),
            1.0,
        );
        test.sync_and_update();

        // The "Processes" segment, last in the Inspector's tab bar — see
        // `/tmp/architect-inspector-files.png` for the layout this assumes.
        test.click_cursor((1332.0, 107.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-inspector-processes.png");
    }

    /// Clicking a process row expands it in place to show its accumulated
    /// log — no tab switch, unlike Files→Diff.
    #[test]
    fn clicking_a_process_row_expands_its_log() {
        let (mut test, ()) = TestingRunner::new(
            populated_with_processes,
            (1440.0, 900.0).into(),
            |_| (),
            1.0,
        );
        test.sync_and_update();

        test.click_cursor((1332.0, 107.0)); // Processes tab
        test.sync_and_update();

        // The first process row — see `/tmp/architect-inspector-processes.png`
        // for the layout this assumes.
        test.click_cursor((1200.0, 145.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-inspector-processes-expanded.png");
    }

    /// The Inspector's tab bar with all six tabs — render it to a PNG in the
    /// workspace so the Browser tab's click coordinate can be read off it.
    #[test]
    fn renders_the_inspector_tabbar_with_six_tabs() {
        let (mut test, ()) = TestingRunner::new(
            populated_with_a_browser,
            (1440.0, 900.0).into(),
            |_| (),
            1.0,
        );
        test.sync_and_update();
        test.render_to_file("tabbar-six.png");

        // Scroll the tab bar right (wheel delta-x negative) to reveal the
        // last tabs.
        test.scroll((1300.0, 107.0), (-400.0, 0.0));
        test.render_to_file("tabbar-six-scrolled.png");
    }

    /// The Plan tab: a goal, a completed step with a completed sub-step,
    /// and an in-progress step with a pending sub-step — one of each
    /// status glyph.
    #[test]
    fn renders_the_inspector_plan_tab() {
        let (mut test, ()) =
            TestingRunner::new(populated_with_a_plan, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // The "Plan" segment, last in the Inspector's tab bar — see
        // `/tmp/architect-inspector-plan.png` for the layout this assumes.
        test.click_cursor((1413.0, 107.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-inspector-plan.png");
    }

    /// Clicking a file in the Files tab must both select it and switch to
    /// the Diff tab in the same action — a diff is the whole reason to pick
    /// a file, not a second click away from it.
    #[test]
    fn clicking_a_file_switches_to_its_diff() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // The edited file's row — see
        // `/tmp/architect-inspector-files.png` for the coordinates this
        // assumes.
        test.click_cursor((1200.0, 155.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-inspector-diff.png");
    }

    /// Pressing a file's "Roll back" button must not also select that row
    /// (which would otherwise fire from the same click) — and the
    /// confirmation dialog's copy must actually say what it undoes, since
    /// this is a destructive, filesystem-wide action.
    #[test]
    fn rolling_back_a_file_shows_a_confirmation_first() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // `NOTES.md`'s "Roll back" button — see
        // `/tmp/architect-inspector-files.png` for the coordinates this
        // assumes.
        test.click_cursor((1373.0, 216.0));
        test.sync_and_update();
        test.poll(
            std::time::Duration::from_millis(10),
            std::time::Duration::from_millis(500),
        );

        test.render_to_file("/tmp/architect-inspector-rollback-confirm.png");
    }

    /// `crates/architect-tools/src/registry.rs`'s `.rs` extension resolves
    /// to a real tree-sitter grammar — its diff must show per-token syntax
    /// color (not just the flat insert/delete color every diff line got
    /// before this), layered with the insert/delete row background.
    #[test]
    fn renders_syntax_highlighted_diff_for_a_recognized_language() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // `crates/architect-tools/src/...`'s row — see
        // `/tmp/architect-inspector-files.png` for the coordinates this
        // assumes.
        test.click_cursor((1150.0, 161.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-inspector-diff-highlighted.png");
    }

    /// `NOTES.md` has no grammar in this pass's small language set — its
    /// diff must still render (plain, diff-colored text), not panic or show
    /// nothing.
    #[test]
    fn diff_falls_back_to_plain_text_for_an_unrecognized_extension() {
        let (mut test, ()) = TestingRunner::new(populated, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();

        // `NOTES.md`'s row — see `/tmp/architect-inspector-files.png` for
        // the coordinates this assumes.
        test.click_cursor((1150.0, 216.0));
        test.sync_and_update();

        test.render_to_file("/tmp/architect-inspector-diff-plain-fallback.png");
    }

    fn populated_with_a_busy_background_session() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            let foreground = transcript.active_session.unwrap();
            let background = transcript.start_new_chat();
            transcript.apply(&EngineEvent::Agent {
                session: background,
                event: architect_agent::AgentEvent::IterationStarted { iteration: 0 },
            });
            transcript.switch_to(foreground);

            transcript.sessions = vec![
                architect_session::SessionSummary {
                    id: foreground,
                    title: "Read the tool registry and tell me what it does.".into(),
                    provider_kind: "openai".into(),
                    model: "qwen/qwen3.8-27b".into(),
                    parent_id: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                },
                architect_session::SessionSummary {
                    id: background,
                    title: "Refactor the session store".into(),
                    provider_kind: "openai".into(),
                    model: "qwen/qwen3.8-27b".into(),
                    parent_id: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                },
            ];
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn populated_with_sessions() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            // The sidebar's "active" row must be the one `seeded()` actually
            // populated — its id is minted fresh each run (`Transcript::
            // default` mints one), not a fixed literal like the other two.
            let active = transcript.active_session.unwrap();
            transcript.sessions = vec![
                architect_session::SessionSummary {
                    id: active,
                    title: "Read the tool registry and tell me what it does — and also explain \
                            the whole agent loop"
                        .into(),
                    provider_kind: "openai".into(),
                    model: "qwen/qwen3.8-27b".into(),
                    parent_id: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                },
                architect_session::SessionSummary {
                    id: "00000000-0000-0000-0000-000000000002".parse().unwrap(),
                    title: "Fix the layout overlap bug".into(),
                    provider_kind: "openai".into(),
                    model: "qwen/qwen3.8-27b".into(),
                    parent_id: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                },
                architect_session::SessionSummary {
                    id: "00000000-0000-0000-0000-000000000003".parse().unwrap(),
                    title: String::new(),
                    provider_kind: "anthropic".into(),
                    model: "claude-opus-5".into(),
                    parent_id: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                },
            ];
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    /// Two sub-agent sessions spawned by `seeded()`'s own session — the
    /// sidebar's nested-child rendering, `session_row`'s `nested` styling
    /// and the connector glyph it adds.
    fn populated_with_nested_subagent_sessions() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            let parent = transcript.active_session.unwrap();
            transcript.sessions = vec![
                architect_session::SessionSummary {
                    id: parent,
                    title: "Investigate every crate in the workspace".into(),
                    provider_kind: "openai".into(),
                    model: "qwen/qwen3.8-27b".into(),
                    parent_id: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                },
                architect_session::SessionSummary {
                    id: "00000000-0000-0000-0000-000000000010".parse().unwrap(),
                    title: "Look at the docs and summarize".into(),
                    provider_kind: "openai".into(),
                    model: "qwen/qwen3.8-27b".into(),
                    parent_id: Some(parent),
                    created_at: "2026-01-01T00:00:00Z".into(),
                    updated_at: String::new(),
                },
                architect_session::SessionSummary {
                    id: "00000000-0000-0000-0000-000000000011".parse().unwrap(),
                    title: "Investigate architect-tools".into(),
                    provider_kind: "openai".into(),
                    model: "qwen/qwen3.8-27b".into(),
                    parent_id: Some(parent),
                    created_at: "2026-01-01T00:01:00Z".into(),
                    updated_at: String::new(),
                },
                architect_session::SessionSummary {
                    id: "00000000-0000-0000-0000-000000000012".parse().unwrap(),
                    title: "An unrelated chat".into(),
                    provider_kind: "openai".into(),
                    model: "qwen/qwen3.8-27b".into(),
                    parent_id: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                },
            ];
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn populated() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(seeded);
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn compacted() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            transcript.apply(&EngineEvent::Compacted {
                session: transcript.active_session.unwrap(),
                summary: "Added a login form (see login.rs) using the existing session \
                          middleware; styling and validation still need to be wired up."
                    .into(),
            });
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn with_context_usage() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            let session = transcript.active_session.unwrap();
            let conversation = transcript.conversations.get_mut(&session).unwrap();
            conversation.context_tokens = 148_234;
            conversation.context_window = Some(200_000);
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn with_discovered_models() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            transcript.discovered_models = vec![
                "qwen/qwen3.8-27b".into(),
                "gpt-oss-20b".into(),
                "deepseek-v4-flash".into(),
            ];
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn with_active_adhoc_model() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            transcript.discovered_models = vec![
                "qwen/qwen3.8-27b".into(),
                "gpt-oss-20b".into(),
                "deepseek-v4-flash".into(),
            ];
            // Mutually exclusive with `active_profile` — an ad-hoc pick
            // supersedes whatever saved profile `seeded()` activated by
            // default.
            transcript.active_profile = None;
            transcript.active_adhoc_model = Some("gpt-oss-20b".into());
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn with_many_discovered_models() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            transcript.discovered_models = (0..30).map(|i| format!("local-model-{i:02}")).collect();
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    /// A valid, minimal 1x1 red PNG — real bytes, not a placeholder, so
    /// `ImageViewer` actually decodes and renders something rather than
    /// falling into its error state.
    const TEST_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn with_an_image_attached() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            let session = transcript.active_session.unwrap();
            transcript
                .conversations
                .get_mut(&session)
                .unwrap()
                .rows
                .push(crate::state::Row::User {
                    text: "what is in this photo?".into(),
                    images: vec![Attachment {
                        media_type: "image/png".into(),
                        bytes: TEST_PNG.to_vec(),
                    }],
                });
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn with_a_vision_capable_model() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            // The header shows the *active saved profile*'s model over
            // whatever the engine started with, once one exists — `seeded()`
            // already activates its local LM Studio profile by default, so
            // switch to its saved Anthropic one instead, a known
            // vision-capable model.
            let anthropic = transcript
                .profiles
                .iter()
                .find(|profile| profile.kind == "anthropic")
                .expect("seeded() saves an anthropic profile")
                .id;
            transcript.active_profile = Some(anthropic);
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn populated_with_a_connected_integration() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            transcript.integrations.github_token = Some("gho_already_connected".into());
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn populated_with_processes() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            transcript.processes.push(ProcessSummary {
                id: "proc-1".into(),
                command: "npm run dev".into(),
                status: ProcessStatus::Running,
                log: "listening on :3000".into(),
            });
            transcript.processes.push(ProcessSummary {
                id: "proc-2".into(),
                command: "echo hello".into(),
                status: ProcessStatus::Exited(0),
                log: "hello".into(),
            });
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    /// A live browser: driver up, session open on a page — the populated
    /// state `renders_the_inspector_browser_tab` shows.
    fn populated_with_a_browser() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            transcript.browser = Some(BrowserSummary {
                driver_running: true,
                has_session: true,
                port: Some(39347),
                url: Some("https://example.com/".into()),
                title: Some("Example Domain".into()),
                last_error: None,
            });
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn populated_with_a_plan() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(|| {
            let mut transcript = seeded();
            let session = transcript.active_session.unwrap();
            transcript.conversations.get_mut(&session).unwrap().plan = Some(Plan {
                goal: Some("Add dark mode support".into()),
                steps: vec![
                    PlanStep {
                        description: "Set up the theme context".into(),
                        status: StepStatus::Completed,
                        substeps: vec![PlanSubstep {
                            description: "Add a ThemeMode enum".into(),
                            status: StepStatus::Completed,
                        }],
                    },
                    PlanStep {
                        description: "Implement the toggle".into(),
                        status: StepStatus::InProgress,
                        substeps: vec![PlanSubstep {
                            description: "Persist the choice".into(),
                            status: StepStatus::Pending,
                        }],
                    },
                ],
            });
            transcript
        });
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    fn populated_with_many_tool_calls() -> impl IntoElement {
        use_init_theme(architect_ui::theme::dark);

        let transcript = use_state(many_tool_calls);
        use_provide_context(|| transcript);
        use_provide_context(|| Engine::start(EngineConfig::default()));
        use_provide_context(|| ScrollToToolCall(use_state(|| None::<String>)));

        Shell
    }

    /// Enough tool calls that the last one starts off-screen in the
    /// Transcript — what `clicking_a_tool_in_the_inspector_scrolls_the_
    /// transcript_to_it` needs to actually exercise scrolling, not just
    /// expansion.
    fn many_tool_calls() -> Transcript {
        let mut transcript = Transcript::default();
        let session = transcript.active_session.unwrap();
        let agent = |event: architect_agent::AgentEvent| EngineEvent::Agent { session, event };

        transcript.apply(&agent(architect_agent::AgentEvent::IterationStarted {
            iteration: 0,
        }));
        for index in 0..20 {
            let id = format!("call_{index}");
            transcript.apply(&agent(architect_agent::AgentEvent::Stream(
                StreamEvent::ToolCallStart {
                    index,
                    id: id.clone(),
                    name: "read_file".into(),
                },
            )));
            transcript.apply(&agent(architect_agent::AgentEvent::Stream(
                StreamEvent::ToolCallEnd {
                    index,
                    call: ToolCall {
                        id: id.clone(),
                        name: "read_file".into(),
                        input: json!({"path": format!("src/file_{index}.rs")}),
                    },
                },
            )));
            transcript.apply(&agent(architect_agent::AgentEvent::ToolFinished {
                result: ToolResult::ok(id, format!("contents of file {index}")),
            }));
        }

        transcript
    }

    /// A transcript holding every row kind, including text long enough to wrap.
    fn seeded() -> Transcript {
        let mut transcript = Transcript::default();
        let session = transcript.active_session.unwrap();
        let agent = |event: architect_agent::AgentEvent| EngineEvent::Agent { session, event };

        transcript.push_user(
            session,
            "Read the tool registry and tell me what it does.",
            Vec::new(),
        );

        for event in [
            agent(architect_agent::AgentEvent::IterationStarted { iteration: 0 }),
            agent(architect_agent::AgentEvent::Stream(
                StreamEvent::ReasoningDelta {
                    text: "The registry is likely in the tools crate. I should read it before \
                           answering, rather than guessing from the name."
                        .into(),
                },
            )),
            agent(architect_agent::AgentEvent::Stream(
                StreamEvent::ToolCallStart {
                    index: 0,
                    id: "call_a".into(),
                    name: "read_file".into(),
                },
            )),
            agent(architect_agent::AgentEvent::Stream(
                StreamEvent::ToolCallEnd {
                    index: 0,
                    call: ToolCall {
                        id: "call_a".into(),
                        name: "read_file".into(),
                        input: json!({"path": "crates/architect-tools/src/registry.rs"}),
                    },
                },
            )),
            agent(architect_agent::AgentEvent::ToolFinished {
                result: ToolResult::ok(
                    "call_a",
                    "pub struct ToolRegistry {\n    tools: HashMap<String, Box<dyn Tool>>,\n}",
                ),
            }),
            agent(architect_agent::AgentEvent::Stream(
                StreamEvent::ToolCallStart {
                    index: 1,
                    id: "call_b".into(),
                    name: "list_dir".into(),
                },
            )),
            agent(architect_agent::AgentEvent::ToolFinished {
                result: ToolResult::error("call_b", "no such directory: crates/architect-tools"),
            }),
            // A long single-line argument payload — this is what triggered
            // the tool-row overlap bug reported against the running app.
            agent(architect_agent::AgentEvent::Stream(
                StreamEvent::ToolCallStart {
                    index: 2,
                    id: "call_c".into(),
                    name: "run_command".into(),
                },
            )),
            agent(architect_agent::AgentEvent::Stream(
                StreamEvent::ToolCallEnd {
                    index: 2,
                    call: ToolCall {
                        id: "call_c".into(),
                        name: "run_command".into(),
                        input: json!({
                            "command": "grep -rn 'ToolRegistry' apps/desktop/src/engine.rs | awk -F: '$1>400' | head",
                            "description": "Locate ignored live tests",
                            "timeout": 120
                        }),
                    },
                },
            )),
            // Reasoning with no answer — went straight to another turn with
            // nothing to show yet. Must not leave a blank card in the
            // transcript.
            agent(architect_agent::AgentEvent::IterationStarted { iteration: 1 }),
            agent(architect_agent::AgentEvent::Stream(
                StreamEvent::ReasoningDelta {
                    text: "\n\n".into(),
                },
            )),
            agent(architect_agent::AgentEvent::IterationStarted { iteration: 2 }),
            agent(architect_agent::AgentEvent::Stream(
                StreamEvent::TextDelta {
                    text: "\n\n## Summary\n\n\
                       The registry maps a **tool name** to its implementation, so the agent loop \
                       can dispatch a call without knowing any concrete tool:\n\n\
                       ```rust\n\
                       pub struct ToolRegistry {\n    \
                       tools: HashMap<String, Box<dyn Tool>>,\n\
                       }\n\
                       ```\n\n\
                       - `read_file` exists and returned real content\n\
                       - `list_dir` does not exist yet — the crate has not been written"
                        .into(),
                },
            )),
            EngineEvent::Failed {
                session: Some(session),
                message: "connection refused: is the model server running?".into(),
            },
        ] {
            transcript.apply(&event);
        }

        // Two files, one edited and one created — what the Inspector's Files
        // and Diff tabs show.
        transcript.apply(&EngineEvent::FileChanged {
            session,
            entry: FileChangeEntry {
                message_seq: 0,
                change: FileChange {
                    file_path: "crates/architect-tools/src/registry.rs".into(),
                    old_content: Some("pub struct ToolRegistry;\n".into()),
                    new_content: "pub struct ToolRegistry {\n    \
                        tools: HashMap<String, Box<dyn Tool>>,\n}\n"
                        .into(),
                    tool_name: "edit_file",
                },
            },
        });
        transcript.apply(&EngineEvent::FileChanged {
            session,
            entry: FileChangeEntry {
                message_seq: 1,
                change: FileChange {
                    file_path: "NOTES.md".into(),
                    old_content: None,
                    new_content: "- registry maps a name to its tool\n".into(),
                    tool_name: "write_file",
                },
            },
        });

        let openai =
            architect_config::Profile::new("Local LM Studio", "openai", "qwen/qwen3.8-27b")
                .base_url("http://localhost:1234/v1");
        let anthropic =
            architect_config::Profile::new("Work Anthropic key", "anthropic", "claude-opus-5");
        transcript.apply(&EngineEvent::ProfilesListed {
            active: Some(openai.id),
            profiles: vec![openai, anthropic],
        });

        let git = architect_config::McpServerConfig::stdio("Git", "uvx").args(["mcp-server-git"]);
        let disabled = architect_config::McpServerConfig {
            enabled: false,
            ..architect_config::McpServerConfig::stdio("Old server", "npx")
        };
        transcript.apply(&EngineEvent::McpServersListed(vec![git, disabled]));

        transcript
    }
}
