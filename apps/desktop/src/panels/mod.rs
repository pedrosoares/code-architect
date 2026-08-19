//! The panels that make up the shell.
//!
//! Each panel holds its own placeholder data. That is deliberate: when
//! `architect-core` lands, a panel starts reading `Event`-fed signals and only
//! that panel changes — there is no shared state blob to untangle first.

use freya::prelude::State;

mod composer;
mod inspector;
mod sessions;
mod settings;
mod transcript;

pub use composer::Composer;
pub use inspector::InspectorPanel;
pub use sessions::SessionsPanel;
pub use settings::SettingsPanel;
pub use transcript::TranscriptPanel;

/// Cross-panel signal: the Inspector's Tools tab writes a tool call's id
/// here to ask the Transcript to scroll to and expand it. `None` most of
/// the time; the Transcript clears it back to `None` once it acts on it, so
/// clicking the same call again later still triggers a fresh scroll.
#[derive(Clone, Copy)]
pub struct ScrollToToolCall(pub State<Option<String>>);

/// Whether the Sessions sidebar is collapsed to a narrow strip — read by
/// `Shell` to decide what to put in that `ResizablePanel` slot, written by
/// `SessionsPanel`'s own header button. `Shell` owns the state (it decides
/// panel layout); the panel itself only ever toggles it.
#[derive(Clone, Copy)]
pub struct SessionsCollapsed(pub State<bool>);

/// The `InspectorPanel` analog of [`SessionsCollapsed`].
#[derive(Clone, Copy)]
pub struct InspectorCollapsed(pub State<bool>);
