use freya::prelude::*;

use crate::theme;

/// The round initial badge that opens a transcript row.
///
/// ```ignore
/// Avatar::new("A").tint(theme::ACCENT)
/// ```
#[derive(PartialEq)]
pub struct Avatar {
    initial: String,
    tint: Color,
}

impl Avatar {
    pub fn new(initial: impl Into<String>) -> Self {
        Self {
            initial: initial.into(),
            tint: theme::TEXT_DIM,
        }
    }

    /// Color of the initial; also tints the badge outline.
    pub fn tint(mut self, tint: Color) -> Self {
        self.tint = tint;
        self
    }
}

impl Component for Avatar {
    fn render(&self) -> impl IntoElement {
        rect()
            .width(Size::px(theme::AVATAR_SIZE))
            .height(Size::px(theme::AVATAR_SIZE))
            .center()
            .rounded_full()
            .background(theme::SURFACE_RAISED)
            .border(Border::new().fill(theme::BORDER).width(1.0))
            .child(
                label()
                    .text(self.initial.clone())
                    .color(self.tint)
                    .font_size(theme::FONT_SMALL)
                    .font_weight(FontWeight::BOLD),
            )
    }
}
