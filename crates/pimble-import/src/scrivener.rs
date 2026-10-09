//! Scrivener project import
//!
//! Parses a `.scriv` project's `.scrivx` manifest and the RTF of each binder
//! item into an [`Imported`] tree: one node named after the project, the
//! binder beneath it. With the `store` feature, [`import_scrivener`] writes
//! that tree into a new Pimble store on disk (the CLI's `import-scrivener`).

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::rtf::rtf_to_blocks;
use crate::{Files, Imported};

/// A parsed binder item from the .scrivx file
#[derive(Debug)]
struct BinderItem {
    uuid: String,
    title: String,
    item_type: String, // "Text", "Folder", "DraftFolder", "Other"
    /// `<MetaData><LabelID>`: the project label (color + name) on this item.
    label_id: Option<i32>,
    /// `<MetaData><IconFileName>`: Scrivener's custom icon, e.g. "To Do (Ticked)".
    icon_name: Option<String>,
    children: Vec<BinderItem>,
}

/// A project label from `<LabelSettings>`: its name and color (`#rrggbb`).
#[derive(Debug, Clone)]
struct Label {
    name: String,
    color: Option<String>,
}

/// Whether a path inside a `.scriv` directory (relative, `/` separated; a
/// directory ends in `/`) is one the import reads: the manifest at the top,
/// and each item's `Files/Data/{uuid}/content.rtf`.
pub fn wants(relative_path: &str) -> bool {
    let p = relative_path;
    (!p.contains('/') && p.ends_with(".scrivx"))
        || p == "Settings/"
        || p == UI_SETTINGS
        || p == "Files/"
        || p == "Files/Data/"
        || (p.starts_with("Files/Data/") && (p.ends_with("/content.rtf") || (p.ends_with('/') && p.matches('/').count() == 3)))
}

/// Parse a Scrivener project's files into one folder titled `title` holding
/// its binder.
pub fn parse(title: &str, files: &Files) -> Result<Imported> {
    let Some((manifest, bytes)) = files.iter().find(|(path, _)| !path.contains('/') && path.ends_with(".scrivx")) else {
        bail!("This is not a Scrivener project: there is no .scrivx file at its top.");
    };
    tracing::info!("Parsing Scrivener manifest: {manifest}");
    let xml = String::from_utf8_lossy(bytes);
    let binder_items = parse_scrivx(&xml)?;
    let labels = parse_labels(&xml)?;

    let display = files.get(UI_SETTINGS).map(|xml| label_display(&String::from_utf8_lossy(xml))).unwrap_or_default();

    let mut stats = ImportStats::default();
    let mut project = Imported::folder(title);
    // Scrivener's Trash and the "Recovered Files" folders it makes after a
    // crash are its own housekeeping, not the person's binder (Joe, 2026-10-09).
    project.children = binder_items
        .iter()
        .filter(|item| !is_housekeeping(item))
        .map(|item| convert(item, files, &labels, display, &mut stats))
        .collect();

    tracing::info!(
        "Parsed: {} folders, {} documents ({} with content); {} paragraphs, {} headings, {} lists, {} tables, {} marked runs; {} labelled, {} with icons",
        stats.folders, stats.documents, stats.with_content, stats.paragraphs, stats.headings, stats.lists, stats.tables, stats.marked_runs, stats.labelled, stats.with_icon
    );
    Ok(project)
}

/// Import a Scrivener `.scriv` project into a new Pimble store on disk, its
/// binder at the store's top.
///
/// - `scriv_path`: path to the `.scriv` directory
/// - `output_path`: where to create the `.pimble` store
#[cfg(feature = "store")]
pub async fn import_scrivener(scriv_path: &std::path::Path, output_path: &std::path::Path) -> Result<()> {
    use pimble_store::LocalStore;

    let files = crate::read_dir(crate::Format::Scrivener, scriv_path)
        .with_context(|| format!("reading {}", scriv_path.display()))?;
    let store_name = scriv_path.file_stem().and_then(|s| s.to_str()).unwrap_or("Imported Store");
    let project = parse(store_name, &files)?;

    tracing::info!("Creating Pimble store: {}", output_path.display());
    let mut store = LocalStore::create(output_path, store_name).await?;
    let root_id = store.root_node_id();
    for item in &project.children {
        write_node(&mut store, root_id, item)?;
    }
    store.flush().await?;
    tracing::info!("Import complete: {} nodes", project.count() - 1);
    Ok(())
}

