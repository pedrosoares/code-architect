//! Bottom of the centre column: the message composer.

use architect_ui::theme;
use bytes::Bytes;
use freya::components::{
    Button, ImageViewer, InputColorsThemePreference, InputLayoutThemePreference, get_theme,
};
use freya::prelude::*;
use freya::text_edit::{EditableConfig, EditableEvent, EditorLine, TextEditor, use_editable};

use crate::{
    engine::{Attachment, Engine},
    state::Transcript,
};

/// Record the user's turn locally, then hand it to the agent thread.
///
/// A free function rather than a closure: both the Enter key and the Send
/// button need it, and a `FnMut` closure cannot be moved into two handlers.
fn submit(
    mut draft: State<String>,
    mut pending_images: State<Vec<Attachment>>,
    mut transcript: State<Transcript>,
    engine: &Engine,
) {
    let text = draft.read().trim().to_owned();
    let images = std::mem::take(&mut *pending_images.write());
    if text.is_empty() && images.is_empty() {
        return;
    }

    // `active_session` is always `Some` in practice (`Transcript::default`
    // starts one) — the fallback only matters if that invariant is ever
    // broken, so sending never silently does nothing.
    let session = {
        let mut transcript = transcript.write();
        let session = transcript
            .active_session
            .unwrap_or_else(|| transcript.start_new_chat());
        transcript.push_user(session, text.clone(), images.clone());
        session
    };
    engine.send(session, text, images);
    draft.set(String::new());
}

/// Extensions the "Attach" file picker offers, and what each maps to for
/// the wire format's `media_type` — anything else is rejected with an
/// inline error rather than silently guessed at.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

fn media_type_for(extension: &str) -> Option<&'static str> {
    match extension.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Opens a native file picker and, on success, appends the picked image to
/// `pending`. Blocking — the same deliberate, user-initiated,
/// briefly-freezes-the-window category as any native dialog, not something
/// that needs the engine's async machinery.
fn attach(mut pending: State<Vec<Attachment>>, mut error: State<Option<String>>) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("Image", IMAGE_EXTENSIONS)
        .pick_file()
    else {
        // Cancelled — not an error, nothing to report.
        return;
    };

    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_owned();

    let Some(media_type) = media_type_for(&extension) else {
        error.set(Some(format!("unsupported file type: .{extension}")));
        return;
    };

    match std::fs::read(&path) {
        Ok(bytes) => {
            error.set(None);
            pending.write().push(Attachment {
                media_type: media_type.to_owned(),
                bytes,
            });
        }
        Err(io_error) => {
            error.set(Some(format!(
                "could not read {}: {io_error}",
                path.display()
            )));
        }
    }
}

#[derive(PartialEq)]
pub struct Composer;

impl Component for Composer {
    fn render(&self) -> impl IntoElement {
        let engine = consume_context::<Engine>();
        let transcript = consume_context::<State<Transcript>>();
        let draft = use_state(String::new);
        let pending_images = use_state(Vec::<Attachment>::new);
        let attach_error = use_state(|| None::<String>);

        let busy = transcript
            .read()
            .active_conversation()
            .is_some_and(|conversation| conversation.status.is_busy());

        let on_submit_engine = engine.clone();
        let on_press_engine = engine.clone();
        let stop_engine = engine;

        rect()
            .width(Size::fill())
            .vertical()
            .spacing(theme::SPACE_SM)
            .padding(Gaps::new_all(theme::SPACE_MD))
            .background(theme::SURFACE)
            .maybe_child((!pending_images.read().is_empty()).then(|| {
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .spacing(theme::SPACE_SM)
                    .children(
                        pending_images.read().iter().cloned().enumerate().map(
                            |(index, attachment)| thumbnail(index, attachment, pending_images),
                        ),
                    )
            }))
            .maybe_child(attach_error.read().clone().map(|message| {
                label()
                    .text(message)
                    .color(theme::ERROR)
                    .font_size(theme::FONT_SMALL)
            }))
            .child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .spacing(theme::SPACE_SM)
                    .cross_align(Alignment::Center)
                    // Flex, not fill: a filling input would consume the whole
                    // row and squash the buttons out past the panel edge.
                    .content(Content::Flex)
                    .child(
                        Button::new()
                            .outline()
                            .on_press(move |_| attach(pending_images, attach_error))
                            .child("Attach"),
                    )
                    .child(
                        ComposerInput::new(draft)
                            .placeholder("Type a message... (@ to reference files)")
                            .on_submit(move |_: String| {
                                submit(draft, pending_images, transcript, &on_submit_engine)
                            }),
                    )
                    .child(if busy {
                        // The same button becomes Stop, so there is always one
                        // obvious action for the state the agent is in.
                        Button::new()
                            .outline()
                            .on_press(move |_| {
                                if let Some(session) = transcript.read().active_session {
                                    stop_engine.cancel(session);
                                }
                            })
                            .child("Stop")
                    } else {
                        Button::new()
                            .filled()
                            .on_press(move |_| {
                                submit(draft, pending_images, transcript, &on_press_engine)
                            })
                            .child("Send")
                    }),
            )
    }
}

