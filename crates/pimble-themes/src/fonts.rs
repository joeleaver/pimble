//! The fonts a theme may name (docs/THEMES_CONTRACT.md section 5): families
//! Pimble provides on both targets, named by family, never by file or URL.
//!
//! Wave 1 holds the core set, bundled with the app (decision 8b), and
//! System, the platform's UI font. The rest of section 5's list arrives with
//! the font library; until then a theme cannot name them.
//!
//! Each family is registered once per target at start under its own family
//! name (the desktop through rinch `App::fonts`, the browser through the
//! `@font-face` rules [`font_face_css`] writes), and a theme's CSS names only
//! that family and its fallbacks, so text draws while a face is loading.

/// What a family is for, which decides its fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Sans,
    /// Made to be read by people with low vision or dyslexia.
    Readable,
    Serif,
    /// A book face: an old-style serif for long reading.
    Book,
    Mono,
    /// The platform's own UI font.
    System,
}

impl Kind {
    /// The CSS generic family drawn while the family is not loaded, or where
    /// it lacks a glyph.
    pub fn fallback(self) -> &'static str {
        match self {
            Kind::Sans | Kind::Readable => "sans-serif",
            Kind::Serif | Kind::Book => "serif",
            Kind::Mono => "monospace",
            Kind::System => "sans-serif",
        }
    }
}

/// One file of a family, under `assets/fonts/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontFile {
    /// The path under `assets/fonts/` (the desktop bundles it from there,
    /// the browser fetches it from `/app/fonts/` + this).
    pub path: &'static str,
    /// The weights the file draws: a variable face's range, or one weight.
    pub weights: (u16, u16),
    pub italic: bool,
}

/// A family a theme may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Family {
    /// What a theme stores (`body.font = "source-serif-4"`).
    pub id: &'static str,
    /// The family name the faces register under, and what menus show.
    pub name: &'static str,
    pub kind: Kind,
    /// The files that draw it; empty for System, which is the platform's.
    pub files: &'static [FontFile],
}

/// The platform's UI font: what rinch's editor draws with when nothing
/// names a font, so a theme that names System, or none, reads as before.
pub const SYSTEM_STACK: &str =
    r#"-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif"#;

/// What code is drawn in when no theme names a font for it: rinch's editor's
/// own monospace stack.
pub const CODE_STACK: &str =
    r#"ui-monospace, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace"#;

const fn file(path: &'static str, weights: (u16, u16), italic: bool) -> FontFile {
    FontFile {
        path,
        weights,
        italic,
    }
}

/// Every family a theme may name, the bundled ones first.
pub const FAMILIES: &[Family] = &[
    Family {
        id: "inter",
        name: "Inter",
        kind: Kind::Sans,
        files: &[
            file("inter/Inter-Variable.ttf", (100, 900), false),
            file("inter/Inter-Italic-Variable.ttf", (100, 900), true),
        ],
    },
    Family {
        id: "source-serif-4",
        name: "Source Serif 4",
        kind: Kind::Serif,
        files: &[
            file(
                "source-serif-4/SourceSerif4-Variable.ttf",
                (200, 900),
                false,
            ),
            file(
                "source-serif-4/SourceSerif4-Italic-Variable.ttf",
                (200, 900),
                true,
            ),
        ],
    },
    Family {
        id: "eb-garamond",
        name: "EB Garamond",
        kind: Kind::Book,
        files: &[
            file("eb-garamond/EBGaramond-Variable.ttf", (400, 800), false),
            file(
                "eb-garamond/EBGaramond-Italic-Variable.ttf",
                (400, 800),
                true,
            ),
        ],
    },
    Family {
        id: "jetbrains-mono",
        name: "JetBrains Mono",
        kind: Kind::Mono,
        files: &[
            file(
                "jetbrains-mono/JetBrainsMono-Variable.ttf",
                (100, 800),
                false,
            ),
            file(
                "jetbrains-mono/JetBrainsMono-Italic-Variable.ttf",
                (100, 800),
                true,
            ),
        ],
    },
    Family {
        id: "atkinson-hyperlegible",
        name: "Atkinson Hyperlegible",
        kind: Kind::Readable,
        files: &[
            file(
                "atkinson-hyperlegible/AtkinsonHyperlegible-Regular.ttf",
                (400, 400),
                false,
            ),
            file(
                "atkinson-hyperlegible/AtkinsonHyperlegible-Bold.ttf",
                (700, 700),
                false,
            ),
            file(
                "atkinson-hyperlegible/AtkinsonHyperlegible-Italic.ttf",
                (400, 400),
                true,
            ),
            file(
                "atkinson-hyperlegible/AtkinsonHyperlegible-BoldItalic.ttf",
                (700, 700),
                true,
            ),
        ],
    },
    Family {
        id: "system",
        name: "System",
        kind: Kind::System,
        files: &[],
    },
];

