//! CSS for the Pimble application.
//!
//! The app's own sheet, [`APP_CSS`], is kept as one CSS file per area of the
//! app beside this module and joined here in the order the cascade needs:
//! a later rule wins over an earlier one of the same specificity, so an area
//! moves in the list only on purpose. A new rule goes into the file of the
//! area it styles; a new area is a new file, named in `app_css!` below.

/// Defines [`APP_CSS`] as the area files joined in the order given, and
/// `AREAS` (tests only) as the same files by name, so a test can check each.
macro_rules! app_css {
    ($($file:literal),* $(,)?) => {
        /// Application-wide layout styles.
        pub(crate) const APP_CSS: &str = concat!("\n", $(include_str!($file)),*);

        /// The areas [`APP_CSS`] is made of, by file name, in order.
        #[cfg(test)]
        const AREAS: &[(&str, &str)] = &[$(($file, include_str!($file))),*];
    };
}

app_css!(
    // The header band, the title bar brand, the toolbar and its text colors.
    "toolbar.css",
    // Presence, shares and AI agents.
    "people.css",
    // A Select inside a modal, the sidebar, the explorer tree, context menus.
    "explorer.css",
    // The appearance picker, the Share modal, "Recently Deleted...".
    "dialogs.css",
    // The editor panel and the split view's frame.
    "split_view.css",
    // The Properties panel (docs/PROPERTIES_CONTRACT.md).
    "properties.css",
    // A pane's slot: its title, focus pad and the dividers between panes.
    "panes.css",
    // Discussions (docs/DISCUSSIONS_CONTRACT.md).
    "discussions.css",
    // Sheets (docs/SHEETS_CONTRACT.md).
    "sheets.css",
    // Boards: the board pane, pointers, markup, pictures in text.
    "boards.css",
    // Measuring, the Meaning section, layers and tool buttons on a board.
    "board_tools.css",
    // The canvas's floating chrome: the context bar, its pickers, the wells.
    "canvas_bar.css",
    // The canvas's panels: a small pane's rail, layers, the Stamp's symbols.
    "canvas_panels.css",
    // The editor's content area, margin markers and pictures.
    "editor_pane.css",
    // A metanode's outline and a query metanode's setup.
    "metanodes.css",
    // The empty state, the search bar and its results.
    "search.css",
    // Settings (docs/SETTINGS_CONTRACT.md section 2).
    "settings.css",
    // The status bar.
    "status_bar.css",
    // The link picker and the tooltip over a link (docs/LINKS_CONTRACT.md).
    "links.css",
);

/// Editor content styles for the editor pane.
///
/// rinch's editor ships its own stylesheet, scoped to `[data-pm-editor]`, with a
/// dark scheme keyed on `data-pm-theme="dark"` (set from `editor::start_editing`).
/// These rules ride on top of that scheme with higher specificity and swap its
/// GitHub-dark palette for the app's own theme tokens, so the pane matches the
/// tree and toolbar around it. `document_theme.css` comes first: the look a
/// document theme changes, read from the `--pd-*` properties a pane's theme
/// writes (docs/THEMES_CONTRACT.md section 7), with today's look as every
/// fallback; `editor_content.css` then holds the colors no theme changes.
pub(crate) const EDITOR_CSS: &str = concat!("\n", include_str!("document_theme.css"), include_str!("editor_content.css"));

/// The document theme rules alone, for their tests.
#[cfg(test)]
const DOCUMENT_THEME_CSS: &str = include_str!("document_theme.css");


/// Pimble's variables for the dark scheme, added to rinch's theme sheet
/// (`app::theme_css`). Dark keeps rinch's palette; these name what the toolbar
/// draws with, so both schemes style it from the same rules.
pub(crate) const DARK_THEME_VARS: &str = "
:root {
    --pimble-toolbar-icon: #909296;
    --pimble-toolbar-active-bg: var(--rinch-color-dark-4);
    --pimble-toolbar-active-fg: var(--rinch-primary-color-4);
    --pimble-toolbar-active-border: var(--rinch-primary-color-4);
    --pimble-sheet-ghost: #1d3a5f;
}
";

