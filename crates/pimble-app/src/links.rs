//! Links in the editor (docs/LINKS_CONTRACT.md): the editor plugin that
//! carries pimble's link behaviour, and the rules for what counts as a web
//! link.
//!
//! A link, Pimble or web, is rinch's `link` mark; nothing here stores links
//! anywhere else.

use rinch_editor_core::commands::range_has_mark;
use rinch_editor_core::{AttrValue, Attrs, InputRule, Mark, Plugin, PluginKey, Pos};

/// The plugin pimble adds to its editor before any content loads (adding a
/// plugin resets history).
pub struct LinksPlugin;

impl Plugin for LinksPlugin {
    fn key(&self) -> PluginKey {
        PluginKey("pimble.links")
    }

    fn input_rules(&self) -> Vec<InputRule> {
        vec![autolink_rule(), picker_rule()]
    }

    /// A pasted link (docs/LINKS_CONTRACT.md "Making a link"): a Pimble or
    /// web URL pasted over selected words links them; with nothing selected
    /// it goes in as linked words: a note's title (or a deep link's quote), a
    /// web link's URL. Anything else is pasted as ever.
    fn handle_paste(&self, state: &rinch_editor_core::state::EditorState, paste: &rinch_editor_core::PasteContent) -> Option<rinch_editor_core::state::Transaction> {
        let (href, words) = pasted_link(paste.text.as_deref()?)?;
        let (from, to) = (state.selection.from().0, state.selection.to().0);
        crate::link_picker::link_transaction(state, from, to, &words, &href, from < to)
    }

    fn keymap(&self) -> Vec<(rinch_editor_core::KeyBinding, &'static str)> {
        [("Mod-Shift-l", COPY_LINK_HERE), ("Mod-l", OPEN_PICKER)]
            .into_iter()
            .filter_map(|(key, command)| rinch_editor_core::KeyBinding::parse(key).map(|k| (k, command)))
            .collect()
    }

    fn commands(&self) -> Vec<(&'static str, rinch_editor_core::Command)> {
        // A command runs while the editor is mid-dispatch: reading the
        // handle waits a turn.
        vec![
            (
                COPY_LINK_HERE,
                std::rc::Rc::new(|_state, dispatch| {
                    if dispatch.is_some() {
                        later(copy_link_here);
                    }
                    true
                }),
            ),
            (
                OPEN_PICKER,
                std::rc::Rc::new(|_state, dispatch| {
                    if dispatch.is_some() {
                        later(|store| crate::link_picker::start_from_selection(store, &crate::editor::editor()));
                    }
                    true
                }),
            ),
        ]
    }
}

/// What a pasted text links to and the words it goes in as, when it is a
/// link: a `pimble:` URL (its note's title as the app knows it, a deep
/// link's quote, or "link") or an `http(s)` URL (itself). A bare domain is
/// not taken as a link when pasted: it may be meant as words.
fn pasted_link(text: &str) -> Option<(String, String)> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return None;
    }
    if let Some(url) = pimble_core::PimbleUrl::parse(text) {
        let quote = url.anchor.as_ref().map(|a| a.quote.trim().to_string()).filter(|q| !q.is_empty());
        let title = APP_STORE.with(|s| s.get()).and_then(|store| {
            let known = rinch::prelude::untracked(|| store.node_data.with(|map| map.contains_key(&(url.store, url.node))));
            known.then(|| store.display_label(url.store, url.node))
        });
        let words = quote.or(title).unwrap_or_else(|| "link".to_string());
        return Some((url.to_string(), words));
    }
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return web_url(text).map(|href| (href, text.to_string()));
    }
    None
}

/// The command "Copy Link to Here" is bound to (Ctrl/Cmd+Shift+L).
const COPY_LINK_HERE: &str = "pimbleCopyLinkHere";

/// The command Ctrl/Cmd+L runs: the link picker over the selection.
const OPEN_PICKER: &str = "pimbleOpenLinkPicker";

/// Run `f` with the app's store a turn later: an editor command or input
/// rule runs mid-dispatch, and the handle is not to be touched until it ends.
fn later(f: impl FnOnce(crate::state::AppStore) + 'static) {
    rinch::prelude::set_timeout(0, move || {
        if let Some(store) = APP_STORE.with(|s| s.get()) {
            f(store);
        }
    });
}

