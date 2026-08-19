//! Reusable widgets shared by the panels.
//!
//! Widgets here are presentation-only: they take what they render as fields and
//! own no application state.

mod avatar;
mod disclosure;
mod panel_header;
mod status_chip;

pub use avatar::Avatar;
pub use disclosure::Disclosure;
pub use panel_header::PanelHeader;
pub use status_chip::StatusChip;

use freya::prelude::*;

use crate::theme;

/// A one-pixel horizontal rule in [`theme::BORDER`].
pub fn divider() -> impl IntoElement {
    rect()
        .width(Size::fill())
        .height(Size::px(1.0))
        .background(theme::BORDER)
}
