use freya::prelude::*;

use crate::theme;

/// The title bar at the top of a panel.
///
/// ```ignore
/// PanelHeader::new("Sessions").trailing(Button::new().child("New"))
/// ```
#[derive(PartialEq)]
pub struct PanelHeader {
    title: String,
    trailing: Option<Element>,
}

impl PanelHeader {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            trailing: None,
        }
    }

    /// Content pinned to the right edge of the header, e.g. an action button.
    pub fn trailing(mut self, trailing: impl Into<Element>) -> Self {
        self.trailing = Some(trailing.into());
        self
    }
}

impl Component for PanelHeader {
    fn render(&self) -> impl IntoElement {
        rect()
            .width(Size::fill())
            .height(Size::px(34.0))
            .horizontal()
            .cross_align(Alignment::Center)
            .main_align(Alignment::SpaceBetween)
            .padding(Gaps::new_symmetric(0.0, theme::SPACE_MD))
            .child(
                label()
                    .text(self.title.clone())
                    .color(theme::TEXT_DIM)
                    .font_size(theme::FONT_SMALL)
                    .font_weight(FontWeight::BOLD),
            )
            .maybe_child(self.trailing.clone())
    }
}
