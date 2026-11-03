//! Pimble's own rinch components with no app state, drawn alike by the app
//! and by plugins (docs/PLUGINS_CONTRACT.md, docs/PRESENCE_CONTRACT.md
//! "Plugins").
//!
//! - [`presence`]: who is here, as the pane strip and the tree draw it: a
//!   person's initials in their color, an agent's robot, a row's dots, and the
//!   words a hover says.
//! - [`appearance`]: a node's icon by name and the colors it is drawn with on
//!   the current theme.
//!
//! The classes these components carry are styled by [`STYLES`], which the app
//! puts in its stylesheet. A plugin's markup is applied under the app's
//! document, so the same rules style it there.

pub mod appearance;
pub mod presence;

/// The stylesheet for this crate's components; the app includes it in its own.
pub const STYLES: &str = presence::STYLES;
