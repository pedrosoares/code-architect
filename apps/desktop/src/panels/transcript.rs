//! Centre panel: the conversation transcript.
//!
//! Rows come straight from [`crate::state::Transcript`], which is folded from
//! the agent's event stream — nothing here decides what a turn looks like, it
//! only draws it.

use architect_ui::{
    theme,
    widgets::{Avatar, Disclosure, PanelHeader},
};
use bytes::Bytes;
use freya::components::{
    ImageViewer, ScrollConfig, ScrollPosition, ScrollView, SelectableText, use_scroll_controller,
};
use freya::prelude::*;

use crate::{
    engine::Attachment,
    panels::ScrollToToolCall,
    state::{Row, ToolStatus, Transcript},
};

#[derive(PartialEq)]
pub struct TranscriptPanel;

impl Component for TranscriptPanel {
    fn render(&self) -> impl IntoElement {
        let transcript = consume_context::<State<Transcript>>();
        let mut scroll = use_scroll_controller(ScrollConfig::default);

        // Follow the stream. Reading the active conversation's rows here is
        // what subscribes the effect, so every delta scrolls the newest
        // content into view — but only while that conversation is the one
        // on screen, not while some other session streams in the background.
        //
        // Guarded on `has_rows`: below, this panel only mounts a `ScrollView`
        // once there is something to show — an empty conversation renders
        // `empty_state()` instead. Queuing a scroll-to-end request while no
        // `ScrollView` exists to consume it left a stale request sitting on
        // this shared `scroll` controller; the first time a `ScrollView`
        // ever mounts for it (the moment the first row arrives) that stale
        // request gets drained before the view's own height has been
        // measured, which is what a `freya-components` panic inside
        // `use_scroll_controller` (`y - height as i32` overflowing) traced
        // back to. Never issuing the request for an empty conversation in
        // the first place avoids ever priming that state.
        use_side_effect(move || {
            let has_rows = transcript
                .read()
                .active_conversation()
                .is_some_and(|conversation| !conversation.rows.is_empty());
            if has_rows {
                scroll.scroll_to(ScrollPosition::End, Direction::Vertical);
            }
        });

        let rows = transcript
            .read()
            .active_conversation()
            .map(|conversation| conversation.rows.clone())
            .unwrap_or_default();
        let empty = rows.is_empty();

        let mut list = rect()
            .width(Size::fill())
            .vertical()
            .spacing(theme::SPACE_MD)
            .padding(Gaps::new_all(theme::SPACE_MD));

        for (index, row) in rows.into_iter().enumerate() {
            if let Some(element) = render_row(index, row) {
                list = list.child(element);
            }
        }

        rect()
            .expanded()
            .vertical()
            .child(PanelHeader::new("Transcript"))
            .child(
                rect()
                    .width(Size::fill())
                    .height(Size::fill())
                    .child(if empty {
                        empty_state().into_element()
                    } else {
                        ScrollView::new_controlled(scroll)
                            .child(list)
                            .into_element()
                    }),
            )
    }
}

fn empty_state() -> impl IntoElement {
    rect().expanded().center().child(
        label()
            .text("Ask the agent to change something.")
            .color(theme::TEXT_DIM)
            .font_size(theme::FONT_SMALL),
    )
}

