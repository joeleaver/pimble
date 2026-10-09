//! Pimble Import - convert external formats into Pimble nodes.
//!
//! Every format parses into the same thing, an [`Imported`] tree: a title,
//! rich blocks and appearance per node, with its children. Parsing touches no
//! store and no disk, so it runs in the browser as well as on the desktop; what
//! the tree becomes is the caller's business (the app writes it through the
//! server or the vault client as ordinary edits, the CLI into a new store on
//! disk with the `store` feature).

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use pimble_core::node_types;
use pimble_crdt::Block;

pub mod docx;
pub mod rtf;
pub mod scrivener;
mod zip;

/// A source's files: the path of each relative to what was picked, `/`
/// separated, to its bytes. A single file is one entry, named by its file
/// name; a Scrivener project is the files inside its `.scriv` directory that
/// [`Format::wants`] keeps.
pub type Files = BTreeMap<String, Vec<u8>>;

/// What can be imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Rtf,
    Docx,
    Scrivener,
}

impl Format {
    /// What the person calls it.
    pub fn label(self) -> &'static str {
        match self {
            Format::Rtf => "RTF",
            Format::Docx => "Word",
            Format::Scrivener => "Scrivener",
        }
    }

    /// The file extensions a picker offers (none for Scrivener, which is a
    /// directory).
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Format::Rtf => &["rtf"],
            Format::Docx => &["docx"],
            Format::Scrivener => &[],
        }
    }

    /// Whether a file inside a picked directory is one the import reads. Only
    /// Scrivener picks a directory; a project's snapshots, backups and media
    /// are never read.
    pub fn wants(self, relative_path: &str) -> bool {
        match self {
            Format::Scrivener => scrivener::wants(relative_path),
            _ => true,
        }
    }
}

/// One node of an import, and the nodes beneath it.
#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    pub title: String,
    /// `document` or `folder`.
    pub node_type: &'static str,
    pub blocks: Vec<Block>,
    /// A Tabler icon name.
    pub icon: Option<String>,
    /// `#rrggbb`, for the icon and title.
    pub color: Option<String>,
    /// `#rrggbb`, for the whole row in the tree.
    pub background: Option<String>,
    pub tags: Vec<String>,
    pub children: Vec<Imported>,
}

impl Imported {
    pub fn document(title: impl Into<String>, blocks: Vec<Block>) -> Self {
        Imported {
            title: title.into(),
            node_type: node_types::DOCUMENT,
            blocks,
            icon: None,
            color: None,
            background: None,
            tags: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn folder(title: impl Into<String>) -> Self {
        Imported { node_type: node_types::FOLDER, ..Self::document(title, Vec::new()) }
    }

    /// How many nodes this is, itself included.
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Imported::count).sum::<usize>()
    }

    /// Whether any block has text in it.
    pub fn has_text(&self) -> bool {
        self.blocks.iter().any(|b| !b.plain_text().trim().is_empty())
    }
}

/// Parse `files` as `format`. `name` is what was picked (a file name, or the
/// `.scriv` directory's), and titles the top node.
pub fn import(format: Format, name: &str, files: &Files) -> Result<Imported> {
    let title = title_from_name(name);
    match format {
        Format::Scrivener => scrivener::parse(&title, files),
        Format::Rtf | Format::Docx => {
            let Some(bytes) = files.get(name).or_else(|| files.values().next()) else {
                bail!("\"{name}\" is empty.");
            };
            let blocks = match format {
                Format::Rtf => {
                    if !bytes.starts_with(b"{\\rtf") {
                        bail!("\"{name}\" is not an RTF file.");
                    }
                    rtf::rtf_to_blocks(bytes)
                }
                _ => docx::docx_to_blocks(bytes).map_err(|e| anyhow::anyhow!("\"{name}\" could not be read as a Word document: {e}"))?,
            };
            Ok(Imported::document(title, blocks))
        }
    }
}

/// A file or directory name without its extension (`Novel.scriv` is
/// "Novel", `Letter.docx` is "Letter").
pub fn title_from_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = match base.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => base,
    };
    if stem.trim().is_empty() { "Imported".to_string() } else { stem.to_string() }
}

/// Read a picked directory from disk, keeping the files `format` wants.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_dir(format: Format, dir: &std::path::Path) -> std::io::Result<Files> {
    fn walk(format: Format, root: &std::path::Path, dir: &std::path::Path, files: &mut Files) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(root) else { continue };
            let relative = relative.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            if entry.file_type()?.is_dir() {
                // Only descend where something wanted can be.
                if format.wants(&format!("{relative}/")) {
                    walk(format, root, &path, files)?;
                }
            } else if format.wants(&relative) {
                files.insert(relative, std::fs::read(&path)?);
            }
        }
        Ok(())
    }
    let mut files = Files::new();
    walk(format, dir, dir, &mut files)?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_drop_the_extension() {
        assert_eq!(title_from_name("Novel.scriv"), "Novel");
        assert_eq!(title_from_name("/home/x/Letter to Sam.docx"), "Letter to Sam");
        assert_eq!(title_from_name("notes"), "notes");
        assert_eq!(title_from_name(".rtf"), ".rtf");
    }

    #[test]
    fn an_rtf_file_is_one_document() {
        let mut files = Files::new();
        files.insert("Hello.rtf".into(), br"{\rtf1\ansi Hello {\b there}\par}".to_vec());
        let imported = import(Format::Rtf, "Hello.rtf", &files).unwrap();
        assert_eq!(imported.title, "Hello");
        assert_eq!(imported.node_type, node_types::DOCUMENT);
        assert!(imported.has_text());
        assert_eq!(imported.count(), 1);
    }

    #[test]
    fn a_file_that_is_not_rtf_says_so() {
        let mut files = Files::new();
        files.insert("x.rtf".into(), b"plain words".to_vec());
        let e = import(Format::Rtf, "x.rtf", &files).unwrap_err().to_string();
        assert!(e.contains("not an RTF file"), "{e}");
    }
}