/// Typing a second `[` right after a first opens the link picker there
/// (docs/LINKS_CONTRACT.md "Making a link"). The rule types the bracket
/// itself (its transaction replaces the plain insert) and opens the picker
/// once the dispatch is over.
fn picker_rule() -> InputRule {
    InputRule::new(r"\[\[$", |state, _caps, start, _end| {
        let mut tr = state.tr();
        tr.insert_text("[").ok()?;
        later(move |store| crate::link_picker::start_typed(store, &crate::editor::editor(), start));
        Some(tr)
    })
}

thread_local! {
    /// The app's store, for the editor's own commands (set by
    /// `editor::start_editing`; an `AppStore` is a handful of signal ids).
    static APP_STORE: std::cell::Cell<Option<crate::state::AppStore>> = const { std::cell::Cell::new(None) };
}

/// Let the editor's commands reach the app.
pub fn set_app_store(store: crate::state::AppStore) {
    APP_STORE.with(|s| s.set(Some(store)));
}

/// "Copy Link to Here" (docs/LINKS_CONTRACT.md "Making a link"): a deep link
/// to the caret in the open note, its sticky position from the
/// collaboration session and a quote of the words after it.
pub fn copy_link_here(store: crate::state::AppStore) {
    let Some(active) = rinch::prelude::untracked(|| store.active_edit.get()) else { return };
    let handle = crate::editor::editor();
    let head = handle.selection().head();
    let sticky = handle.collab_sticky_index(head);
    let after = text_after(&handle.doc(), head);
    let url = pimble_core::PimbleUrl::deep(active.store_id, active.node_id, pimble_core::Anchor::new(sticky, &after));
    match copy_text(&url.to_string()) {
        Ok(()) => crate::events::show_notice(store, "Link to here copied.".to_string()),
        Err(e) => crate::events::show_notice(store, format!("The link could not be copied: {e}")),
    }
}

/// The text after `pos` in its textblock, an inline atom as U+FFFC (as the
/// collaboration projection holds it, so a quote matches there).
fn text_after(doc: &rinch_editor_core::Node, pos: Pos) -> String {
    let Ok(resolved) = doc.resolve(pos) else { return String::new() };
    let parent = resolved.parent();
    let mut text = String::new();
    for i in 0..parent.child_count() {
        let child = parent.child(i);
        match child.text() {
            Some(t) => text.push_str(t),
            None => text.push('\u{fffc}'),
        }
    }
    text.chars().skip(resolved.parent_offset()).collect()
}

/// The model position of `spot` in `doc`: the `textblock`-th textblock in
/// document order and a character offset into it, clamped to its end. The
/// inverse of what a deep link's anchor names (`pimble_crdt::Spot`).
pub fn pos_of_spot(doc: &rinch_editor_core::Node, spot: pimble_crdt::Spot) -> Option<Pos> {
    fn walk(node: &rinch_editor_core::Node, content_start: usize, seen: &mut usize, spot: pimble_crdt::Spot) -> Option<Pos> {
        let mut at = content_start;
        for i in 0..node.child_count() {
            let child = node.child(i);
            if child.is_textblock() {
                if *seen == spot.textblock {
                    return Some(Pos(at + 1 + spot.offset.min(child.content_size())));
                }
                *seen += 1;
            } else if !child.is_leaf() {
                if let Some(pos) = walk(child, at + 1, seen, spot) {
                    return Some(pos);
                }
            }
            at += child.node_size();
        }
        None
    }
    walk(doc, 0, &mut 0, spot)
}

