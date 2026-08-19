use freya::prelude::*;

use crate::theme;

/// A pill in the header: model name, mode, connection status.
///
/// Named `StatusChip` rather than `Chip` to stay clear of Freya's own `Chip`
/// component.
#[derive(PartialEq)]
pub struct StatusChip {
    text: String,
    active: bool,
}

impl StatusChip {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            active: false,
        }
    }

    /// Fill the chip with the accent color — used for the current mode.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
}

impl Component for StatusChip {
    fn render(&self) -> impl IntoElement {
        let (background, color) = if self.active {
            (theme::ACCENT, theme::ON_ACCENT)
        } else {
            (theme::SURFACE_RAISED, theme::TEXT_DIM)
        };

        rect()
            .center()
            .rounded_full()
            .background(background)
            .padding(Gaps::new_symmetric(5.0, 12.0))
            .child(
                label()
                    .text(self.text.clone())
                    .color(color)
                    .font_size(theme::FONT_SMALL),
            )
    }
}