/// Pimble's light scheme (2026-10-20), added to rinch's theme sheet
/// (`app::theme_css`). rinch's light theme is Mantine's stock grays with a bright
/// blue primary and a blue title bar; this one uses the site's cool neutrals
/// (`site/style.css`) and the brand slate (`assets/brand/README.md`) as the
/// primary, ten shades around `#3f6491` at 6. The chrome (title bar, explorer,
/// pane headers) sits on a cool off-white, and the page being written on is white.
pub(crate) const LIGHT_THEME_VARS: &str = "
:root {
    --rinch-color-body: #f5f7f8;
    --rinch-color-surface: #ffffff;
    --rinch-color-text: #1c2024;
    --rinch-color-dimmed: #5c6570;
    --rinch-color-border: #dde1e5;
    --rinch-color-default-border: #dde1e5;
    --rinch-color-placeholder: #8a929c;
    --rinch-color-default: #eceff2;
    --rinch-color-default-hover: #e2e6ea;
    --rinch-color-filled: #f0f2f4;
    --rinch-color-filled-hover: #e6e9ec;
    --rinch-color-state-disabled: #f0f2f4;
    --rinch-color-option-hover: #edf1f5;
    --rinch-color-option-selected: #e1e8f0;

    --rinch-primary-color: #3f6491;
    --rinch-primary-color-0: #eef2f7;
    --rinch-primary-color-1: #dde5ef;
    --rinch-primary-color-2: #bccbdf;
    --rinch-primary-color-3: #98aecb;
    --rinch-primary-color-4: #7793b8;
    --rinch-primary-color-5: #5a7ba6;
    --rinch-primary-color-6: #3f6491;
    --rinch-primary-color-7: #34557c;
    --rinch-primary-color-8: #2a4665;
    --rinch-primary-color-9: #1f354d;

    --rinch-titlebar-bg: #eaeef1;
    --rinch-titlebar-text: #1c2024;
    --rinch-titlebar-icon: #5c6570;
    --rinch-titlebar-hover: rgba(28, 32, 36, 0.07);
    --rinch-titlebar-active: rgba(28, 32, 36, 0.12);

    --pimble-toolbar-icon: #5c6570;
    --pimble-toolbar-active-bg: #e1e8f0;
    --pimble-toolbar-active-fg: #2a4665;
    --pimble-toolbar-active-border: #bccbdf;
    --pimble-sheet-ghost: #dbe7f5;
}
";

#[cfg(test)]
mod tests {
    use super::*;

    /// What is left of `css` once its comments are taken out, or `None` when
    /// a comment is not closed.
    fn without_comments(css: &str) -> Option<String> {
        let mut out = String::new();
        let mut rest = css;
        while let Some(at) = rest.find("/*") {
            out.push_str(&rest[..at]);
            let end = rest[at + 2..].find("*/")?;
            rest = &rest[at + 2 + end + 2..];
        }
        out.push_str(rest);
        Some(out)
    }

    /// Every area is whole: its comments close, its braces balance and it
    /// ends a line, so no rule or comment runs on into the next area and
    /// joining them changes nothing but where one file ends.
    #[test]
    fn every_area_is_whole_rules() {
        for (file, css) in AREAS {
            assert!(css.ends_with('\n'), "{file} ends a line");
            let rules = without_comments(css).unwrap_or_else(|| panic!("{file} closes its comments"));
            let mut depth = 0i32;
            for c in rules.chars() {
                match c {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                assert!(depth >= 0, "{file} closes a brace it never opened");
            }
            assert_eq!(depth, 0, "{file} closes every brace it opens");
        }
    }

    /// The sheet is the areas in order and nothing else.
    #[test]
    fn the_sheet_is_its_areas_in_order() {
        let joined: String = std::iter::once("\n").chain(AREAS.iter().map(|(_, css)| *css)).collect();
        assert_eq!(APP_CSS, joined);
        assert!(EDITOR_CSS.starts_with('\n') && EDITOR_CSS.contains("[data-pm-editor]"));
    }
}

#[cfg(test)]
mod document_theme_tests;
