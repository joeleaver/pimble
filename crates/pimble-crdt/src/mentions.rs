//! Mentions in a node's content (docs/NOTIFICATIONS_CONTRACT.md section 1,
//! "Built in wave 0"): rinch's `link` mark whose href is a
//! `pimble-person:` href (`pimble_core::mention`), over the words `@Name`.
//!
//! Read out of the content like links ([`of_model`]); nothing stores them
//! twice. [`added_between`] and [`crate::NodeDoc::mentions_added_by`] are the
//! hook later waves notify from: what one edit added, each mention with its
//! id.

use pimble_core::mention::{self, Mention};
use rinch_editor_core::{AttrValue, Fragment, Mark, Node as ModelNode, Schema};

const LINK_MARK: &str = "link";
const HREF: &str = "href";

/// The href of `node`'s link mark when it is a mention's.
fn mention_href(node: &ModelNode) -> Option<&str> {
    node.marks()
        .iter()
        .find(|m| m.type_name() == LINK_MARK)
        .and_then(|m| m.attrs.get_str(HREF))
        .filter(|href| mention::parse_href(href).is_some())
}

/// Every mention in `doc` (the editor model of a node's content), in
/// document order. Adjacent text runs with the same href (a mention partly
/// bold) are one mention; a mention never runs from one textblock into the
/// next.
pub fn of_model(doc: &ModelNode) -> Vec<Mention> {
    let mut out = Vec::new();
    let mut open: Option<(String, String)> = None;
    collect(doc, &mut open, &mut out);
    flush(&mut open, &mut out);
    out
}

fn collect(node: &ModelNode, open: &mut Option<(String, String)>, out: &mut Vec<Mention>) {
    if let Some(text) = node.text() {
        match (mention_href(node), open.as_mut()) {
            (Some(href), Some((current, words))) if current == href => words.push_str(text),
            (Some(href), _) => {
                flush(open, out);
                *open = Some((href.to_string(), text.to_string()));
            }
            (None, _) => flush(open, out),
        }
        return;
    }
    if node.is_textblock() || node.is_atom() {
        flush(open, out);
    }
    for child in node.content().iter() {
        collect(child, open, out);
    }
    if node.is_textblock() {
        flush(open, out);
    }
}

fn flush(open: &mut Option<(String, String)>, out: &mut Vec<Mention>) {
    if let Some((href, words)) = open.take() {
        out.extend(Mention::from_link(&href, &words));
    }
}

/// The mentions `after` holds that `before` did not, leaving out `writer`'s
/// own account (`pimble_core::mention::added`).
pub fn added_between(before: &ModelNode, after: &ModelNode, writer: Option<&str>) -> Vec<Mention> {
    mention::added(&of_model(before), &of_model(after), writer)
}

/// `doc` with an id given to every mention written without one, a mention
/// split over several runs taking one id. The same `Rc`s where nothing
/// changes, so a block with no such mention is the block it was.
pub(crate) fn with_ids(doc: &ModelNode, schema: &Schema) -> ModelNode {
    fn needs(node: &ModelNode) -> bool {
        mention_href(node).and_then(mention::parse_href).is_some_and(|(_, id)| id.is_none())
    }
    let mut any = false;
    doc.nodes_between(0, doc.content_size(), &mut |node, _, _| {
        any |= needs(node);
        !any
    });
    if !any {
        return doc.clone();
    }
    fn walk(node: &ModelNode, schema: &Schema) -> ModelNode {
        if node.is_text() || node.child_count() == 0 {
            return node.clone();
        }
        if !node.is_textblock() {
            return node.copy_with_content(Fragment::from_children(node.content().iter().map(|c| walk(c, schema)).collect()));
        }
        // The previous run's href as written and the href it was given.
        let mut last: Option<(String, String)> = None;
        let children = node
            .content()
            .iter()
            .map(|run| {
                let Some(href) = mention_href(run).filter(|_| needs(run)).map(str::to_string) else {
                    last = None;
                    return run.clone();
                };
                let given = match &last {
                    Some((was, given)) if *was == href => given.clone(),
                    _ => {
                        let (account, _) = mention::parse_href(&href).expect("needs() parsed it");
                        mention::href(&account, Some(&mention::new_id()))
                    }
                };
                last = Some((href, given.clone()));
                let marks: Vec<Mark> = run
                    .marks()
                    .iter()
                    .map(|m| if m.type_name() == LINK_MARK { Mark::new(m.typ.clone(), m.attrs.clone().with(HREF, AttrValue::from(given.as_str()))) } else { m.clone() })
                    .collect();
                schema.text_with_marks(run.text().unwrap_or_default(), marks).unwrap_or_else(|_| run.clone())
            })
            .collect();
        node.copy_with_content(Fragment::from_children(children))
    }
    walk(doc, schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn schema() -> Rc<Schema> {
        Rc::new(Schema::starter_kit())
    }

    fn doc(md: &str) -> ModelNode {
        crate::markdown::parse(&schema(), md).unwrap()
    }

    #[test]
    fn mentions_are_read_once_each_with_their_ids() {
        let d = doc("Hi [@Will](pimble-person:w1?m=aa) and [@**Sam**](pimble-person:s2?m=bb), see [x](pimble:s/n).\n\n- [@Will](pimble-person:w1?m=cc)");
        let found = of_model(&d);
        assert_eq!(
            found.iter().map(|m| (m.account.as_str(), m.id.as_deref(), m.name.as_str())).collect::<Vec<_>>(),
            vec![("w1", Some("aa"), "Will"), ("s2", Some("bb"), "Sam"), ("w1", Some("cc"), "Will")]
        );
    }

    #[test]
    fn a_mention_partly_bold_is_one_mention() {
        let d = doc("[@Will **Smith**](pimble-person:w1?m=aa)");
        let found = of_model(&d);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Will Smith");
    }

    #[test]
    fn added_between_names_what_an_edit_added() {
        let before = doc("Hi [@Will](pimble-person:w1?m=aa)");
        let after = doc("Hi [@Will](pimble-person:w1?m=aa), [@Sam](pimble-person:s2?m=bb) and [@Me](pimble-person:me?m=cc)");
        let added = added_between(&before, &after, Some("me"));
        assert_eq!(added.iter().map(|m| m.account.as_str()).collect::<Vec<_>>(), vec!["s2"]);
        assert!(added_between(&after, &before, None).is_empty(), "taking mentions out adds none");
    }

    #[test]
    fn markdown_without_an_id_is_given_one_and_keeps_it() {
        let d = doc("[@Will](pimble-person:w1) and [@Sam **S**](pimble-person:s2)");
        let found = of_model(&d);
        assert_eq!(found.len(), 2, "the split one is still one: {found:?}");
        assert!(found.iter().all(|m| m.id.is_some()));
        let md = crate::markdown::render(&d);
        assert!(md.contains(&format!("pimble-person:w1?m={}", found[0].id.as_ref().unwrap())), "{md}");
        // What Markdown says back keeps the ids, so rewriting a note with it
        // adds no mention.
        assert!(added_between(&d, &doc(&md), None).is_empty());
    }
}