/// How tall the composer can grow before its content scrolls internally
/// instead — roughly six lines at the theme's default input font size.
const COMPOSER_MAX_HEIGHT: f32 = 160.0;

/// A multi-line, auto-growing message box.
///
/// `freya::components::Input` — used here until now — hardcodes a single
/// line (`.max_lines(1)` on its inner `paragraph()`, no builder method to
/// change it) and scrolls long text horizontally inside a fixed-height box.
/// That meant a long message stayed readable only by scrolling sideways
/// inside a one-line-tall strip — not what a chat composer should do; typed
/// text should wrap and the box should grow with it, the way every other
/// chat client behaves. Since `Input` can't be configured for that, this
/// reimplements its editing plumbing directly on the same `freya_edit`
/// primitives `Input` itself is built on (`use_editable`, its click/focus/
/// selection wiring), changed in exactly two ways: the paragraph is allowed
/// to wrap (no `max_lines`), and it sits in a *vertical* `ScrollView`
/// bounded by `COMPOSER_MAX_HEIGHT` instead of `Input`'s horizontal one — so
/// the box grows with its content and only scrolls once it hits that cap.
/// Enter sends the message, mirroring `Input::on_submit`; Shift+Enter
/// inserts a newline.
///
/// Cursor blinking is deliberately left out: `Input`'s comes from
/// `freya_components::cursor_blink`, a module the `freya` facade crate
/// doesn't reexport, and adding `freya-components` as a direct dependency
/// just for a blink timer isn't worth it — a solid (non-blinking) cursor
/// still shows where typing will land.
#[derive(Clone, PartialEq)]
struct ComposerInput {
    value: State<String>,
    placeholder: &'static str,
    on_submit: Option<EventHandler<String>>,
}

impl ComposerInput {
    fn new(value: State<String>) -> Self {
        Self {
            value,
            placeholder: "",
            on_submit: None,
        }
    }

    fn placeholder(mut self, placeholder: &'static str) -> Self {
        self.placeholder = placeholder;
        self
    }

    fn on_submit(mut self, on_submit: impl Into<EventHandler<String>>) -> Self {
        self.on_submit = Some(on_submit.into());
        self
    }
}