/// Create `item` and what is under it in a store that nothing else has open.
/// The content goes into the node before it is placed: `create_node` merges
/// `node.content` into the new document as its first update, which is the one
/// way content reaches a document, and right here because nothing else holds
/// any of it yet.
#[cfg(feature = "store")]
fn write_node(store: &mut pimble_store::LocalStore, parent_id: pimble_core::NodeId, item: &Imported) -> Result<()> {
    use pimble_core::Node;
    use pimble_crdt::NodeDoc;

    let mut node = Node::new(item.node_type);
    node.metadata.title = item.title.clone();
    // The binder's title, not the content's first words, as the app's import does.
    node.metadata.custom.insert(pimble_core::custom_keys::EXPLICIT_TITLE.to_string(), true.into());
    node.metadata.set_color(item.color.clone());
    node.metadata.set_background(item.background.clone());
    node.metadata.set_icon(item.icon.clone());
    node.metadata.tags = item.tags.clone();
    if item.has_text() {
        node.content = NodeDoc::from_blocks(&item.blocks)
            .with_context(|| format!("building content for {}", item.title))?
            .save();
    }
    let (node_id, _edit) = store.create_node(node, Some(parent_id))?;
    for child in &item.children {
        write_node(store, node_id, child)?;
    }
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
    tables: usize,
    marked_runs: usize,
    labelled: usize,
    with_icon: usize,
}

impl ImportStats {
    fn count_blocks(&mut self, blocks: &[pimble_crdt::Block]) {
        use pimble_crdt::{Block, Inline};
        for block in blocks {
            match block {
                Block::Paragraph { runs, .. } => {
                    self.paragraphs += 1;
                    self.marked_runs += runs.iter().filter_map(Inline::as_run).filter(|r| !r.marks.is_empty()).count();
                }
                Block::Heading { runs, .. } => {
                    self.headings += 1;
                    self.marked_runs += runs.iter().filter_map(Inline::as_run).filter(|r| !r.marks.is_empty()).count();
                }
                Block::CodeBlock { .. } => self.paragraphs += 1,
                Block::HorizontalRule => {}
                Block::BulletList { items } | Block::OrderedList { items, .. } => {
                    self.lists += 1;
                    for item in items {
                        self.count_blocks(&item.blocks);
                    }
                }
                Block::Blockquote { blocks } => self.count_blocks(blocks),
                Block::Table { rows } => {
                    self.tables += 1;
                    for cell in rows.iter().flat_map(|row| &row.cells) {
                        self.count_blocks(&cell.blocks);
                    }
                }
            }
        }
    }
}

/// A binder item and its children as imported nodes.
/// The project's interface settings, where it says how labels are shown.
const UI_SETTINGS: &str = "Settings/ui-common.xml";

/// Where a project shows its label colors in the binder, from
/// `ui-common.xml`'s `<Labels>`: `<Binder>Yes</Binder>` tints the whole row
/// (full width or behind the title), `<Icons>Yes</Icons>` tints the icon. A
/// project without the file (Scrivener for Mac keeps it elsewhere) tints the
/// icon and title, as the import always did.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LabelDisplay {
    row: bool,
    icon: bool,
}

impl Default for LabelDisplay {
    fn default() -> Self {
        LabelDisplay { row: false, icon: true }
    }
}

