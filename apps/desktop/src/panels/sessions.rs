//! Left panel: sessions.
//!
//! Lists every session `architect-session` has saved and lets you switch
//! between them. "New Chat" starts a fresh conversation without losing the
//! old one — it stays in this list.

use std::collections::HashMap;

use architect_session::{SessionId, SessionSummary};
use architect_ui::{theme, widgets::PanelHeader};
use freya::components::{Button, ButtonLayoutThemePartial, ScrollView};
use freya::prelude::*;

use crate::{engine::Engine, panels::SessionsCollapsed, state::Transcript};

#[derive(PartialEq)]
pub struct SessionsPanel;

impl Component for SessionsPanel {
    fn render(&self) -> impl IntoElement {
        let engine = consume_context::<Engine>();
        let mut transcript = consume_context::<State<Transcript>>();
        let mut collapsed = consume_context::<SessionsCollapsed>().0;

        let (sessions, active) = {
            let transcript = transcript.read();
            (transcript.sessions.clone(), transcript.active_session)
        };

        rect()
            .expanded()
            .vertical()
            .background(theme::SURFACE)
            .child(
                rect()
                    .width(Size::fill())
                    .padding(Gaps::new_all(theme::SPACE_MD))
                    .child(
                        Button::new()
                            .filled()
                            // `Button::expanded` is a padding variant, not a
                            // width — filling the sidebar needs a layout theme.
                            .theme_layout(ButtonLayoutThemePartial::new().width(Size::fill()))
                            .on_press(move |_| {
                                // No engine round-trip: the old conversation
                                // is still saved and still listed below,
                                // only the active one changes, and nothing
                                // durable exists for this one until its
                                // first message is actually sent.
                                transcript.write().start_new_chat();
                            })
                            .child("+ New Chat"),
                    ),
            )
            .child(
                PanelHeader::new("Sessions").trailing(
                    Button::new()
                        .outline()
                        .on_press(move |_| collapsed.set(true))
                        .child(label().text("\u{2039}").font_size(theme::FONT_SMALL)),
                ),
            )
            .child(if sessions.is_empty() {
                empty_state().into_element()
            } else {
                rect()
                    .width(Size::fill())
                    .height(Size::fill())
                    .child(ScrollView::new().child({
                        let mut list = rect()
                            .width(Size::fill())
                            .vertical()
                            .spacing(theme::SPACE_SM)
                            .padding(Gaps::new_symmetric(theme::SPACE_SM, theme::SPACE_MD));

                        // A sub-agent's session (`parent_id: Some(..)`) is
                        // rendered nested directly beneath its parent, in
                        // the order it was spawned, rather than mixed into
                        // the top-level list by `updated_at` like every
                        // other row is.
                        let mut children: HashMap<SessionId, Vec<SessionSummary>> = HashMap::new();
                        let mut top_level = Vec::new();
                        for session in sessions {
                            match session.parent_id {
                                Some(parent) => children.entry(parent).or_default().push(session),
                                None => top_level.push(session),
                            }
                        }
                        for group in children.values_mut() {
                            group.sort_by(|a, b| a.created_at.cmp(&b.created_at));
                        }

                        for session in top_level {
                            let id = session.id;
                            let is_busy = transcript
                                .read()
                                .conversation(id)
                                .is_some_and(|conversation| conversation.status.is_busy());
                            list = list.child(session_row(
                                session,
                                Some(id) == active,
                                is_busy,
                                false,
                                engine.clone(),
                                transcript,
                            ));

                            for child in children.remove(&id).into_iter().flatten() {
                                let child_id = child.id;
                                let is_busy = transcript
                                    .read()
                                    .conversation(child_id)
                                    .is_some_and(|conversation| conversation.status.is_busy());
                                list = list.child(session_row(
                                    child,
                                    Some(child_id) == active,
                                    is_busy,
                                    true,
                                    engine.clone(),
                                    transcript,
                                ));
                            }
                        }

                        list
                    }))
                    .into_element()
            })
    }
}