impl Component for ComposerInput {
    fn render(&self) -> impl IntoElement {
        let a11y_id = use_hook(AccessibilityId::new_unique);
        let focus = use_focus(a11y_id);
        let holder = use_state(ParagraphHolder::default);
        let mut area = use_state(Area::default);
        let mut is_dragging = use_state(|| false);
        let mut value = self.value;
        let mut editable = use_editable(|| self.value.read().to_string(), EditableConfig::new);

        let theme_colors = get_theme!(&None, InputColorsThemePreference, "input");
        let theme_layout = get_theme!(&None, InputLayoutThemePreference, "input_layout");

        let display_placeholder =
            value.read().is_empty() && !editable.editor().read().has_preedit();
        let on_submit = self.on_submit.clone();

        // Keep the editor in sync with external resets to `value` — the
        // composer clears `draft` after a successful send, which doesn't go
        // through `editable` at all.
        if *value.read() != editable.editor().read().committed_text() {
            let mut editor = editable.editor_mut().write();
            editor.clear_preedit();
            editor.set(&value.read());
            editor.editor_history().clear();
            editor.clear_selection();
        }

        let on_ime_preedit = move |e: Event<ImePreeditEventData>| {
            let mut editor = editable.editor_mut().write();
            if e.data().text.is_empty() {
                editor.clear_preedit();
            } else {
                editor.set_preedit(&e.data().text);
            }
        };

        let on_key_down = move |e: Event<KeyboardEventData>| {
            let key = e.key.clone();
            let modifiers = e.modifiers;

            match &key {
                // Enter sends; Shift+Enter falls through to the default arm
                // below, which inserts the newline like any other character.
                Key::Named(NamedKey::Enter) if !modifiers.contains(Modifiers::SHIFT) => {
                    if let Some(on_submit) = &on_submit {
                        on_submit.call(editable.editor().peek().committed_text());
                    }
                }
                Key::Named(NamedKey::Escape) => {
                    a11y_id.request_unfocus();
                    Cursor::set(CursorIcon::default());
                }
                // Left unhandled so focus moves to the next element instead
                // of inserting a tab character.
                Key::Named(NamedKey::Tab) => {}
                _ => {
                    e.stop_propagation();
                    e.prevent_default();
                    editable.process_event(EditableEvent::KeyDown {
                        key: &key,
                        modifiers,
                    });
                    *value.write() = editable.editor().read().committed_text();
                }
            }
        };

        let on_key_up = move |e: Event<KeyboardEventData>| {
            e.stop_propagation();
            editable.process_event(EditableEvent::KeyUp { key: &e.key });
        };

        let on_input_focus_press = move |e: Event<FocusPressEventData>| {
            e.stop_propagation();
            e.prevent_default();
            is_dragging.set_if_modified(true);
            if !display_placeholder {
                let area = area.read().to_f64();
                let global_location = e.global_location().clamp(area.min(), area.max());
                let location = (global_location - area.min()).to_point();
                editable.process_event(EditableEvent::Down {
                    location,
                    editor_line: EditorLine::SingleParagraph,
                    holder: &holder.read(),
                });
            }
            a11y_id.request_focus();
        };

        let on_focus_press = move |e: Event<FocusPressEventData>| {
            e.stop_propagation();
            e.prevent_default();
            is_dragging.set_if_modified(true);
            if !display_placeholder {
                editable.process_event(EditableEvent::Down {
                    location: e.element_location(),
                    editor_line: EditorLine::SingleParagraph,
                    holder: &holder.read(),
                });
            }
            a11y_id.request_focus();
        };

        let on_global_pointer_move = move |e: Event<PointerEventData>| {
            if a11y_id.is_focused() && *is_dragging.read() {
                let mut location = e.global_location();
                location.x -= area.read().min_x() as f64;
                location.y -= area.read().min_y() as f64;
                editable.process_event(EditableEvent::Move {
                    location,
                    editor_line: EditorLine::SingleParagraph,
                    holder: &holder.read(),
                });
            }
        };

        let on_pointer_enter = move |_: Event<PointerEventData>| Cursor::set(CursorIcon::Text);
        let on_pointer_leave = move |_: Event<PointerEventData>| Cursor::set(CursorIcon::default());

        let on_global_pointer_press = move |_: Event<PointerEventData>| {
            if a11y_id.is_focused() {
                editable.process_event(EditableEvent::Release);
                if *is_dragging.read() {
                    is_dragging.set(false);
                } else {
                    a11y_id.request_unfocus();
                }
            }
        };

        let on_pointer_press = move |_: Event<PointerEventData>| {
            if a11y_id.is_focused() {
                editable.process_event(EditableEvent::Release);
                is_dragging.set_if_modified(false);
            }
        };

        let (background, cursor_index, text_selection) = if focus() != Focus::Not {
            (
                theme_colors.focus_background,
                Some(editable.editor().read().cursor_pos()),
                editable
                    .editor()
                    .read()
                    .get_visible_selection(EditorLine::SingleParagraph),
            )
        } else {
            (theme_colors.background, None, None)
        };

        let border = if focus().is_focused() {
            Border::new()
                .fill(theme_colors.focus_border_fill)
                .width(2.)
                .alignment(BorderAlignment::Inner)
        } else {
            Border::new()
                .fill(theme_colors.border_fill)
                .width(1.)
                .alignment(BorderAlignment::Inner)
        };

        let color = if display_placeholder {
            theme_colors.placeholder_color
        } else {
            theme_colors.color
        };

        let value_text = self.value.read().clone();
        let placeholder = self.placeholder;

        rect()
            .a11y_id(a11y_id)
            .a11y_focusable(true)
            .a11y_alt(if display_placeholder {
                placeholder.to_owned()
            } else {
                value_text.clone()
            })
            .a11y_role(AccessibilityRole::TextInput)
            .on_key_up(on_key_up)
            .on_key_down(on_key_down)
            .on_focus_press(on_input_focus_press)
            .on_ime_preedit(on_ime_preedit)
            .on_pointer_press(on_pointer_press)
            .on_global_pointer_press(on_global_pointer_press)
            .on_global_pointer_move(on_global_pointer_move)
            .on_pointer_enter(on_pointer_enter)
            .on_pointer_leave(on_pointer_leave)
            // Flex, not fill: this sits in the composer's `Content::Flex`
            // row alongside the Attach/Send buttons — `Size::fill()` there
            // is circular (this child's width would depend on the row's
            // content-driven width, which depends on this child), and threw
            // the ScrollView's inner-content measurement off so badly its
            // scrollbar thumb rendered past the window's edge.
            .width(Size::flex(1.0))
            .background(background)
            .border(border)
            .corner_radius(theme_layout.corner_radius)
            // A second, outer clamp on top of `ScrollView`'s own internal
            // `Size::Inner` + `max_height` below (which is otherwise
            // reliable for anything a person actually types, but past
            // several hundred characters pasted as one unbroken run, stops
            // capping height at all). This doesn't fully close that gap —
            // an extreme paste can still bleed a line or two past the cap —
            // but it keeps the box itself, and the window, bounded either
            // way.
            .height(Size::Inner)
            .max_height(Size::px(COMPOSER_MAX_HEIGHT))
            .overflow(Overflow::Clip)
            .child(
                ScrollView::new()
                    .width(Size::fill())
                    .height(Size::Inner)
                    .max_height(Size::px(COMPOSER_MAX_HEIGHT))
                    .child(
                        paragraph()
                            .holder(holder.read().clone())
                            .on_sized(move |e: Event<SizedEventData>| area.set(e.visible_area))
                            // Both a floor *and* a ceiling, to the same
                            // value: `min_width` alone (what `Input` uses,
                            // for its unwrapped single line) left the
                            // paragraph's own natural width just wide
                            // enough that it never actually fit inside the
                            // ScrollView's viewport — invisibly clipped, so
                            // wrapped text still looked right, but the
                            // ScrollView measured it as genuinely wider than
                            // its viewport and grew a phantom horizontal
                            // scrollbar. Pinning `max_width` to the same
                            // value forces the wrap boundary and the
                            // viewport width to actually agree.
                            .min_width(Size::func(move |context| {
                                Some(context.parent - theme_layout.inner_margin.horizontal())
                            }))
                            .max_width(Size::func(move |context| {
                                Some(context.parent - theme_layout.inner_margin.horizontal())
                            }))
                            .on_focus_press(on_focus_press)
                            .margin(theme_layout.inner_margin)
                            .cursor_index(cursor_index)
                            .cursor_color(theme_colors.color)
                            .color(color)
                            .highlights(text_selection.map(|h| vec![h]))
                            .maybe(display_placeholder, |el| el.span(placeholder.to_owned()))
                            .maybe(!display_placeholder, |el| {
                                let editor = editable.editor().read();
                                if editor.has_preedit() {
                                    let (before, preedit, after) = editor.preedit_text_segments();
                                    el.span(before)
                                        .span(
                                            Span::new(preedit)
                                                .text_decoration(TextDecoration::Underline),
                                        )
                                        .span(after)
                                } else {
                                    el.span(editor.rope().to_string())
                                }
                            }),
                    ),
            )
    }
}