fn label_display(xml: &str) -> LabelDisplay {
    let mut reader = Reader::from_str(xml);
    let mut in_labels = false;
    let mut found = None::<LabelDisplay>;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == b"Labels" => in_labels = true,
            Ok(Event::End(e)) if e.name().as_ref() == b"Labels" => break,
            Ok(Event::Start(e)) if in_labels && matches!(e.name().as_ref(), b"Binder" | b"Icons") => {
                let name = e.name().as_ref().to_vec();
                let yes = reader.read_text(e.name()).map(|t| t.trim().eq_ignore_ascii_case("yes")).unwrap_or(false);
                let shown = found.get_or_insert(LabelDisplay { row: false, icon: false });
                if name == b"Binder" {
                    shown.row = yes;
                } else {
                    shown.icon = yes;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    match found {
        // A label shown nowhere in the binder still keeps its color, on the
        // icon, rather than being lost.
        Some(LabelDisplay { row: false, icon: false }) | None => LabelDisplay::default(),
        Some(shown) => shown,
    }
}

/// Scrivener's Trash, and a top-level "Recovered Files" folder.
fn is_housekeeping(item: &BinderItem) -> bool {
    item.item_type == "TrashFolder" || (item.item_type == "Folder" && item.title.starts_with("Recovered Files"))
}

fn convert(item: &BinderItem, files: &Files, labels: &HashMap<i32, Label>, display: LabelDisplay, stats: &mut ImportStats) -> Imported {
    let is_folder = matches!(item.item_type.as_str(), "Folder" | "DraftFolder");

    // The item's RTF, as rich blocks: paragraphs with their marks (bold, italic,
    // underline, strike, link, color, highlight, code, sub/superscript),
    // headings, nested bullet and ordered lists, tables, alignment and indent.
    let blocks = files
        .get(&format!("Files/Data/{}/content.rtf", item.uuid))
        .map(|rtf| rtf_to_blocks(rtf))
        .unwrap_or_default();

    let mut node = if is_folder {
        stats.folders += 1;
        Imported { blocks, ..Imported::folder(&item.title) }
    } else {
        stats.documents += 1;
        Imported::document(&item.title, blocks)
    };
    if node.has_text() {
        stats.with_content += 1;
        stats.count_blocks(&node.blocks);
    } else {
        node.blocks.clear();
    }

    // A Scrivener label is a color with a name: the color goes where the
    // project shows it (the row, the icon and title, or both), the name
    // becomes a tag.
    // Label -1 is Scrivener's "No Label": no color, and not a tag.
    if let Some(label) = item.label_id.filter(|&id| id >= 0).and_then(|id| labels.get(&id)) {
        if display.row {
            node.background = label.color.clone();
        }
        if display.icon {
            node.color = label.color.clone();
        }
        if !label.name.is_empty() && !node.tags.contains(&label.name) {
            node.tags.push(label.name.clone());
        }
        stats.labelled += 1;
    }
    if let Some(icon) = item.icon_name.as_deref().and_then(tabler_icon_for_scrivener_icon) {
        node.icon = Some(icon.to_string());
        stats.with_icon += 1;
    }

    node.children = item.children.iter().map(|child| convert(child, files, labels, display, stats)).collect();
    node
}

/// The Tabler icon name for one of Scrivener's built-in binder icons, by the
/// `<IconFileName>` it records (e.g. "To Do (Ticked)", "Warning.tiff"). Only
/// icons the app's picker offers (`pimble-app/src/appearance.rs`) are mapped;
/// anything else keeps the node type's icon.
fn tabler_icon_for_scrivener_icon(name: &str) -> Option<&'static str> {
    let base = name.trim().trim_end_matches(".tiff").trim_end_matches(".png").to_ascii_lowercase();
    if base.starts_with("to do (ticked)") || base.starts_with("checkmark") || base.starts_with("check") {
        return Some("checkbox");
    }
    let stem = base.split(" (").next().unwrap_or(&base).trim();
    let icon = match stem {
        "to do" => "square",
        "warning" | "caution" => "alert-triangle",
        "important" | "exclamation" => "alert-circle",
        "information" | "info" => "info-circle",
        "question" => "help",
        "idea" | "light bulb" | "lightbulb" => "bulb",
        "star" | "favorite" | "favourite" => "star",
        "heart" => "heart",
        "flag" => "flag",
        "bookmark" => "bookmark",
        "pin" | "pushpin" => "pin",
        "tag" => "tag",
        "book" | "notebook" => "book",
        "notes" | "notepad" | "note" => "notes",
        "character" | "person" => "user",
        "characters" | "people" | "group" => "users",
        "location" | "place" | "setting" | "map" => "map-pin",
        "house" | "home" => "home",
        "building" => "building",
        "briefcase" | "work" => "briefcase",
        "calendar" | "date" => "calendar",
        "clock" | "time" => "clock",
        "phone" | "telephone" => "phone",
        "envelope" | "mail" | "email" => "mail",
        "money" | "cash" | "dollar" => "cash",
        "credit card" => "credit-card",
        "receipt" => "receipt",
        "shopping" | "cart" => "shopping-cart",
        "gift" | "present" => "gift",
        "recipe" | "food" | "cooking" => "chef-hat",
        "coffee" => "coffee",
        "medical" | "health" | "hospital" => "medical-cross",
        "pill" | "medicine" => "pill",
        "car" | "vehicle" => "car",
        "plane" | "travel" | "airplane" => "plane",
        "tool" | "tools" | "wrench" => "tool",
        "key" => "key",
        "lock" | "padlock" => "lock",
        "bug" => "bug",
        "code" => "code",
        "database" => "database",
        "chart" | "graph" => "chart-bar",
        "music" => "music",
        "camera" | "photo" | "picture" => "camera",
        "palette" | "art" => "palette",
        "plant" | "leaf" => "plant",
        "tree" => "tree",
        "paw" | "pet" => "paw",
        "dog" => "dog",
        "cat" => "cat",
        "sun" => "sun",
        "moon" => "moon",
        "umbrella" => "umbrella",
        "link" => "link",
        "rocket" => "rocket",
        "trophy" | "award" => "trophy",
        "target" => "target",
        "archive" | "box" => "archive",
        "pencil" | "edit" => "pencil",
        _ => return None,
    };
    Some(icon)
}

/// Scrivener's label color ("0.137255 0.090196 0.745098", RGB in 0..1) as CSS.
fn label_color_to_css(value: &str) -> Option<String> {
    let mut parts = value.split_whitespace().map(|p| p.parse::<f32>().ok());
    let r = parts.next()??;
    let g = parts.next()??;
    let b = parts.next()??;
    let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b)))
}