/// Put the caret at a followed deep link's spot in the note just opened,
/// with the quoted words selected so the eye finds it, scrolled into view and
/// the editor focused
/// (docs/LINKS_CONTRACT.md "Following a link"). The spot is found the way
/// `NodeDoc::resolve_anchor` finds it, in the session's own content; the top
/// of the note when it is found nowhere.
pub fn place_anchor(handle: &crate::rinch_editor::EditorHandle, anchor: &pimble_core::Anchor) {
    let Some(snapshot) = handle.collab_snapshot() else { return };
    let Ok(content) = pimble_crdt::NodeDoc::load(&snapshot) else { return };
    let doc = handle.doc();
    let Some(from) = content.resolve_anchor(anchor).and_then(|found| pos_of_spot(&doc, found.spot())) else {
        return;
    };
    let quoted = anchor.quote.chars().count();
    let block_end = doc.resolve(from).map(|r| from.0 + (r.parent().content_size() - r.parent_offset())).unwrap_or(from.0);
    let to = Pos((from.0 + quoted).min(block_end));
    let selection = if to.0 > from.0 {
        rinch_editor_core::selection::Selection::text(from, to)
    } else {
        rinch_editor_core::selection::Selection::cursor(from)
    };
    handle.set_selection(selection);
    // A range never scrolls by itself, and nothing was clicked to focus the
    // editor: bring the words on screen and give it the keyboard (rinch #922).
    handle.scroll_into_view(from, to);
    handle.focus();
}

/// Follow a clicked link's `href` (docs/LINKS_CONTRACT.md "One experience
/// for both kinds"): a Pimble link is resolved (`ResolveLink`, answered by
/// `events::follow_resolution`), a web link opens outside the app, anything
/// else is left alone. Answers what it did, for the caller's tests and logs.
pub fn follow_href(store: crate::state::AppStore, href: &str) -> Followed {
    if let Some(url) = pimble_core::PimbleUrl::parse(href) {
        store.send(crate::protocol::BackendCommand::ResolveLink { url, hops: 0 });
        return Followed::Resolving;
    }
    let Some(url) = web_url(href).filter(|_| {
        let lower = href.trim().to_ascii_lowercase();
        lower.starts_with("http://") || lower.starts_with("https://")
    }) else {
        return Followed::Ignored;
    };
    if let Err(e) = open_external(&url) {
        crate::events::show_notice(store, format!("The link could not be opened: {e}"));
    }
    Followed::Opened
}

/// What [`follow_href`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Followed {
    Resolving,
    Opened,
    Ignored,
}

/// The modifier a link opens with, as the tooltip names it.
pub const OPEN_HINT: &str = if cfg!(target_os = "macos") { "Cmd+click to open" } else { "Ctrl+click to open" };

/// How long the pointer rests on a link before its tooltip shows.
const HOVER_DELAY_MS: u32 = 400;

thread_local! {
    /// The pending tooltip, until the pointer has rested long enough.
    static HOVER_TIMER: std::cell::RefCell<Option<rinch::prelude::TimeoutHandle>> = const { std::cell::RefCell::new(None) };
}

/// The editor's hover callback: the link under the pointer changed
/// (`None`: it left every link). The tooltip shows after the pointer has
/// rested for [`HOVER_DELAY_MS`] and goes at once when it leaves.
pub fn hover_link(store: crate::state::AppStore, hover: Option<&crate::rinch_editor::LinkHover>) {
    use rinch::prelude::{clear_timeout, set_timeout};
    if let Some(pending) = HOVER_TIMER.with(|t| t.borrow_mut().take()) {
        clear_timeout(pending);
    }
    let Some(hover) = hover else {
        store.link_hover.set(None);
        return;
    };
    let tooltip = crate::state::LinkTooltip {
        target: tooltip_target(store, &hover.link.href),
        x: hover.rect.x,
        y: hover.rect.y + hover.rect.height + 4.0,
    };
    store.link_hover.set(None);
    let handle = set_timeout(HOVER_DELAY_MS, move || {
        HOVER_TIMER.with(|t| t.borrow_mut().take());
        store.link_hover.set(Some(tooltip));
    });
    HOVER_TIMER.with(|t| *t.borrow_mut() = Some(handle));
}

/// What the tooltip says a link points at: a Pimble link's note title and
/// store as this app knows them (no request is made: hovering must stay
/// free), or a web link's URL.
pub fn tooltip_target(store: crate::state::AppStore, href: &str) -> String {
    let Some(url) = pimble_core::PimbleUrl::parse(href) else {
        return href.to_string();
    };
    let store_name = store
        .get_store_signal(url.store)
        .map(|sig| rinch::prelude::untracked(|| sig.with(|s| s.name.clone())));
    let known = rinch::prelude::untracked(|| store.node_data.with(|map| map.contains_key(&(url.store, url.node))));
    let place = if url.anchor.is_some() { "A place in " } else { "" };
    match (store_name, known) {
        (Some(name), true) => format!("{place}{} · {name}", store.display_label(url.store, url.node)),
        (Some(name), false) => format!("A note in {name}"),
        (None, _) => "A note in a store that isn't open here".to_string(),
    }
}