/// One pending attachment's thumbnail, with its own remove button — `index`
/// identifies which entry of `pending` to drop, since attachments carry no
/// id of their own.
fn thumbnail(index: usize, attachment: Attachment, mut pending: State<Vec<Attachment>>) -> Element {
    rect()
        .vertical()
        .spacing(4.0)
        .cross_align(Alignment::Center)
        .child(
            ImageViewer::new((index, Bytes::from(attachment.bytes)))
                .width(Size::px(56.0))
                .height(Size::px(56.0)),
        )
        .child(
            Button::new()
                .compact()
                .outline()
                .on_press(move |_| {
                    pending.write().remove(index);
                })
                .child("Remove"),
        )
        .into_element()
}

#[cfg(test)]
mod tests {
    use freya_testing::TestingRunner;

    use super::*;

    /// `ComposerInput` sits in the real composer's `Content::Flex` row next
    /// to the Attach/Send buttons — `Size::fill()` there is circular (this
    /// child's width would depend on the row's content-driven width, which
    /// depends on this child) and once threw the `ScrollView`'s internal
    /// content measurement off badly enough that its scrollbar thumb
    /// rendered past the window's edge, with Send pushed out of the row
    /// entirely. `ComposerInput::render` uses `Size::flex(1.0)` instead, for
    /// exactly this reason — this test pins that choice in the same layout
    /// context that exposed the bug, rather than the isolated single-child
    /// case that didn't.
    #[test]
    fn renders_correctly_in_a_flex_row() {
        fn app() -> impl IntoElement {
            let draft = use_state(String::new);
            rect()
                .width(Size::fill())
                .height(Size::fill())
                .padding(Gaps::new_all(20.0))
                .child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .spacing(8.0)
                        .cross_align(Alignment::Center)
                        .content(Content::Flex)
                        .child(Button::new().outline().child("Attach"))
                        .child(ComposerInput::new(draft).placeholder("Type a message..."))
                        .child(Button::new().filled().child("Send")),
                )
        }
        let (mut test, ()) = TestingRunner::new(app, (1440.0, 900.0).into(), |_| (), 1.0);
        test.sync_and_update();
        test.render_to_file("/tmp/architect-composer-input-flex-row.png");
    }
}
