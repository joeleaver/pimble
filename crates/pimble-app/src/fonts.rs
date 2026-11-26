//! The fonts the desktop carries for document themes
//! (docs/THEMES_CONTRACT.md section 5): every file `pimble_themes::fonts`
//! names, registered with rinch at start (`App::fonts`) under the family
//! name inside it, which is the name a theme's CSS asks for. None claims a
//! generic family (`serif`, `monospace`): text that names no bundled family
//! draws exactly as before. The browser loads the same files through
//! `@font-face` instead (`pimble_themes::fonts::font_face_css`), so they are
//! not compiled into the web build. The files and their licenses are in
//! `assets/fonts/` (README.md there).

use rinch::prelude::AppFont;

/// Each bundled file by its path under `assets/fonts/`, and its bytes.
macro_rules! bundled {
    ($($path:literal),* $(,)?) => {
        /// The bundled files' paths, in the order [`FACES`] holds them.
        #[cfg(test)]
        const PATHS: &[&str] = &[$($path),*];

        /// Every bundled face, for `App::fonts`.
        pub const FACES: &[AppFont] = &[$(AppFont::new(include_bytes!(concat!("../../../assets/fonts/", $path)))),*];
    };
}

bundled!(
    "inter/Inter-Variable.ttf",
    "inter/Inter-Italic-Variable.ttf",
    "source-serif-4/SourceSerif4-Variable.ttf",
    "source-serif-4/SourceSerif4-Italic-Variable.ttf",
    "eb-garamond/EBGaramond-Variable.ttf",
    "eb-garamond/EBGaramond-Italic-Variable.ttf",
    "jetbrains-mono/JetBrainsMono-Variable.ttf",
    "jetbrains-mono/JetBrainsMono-Italic-Variable.ttf",
    "atkinson-hyperlegible/AtkinsonHyperlegible-Regular.ttf",
    "atkinson-hyperlegible/AtkinsonHyperlegible-Bold.ttf",
    "atkinson-hyperlegible/AtkinsonHyperlegible-Italic.ttf",
    "atkinson-hyperlegible/AtkinsonHyperlegible-BoldItalic.ttf",
);

#[cfg(test)]
mod tests {
    use super::*;

    /// The desktop carries exactly the files the theme catalog names, so a
    /// family a theme may name is never missing here and nothing is carried
    /// that no theme can reach.
    #[test]
    fn the_desktop_carries_every_file_the_catalog_names() {
        let named: Vec<&str> = pimble_themes::fonts::bundled_files().map(|(_, file)| file.path).collect();
        assert_eq!(PATHS, named.as_slice());
        assert_eq!(FACES.len(), PATHS.len());
        for face in FACES {
            assert!(face.generics.is_empty(), "a bundled face claims no generic family");
        }
    }
}
