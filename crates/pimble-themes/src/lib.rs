//! Pimble's themes as data (docs/THEMES_CONTRACT.md): the document theme's
//! typed tokens and their checks, the built-in themes, the fonts a theme may
//! name, resolution (which theme a node reads in, and a theme with its bases
//! folded in) and the CSS custom properties a theme becomes.
//!
//! Pure data and functions, for the desktop and the browser alike: the app
//! holds the documents and draws what this crate answers.

pub mod color;
pub mod css;
pub mod edit;
pub mod fonts;
pub mod resolve;
pub mod theme;
pub mod tokens;
pub mod value;

pub use color::{ColorRef, Pair, Rgb, Slot};
pub use css::{draw, Drawn};
pub use resolve::{fold, in_force, Builtins, Folded, InForce, Link, Origin, ThemeSource, WithStore};
pub use theme::{builtin, builtin_ids, builtins, Theme, Tokens, BASE, COPIED_FROM, DEFAULT, DOCUMENT};
pub use value::{Refusal, Value};
