//! A node's content as Markdown (docs/MCP_CONTRACT.md): reading it whole or in
//! part, and the edits that touch only the blocks they name, each an edit of
//! the content's existing history answering the delta it applied.

use std::rc::Rc;

use rinch_editor_core::Schema;
use yrs::{Map, Transact};

use super::split::data_keys_named;
use super::NodeDoc;
use crate::content_doc::{model_edit_delta, model_edit_steps, model_of};
use crate::error::{CrdtError, Result};
use crate::markdown::{self, BlockTarget, Echo, HeadingTarget, InsertAt, MarkdownEdit, Quote};

impl NodeDoc {
    // ── Content as Markdown (docs/MCP_CONTRACT.md) ───────────────────────
    //
    // Each edit below is an edit of the content's existing history, like
    // `append_blocks`: it answers the delta it applied, and touches only the
    // blocks it names, so whatever anyone else typed elsewhere merges beside
    // it. A refusal (`CrdtError::Refused`) changes nothing.

    /// The content as Markdown; empty when nothing was ever written. See
    /// [`markdown::render`] for what Markdown cannot say yet.
    pub fn markdown(&self) -> Result<String> {
        let seed = !self.has_projection();
        let model = model_of(&self.save(), seed)?;
        Ok(if markdown::is_blank(&model) { String::new() } else { markdown::render(&model) })
    }

    /// The first content of a node nothing was written in yet; refused when it
    /// has content.
    pub fn write_markdown(&mut self, md: &str) -> Result<Vec<u8>> {
        self.markdown_edit(|schema, doc| markdown::write_new(schema, doc, md))
    }

    /// `md` after the content (instead of it, when the content is blank).
    pub fn append_markdown(&mut self, md: &str) -> Result<Vec<u8>> {
        self.markdown_edit(|schema, doc| markdown::append(schema, doc, md))
    }

    /// Make `edit` (docs/MCP_CONTRACT.md "Edits that touch only what they
    /// name"): the delta it applied, and what it took out or replaced, from
    /// the content before it. A refusal changes nothing.
    pub fn edit_markdown(&mut self, edit: MarkdownEdit<'_>) -> Result<(Vec<u8>, Vec<Echo>)> {
        let seed = !self.has_projection();
        let held = self.held_canvases();
        let mut echoes = Vec::new();
        let delta = model_edit_steps(&self.save(), seed, |schema, doc| {
            let (steps, said) = markdown::steps(schema, doc, edit)?;
            if let Some(last) = steps.last() {
                markdown::check_canvases(doc, last, &held)?;
            }
            echoes = said;
            Ok(steps)
        })?;
        Ok((self.apply_markdown_delta(delta)?, echoes))
    }

    /// What `edit` would take out or replace, judged exactly as
    /// [`NodeDoc::edit_markdown`] judges it (the same refusals, "That would
    /// change nothing" included), with nothing changed: the dry run.
    pub fn preview_markdown(&self, edit: MarkdownEdit<'_>) -> Result<Vec<Echo>> {
        let schema = Rc::new(Schema::starter_kit());
        let doc = self.content_model()?;
        let (steps, echoes) = markdown::steps(&schema, &doc, edit)?;
        match steps.last() {
            Some(last) if *last != doc => {
                markdown::check_canvases(&doc, last, &self.held_canvases())?;
                Ok(echoes)
            }
            _ => Err(nothing_changes()),
        }
    }

    /// `md` after the top-level block `quote` names, or with `after_section`
    /// after the section under the heading it names. The quote must appear
    /// exactly once: a caller of this offers nothing else to choose by, so a
    /// refusal of one that appears several times asks for more of the text.
    pub fn insert_markdown_after(&mut self, quote: &str, after_section: bool, md: &str) -> Result<Vec<u8>> {
        let at = if after_section { InsertAt::Section(HeadingTarget::Name(quote)) } else { InsertAt::Block(BlockTarget::quote(quote)) };
        Ok(self.edit_markdown(MarkdownEdit::InsertAfter { at, markdown: md })?.0)
    }

    /// The blocks under the heading `heading` names, up to the next heading of
    /// its level or higher, replaced by `md`. The heading itself stays.
    pub fn replace_section_markdown(&mut self, heading: &str, md: &str) -> Result<Vec<u8>> {
        Ok(self.edit_markdown(MarkdownEdit::ReplaceSection { heading: HeadingTarget::Name(heading), markdown: md })?.0)
    }

    /// The one occurrence of `quote` in the content replaced by `text` (plain
    /// text, taking the marks of the quote's first character). Deleting (an
    /// empty `text`) by a short or number-like quote is refused; quote the
    /// words either side and give them back instead.
    pub fn replace_text(&mut self, quote: &str, text: &str) -> Result<Vec<u8>> {
        Ok(self.edit_markdown(MarkdownEdit::ReplaceText { quote: Quote::alone(quote), text })?.0)
    }

