//! The panel's tags: the node's chips (a #hashtag in its text marked with its
//! places), the tag box with a comma finishing a tag, and the store's tags
//! suggested as one types. Every write is one edit of the node's tags
//! (`BackendCommand::SetNodeAppearance` with a `TagEdit`).

use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;

use super::{panel_context, panel_node, panel_writable, shown};
use crate::protocol::{BackendCommand, TagEdit};
use crate::state::AppStore;

/// How many tags already used in the store are suggested at once.
const SUGGESTIONS: usize = 6;

/// The tags `typed` suggests among `used`, leaving out the ones `have`
/// already: those that start with it first, then those that contain it,
/// case-insensitively; nothing for nothing typed.
pub(crate) fn suggestions(used: &[String], have: &[String], typed: &str) -> Vec<String> {
    let typed = typed.trim().to_lowercase();
    if typed.is_empty() {
        return Vec::new();
    }
    let free = |t: &&String| !have.iter().any(|h| h.eq_ignore_ascii_case(t));
    let mut out: Vec<String> = used.iter().filter(free).filter(|t| t.to_lowercase().starts_with(&typed)).cloned().collect();
    out.extend(used.iter().filter(free).filter(|t| {
        let t = t.to_lowercase();
        !t.starts_with(&typed) && t.contains(&typed)
    }).cloned());
    out.truncate(SUGGESTIONS);
    out
}

/// What typing `text` in the tag box gives: the tags a comma finished, and
/// what is left to keep typing.
pub(crate) fn split_typed(text: &str) -> (Vec<String>, String) {
    match text.rfind(',') {
        Some(at) => {
            let done = text[..at].split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
            (done, text[at + 1..].trim_start().to_string())
        }
        None => (Vec::new(), text.to_string()),
    }
}

/// Add or take off one tag of `(s, n)`, as typed (the spelling the store
/// already uses for it when it has one).
fn edit_tag(store: AppStore, s: StoreId, n: NodeId, change: TagEdit) {
    if !store.node_access(s, n).allows_write() {
        crate::events::show_notice(store, store.node_access(s, n).refusal().to_string());
        return;
    }
    let change = match change {
        TagEdit::Add(tag) => {
            let used = untracked(|| store.properties_context.with(|c| c.as_ref().map(|c| c.store_tags.clone()).unwrap_or_default()));
            let tag = tag.trim().trim_start_matches('#').trim().to_string();
            if tag.is_empty() {
                return;
            }
            TagEdit::Add(used.into_iter().find(|t| t.eq_ignore_ascii_case(&tag)).unwrap_or(tag))
        }
        other => other,
    };
    store.send(BackendCommand::SetNodeAppearance { store_id: s, node_id: n, icon: None, color: None, background: None, tags: Some(change), theme: None });
}

/// One tag chip: the tag, and when it is a #hashtag in the node's text, how
/// many places the text writes it (docs/HASHTAGS_CONTRACT.md "Where it shows").
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Chip {
    pub tag: String,
    pub in_text: Option<usize>,
    /// [`Chip::tooltip`], kept apart so the chip's attributes each take
    /// their own field.
    pub tip: String,
    /// What the × says when hovered.
    pub remove_tip: String,
}

impl Chip {
    /// What the chip says when hovered.
    pub(crate) fn tooltip(&self) -> String {
        match self.in_text {
            Some(n) => format!(
                "From the text: #{} ({n} {}). \u{d7} removes it from the text.",
                self.tag,
                if n == 1 { "place" } else { "places" }
            ),
            None => "Remove this tag".to_string(),
        }
    }
}

/// The chips of a node: every tag, each text tag with its places in `content`
/// (the node's document; counted only when it has text tags).
pub(crate) fn chips(tags: &[String], text_tags: &[String], content: &[u8]) -> Vec<Chip> {
    let counts = if text_tags.is_empty() {
        Vec::new()
    } else {
        pimble_crdt::NodeDoc::load(content).map(|d| d.text_tag_counts()).unwrap_or_default()
    };
    let same = pimble_crdt::hashtags::same_tag;
    tags.iter()
        .map(|tag| {
            let in_text = text_tags
                .iter()
                .any(|t| same(t, tag))
                .then(|| counts.iter().find(|(t, _)| same(t, tag)).map_or(1, |(_, n)| *n));
            let mut chip = Chip { tag: tag.clone(), in_text, tip: String::new(), remove_tip: String::new() };
            chip.tip = chip.tooltip();
            chip.remove_tip = if in_text.is_some() { "Remove it from the text".into() } else { "Remove this tag".into() };
            chip
        })
        .collect()
}

/// The chips of `(s, n)` as this window has them, tracked.
fn node_chips(store: AppStore, node: Option<(StoreId, NodeId)>) -> Vec<Chip> {
    let Some((s, n)) = node else { return Vec::new() };
    let Some(sig) = store.node_data.with(|map| map.get(&(s, n)).copied()) else { return Vec::new() };
    sig.with(|node| chips(&node.metadata.tags, &node.metadata.text_tags, &node.content))
}

