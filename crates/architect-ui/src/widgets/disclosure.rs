use freya::prelude::*;

use crate::theme;

/// A collapsible section introduced by a small triangle, as used for the
/// agent's `reasoning` blocks.
#[derive(PartialEq)]
pub struct Disclosure {
    label: String,
    content: Option<Element>,
    open: bool,
}

impl Disclosure {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            content: None,
            open: false,
        }
    }

    pub fn content(mut self, content: impl Into<Element>) -> Self {
        self.content = Some(content.into());
        self
    }

    /// Whether the section starts expanded.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }
}

impl Component for Disclosure {
    fn render(&self) -> impl IntoElement {
        let mut open = use_state(|| self.open);
        let is_open = *open.read();

        rect()
            .width(Size::fill())
            .vertical()
            .spacing(4.0)
            .child(
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(4.0)
                    .on_press(move |_| open.toggle())
                    .child(
                        label()
                            .text(if is_open { "\u{25BE}" } else { "\u{25B8}" })
                            .color(theme::TEXT_DIM)
                            .font_size(theme::FONT_SMALL),
                    )
                    .child(
                        label()
                            .text(self.label.clone())
                            .color(theme::TEXT_DIM)
                            .font_size(theme::FONT_SMALL),
                    ),
            )
            .maybe_child(is_open.then(|| self.content.clone()).flatten())
    }
}
