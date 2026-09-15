//! Scrivener project import
//!
//! Parses a `.scriv` project directory containing a `.scrivx` manifest
//! and converts it into a Pimble store.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use pimble_core::Node;
use pimble_crdt::ContentDoc;
use pimble_store::LocalStore;
use quick_xml::events::Event;
use quick_xml::Reader;
use tokio::fs;
use tracing::info;

use crate::rtf::rtf_to_blocks;

/// A parsed binder item from the .scrivx file
#[derive(Debug)]
struct BinderItem {
    uuid: String,
    title: String,
    item_type: String, // "Text", "Folder", "DraftFolder", "Other"
    children: Vec<BinderItem>,
}

/// Import a Scrivener `.scriv` project into a new Pimble store.
///
/// - `scriv_path`: path to the `.scriv` directory
/// - `output_path`: where to create the `.pimble` store
pub async fn import_scrivener(scriv_path: &Path, output_path: &Path) -> Result<()> {
    // Find the .scrivx manifest
    let scrivx_path = find_scrivx(scriv_path).await?;
    let data_dir = scriv_path.join("Files").join("Data");

    info!("Parsing Scrivener manifest: {}", scrivx_path.display());

    // Parse the binder tree from .scrivx
    let xml = fs::read_to_string(&scrivx_path).await?;
    let binder_items = parse_scrivx(&xml)?;

    // Derive store name from directory name
    let store_name = scriv_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported Store");

    info!("Creating Pimble store: {}", output_path.display());

    // Create the Pimble store
    let mut store = LocalStore::create(output_path, store_name).await?;
    let root_id = store.root_node_id();

    // Recursively create nodes from binder items
    let mut stats = ImportStats::default();
    for item in &binder_items {
        import_binder_item(&mut store, root_id, item, &data_dir, &mut stats).await?;
    }

    // Flush everything to disk
    store.flush().await?;

    info!(
        "Import complete: {} folders, {} documents ({} with content); {} paragraphs, {} headings, {} lists, {} marked runs",
        stats.folders, stats.documents, stats.with_content, stats.paragraphs, stats.headings, stats.lists, stats.marked_runs
    );

    Ok(())
}

#[derive(Default)]
struct ImportStats {
    folders: usize,
    documents: usize,
    with_content: usize,
    paragraphs: usize,
    headings: usize,
    lists: usize,
    marked_runs: usize,
}

impl ImportStats {
    fn count_blocks(&mut self, blocks: &[pimble_crdt::Block]) {
        use pimble_crdt::Block;
        for block in blocks {
            match block {
                Block::Paragraph { runs, .. } => {
                    self.paragraphs += 1;
                    self.marked_runs += runs.iter().filter(|r| !r.marks.is_empty()).count();
                }
                Block::Heading { runs, .. } => {
                    self.headings += 1;
                    self.marked_runs += runs.iter().filter(|r| !r.marks.is_empty()).count();
                }
                Block::CodeBlock { .. } => self.paragraphs += 1,
                Block::BulletList { items } | Block::OrderedList { items, .. } => {
                    self.lists += 1;
                    for item in items {
                        self.count_blocks(&item.blocks);
                    }
                }
            }
        }
    }
}

/// Recursively import a BinderItem and its children into the store.
async fn import_binder_item(
    store: &mut LocalStore,
    parent_id: pimble_core::NodeId,
    item: &BinderItem,
    data_dir: &Path,
    stats: &mut ImportStats,
) -> Result<()> {
    let is_folder = matches!(item.item_type.as_str(), "Folder" | "DraftFolder");

    let node = if is_folder {
        stats.folders += 1;
        Node::folder(&item.title)
    } else {
        stats.documents += 1;
        Node::document(&item.title)
    };

    let node_id = store.create_node(node, Some(parent_id)).await?;

    // The item's RTF, as rich blocks: paragraphs with their marks (bold, italic,
    // underline, strike, link, colour, highlight, code, sub/superscript),
    // headings, nested bullet and ordered lists, alignment and indent. The
    // node has no content yet, so seeding it with a whole document is right
    // here (see CLAUDE.md on `updateNodeContent`).
    let rtf_path = data_dir.join(&item.uuid).join("content.rtf");
    if rtf_path.exists() {
        if let Ok(rtf_bytes) = fs::read(&rtf_path).await {
            let blocks = rtf_to_blocks(&rtf_bytes);
            let has_text = blocks.iter().any(|b| !b.plain_text().trim().is_empty());
            if has_text {
                let content_bytes = ContentDoc::from_blocks(&blocks)
                    .with_context(|| format!("building content for {} ({})", item.title, item.uuid))?
                    .save();
                store.update_node_content(node_id, content_bytes).await?;
                stats.with_content += 1;
                stats.count_blocks(&blocks);
            }
        }
    }

    // Recurse into children
    for child in &item.children {
        Box::pin(import_binder_item(store, node_id, child, data_dir, stats)).await?;
    }

    Ok(())
}

/// Find the .scrivx file inside a .scriv directory.
async fn find_scrivx(scriv_path: &Path) -> Result<PathBuf> {
    let mut entries = fs::read_dir(scriv_path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "scrivx") {
            return Ok(path);
        }
    }
    bail!(
        "No .scrivx file found in {}",
        scriv_path.display()
    );
}