fn empty_state() -> impl IntoElement {
    rect()
        .width(Size::fill())
        .height(Size::fill())
        .center()
        .padding(Gaps::new_all(theme::SPACE_MD))
        .child(
            label()
                .text("Sessions are saved automatically and\nresume when you reopen the app.")
                .color(theme::TEXT_DIM)
                .font_size(theme::FONT_SMALL)
                .text_align(TextAlign::Center),
        )
}

fn session_row(
    session: SessionSummary,
    is_active: bool,
    is_busy: bool,
    // True for a sub-agent's session — spawned by a `spawn_subagents` tool
    // call in another session's turn — rendered indented directly beneath
    // its parent rather than as a peer of it.
    nested: bool,
    engine: Engine,
    mut transcript: State<Transcript>,
) -> impl IntoElement {
    let id: SessionId = session.id;
    let title = if session.title.trim().is_empty() {
        "Untitled".to_owned()
    } else {
        session.title
    };

    let (background, title_color) = if is_active {
        (theme::ACCENT_SOFT, theme::TEXT)
    } else {
        (theme::SURFACE, theme::TEXT_DIM)
    };

    let padding = if nested {
        Gaps::new(10.0, theme::SPACE_MD, 10.0, theme::SPACE_MD + 20.0)
    } else {
        Gaps::new_symmetric(10.0, theme::SPACE_MD)
    };

    let delete_engine = engine.clone();

    rect()
        .width(Size::fill())
        .horizontal()
        // The title/model column shares the row with a Delete button —
        // without flex content, `Size::flex(1.0)` below claims everything
        // and squeezes the button down to a single-character-wide column.
        .content(Content::Flex)
        .cross_align(Alignment::Center)
        .spacing(theme::SPACE_SM)
        .padding(padding)
        .background(background)
        .rounded_lg()
        .on_press(move |_| {
            if is_active {
                return;
            }
            // Already resident (running in the background, or loaded
            // earlier this app run) — just change which one is displayed,
            // no fetch. Otherwise fall back to the engine, which fetches it
            // from the store and reports back via `HistoryLoaded`.
            if transcript.read().conversations.contains_key(&id) {
                transcript.write().switch_to(id);
            } else {
                engine.load_session(id);
            }
        })
        .child(
            rect()
                .width(Size::flex(1.0))
                .vertical()
                .spacing(3.0)
                .child(
                    rect()
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(6.0)
                        // A sub-agent session, spawned by its parent's
                        // `spawn_subagents` tool call — the connector this
                        // row's indent alone would otherwise leave implicit.
                        .maybe_child(nested.then(|| {
                            label()
                                .text("\u{2514}")
                                .color(theme::TEXT_DIM)
                                .font_size(theme::FONT_SMALL)
                        }))
                        // A session still working in the background, seen
                        // from anywhere else in the sidebar — the visible
                        // proof it kept running while off-screen.
                        .maybe_child(is_busy.then(|| {
                            label()
                                .text("\u{25CF}")
                                .color(theme::ACCENT)
                                .font_size(theme::FONT_SMALL)
                        }))
                        .child(
                            label()
                                .text(title)
                                .color(title_color)
                                .max_lines(2)
                                .text_overflow(TextOverflow::Ellipsis),
                        ),
                )
                .child(
                    label()
                        .text(session.model)
                        .color(theme::TEXT_DIM)
                        .font_size(theme::FONT_SMALL),
                ),
        )
        .child(
            Button::new()
                .outline()
                .on_press(move |e: Event<PressEventData>| {
                    // Deleting must not also select the row it's sitting
                    // in — the row's own `on_press` would otherwise fire
                    // right alongside this one.
                    e.stop_propagation();
                    delete_engine.delete_session(id);
                })
                .child("Delete"),
        )
}