/// The family a theme names by `id`.
pub fn family(id: &str) -> Option<&'static Family> {
    FAMILIES.iter().find(|family| family.id == id)
}

impl Family {
    /// The `font-family` value that draws it: the family, then its kind's
    /// fallback. System is the platform's stack.
    pub fn css_stack(&self) -> String {
        if self.kind == Kind::System {
            return SYSTEM_STACK.to_string();
        }
        format!("\"{}\", {}", self.name, self.kind.fallback())
    }
}

/// Every bundled file, in [`FAMILIES`]' order.
pub fn bundled_files() -> impl Iterator<Item = (&'static Family, &'static FontFile)> {
    FAMILIES
        .iter()
        .flat_map(|family| family.files.iter().map(move |file| (family, file)))
}

/// The `@font-face` rules that make every bundled family drawable in a
/// browser, each face fetched from `base` + its path the first time text
/// uses it (`base` is where the build serves `assets/fonts/`, ending in
/// `/`). The desktop has no `@font-face` and registers the same files by
/// the same family names instead.
pub fn font_face_css(base: &str) -> String {
    let mut css = String::new();
    for (family, file) in bundled_files() {
        let (from, to) = file.weights;
        let weight = if from == to {
            from.to_string()
        } else {
            format!("{from} {to}")
        };
        let style = if file.italic { "italic" } else { "normal" };
        css.push_str(&format!(
            "@font-face {{ font-family: \"{}\"; src: url(\"{base}{}\") format(\"truetype\"); font-weight: {weight}; font-style: {style}; font-display: swap; }}\n",
            family.name, file.path
        ));
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core set of decision 8b is bundled, each family has a regular
    /// face, and every id is distinct and plain (it is what a theme stores).
    #[test]
    fn the_core_set_is_bundled() {
        for id in [
            "inter",
            "source-serif-4",
            "eb-garamond",
            "jetbrains-mono",
            "atkinson-hyperlegible",
        ] {
            let family = family(id).unwrap_or_else(|| panic!("{id} is in the catalog"));
            assert!(
                family
                    .files
                    .iter()
                    .any(|f| !f.italic && f.weights.0 <= 400 && 400 <= f.weights.1),
                "{id} has a regular face"
            );
            assert!(
                family.files.iter().any(|f| f.italic),
                "{id} has an italic face"
            );
        }
        let mut ids: Vec<_> = FAMILIES.iter().map(|f| f.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), FAMILIES.len());
        for f in FAMILIES {
            assert!(
                f.id.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{}",
                f.id
            );
            assert!(!f.name.contains('"') && !f.name.contains('\\'));
        }
    }

    #[test]
    fn a_stack_names_the_family_then_its_fallback() {
        assert_eq!(
            family("eb-garamond").unwrap().css_stack(),
            "\"EB Garamond\", serif"
        );
        assert_eq!(
            family("jetbrains-mono").unwrap().css_stack(),
            "\"JetBrains Mono\", monospace"
        );
        assert_eq!(family("system").unwrap().css_stack(), SYSTEM_STACK);
    }

    #[test]
    fn every_bundled_file_has_a_face_rule() {
        let css = font_face_css("/app/fonts/");
        assert_eq!(css.matches("@font-face").count(), bundled_files().count());
        assert!(css.contains("font-family: \"Inter\"; src: url(\"/app/fonts/inter/Inter-Variable.ttf\") format(\"truetype\"); font-weight: 100 900; font-style: normal;"));
        assert!(css.contains("AtkinsonHyperlegible-BoldItalic.ttf\") format(\"truetype\"); font-weight: 700; font-style: italic;"));
    }

    /// Every file the catalog names is in `assets/fonts`, with its license.
    #[test]
    fn every_file_is_in_the_assets() {
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
        for (_, file) in bundled_files() {
            let path = assets.join(file.path);
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(
                &bytes[..4],
                &[0, 1, 0, 0],
                "{} is a TrueType file",
                file.path
            );
            let license = path.parent().unwrap().join("OFL.txt");
            assert!(
                std::fs::read_to_string(&license)
                    .unwrap()
                    .contains("SIL Open Font License"),
                "{}",
                license.display()
            );
        }
    }
}