/// Parse the .scrivx XML and extract the binder tree.
fn parse_scrivx(xml: &str) -> Result<Vec<BinderItem>> {
    let mut reader = Reader::from_str(xml);

    // Navigate to <Binder>
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.name().as_ref() == b"Binder" => break,
            Event::Eof => bail!("No <Binder> element found in .scrivx"),
            _ => {}
        }
    }

    // Parse top-level BinderItems inside <Binder>
    parse_children(&mut reader, b"Binder")
}

/// Parse BinderItem elements until we hit the closing tag for `parent_tag`.
fn parse_children(reader: &mut Reader<&[u8]>, parent_tag: &[u8]) -> Result<Vec<BinderItem>> {
    let mut items = Vec::new();

    loop {
        match reader.read_event()? {
            Event::Start(e) if e.name().as_ref() == b"BinderItem" => {
                let item = parse_binder_item(reader, &e)?;
                items.push(item);
            }
            Event::End(e) if e.name().as_ref() == parent_tag => break,
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(items)
}

/// Parse a single <BinderItem> element (already consumed the Start event).
fn parse_binder_item(
    reader: &mut Reader<&[u8]>,
    start: &quick_xml::events::BytesStart,
) -> Result<BinderItem> {
    // Extract attributes
    let mut uuid = String::new();
    let mut item_type = String::new();

    for attr in start.attributes() {
        let attr = attr?;
        match attr.key.as_ref() {
            b"UUID" => uuid = String::from_utf8_lossy(&attr.value).to_string(),
            b"Type" => item_type = String::from_utf8_lossy(&attr.value).to_string(),
            _ => {}
        }
    }

    let mut title = String::new();
    let mut children = Vec::new();

    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.name().as_ref() {
                b"Title" => {
                    title = reader
                        .read_text(e.name())
                        .context("reading Title text")?
                        .to_string();
                }
                b"Children" => {
                    children = parse_children(reader, b"Children")?;
                }
                _ => {
                    // Skip unknown elements
                    skip_element(reader, e.name().as_ref())?;
                }
            },
            Event::End(e) if e.name().as_ref() == b"BinderItem" => break,
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(BinderItem {
        uuid,
        title,
        item_type,
        children,
    })
}

/// Skip an element and all its nested content.
fn skip_element(reader: &mut Reader<&[u8]>, tag_name: &[u8]) -> Result<()> {
    let mut depth = 1i32;
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.name().as_ref() == tag_name => depth += 1,
            Event::End(e) if e.name().as_ref() == tag_name => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Builds a minimal `.scriv` project (one text document with RTF content)
    /// in a tempdir, imports it, and checks the resulting store's tree and
    /// content survive the round trip through the yrs content model.
    #[tokio::test]
    async fn imports_a_minimal_scrivener_project() {
        let root = tempdir().unwrap();
        let uuid = "11111111-1111-1111-1111-111111111111";
        let scriv_path = root.path().join("MyBook.scriv");
        let data_dir = scriv_path.join("Files").join("Data").join(uuid);
        fs::create_dir_all(&data_dir).await.unwrap();

        let scrivx = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ScrivenerProject>
  <Binder>
    <BinderItem UUID="{uuid}" Type="Text">
      <Title>Chapter One</Title>
    </BinderItem>
  </Binder>
</ScrivenerProject>"#
        );
        fs::write(scriv_path.join("MyBook.scrivx"), scrivx).await.unwrap();

        let rtf = br"{\rtf1\ansi\deff0 Hello {\b RTF} World\par\pard\ls1\ilvl0{\listtext	\u9679\'3F	}{\f0 an item}\par}";
        fs::write(data_dir.join("content.rtf"), rtf).await.unwrap();

        let output_path = root.path().join("MyBook.pimble");
        import_scrivener(&scriv_path, &output_path).await.unwrap();

        // Reopen from disk to prove the import persisted, not just left an
        // in-memory store looking right.
        let mut store = LocalStore::open(&output_path).await.unwrap();
        let root_id = store.root_node_id();
        let root_node = store.get_node(root_id).await.unwrap();
        assert_eq!(root_node.children.len(), 1, "expected one imported document");

        let child_id = root_node.children[0];
        let child = store.get_node(child_id).await.unwrap();
        assert_eq!(child.metadata.title, "Chapter One");
        assert_eq!(child.node_type, pimble_core::node_types::DOCUMENT);

        let text = ContentDoc::text_of(&child.content);
        assert!(
            text.contains("Hello RTF World"),
            "expected imported content to contain the RTF text, got {:?}",
            text
        );
        // The formatting made it into the CRDT, not only the text: a bold run
        // and a bullet list (the `\listtext` bullet itself is not in the text).
        let units = ContentDoc::load(&child.content).unwrap().units();
        assert_eq!(units.len(), 2, "{units:?}");
        assert!(matches!(units[1].kind, pimble_core::UnitKind::Other(ref k) if k == "bullet_list"), "{:?}", units[1]);
        assert_eq!(units[1].text, "an item");
        assert!(!text.contains('\u{25CF}'), "the bullet glyph must not be imported as text: {text:?}");
    }
}