/// Open a web link outside the app: the system browser on the desktop, a new
/// tab (no opener) in the browser.
pub fn open_external(url: &str) -> Result<(), String> {
    #[cfg(feature = "native")]
    {
        #[cfg(test)]
        {
            OPENED.with(|opened| opened.borrow_mut().push(url.to_string()));
            Ok(())
        }
        #[cfg(not(test))]
        {
            open::that_detached(url).map_err(|e| e.to_string())
        }
    }
    #[cfg(not(feature = "native"))]
    {
        let window = web_sys::window().ok_or("no window")?;
        window.open_with_url_and_target_and_features(url, "_blank", "noopener,noreferrer").map_err(|e| format!("{e:?}"))?;
        Ok(())
    }
}

#[cfg(all(test, feature = "native"))]
thread_local! {
    /// Test only: what `open_external` was asked to open, instead of a browser.
    static OPENED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Put `text` on the system clipboard: rinch's clipboard on the desktop, the
/// browser's (`navigator.clipboard`, which answers later; a refusal is
/// logged) in the web build.
pub fn copy_text(text: &str) -> Result<(), String> {
    #[cfg(feature = "native")]
    {
        rinch::clipboard::copy_text(text).map_err(|e| e.to_string())
    }
    #[cfg(not(feature = "native"))]
    {
        let window = web_sys::window().ok_or("no window")?;
        let promise = window.navigator().clipboard().write_text(text);
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = wasm_bindgen_futures::JsFuture::from(promise).await {
                tracing::warn!("copying to the clipboard was refused: {e:?}");
            }
        });
        Ok(())
    }
}

/// `text` as a web link's href, or `None`: an `http:`/`https:` URL with a
/// host, or a bare domain with a path or not (`example.com/x`), which
/// becomes `https://`. What the Ctrl+L picker offers as "Link to <url>" and
/// what a paste links (docs/LINKS_CONTRACT.md "One experience for both
/// kinds").
pub fn web_url(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let candidate = if lower.starts_with("http://") || lower.starts_with("https://") {
        text.to_string()
    } else if lower.contains("://") || lower.starts_with("pimble:") || lower.starts_with("mailto:") {
        return None;
    } else {
        // A bare domain: a dot in the host, with letters after the last one.
        let host = text.split(['/', '?', '#']).next().unwrap_or("");
        let tld = host.rsplit('.').next().unwrap_or("");
        if !host.contains('.') || tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        format!("https://{text}")
    };
    let parsed = url::Url::parse(&candidate).ok()?;
    matches!(parsed.scheme(), "http" | "https").then_some(())?;
    parsed.host_str().filter(|h| !h.is_empty())?;
    Some(candidate)
}

