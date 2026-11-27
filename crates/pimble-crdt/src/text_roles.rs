//! The roles a block of a note's text can be given (docs/THEMES_CONTRACT.md
//! "Subtitles and pictures"). A role is an app data attribute rinch keeps on
//! the block (`data-role`, joeleaver/rinch#3093): it says what the block is,
//! and the document theme in force says how that looks.

/// The block attribute naming a block's role.
pub const ROLE_ATTR: &str = "data-role";

/// A paragraph that belongs to the title above it: a subtitle or a byline.
pub const SUBTITLE: &str = "subtitle";

/// Whether `node` is a paragraph with the subtitle role.
pub fn is_subtitle(node: &rinch_editor_core::Node) -> bool {
    node.type_name() == "paragraph" && node.attrs().get_str(ROLE_ATTR) == Some(SUBTITLE)
}