/// `None` when a row has nothing left to show — an assistant turn that ended
/// up with neither reasoning nor text (interrupted, went straight to another
/// turn) would otherwise leave an orphaned avatar with no content beside it.
fn render_row(index: usize, row: Row) -> Option<Element> {
    let element = match row {
        Row::User { text, images } => {
            let mut content = rect()
                .width(Size::fill())
                .vertical()
                .spacing(theme::SPACE_SM);

            if !images.is_empty() {
                content = content.child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .spacing(theme::SPACE_SM)
                        .children(images.into_iter().enumerate().map(|(image_index, image)| {
                            ImageViewer::new((image_index, Bytes::from(image.bytes)))
                                .width(Size::px(160.0))
                                .into_element()
                        })),
                );
            }
            if !text.is_empty() {
                content = content.child(message_card(&text));
            }

            turn(Avatar::new("U"), content).key(index).into_element()
        }
        Row::Assistant { reasoning, text } => {
            // Trimmed, not just non-empty: some models pad their reply with
            // leading newlines that would otherwise pass an `.is_empty()`
            // check and still render as a blank card.
            let reasoning = reasoning.trim();
            let text = text.trim();
            if reasoning.is_empty() && text.is_empty() {
                return None;
            }

            let mut content = rect()
                .width(Size::fill())
                .vertical()
                .spacing(theme::SPACE_SM);

            if !reasoning.is_empty() {
                content = content.child(
                    Disclosure::new("reasoning").content(
                        SelectableText::new()
                            .color(theme::TEXT_DIM)
                            .font_size(theme::FONT_SMALL)
                            .span(reasoning.to_owned()),
                    ),
                );
            }
            if !text.is_empty() {
                content = content.child(message_card(text));
            }

            turn(Avatar::new("A").tint(theme::ACCENT), content)
                .key(index)
                .into_element()
        }
        Row::Tool {
            id,
            name,
            arguments,
            status,
            output,
            image,
            ..
        } => rect()
            .key(index)
            .child(ToolRow {
                id,
                name,
                arguments,
                status,
                output,
                image,
            })
            .into_element(),
        Row::Error { message } => rect()
            .key(index)
            .width(Size::fill())
            .padding(Gaps::new_all(theme::SPACE_MD))
            .background(theme::SURFACE_RAISED)
            .border(Border::new().fill(theme::ERROR).width(1.0))
            .rounded_lg()
            .child(
                SelectableText::new()
                    .color(theme::ERROR)
                    .font_size(theme::FONT_SMALL)
                    .span(message),
            )
            .into_element(),
        Row::Compacted { summary } => rect()
            .key(index)
            .width(Size::fill())
            .vertical()
            .spacing(theme::SPACE_SM)
            .padding(Gaps::new_all(theme::SPACE_MD))
            .background(theme::SURFACE_RAISED)
            .rounded_lg()
            .child(
                label()
                    .text("Conversation compacted")
                    .color(theme::TEXT_DIM)
                    .font_size(theme::FONT_SMALL)
                    .font_weight(FontWeight::BOLD),
            )
            .child(
                SelectableText::new()
                    .color(theme::TEXT_DIM)
                    .font_size(theme::FONT_SMALL)
                    .span(summary),
            )
            .into_element(),
    };

    Some(element)
}

/// Avatar column plus content column.
fn turn(avatar: Avatar, content: impl IntoElement) -> Rect {
    rect()
        .width(Size::fill())
        .horizontal()
        .spacing(theme::SPACE_MD)
        .cross_align(Alignment::Start)
        .child(avatar)
        .child(
            rect()
                .width(Size::fill())
                .vertical()
                .spacing(theme::SPACE_SM)
                .child(content),
        )
}

fn message_card(text: &str) -> impl IntoElement {
    rect()
        .width(Size::fill())
        .padding(Gaps::new_all(theme::SPACE_MD))
        .background(theme::SURFACE)
        .border(Border::new().fill(theme::BORDER).width(1.0))
        .rounded_lg()
        .child(
            // Plain text, not markdown — `SelectableText` is what makes
            // this bubble click-drag selectable and Ctrl+C-able, and it
            // has no markdown renderer. A deliberate tradeoff: this app
            // used to render bold/code/lists here via `MarkdownViewer`,
            // which has no selection support at all.
            SelectableText::new()
                .color(theme::TEXT)
                .font_size(theme::FONT_BODY)
                .span(text.to_owned()),
        )
}

/// One tool call: status, name, arguments; pressing the header expands or
/// collapses the output. A `Component` in its own right, not a plain
/// function — it needs its own hook scope to hold that toggle state
/// independently for every tool row in the transcript.
#[derive(PartialEq)]
struct ToolRow {
    id: String,
    name: String,
    arguments: String,
    status: ToolStatus,
    output: Option<String>,
    /// Image the tool produced — a `view_image` / `firefox_screenshot` result.
    /// Its presence is what makes the row auto-open (see `render`) so the
    /// image is visible immediately.
    image: Option<Attachment>,
}