/// The URL at the end of `typed` with the punctuation a sentence puts after
/// one taken off: `.`, `,`, `;`, `:`, `!`, `?`, quotes, and a `)` with no
/// `(` of its own in the URL.
fn trim_url_end(url: &str) -> &str {
    let mut end = url.len();
    loop {
        let head = &url[..end];
        let Some(last) = head.chars().last() else { break };
        let unbalanced_paren = last == ')' && head.matches('(').count() < head.matches(')').count();
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'') || unbalanced_paren {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    &url[..end]
}

/// Typing a web URL followed by a space links it (docs/LINKS_CONTRACT.md
/// "Making a link"). The rule's transaction replaces the plain insert, so it
/// types the space itself, with the marks the text there had but the link,
/// so the words after it are not linked. One undo step takes the link and
/// the space back.
fn autolink_rule() -> InputRule {
    InputRule::new(r"(?:^|\s)(https?://\S+)(\s)$", |state, caps, start, end| {
        let whole = caps.get(0)?;
        let url_match = caps.get(1)?;
        let typed = caps.get(2)?.as_str();
        let url = trim_url_end(url_match.as_str());
        let href = web_url(url)?;
        // `start` is where the match begins in the document; the URL starts
        // as many characters in as precede it in the match.
        let lead = whole.as_str()[..url_match.start() - whole.start()].chars().count();
        let from = start + lead;
        let to = from + url.chars().count();
        let link = state.schema().mark_type("link")?.clone();
        if range_has_mark(&state.doc, from, to, &link) {
            return None;
        }
        let kept: Vec<Mark> = state
            .doc
            .resolve(Pos(end))
            .ok()?
            .marks()
            .into_iter()
            .filter(|m| m.type_name() != "link")
            .collect();
        let mut tr = state.tr();
        tr.add_mark(from, to, Mark::new(link, Attrs::from_iter([("href", AttrValue::from(href))]))).ok()?;
        tr.set_stored_marks(Some(kept));
        tr.insert_text(typed).ok()?;
        Some(tr)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn following_a_pimble_link_resolves_it_a_web_link_opens_it_and_nothing_else_moves() {
        use crate::protocol::{BackendCommand, BackendHandle};
        use crate::state::AppStore;
        use crossbeam_channel::bounded;

        let store = AppStore::new();
        let (cmd_tx, commands) = bounded::<BackendCommand>(8);
        let (_event_tx, event_rx) = bounded(8);
        store.backend.set(Some(BackendHandle { cmd_tx, event_rx }));

        let url = pimble_core::PimbleUrl::node(pimble_core::StoreId::new(), pimble_core::NodeId::new());
        assert_eq!(follow_href(store, &url.to_string()), Followed::Resolving);
        match commands.try_recv().unwrap() {
            BackendCommand::ResolveLink { url: asked, hops } => assert_eq!((asked, hops), (url, 0)),
            other => panic!("unexpected {other:?}"),
        }

        assert_eq!(follow_href(store, "https://example.com/a"), Followed::Opened);
        OPENED.with(|opened| assert_eq!(*opened.borrow(), vec!["https://example.com/a".to_string()]));
        for href in ["example.com", "javascript:alert(1)", "file:///etc/passwd", "mailto:a@example.com", ""] {
            assert_eq!(follow_href(store, href), Followed::Ignored, "{href:?}");
        }
        OPENED.with(|opened| assert_eq!(opened.borrow().len(), 1, "nothing else was opened"));
        assert!(commands.try_recv().is_err());
    }

    /// An anchor made from the session's content, then the note typed into
    /// before it: opening the link puts the caret where the words went and
    /// selects them; Copy Link to Here's quote is what follows the caret.
    #[test]
    fn a_deep_link_lands_on_its_words_after_edits_before_them() {
        let handle = crate::rinch_editor::create_editor();
        let typed = |text: &'static str, at: usize| {
            assert!(handle.update(move |s| {
                let mut tr = s.tr();
                tr.set_selection(rinch_editor_core::selection::Selection::cursor(Pos(at)));
                tr.insert_text(text).ok()?;
                Some(tr)
            }));
        };
        typed("the part that matters", 1);
        handle.start_collaboration_host(|_| {}).expect("a session");
        let content = pimble_crdt::NodeDoc::load(&handle.collab_snapshot().unwrap()).unwrap();
        let anchor = content.anchor_at(pimble_crdt::Spot { textblock: 0, offset: 4 }).unwrap();
        assert_eq!(anchor.quote, "part that matters");
        assert_eq!(text_after(&handle.doc(), Pos(5)), "part that matters");

        typed("first, ", 1);
        place_anchor(&handle, &anchor);
        let selection = handle.selection();
        assert_eq!((selection.from(), selection.to()), (Pos(12), Pos(29)), "the quoted words, moved by what was typed before them");
    }

    #[test]
    fn a_spot_names_the_nth_textblock_in_document_order_lists_included() {
        use rinch_editor_core::{Fragment, Schema};
        let schema = Schema::starter_kit();
        let para = |t: &str| schema.branch("paragraph", Fragment::from_node(schema.text(t).unwrap())).unwrap();
        let item = |t: &str| schema.branch("list_item", Fragment::from_children(vec![para(t)])).unwrap();
        let list = schema.branch("bullet_list", Fragment::from_children(vec![item("one"), item("two")])).unwrap();
        let doc = schema.branch(&schema.top_node, Fragment::from_children(vec![para("intro"), list, para("end")])).unwrap();
        let spot = |textblock, offset| pimble_crdt::Spot { textblock, offset };
        for (s, word) in [(spot(0, 0), "intro"), (spot(1, 0), "one"), (spot(2, 1), "wo"), (spot(3, 0), "end")] {
            let pos = pos_of_spot(&doc, s).unwrap();
            assert_eq!(text_after(&doc, pos), word, "{s:?}");
        }
        assert_eq!(text_after(&doc, pos_of_spot(&doc, spot(0, 99)).unwrap()), "", "an offset past the end is its end");
        assert_eq!(pos_of_spot(&doc, spot(4, 0)), None);
    }

    /// Each run of the first paragraph with its link's href.
    fn handle_runs(handle: &crate::rinch_editor::EditorHandle) -> Vec<(String, Option<String>)> {
        let doc = handle.doc();
        let para = doc.child(0);
        (0..para.child_count())
            .map(|i| {
                let run = para.child(i);
                let href = run.marks().iter().find(|m| m.type_name() == "link").and_then(|m| m.attrs.get_str("href")).map(str::to_string);
                (run.text().unwrap_or_default().to_string(), href)
            })
            .collect()
    }

    fn editor_with(text: &'static str) -> crate::rinch_editor::EditorHandle {
        let handle = crate::rinch_editor::create_editor();
        handle.add_plugin(std::rc::Rc::new(LinksPlugin));
        assert!(handle.update(move |s| {
            let mut tr = s.tr();
            tr.insert_text(text).ok()?;
            Some(tr)
        }));
        handle
    }

    fn select(handle: &crate::rinch_editor::EditorHandle, from: usize, to: usize) {
        handle.set_selection(rinch_editor_core::selection::Selection::text(Pos(from), Pos(to)));
    }

    #[test]
    fn a_link_pasted_over_words_links_them_and_one_pasted_alone_goes_in_linked() {
        use rinch_editor_core::PasteContent;
        let (s, n) = (pimble_core::StoreId::new(), pimble_core::NodeId::new());
        let pimble = pimble_core::PimbleUrl::node(s, n).to_string();

        let handle = editor_with("read the plan today");
        select(&handle, 6, 14);
        assert!(handle.paste(&PasteContent::text(format!(" {pimble} "))));
        assert_eq!(handle_runs(&handle), vec![("read ".into(), None), ("the plan".into(), Some(pimble.clone())), (" today".into(), None)]);

        let handle = editor_with("see ");
        assert!(handle.paste(&PasteContent::text("https://example.com/a".to_string())));
        assert!(handle.update(|s| {
            let mut tr = s.tr();
            tr.insert_text(" next").ok()?;
            Some(tr)
        }));
        assert_eq!(
            handle_runs(&handle),
            vec![("see ".into(), None), ("https://example.com/a".into(), Some("https://example.com/a".into())), (" next".into(), None)]
        );

        // A deep link goes in as its quote; an unknown note as "link".
        let deep = pimble_core::PimbleUrl::deep(s, n, pimble_core::Anchor::new(None, "the spot")).to_string();
        let handle = editor_with("");
        assert!(handle.paste(&PasteContent::text(deep.clone())));
        assert_eq!(handle_runs(&handle), vec![("the spot".into(), Some(deep))]);
        let handle = editor_with("");
        assert!(handle.paste(&PasteContent::text(pimble.clone())));
        assert_eq!(handle_runs(&handle), vec![("link".into(), Some(pimble))]);
    }

    #[test]
    fn anything_else_pastes_as_ever() {
        use rinch_editor_core::PasteContent;
        for text in ["example.com", "two words", "javascript:alert(1)"] {
            let handle = editor_with("x ");
            assert!(handle.paste(&PasteContent::text(text.to_string())));
            assert_eq!(handle_runs(&handle), vec![(format!("x {text}"), None)], "{text:?}");
        }
    }

    #[test]
    fn web_urls_and_bare_domains_are_web_links_and_nothing_else_is() {
        for (text, want) in [
            ("https://example.com", Some("https://example.com")),
            ("http://example.com/a?b=c#d", Some("http://example.com/a?b=c#d")),
            ("  HTTPS://Example.com/x  ", Some("HTTPS://Example.com/x")),
            ("example.com", Some("https://example.com")),
            ("docs.rs/yrs/latest", Some("https://docs.rs/yrs/latest")),
            ("not a url", None),
            ("hello", None),
            ("version 1.2", None),
            ("v1.23", None),
            ("pimble:6f1c2b1e-8d3a-4f63-9a54-2c0f5a1e7b90/0b7e7c4d-1f2a-4c1b-8e3d-5a6b7c8d9e0f", None),
            ("ftp://example.com", None),
            ("mailto:a@example.com", None),
            ("https://", None),
            ("", None),
        ] {
            assert_eq!(web_url(text).as_deref(), want, "{text:?}");
        }
    }

    use rinch_editor_core::state::EditorState;
    use rinch_editor_core::{apply_input_rules, default_plugins, Fragment, Schema};
    use std::rc::Rc;

    /// A one-paragraph document holding `text`, the caret at its end, with
    /// this plugin: typing `typed` there, as the editor types it (the rules
    /// first, a plain insert when none fires).
    fn type_after(text: &str, typed: &str) -> EditorState {
        let schema = Rc::new(Schema::starter_kit());
        let para = schema.branch("paragraph", Fragment::from_node(schema.text(text).unwrap())).unwrap();
        let doc = schema.branch(&schema.top_node, Fragment::from_children(vec![para])).unwrap();
        let mut plugins = default_plugins();
        plugins.push(Rc::new(LinksPlugin) as Rc<dyn Plugin>);
        let mut state = EditorState::create(schema, doc, plugins);
        let end = 1 + text.chars().count();
        let mut tr = state.tr();
        tr.set_selection(rinch_editor_core::selection::Selection::cursor(Pos(end)));
        state = state.apply(tr);
        let rules = LinksPlugin.input_rules();
        let tr = apply_input_rules(&state, &rules, end, typed).unwrap_or_else(|| {
            let mut tr = state.tr();
            tr.insert_text(typed).unwrap();
            tr
        });
        state.apply(tr)
    }

    /// Each text run of the paragraph with its link's href, if any.
    fn runs(state: &EditorState) -> Vec<(String, Option<String>)> {
        let para = state.doc.child(0);
        (0..para.child_count())
            .map(|i| {
                let run = para.child(i);
                let href = run.marks().iter().find(|m| m.type_name() == "link").and_then(|m| m.attrs.get_str("href")).map(str::to_string);
                (run.text().unwrap_or_default().to_string(), href)
            })
            .collect()
    }

    #[test]
    fn a_typed_url_followed_by_a_space_becomes_a_link_and_the_next_word_does_not() {
        let state = type_after("see https://example.com/a.", " ");
        assert_eq!(
            runs(&state),
            vec![
                ("see ".into(), None),
                ("https://example.com/a".into(), Some("https://example.com/a".into())),
                (". ".into(), None),
            ]
        );
        let state = type_after("(see https://example.com)", " ");
        assert_eq!(
            runs(&state),
            vec![("(see ".into(), None), ("https://example.com".into(), Some("https://example.com".into())), (") ".into(), None)]
        );
        let state = type_after("https://example.com", " ");
        assert_eq!(runs(&state), vec![("https://example.com".into(), Some("https://example.com".into())), (" ".into(), None)]);
    }

    #[test]
    fn a_space_after_anything_else_is_only_a_space() {
        for text in ["see example.com", "just words", "pimble:6f1c2b1e-8d3a-4f63-9a54-2c0f5a1e7b90/0b7e7c4d-1f2a-4c1b-8e3d-5a6b7c8d9e0f", "https://"] {
            let state = type_after(text, " ");
            assert_eq!(runs(&state), vec![(format!("{text} "), None)], "{text:?}");
        }
    }

    #[test]
    fn sentence_punctuation_after_a_url_is_not_part_of_it() {
        for (typed, want) in [
            ("https://example.com.", "https://example.com"),
            ("https://example.com/a,", "https://example.com/a"),
            ("https://example.com/x?\"", "https://example.com/x"),
            ("https://en.wikipedia.org/wiki/Rust_(programming_language)", "https://en.wikipedia.org/wiki/Rust_(programming_language)"),
            ("https://example.com)", "https://example.com"),
            ("https://example.com/a).", "https://example.com/a"),
        ] {
            assert_eq!(trim_url_end(typed), want, "{typed:?}");
        }
    }
}
