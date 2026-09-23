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
        vec![autolink_rule()]
    }
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