impl Component for ToolRow {
    fn render(&self) -> impl IntoElement {
        let (mark, color) = match self.status {
            ToolStatus::Running => ("\u{25CF}", theme::TEXT_DIM),
            ToolStatus::Ok => ("\u{2713}", theme::SUCCESS),
            ToolStatus::Failed => ("\u{2717}", theme::ERROR),
        };

        // Starts collapsed: a long text-only output would otherwise dominate
        // the transcript before anyone asked to see it. A row that produced an
        // image is the exception — see the auto-open effect below.
        let mut open = use_state(|| false);
        let is_open = *open.read();
        let has_output = self.output.is_some();
        let has_image = self.image.is_some();
        // Expandable if there is anything to reveal — text output, an image
        // (a `view_image` / screenshot result), or both. A still-running call
        // has neither, so it never shows the chevron.
        let has_content = has_output || has_image;

        // Stable per mounted row — what the Inspector's Tools tab asks
        // Freya to scroll into view when this call is picked there.
        let a11y_id = use_a11y();
        let mut scroll_target = consume_context::<ScrollToToolCall>().0;
        let id = self.id.clone();
        use_side_effect(move || {
            if *scroll_target.read() == Some(id.clone()) {
                a11y_id.request_focus();
                if has_content {
                    open.set(true);
                }
                scroll_target.set(None);
            }
        });

        // A row that produced an image (a `view_image` / `firefox_screenshot`
        // result) is *about* that image — its whole point is to show the page —
        // so open it the moment the image lands, rather than waiting for a
        // click. The image arrives *after* mount (the row is created at
        // `ToolStarted`, the image at `ToolFinished`), so it can't be baked
        // into the initial `open` state; instead open once, the first time an
        // image shows up, then let the user collapse it. Text-only rows stay
        // collapsed.
        let mut auto_opened = use_state(|| false);
        use_side_effect(move || {
            if has_image && !*auto_opened.read() {
                open.set(true);
                auto_opened.set(true);
            }
        });

        rect()
            .width(Size::fill())
            .vertical()
            .a11y_id(a11y_id)
            .a11y_focusable(true)
            .background(theme::SURFACE_RAISED)
            .border(Border::new().fill(theme::BORDER).width(1.0))
            .rounded()
            .child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(theme::SPACE_SM)
                    .padding(Gaps::new_symmetric(8.0, 10.0))
                    // `SpaceBetween` alone doesn't stop two unconstrained-width
                    // children from both claiming their full natural width when
                    // that's more than the row has — the arguments label was
                    // rendering right on top of the name. Flex content forces it
                    // to actually share the row instead of overlapping it.
                    .content(Content::Flex)
                    .maybe_child(has_content.then(|| {
                        label()
                            .text(if is_open { "\u{25BE}" } else { "\u{25B8}" })
                            .color(theme::TEXT_DIM)
                            .font_size(theme::FONT_SMALL)
                    }))
                    .child(
                        rect()
                            .horizontal()
                            .cross_align(Alignment::Center)
                            .spacing(8.0)
                            .child(label().text(mark).color(color).font_size(theme::FONT_SMALL))
                            .child(
                                label()
                                    .text(self.name.clone())
                                    .font_family(theme::FONT_MONO),
                            ),
                    )
                    .child(
                        label()
                            .width(Size::flex(1.0))
                            .text(self.arguments.clone())
                            .color(theme::TEXT_DIM)
                            .font_family(theme::FONT_MONO)
                            .font_size(theme::FONT_SMALL)
                            .text_align(TextAlign::Right)
                            .max_lines(1)
                            .text_overflow(TextOverflow::Ellipsis),
                    )
                    // Only worth a click once there is something to reveal —
                    // a still-running call has no output or image yet.
                    .on_press(move |_| {
                        if has_content {
                            open.toggle();
                        }
                    }),
            )
            .maybe_child(is_open.then(|| {
                // Image on top, text below — a `view_image` result carries
                // both ("Showing …" plus the PNG). The row auto-opens when an
                // image is present, so this body is visible the moment the
                // tool finishes; the text output is capped at six lines.
                let mut body = rect()
                    .width(Size::fill())
                    .vertical()
                    .spacing(theme::SPACE_SM)
                    .padding(Gaps::new_symmetric(8.0, 10.0))
                    .background(theme::BACKGROUND);

                if let Some(image) = self.image.clone() {
                    body = body.child(
                        rect().width(Size::fill()).child(
                            ImageViewer::new(Bytes::from(image.bytes))
                                // Same explicit-width pattern the
                                // `Row::User` thumbnails use — freya
                                // derives a proportional height from the
                                // width, so a wide viewport screenshot
                                // scales down rather than overflowing.
                                // 600px so the expanded view is large
                                // enough to read a page comfortably.
                                .width(Size::px(600.0))
                                .corner_radius(4.0)
                                .into_element(),
                        ),
                    );
                }

                if let Some(output) = self.output.clone() {
                    body = body.child(
                        // Not a click target (unlike the header above,
                        // which toggles this open/closed) — safe to make
                        // selectable, and the single most useful spot in
                        // the transcript to copy from: file contents,
                        // command output, tool results.
                        SelectableText::new()
                            .color(theme::TEXT_DIM)
                            .font_family(theme::FONT_MONO)
                            .font_size(theme::FONT_SMALL)
                            .max_lines(6)
                            .span(output),
                    );
                }

                body.into_element()
            }))
    }
}