    /// The top-level block `quote` names replaced by `md`, or removed when `md`
    /// is empty (one heading removed or re-levelled, a paragraph made a list).
    /// Removing by a short or number-like quote is refused
    /// (`markdown::MIN_DELETE_QUOTE`); name the block by its number instead
    /// ([`NodeDoc::edit_markdown`] with `BlockTarget::index`).
    pub fn replace_block_markdown(&mut self, quote: &str, md: &str) -> Result<Vec<u8>> {
        Ok(self.edit_markdown(MarkdownEdit::ReplaceBlock { block: BlockTarget::quote(quote), markdown: md })?.0)
    }

    /// The heading `heading` names and everything under it removed. A short or
    /// number-like name that matches only part of the heading's text is
    /// refused; the heading's whole text is always sure.
    pub fn remove_section(&mut self, heading: &str) -> Result<Vec<u8>> {
        Ok(self.edit_markdown(MarkdownEdit::RemoveSection { heading: HeadingTarget::Name(heading) })?.0)
    }

    // ── Reading part of the content, and splitting it by its headings ──

    /// The content's top-level model (a never-written document reads as one
    /// empty paragraph).
    fn content_model(&self) -> Result<rinch_editor_core::Node> {
        model_of(&self.save(), !self.has_projection())
    }

    /// The content's outline: every top-level heading with the size of its
    /// section (heading included) in characters of Markdown, and the sizes of
    /// what comes before the first heading and of the whole.
    pub fn outline(&self) -> Result<Outline> {
        let schema = rinch_editor_core::Schema::starter_kit();
        let model = self.content_model()?;
        if markdown::is_blank(&model) {
            return Ok(Outline::default());
        }
        let headings = markdown::headings(&model);
        let size = |from: usize, to: usize| -> Result<usize> { Ok(markdown::blocks_markdown(&schema, &model, from, to)?.chars().count()) };
        let mut sections = Vec::with_capacity(headings.len());
        for h in headings {
            let chars = size(h.index, h.end)?;
            sections.push((h, chars));
        }
        let first = sections.first().map_or(model.child_count(), |(h, _)| h.index);
        Ok(Outline { intro_chars: size(0, first)?, total_chars: size(0, model.child_count())?, blocks: model.child_count(), sections })
    }

    /// The heading `name` names ([`markdown::find_heading`]: some of its text,
    /// or a heading path) and its section, heading included, as Markdown.
    pub fn section_markdown(&self, name: &str) -> Result<(markdown::Heading, String)> {
        let schema = rinch_editor_core::Schema::starter_kit();
        let model = self.content_model()?;
        let heading = markdown::find_heading(&model, name)?;
        let md = markdown::blocks_markdown(&schema, &model, heading.index, heading.end)?;
        Ok((heading, md))
    }

    /// The top-level blocks `from..=to` (0-based, as the outline numbers
    /// them) as Markdown, and how many blocks the content has.
    pub fn blocks_markdown(&self, from: usize, to: usize) -> Result<(String, usize)> {
        let schema = rinch_editor_core::Schema::starter_kit();
        let model = self.content_model()?;
        let count = if markdown::is_blank(&model) { 0 } else { model.child_count() };
        if from >= count || to < from {
            return Err(CrdtError::Refused(match count {
                0 => "This node has no content, so it has no blocks.".to_string(),
                n => format!("This node has blocks 0 to {}; give `blocks` as \"first-last\" inside that.", n - 1),
            }));
        }
        Ok((markdown::blocks_markdown(&schema, &model, from, to.saturating_add(1))?, count))
    }

    /// What splitting the content at its headings of `level` (the highest
    /// level present when `None`) makes: one part per such heading, its title
    /// the heading's text, its Markdown the section under the heading (the
    /// heading itself left out), and the `data` keys of the canvases and
    /// sheets in it. Refused when there is no heading to split at.
    pub fn split_parts(&self, level: Option<i64>) -> Result<SplitParts> {
        let schema = rinch_editor_core::Schema::starter_kit();
        let model = self.content_model()?;
        let headings = markdown::headings(&model);
        let level = match level.or_else(|| headings.iter().map(|h| h.level).min()) {
            Some(l) => l,
            None => return Err(CrdtError::Refused("This note has no headings, so there is nothing to split it at.".into())),
        };
        let mut parts = Vec::new();
        for h in headings.iter().filter(|h| h.level == level) {
            let body = rinch_editor_core::Node::clone(&model).copy_with_content(rinch_editor_core::Fragment::from_children(
                model.content().children()[h.index + 1..h.end].to_vec(),
            ));
            parts.push(SplitPart {
                title: h.text.replace(markdown_atom(), "").trim().to_string(),
                markdown: markdown::blocks_markdown(&schema, &model, h.index + 1, h.end)?,
                keys: data_keys_named(&body),
                heading: h.clone(),
            });
        }
        if parts.is_empty() {
            return Err(CrdtError::Refused(format!("This note has no level-{level} headings to split it at.")));
        }
        Ok(SplitParts { level, parts })
    }