/// The project's labels from `<LabelSettings><Labels>`, by id. A label with no
/// color attribute (Scrivener's "No Label") has `color: None`.
fn parse_labels(xml: &str) -> Result<HashMap<i32, Label>> {
    let mut reader = Reader::from_str(xml);
    let mut labels = HashMap::new();
    let mut in_settings = false;
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.name().as_ref() == b"LabelSettings" => in_settings = true,
            Event::End(e) if e.name().as_ref() == b"LabelSettings" => break,
            Event::Start(e) if in_settings && e.name().as_ref() == b"Label" => {
                let mut id = None;
                let mut color = None;
                for attr in e.attributes() {
                    let attr = attr?;
                    match attr.key.as_ref() {
                        b"ID" => id = String::from_utf8_lossy(&attr.value).parse::<i32>().ok(),
                        b"Color" => color = label_color_to_css(&String::from_utf8_lossy(&attr.value)),
                        _ => {}
                    }
                }
                let name = reader.read_text(e.name()).context("reading Label text")?.trim().to_string();
                if let Some(id) = id {
                    labels.insert(id, Label { name, color });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(labels)
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

/// Parse a `<MetaData>` element (Start already consumed) for the item's label id
/// and custom icon name; everything else in it is skipped.
fn parse_metadata(reader: &mut Reader<&[u8]>) -> Result<(Option<i32>, Option<String>)> {
    let mut label_id = None;
    let mut icon_name = None;
    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.name().as_ref() {
                b"LabelID" => {
                    label_id = reader.read_text(e.name()).context("reading LabelID")?.trim().parse::<i32>().ok();
                }
                b"IconFileName" => {
                    let name = reader.read_text(e.name()).context("reading IconFileName")?.trim().to_string();
                    if !name.is_empty() {
                        icon_name = Some(name);
                    }
                }
                _ => skip_element(reader, e.name().as_ref())?,
            },
            Event::End(e) if e.name().as_ref() == b"MetaData" => break,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((label_id, icon_name))
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
    let mut label_id = None;
    let mut icon_name = None;

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
                b"MetaData" => {
                    let (label, icon) = parse_metadata(reader)?;
                    label_id = label;
                    icon_name = icon;
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
        label_id,
        icon_name,
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

#[cfg(all(test, feature = "store"))]
mod tests {
    use super::*;
    use pimble_crdt::NodeDoc;
    use pimble_store::LocalStore;
    use tempfile::tempdir;
    use tokio::fs;

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
      <MetaData>
        <LabelID>3</LabelID>
        <IconFileName>To Do (Ticked)</IconFileName>
      </MetaData>
    </BinderItem>
  </Binder>
  <LabelSettings>
    <Labels>
      <Label ID="-1">No Label</Label>
      <Label ID="3" Color="1.000000 0.000000 0.501961">Important</Label>
    </Labels>
  </LabelSettings>
</ScrivenerProject>"#
        );
        fs::write(scriv_path.join("MyBook.scrivx"), scrivx).await.unwrap();

        let rtf = br"{\rtf1\ansi\deff0 Hello {\b RTF} World\par\pard\ls1\ilvl0{\listtext	\u9679\'3F	}{\f0 an item}\par}";
        fs::write(data_dir.join("content.rtf"), rtf).await.unwrap();

        let output_path = root.path().join("MyBook.pimble");
        import_scrivener(&scriv_path, &output_path).await.unwrap();

        // Reopen from disk to prove the import persisted, not just left an
        // in-memory store looking right.
        let store = LocalStore::open(&output_path).await.unwrap();
        let root_id = store.root_node_id();
        let root_node = store.get_node(root_id).unwrap();
        assert_eq!(root_node.children.len(), 1, "expected one imported document");

        let child_id = root_node.children[0];
        let child = store.get_node(child_id).unwrap();
        assert_eq!(child.metadata.title, "Chapter One");
        assert_eq!(
            child.metadata.custom.get(pimble_core::custom_keys::EXPLICIT_TITLE),
            Some(&serde_json::Value::Bool(true)),
            "the binder title is kept, not replaced by the content's first words"
        );
        assert_eq!(child.node_type, pimble_core::node_types::DOCUMENT);
        assert_eq!(child.metadata.color(), Some("#ff0080"), "the label color becomes the node color");
        assert_eq!(child.metadata.tags, vec!["Important".to_string()], "the label name becomes a tag");
        assert_eq!(child.metadata.icon(), Some("checkbox"), "Scrivener's ticked to-do icon maps to a checkbox");

        let text = NodeDoc::text_of(&child.content);
        assert!(
            text.contains("Hello RTF World"),
            "expected imported content to contain the RTF text, got {:?}",
            text
        );
        // The formatting made it into the CRDT, not only the text: a bold run
        // and a bullet list (the `\listtext` bullet itself is not in the text).
        let units = NodeDoc::load(&child.content).unwrap().units();
        assert_eq!(units.len(), 2, "{units:?}");
        assert!(matches!(units[1].kind, pimble_core::UnitKind::Other(ref k) if k == "bullet_list"), "{:?}", units[1]);
        assert_eq!(units[1].text, "an item");
        assert!(!text.contains('\u{25CF}'), "the bullet glyph must not be imported as text: {text:?}");
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    fn project(binder: &str) -> Files {
        let scrivx = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ScrivenerProject Version="2.0">
  <Binder>{binder}</Binder>
  <LabelSettings>
    <Labels>
      <Label ID="-1">No Label</Label>
      <Label ID="2" Color="0.137255 0.090196 0.745098">TOP LEVEL TOPIC</Label>
      <Label ID="3" Color="0.690196 0.843137 1.000000">First Level Topic</Label>
    </Labels>
  </LabelSettings>
</ScrivenerProject>"#
        );
        let mut files = Files::new();
        files.insert("P.scrivx".into(), scrivx.into_bytes());
        files
    }

    /// Scrivener 3's labels as its Windows edition writes them: every labelled
    /// item takes its label's color and name, and "No Label" (-1) is neither.
    #[test]
    fn labels_color_items_and_no_label_is_nothing() {
        let files = project(
            r#"<BinderItem UUID="A" Type="Folder"><Title>Top</Title><MetaData><LabelID>2</LabelID></MetaData>
                 <Children>
                   <BinderItem UUID="B" Type="Text"><Title>Light</Title><MetaData><LabelID>3</LabelID><IconFileName>Warning.tiff</IconFileName></MetaData></BinderItem>
                   <BinderItem UUID="C" Type="Text"><Title>None</Title><MetaData><LabelID>-1</LabelID></MetaData></BinderItem>
                 </Children>
               </BinderItem>"#,
        );
        let imported = parse("P", &files).unwrap();
        let top = &imported.children[0];
        assert_eq!(top.color.as_deref(), Some("#2317be"));
        assert_eq!(top.tags, vec!["TOP LEVEL TOPIC".to_string()]);
        let light = &top.children[0];
        assert_eq!(light.color.as_deref(), Some("#b0d7ff"));
        assert_eq!(light.icon.as_deref(), Some("alert-triangle"));
        let none = &top.children[1];
        assert_eq!(none.color, None);
        assert!(none.tags.is_empty(), "{:?}", none.tags);
    }

    const UI: &str = r#"<?xml version="1.0"?><UI><Labels>
        <Binder FullWidth="Yes">Yes</Binder>
        <Icons>No</Icons>
        <IndexCards>No</IndexCards>
    </Labels></UI>"#;

    /// A project that shows labels as full-width binder rows and not on icons
    /// imports them as row colors only.
    #[test]
    fn full_width_binder_labels_become_row_colors() {
        let mut files = project(r#"<BinderItem UUID="A" Type="Folder"><Title>Top</Title><MetaData><LabelID>3</LabelID></MetaData></BinderItem>"#);
        files.insert(UI_SETTINGS.into(), UI.as_bytes().to_vec());
        let top = &parse("P", &files).unwrap().children[0];
        assert_eq!(top.background.as_deref(), Some("#b0d7ff"));
        assert_eq!(top.color, None);
        assert_eq!(top.tags, vec!["First Level Topic".to_string()]);

        let both = UI.replace("<Icons>No</Icons>", "<Icons>Yes</Icons>");
        files.insert(UI_SETTINGS.into(), both.into_bytes());
        let top = &parse("P", &files).unwrap().children[0];
        assert_eq!((top.background.as_deref(), top.color.as_deref()), (Some("#b0d7ff"), Some("#b0d7ff")));

        let neither = UI.replace(">Yes</Binder>", ">No</Binder>");
        files.insert(UI_SETTINGS.into(), neither.into_bytes());
        let top = &parse("P", &files).unwrap().children[0];
        assert_eq!((top.background.as_deref(), top.color.as_deref()), (None, Some("#b0d7ff")));
    }

    /// The Trash and crash-recovery folders are not imported; an ordinary
    /// folder that merely mentions recovery is.
    #[test]
    fn trash_and_recovered_files_are_skipped() {
        let files = project(
            r#"<BinderItem UUID="A" Type="DraftFolder"><Title>Draft</Title></BinderItem>
               <BinderItem UUID="B" Type="TrashFolder"><Title>Trash</Title></BinderItem>
               <BinderItem UUID="C" Type="Folder"><Title>Recovered Files (4/10/2023, 7:08 PM)</Title></BinderItem>
               <BinderItem UUID="D" Type="Folder"><Title>Recovery plan</Title></BinderItem>"#,
        );
        let titles: Vec<_> = parse("P", &files).unwrap().children.into_iter().map(|c| c.title).collect();
        assert_eq!(titles, ["Draft", "Recovery plan"]);
    }

    #[test]
    fn the_settings_file_is_read_and_nothing_else_in_settings() {
        assert!(wants("Settings/"));
        assert!(wants("Settings/ui-common.xml"));
        assert!(!wants("Settings/ui.ini"));
        assert!(!wants("Snapshots/"));
    }
}