/// The panel's "Tags" section.
pub(super) fn render_tags(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let node = move || panel_node(store);
    let writable = move || panel_writable(store);
    let context = move || panel_context(store);
    let typed = Signal::new(String::new());
    let take_typed = move || {
        let Some((s, n)) = untracked(|| shown(store)) else { return };
        let text = untracked(|| typed.get());
        typed.set(String::new());
        for tag in text.split(',') {
            if !tag.trim().is_empty() {
                edit_tag(store, s, n, TagEdit::Add(tag.to_string()));
            }
        }
    };
    let offered = move || {
        let have = node().map(|n| n.tags).unwrap_or_default();
        let used = context().map(|c| c.store_tags).unwrap_or_default();
        suggestions(&used, &have, &typed.get())
    };
    rsx! {
        div {
            class: "pimble-properties__section",
            div { class: "pimble-properties__heading", "Tags" }
            div {
                class: "pimble-properties__chips",
                for chip in node_chips(store, shown(store)) {
                    span {
                        key: format!("{}#{}", chip.tag, chip.in_text.map_or(0, |n| n)),
                        class: "pimble-properties__chip",
                        title: chip.tip.clone(),
                        span {
                            class: "pimble-properties__chip-mark",
                            style: if chip.in_text.is_some() { "" } else { "display: none;" },
                            "#"
                        }
                        {chip.tag.clone()}
                        span {
                            class: "pimble-properties__chip-remove",
                            style: {move || if writable() { "" } else { "display: none;" }},
                            title: chip.remove_tip.clone(),
                            onclick: {
                                let tag = chip.tag.clone();
                                move || {
                                    if let Some((s, n)) = untracked(|| shown(store)) {
                                        edit_tag(store, s, n, TagEdit::Remove(tag.clone()));
                                    }
                                }
                            },
                            "×"
                        }
                    }
                }
                span {
                    class: "pimble-properties__empty",
                    style: {move || if !writable() && node().is_none_or(|n| n.tags.is_empty()) { "" } else { "display: none;" }},
                    "No tags"
                }
            }
            div {
                style: {move || if writable() { "" } else { "display: none;" }},
                input {
                    class: "pimble-properties__input pimble-properties__tag-input",
                    placeholder: "Add a tag",
                    value: move || typed.get(),
                    oninput: move |v: String| {
                        // A comma finishes the tags before it.
                        let (done, rest) = split_typed(&v);
                        if done.is_empty() {
                            typed.set(v);
                            return;
                        }
                        typed.set(rest);
                        if let Some((s, n)) = untracked(|| shown(store)) {
                            for tag in done {
                                edit_tag(store, s, n, TagEdit::Add(tag));
                            }
                        }
                    },
                    onsubmit: take_typed,
                }
                div {
                    class: "pimble-properties__suggestions",
                    for tag in offered() {
                        div {
                            key: tag.clone(),
                            class: "pimble-properties__suggestion",
                            onclick: {
                                let tag = tag.clone();
                                move || {
                                    typed.set(String::new());
                                    if let Some((s, n)) = untracked(|| shown(store)) {
                                        edit_tag(store, s, n, TagEdit::Add(tag.clone()));
                                    }
                                }
                            },
                            {tag.clone()}
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn suggestions_start_with_what_is_typed_then_contain_it() {
        let used = tags(&["Important", "Tax", "Waiting", "Travel", "Taxes"]);
        assert_eq!(suggestions(&used, &[], "ta"), tags(&["Tax", "Taxes", "Important"]));
        assert_eq!(suggestions(&used, &tags(&["tax"]), "TA"), tags(&["Taxes", "Important"]), "a tag the node has is not offered, in any case");
        assert!(suggestions(&used, &[], "  ").is_empty(), "nothing typed offers nothing");
    }

    #[test]
    fn text_tags_are_marked_with_their_places() {
        let mut doc = pimble_crdt::NodeDoc::new();
        doc.init("document", "Note", None, "2026-11-20T10:00:00Z").unwrap();
        doc.replace_content_markdown("About #Pimble and #pimble, and #home.").unwrap();
        let node = doc.fields().unwrap().into_node(NodeId::new());
        let chips = chips(&node.metadata.tags, &node.metadata.text_tags, &doc.save());
        assert_eq!(chips.iter().map(|c| (c.tag.as_str(), c.in_text)).collect::<Vec<_>>(), [("Pimble", Some(2)), ("home", Some(1))]);
        assert_eq!(chips[0].tooltip(), "From the text: #Pimble (2 places). \u{d7} removes it from the text.");
        let entered = chips_of_entered();
        assert_eq!(entered[0].in_text, None);
        assert_eq!(entered[0].tooltip(), "Remove this tag");
    }

    fn chips_of_entered() -> Vec<Chip> {
        chips(&tags(&["Tax"]), &[], &[])
    }

    #[test]
    fn a_comma_finishes_the_tags_before_it() {
        assert_eq!(split_typed("tax"), (Vec::new(), "tax".to_string()));
        assert_eq!(split_typed("tax,"), (tags(&["tax"]), String::new()));
        assert_eq!(split_typed("tax, , waiting, im"), (tags(&["tax", "waiting"]), "im".to_string()));
    }

    #[test]
    fn a_tag_edit_keeps_one_of_each_whatever_its_case() {
        let have = tags(&["Tax", "Waiting"]);
        assert_eq!(TagEdit::Add("tax".into()).apply(&have), have, "a tag it has in another case is that tag");
        assert_eq!(TagEdit::Add(" Home ".into()).apply(&have), tags(&["Tax", "Waiting", "Home"]));
        assert_eq!(TagEdit::Remove("waiting".into()).apply(&have), tags(&["Tax"]));
    }
}