    /// Take the sections [`NodeDoc::split_parts`] names out of the content
    /// (each heading of `level` and what is under it), as one edit: whatever
    /// else the content holds stays, and `links` (Markdown, such as a list of
    /// links to where the sections went) goes where the first one was. The
    /// `data` of canvases and sheets no longer in the text goes too.
    pub fn cut_parts(&mut self, level: i64, links: Option<&str>) -> Result<Vec<u8>> {
        let before = self.content_model()?;
        let delta = self.markdown_edit(|schema, doc| markdown::cut_sections(schema, doc, level, links))?;
        let after = self.content_model()?;
        let kept = data_keys_named(&after);
        let gone: Vec<String> = data_keys_named(&before).into_iter().filter(|k| !kept.contains(k)).collect();
        if !gone.is_empty() {
            let mut txn = self.doc.transact_mut();
            for key in &gone {
                self.data.remove(&mut txn, key);
            }
        }
        Ok(delta)
    }

    /// The whole content replaced by `md`, recorded as the smallest set of block
    /// edits: the blocks that read the same stay untouched (see
    /// [`markdown::replace_content`]). The node keeps its id and its link.
    pub fn replace_content_markdown(&mut self, md: &str) -> Result<Vec<u8>> {
        Ok(self.edit_markdown(MarkdownEdit::ReplaceContent { markdown: md })?.0)
    }

    /// Take every `embed` block whose attribute `attr` is `value` out of the
    /// content, at any depth (an app's atom whose thing has moved elsewhere).
    /// Refused as "nothing to change" when there is none.
    pub fn remove_embeds(&mut self, attr: &str, value: &str) -> Result<Vec<u8>> {
        fn without(node: &rinch_editor_core::Node, attr: &str, value: &str) -> rinch_editor_core::Node {
            if node.is_leaf() || node.is_textblock() {
                return node.clone();
            }
            let children: Vec<rinch_editor_core::Node> = node
                .content()
                .iter()
                .filter(|c| !(c.type_name() == "embed" && c.attrs().get_str(attr) == Some(value)))
                .map(|c| without(c, attr, value))
                .collect();
            node.copy_with_content(rinch_editor_core::Fragment::from_children(children))
        }
        let (attr, value) = (attr.to_string(), value.to_string());
        self.markdown_edit(move |schema, doc| {
            let after = without(doc, &attr, &value);
            if after.content().child_count() == 0 {
                // A document needs a block: an empty paragraph.
                let p = schema.branch("paragraph", rinch_editor_core::Fragment::empty()).map_err(|e| CrdtError::Collab(e.to_string()))?;
                return Ok(after.copy_with_content(rinch_editor_core::Fragment::from_node(p)));
            }
            Ok(after)
        })
    }

    pub(super) fn markdown_edit(
        &mut self,
        edit: impl FnOnce(&std::rc::Rc<rinch_editor_core::Schema>, &rinch_editor_core::Node) -> Result<rinch_editor_core::Node>,
    ) -> Result<Vec<u8>> {
        let seed = !self.has_projection();
        let held = self.held_canvases();
        let delta = model_edit_delta(&self.save(), seed, |schema, doc| {
            let after = edit(schema, doc)?;
            markdown::check_canvases(doc, &after, &held)?;
            Ok(after)
        })?;
        self.apply_markdown_delta(delta)
    }

    /// The ids of the canvases of pictures in this node's text: the keys
    /// `board:<id>` of its `data` (docs/CANVAS_CONTRACT.md).
    pub fn held_canvases(&self) -> std::collections::HashSet<String> {
        let txn = self.doc.transact();
        self.data
            .keys(&txn)
            .filter_map(|k| k.strip_prefix("board:").map(str::to_string))
            .collect()
    }

    fn apply_markdown_delta(&mut self, delta: Vec<u8>) -> Result<Vec<u8>> {
        // Nothing recorded (an empty delta), or a delta whose merge changes nothing: the
        // model after is the model before. Judged by the merge, never by the state
        // vector, which a deletion alone does not move.
        if delta.is_empty() || !self.apply_update(&delta)?.changed {
            return Err(nothing_changes());
        }
        Ok(delta)
    }
}

/// The refusal of an edit that would leave the content as it is.
fn nothing_changes() -> CrdtError {
    CrdtError::Refused("That would change nothing, so nothing was written.".into())
}

/// A node's content outline ([`NodeDoc::outline`]).
#[derive(Debug, Clone, Default)]
pub struct Outline {
    /// Each top-level heading and its section's size in characters.
    pub sections: Vec<(markdown::Heading, usize)>,
    /// The characters before the first heading.
    pub intro_chars: usize,
    /// The characters of the whole content.
    pub total_chars: usize,
    /// How many top-level blocks the content has.
    pub blocks: usize,
}

/// One section of a split ([`NodeDoc::split_parts`]).
#[derive(Debug, Clone)]
pub struct SplitPart {
    pub heading: markdown::Heading,
    pub title: String,
    pub markdown: String,
    pub keys: Vec<String>,
}

/// What a split by headings makes ([`NodeDoc::split_parts`]).
#[derive(Debug, Clone)]
pub struct SplitParts {
    pub level: i64,
    pub parts: Vec<SplitPart>,
}

/// The character an atom reads as in a heading's text.
fn markdown_atom() -> char {
    '\u{FFFC}'
}
