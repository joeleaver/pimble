//! The discussion view's DOM (docs/DISCUSSIONS_CONTRACT.md sections 4-6): the
//! topic, the thread with its replies indented, each message's actions, the
//! message box with its emoji picker, and under a node's content the list of
//! the discussions on it. Rendered once per pane slot; a message is keyed by
//! its id and what it shows, so only a message that changed is built again.

use std::hash::{Hash, Hasher};

use rinch_tabler_icons::{render_tabler_icon, TablerIcon, TablerIconStyle};

use super::*;
use crate::panes::MAX_PANES;

/// The emoji the picker offers (contract section 5: emoji are plain Unicode).
const EMOJI: [&str; 40] = [
    "👍", "👎", "😀", "😂", "🙂", "😉", "😍", "🤔", "😮", "😢", "😡", "🙏", "👏", "🙌", "💪", "👀", "🎉", "🔥", "✅", "❌",
    "❓", "❗", "💡", "⭐", "❤️", "💯", "🚀", "📌", "📎", "📅", "⏰", "✏️", "📝", "📷", "🔗", "🤝", "👋", "☕", "🍕", "🌱",
];

thread_local! {
    /// Each slot's message boxes: (document key, node id), for focusing them.
    static BOXES: RefCell<HashMap<(Slot, usize), (u64, usize)>> = RefCell::new(HashMap::new());
}

fn box_index(which: Composer) -> usize {
    match which {
        Composer::New => 0,
        Composer::Edit => 1,
        Composer::Topic => 2,
    }
}

fn box_class(which: Composer) -> &'static str {
    match which {
        Composer::New => "textarea.pimble-discussion__input",
        Composer::Edit => "textarea.pimble-discussion__edit-input",
        Composer::Topic => "input.pimble-discussion__topic",
    }
}

/// Put the keyboard in one of pane `pane`'s message boxes once it shows, and
/// let it hear Enter.
pub(crate) fn focus_composer(slot: Slot, which: Composer) {
    let Some(store) = APP.with(Cell::get) else { return };
    begin_composing(store, slot, which);
    PRESS_IN_BOX.with(|p| p.set(false));
    // The edit box is built when its message is drawn again, a turn or two
    // after the click that asked for it: ask once it is there.
    // Armed by no component, for the same reason as the interceptor.
    let delay = if which == Composer::Edit { 30 } else { 0 };
    rinch::core::reactive::unowned(|| {
        set_timeout(delay, move || {
            if let Some((doc_key, node)) = BOXES.with(|b| b.borrow().get(&(slot, box_index(which))).copied()) {
                rinch::core::request_focus(doc_key, node);
            }
        })
    });
    #[cfg(not(feature = "native"))]
    {
        use wasm_bindgen::JsCast;
        let selector = box_class(which);
        rinch::core::reactive::unowned(|| set_timeout(delay + 40, move || {
            let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
            let Ok(list) = document.query_selector_all(selector) else { return };
            for i in 0..list.length() {
                let Some(el) = list.get(i).and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok()) else { continue };
                if el.offset_width() > 0 {
                    let _ = el.focus();
                    return;
                }
            }
        }));
    }
    #[cfg(feature = "native")]
    let _ = box_class(which);
}

fn read_only(store: AppStore, slot: Slot) -> bool {
    let held = match slot {
        Slot::Pane(pane) => store.pane(pane).discussion.get().map(|e| (e.store_id, e.node_id)),
        Slot::Inline(_, s, n) => Some((s, n)),
    };
    held.is_some_and(|(s, n)| !store.node_access_tracked(s, n).allows_write())
}

/// Pane `pane`'s discussion view. Rendered once per pane slot.
pub(crate) fn render_discussion(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    discussion_view(__scope, store, Slot::Pane(pane), state.discussion_ui, move || state.show_discussion.get())
}

/// A discussion's view in `slot`: the topic, the thread, the message box.
fn discussion_view(__scope: &mut RenderScope, store: AppStore, slot: Slot, ui: DiscussionUi, shown: impl Fn() -> bool + 'static) -> NodeHandle {
    let inline = matches!(slot, Slot::Inline(..));
    let shown = std::rc::Rc::new(shown);

    // The topic line follows the node's title, except while it is typed in.
    let _ = rinch::Effect::new(move || {
        let title = topic_of(store, slot);
        let typing = untracked(|| COMPOSING.with(Cell::get)) == Some((slot, Composer::Topic));
        if !typing {
            ui.topic.set(title);
        }
    });

    let topic = rsx! {
        input {
            class: "pimble-discussion__topic",
            placeholder: "Topic",
            value: move || ui.topic.get(),
            oninput: move |v: String| ui.topic.set(v),
            onmousedown: move || {
                crate::pane_view::press_in_view();
                begin_composing(store, slot, Composer::Topic)
            },
        }
    };
    BOXES.with(|b| b.borrow_mut().insert((slot, 2), (topic.doc_key(), topic.node_id().0)));

    let input = rsx! {
        textarea {
            class: "pimble-discussion__input",
            placeholder: "Write a message",
            rows: "3",
            value: move || ui.draft.get(),
            oninput: move |v: String| ui.draft.set(v),
            onmousedown: move || {
                crate::pane_view::press_in_view();
                begin_composing(store, slot, Composer::New)
            },
        }
    };
    BOXES.with(|b| b.borrow_mut().insert((slot, 0), (input.doc_key(), input.node_id().0)));
    let picker = emoji_picker(__scope, store, slot, ui, Composer::New);
    // Inline: a link to open the discussion in a pane of its own.
    let open_link = inline.then(|| {
        rsx! {
            button {
                class: "pimble-discussion__head-button",
                title: "Open this discussion beside the text",
                onclick: move || {
                    if let Slot::Inline(pane, s, n) = slot {
                        open_beside(store, pane, s, n);
                    }
                },
                "Open"
            }
        }
    });

    rsx! {
        div {
            class: if inline { "pimble-discussion pimble-discussion--inline" } else { "pimble-discussion" },
            style: {
                let shown = shown.clone();
                move || if shown() { "" } else { "display: none;" }
            },
            div {
                class: "pimble-discussion__head",
                {topic}
                {open_link}
                if ui.resolved.get().is_some() {
                    button { class: "pimble-discussion__head-button", disabled: {move || read_only(store, slot)}, onclick: move || resolve(store, slot, true), "Reopen" }
                } else {
                    button { class: "pimble-discussion__head-button", title: "Close this discussion", disabled: {move || read_only(store, slot)}, onclick: move || resolve(store, slot, false), "Resolve" }
                }
            }
            if let Some(resolved) = ui.resolved.get() {
                div { class: "pimble-discussion__resolved", {resolved} }
            }
            if let Some(message) = ui.message.get() {
                div { class: "pimble-discussion__message", {message} }
            }
            div {
                class: "pimble-discussion__thread",
                if ui.messages.get().is_empty() {
                    div { class: "pimble-discussion__empty", "No messages yet. Write the first one below." }
                }
                for (i, m) in ui.messages.get().into_iter().enumerate() {
                    div {
                        key: message_key(&m, ui.editing.get().as_deref() == Some(m.id.as_str())),
                        // Where what is new begins (contract section 8).
                        if m.fresh && ui.messages.with(|all| all.iter().position(|m| m.fresh) == Some(i)) {
                            div {
                                class: "pimble-discussion__new",
                                {move || {
                                    let n = ui.messages.with(|all| all.iter().filter(|m| m.fresh).count());
                                    format!("{n} new")
                                }}
                            }
                        }
                        {message_node(__scope, store, slot, ui, m.clone())}
                    }
                }
            }
            if ui.hidden.get() > 0 {
                div {
                    class: "pimble-discussion__deleted",
                    button {
                        class: "pimble-discussion__link-button",
                        onclick: move || show_all(store, slot),
                        {move || format!("Show all {} messages", ui.hidden.get() + 1)}
                    }
                }
            }
            if !ui.deleted.get().is_empty() {
                div {
                    class: "pimble-discussion__deleted",
                    button {
                        class: "pimble-discussion__link-button",
                        onclick: move || ui.show_deleted.update(|v| *v = !*v),
                        {move || format!("{} deleted message{}", ui.deleted.get().len(), if ui.deleted.get().len() == 1 { "" } else { "s" })}
                    }
                    if ui.show_deleted.get() {
                        for (id, text) in ui.deleted.get() {
                            div {
                                key: id.clone(),
                                class: "pimble-discussion__deleted-row",
                                span { class: "pimble-discussion__deleted-text", {text.clone()} }
                                button {
                                    class: "pimble-discussion__link-button",
                                    disabled: {move || read_only(store, slot)},
                                    onclick: {
                                        let id = id.clone();
                                        move || restore_message(store, slot, &id)
                                    },
                                    "Put Back"
                                }
                            }
                        }
                    }
                }
            }
            div {
                class: "pimble-discussion__composer",
                style: {move || if read_only(store, slot) { "display: none;" } else { "" }},
                if let Some((_, who)) = ui.replying_to.get() {
                    div {
                        class: "pimble-discussion__replying",
                        {format!("Replying to {who}")}
                        button { class: "pimble-discussion__link-button", title: "Not a reply", onclick: move || ui.replying_to.set(None), "×" }
                    }
                }
                {input}
                div {
                    class: "pimble-discussion__composer-row",
                    button {
                        class: "pimble-discussion__emoji-button",
                        title: "Emoji",
                        onclick: move || ui.emoji.update(|e| *e = if *e == Some(Composer::New) { None } else { Some(Composer::New) }),
                        "🙂"
                    }
                    span { class: "pimble-discussion__hint", "Enter sends · Shift+Enter: new line" }
                    button { class: "pimble-discussion__send", onclick: move || post(store, slot), "Send" }
                }
                {picker}
            }
            div {
                class: "pimble-discussion__read-only",
                style: {move || if read_only(store, slot) { "" } else { "display: none;" }},
                {pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string()}
            }
        }
    }
}

fn message_key(m: &MessageView, editing: bool) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{m:?}").hash(&mut h);
    editing.hash(&mut h);
    format!("{}:{:x}", m.id, h.finish())
}

fn emoji_picker(__scope: &mut RenderScope, _store: AppStore, slot: Slot, ui: DiscussionUi, which: Composer) -> NodeHandle {
    let buttons: Vec<NodeHandle> = EMOJI
        .iter()
        .map(|e| {
            let e = *e;
            rsx! {
                button {
                    class: "pimble-discussion__emoji",
                    onclick: move || {
                        let target = if which == Composer::New { ui.draft } else { ui.edit_text };
                        target.update(|t| t.push_str(e));
                        ui.emoji.set(None);
                        focus_composer(slot, which);
                    },
                    {e.to_string()}
                }
            }
        })
        .collect();
    rsx! {
        div {
            class: "pimble-discussion__emoji-picker",
            style: {move || if ui.emoji.get() == Some(which) { "" } else { "display: none;" }},
            {buttons}
        }
    }
}

/// A message's text: its spans, each bold, italic or a link as it is.
fn body_node(__scope: &mut RenderScope, store: AppStore, spans: &[Span]) -> NodeHandle {
    let parts: Vec<NodeHandle> = spans
        .iter()
        .map(|span| {
            let mut style = String::new();
            if span.bold {
                style.push_str("font-weight: 600;");
            }
            if span.italic {
                style.push_str("font-style: italic;");
            }
            let text = span.text.clone();
            match span.link.clone() {
                Some(href) => {
                    let tip = crate::links::tooltip_target(store, &href);
                    rsx! {
                    span {
                        class: "pimble-discussion__a",
                        style: style,
                        title: tip,
                        onclick: move || { crate::links::follow_href(store, &href); },
                        {text}
                    }
                    }
                }
                None => rsx! { span { style: style, {text} } },
            }
        })
        .collect();
    rsx! { div { class: "pimble-discussion__body", {parts} } }
}

fn message_node(__scope: &mut RenderScope, store: AppStore, slot: Slot, ui: DiscussionUi, m: MessageView) -> NodeHandle {
    let editing = untracked(|| ui.editing.get()).as_deref() == Some(m.id.as_str());
    let indent_level = m.indent;
    let indent = format!("margin-left: {}px;", m.indent * 22);
    let (author, agent, at, when) = (m.author.clone(), m.agent, m.at.clone(), m.when.clone());
    let (edited, deleted, own, fresh) = (m.edited && !m.deleted, m.deleted, m.own, m.fresh);
    let replying = m.replying_to.clone().map(|to| format!("replying to {to}"));
    let author_node = (!author.is_empty()).then(|| rsx! { span { class: "pimble-discussion__author", {author} } });
    let agent_node = agent.then(|| rsx! { span { class: "pimble-discussion__agent", "agent" } });
    let edited_node = edited.then(|| rsx! { span { class: "pimble-discussion__edited", "edited" } });
    let replying_node = replying.map(|text| rsx! { span { class: "pimble-discussion__to", {text} } });
    let meta = rsx! {
        div {
            class: "pimble-discussion__meta",
            {author_node}
            {agent_node}
            span { class: "pimble-discussion__when", title: at, {when} }
            {edited_node}
            {replying_node}
        }
    };
    let content = if deleted {
        rsx! { div { class: "pimble-discussion__body pimble-discussion__body--deleted", "Message deleted" } }
    } else if editing {
        let edit = rsx! {
            textarea {
                class: "pimble-discussion__edit-input",
                rows: "3",
                value: move || ui.edit_text.get(),
                oninput: move |v: String| ui.edit_text.set(v),
                onmousedown: move || {
                    crate::pane_view::press_in_view();
                    begin_composing(store, slot, Composer::Edit)
                },
            }
        };
        BOXES.with(|b| b.borrow_mut().insert((slot, 1), (edit.doc_key(), edit.node_id().0)));
        let picker = emoji_picker(__scope, store, slot, ui, Composer::Edit);
        rsx! {
            div {
                class: "pimble-discussion__editing",
                {edit}
                div {
                    class: "pimble-discussion__composer-row",
                    button {
                        class: "pimble-discussion__emoji-button",
                        title: "Emoji",
                        onclick: move || ui.emoji.update(|e| *e = if *e == Some(Composer::Edit) { None } else { Some(Composer::Edit) }),
                        "🙂"
                    }
                    span { class: "pimble-discussion__hint", "Enter saves · Escape cancels" }
                    button { class: "pimble-discussion__link-button", onclick: move || cancel_edit(store, slot), "Cancel" }
                    button { class: "pimble-discussion__send", onclick: move || save_edit(store, slot), "Save" }
                }
                {picker}
            }
        }
    } else {
        body_node(__scope, store, &m.spans)
    };
    let (reply_id, edit_id, delete_id) = (m.id.clone(), m.id.clone(), m.id.clone());
    let mine = own.then(|| {
        rsx! {
            span {
                class: "pimble-discussion__actions",
                button { class: "pimble-discussion__link-button", onclick: move || begin_edit(store, slot, &edit_id), "Edit" }
                button { class: "pimble-discussion__link-button", onclick: move || delete_message(store, slot, &delete_id), "Delete" }
            }
        }
    });
    let actions = (!deleted && !editing).then(|| {
        rsx! {
            div {
                class: "pimble-discussion__actions",
                style: {move || if read_only(store, slot) { "display: none;" } else { "" }},
                button { class: "pimble-discussion__link-button", onclick: move || reply_to(store, slot, &reply_id), "Reply" }
                {mine}
            }
        }
    });
    rsx! {
        div {
            class: match (indent_level > 0, fresh) {
                (true, true) => "pimble-discussion__msg pimble-discussion__msg--reply pimble-discussion__msg--fresh",
                (true, false) => "pimble-discussion__msg pimble-discussion__msg--reply",
                (false, true) => "pimble-discussion__msg pimble-discussion__msg--fresh",
                (false, false) => "pimble-discussion__msg",
            },
            style: indent,
            {meta}
            {content}
            {actions}
        }
    }
}

// ── The discussions on a node ───────────────────────────────────────────

/// The node pane `pane` shows that can hold discussions (a note, a board, a
/// sheet), tracked.
fn pane_host(store: AppStore, pane: PaneId) -> Option<(StoreId, NodeId)> {
    let state = store.pane(pane);
    state.active_edit.get().or_else(|| state.board.get()).or_else(|| state.sheet.get()).map(|e| (e.store_id, e.node_id))
}

/// Under a node's content: the discussions on it, each a line that opens it
/// beside the node, and "Start a Discussion". Shown when the node has any.
/// `in_text`: the instance in a note's text (it scrolls with it); otherwise
/// the one under a board's or a sheet's view.
pub(crate) fn render_host_panel(__scope: &mut RenderScope, store: AppStore, pane: PaneId, in_text: bool) -> NodeHandle {
    let state = store.pane(pane);
    // The pane's node's list is asked for once per node shown, when the
    // node says it has discussions.
    let asked: Signal<Option<(StoreId, NodeId)>> = Signal::new(None);
    let _ = rinch::Effect::new(move || {
        if !in_text {
            return;
        }
        let Some((store_id, node_id)) = pane_host(store, pane) else { return };
        let count = store.node_data.with(|map| map.get(&(store_id, node_id)).copied()).map(|sig| sig.with(|n| n.attachments)).unwrap_or(0);
        let listed = store.attachments.with(|map| map.contains_key(&(store_id, node_id)));
        if count > 0 && !listed && untracked(|| asked.get()) != Some((store_id, node_id)) {
            asked.set(Some((store_id, node_id)));
            store.send(BackendCommand::ListAttachments { store_id, node_id });
        }
    });
    let rows = move || -> Vec<DiscussionRow> {
        store.read_version.get();
        let Some((store_id, host)) = pane_host(store, pane) else { return Vec::new() };
        let now = crate::boards::now_ms();
        host_discussions(store, store_id, host)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, node)| row_of(store_id, id, node.as_ref(), now))
            .collect()
    };
    let shown = move || {
        let s = store.pane(pane);
        let here = if in_text {
            s.show_editor.get() && !s.show_board.get() && !s.show_sheet.get()
        } else {
            (s.show_sheet.get() || s.show_board.get()) && !s.markup.get()
        };
        here && !rows().is_empty()
    };
    let icon = render_tabler_icon(__scope, TablerIcon::Messages, TablerIconStyle::Outline);
    rsx! {
        div {
            class: "pimble-discussions",
            style: {move || if shown() && !state.show_discussion.get() { "" } else { "display: none;" }},
            div {
                class: "pimble-discussions__head",
                span { class: "pimble-discussions__icon", {icon} }
                span { class: "pimble-discussions__title", {move || format!("Discussions ({})", rows().len())} }
                button {
                    class: "pimble-discussion__link-button",
                    onclick: move || {
                        if let Some((store_id, host)) = untracked(|| pane_host(store, pane)) {
                            start_on(store, pane, store_id, host);
                        }
                    },
                    "Start a Discussion"
                }
            }
            for row in rows() {
                div {
                    key: format!("{:?}", row),
                    {host_row(__scope, store, pane, row.clone())}
                }
            }
        }
    }
}

fn host_row(__scope: &mut RenderScope, store: AppStore, pane: PaneId, row: DiscussionRow) -> NodeHandle {
    let (s, n) = (row.store_id, row.node_id);
    let resolved = row.resolved.then(|| rsx! { span { class: "pimble-discussions__resolved", "resolved" } });
    let unread = (row.unread > 0).then(|| rsx! { span { class: "pimble-discussions__unread", {format!("{} new", row.unread)} } });
    let when = (!row.when.is_empty()).then(|| rsx! { span { class: "pimble-discussions__when", {format!(" · {}", row.when)} } });
    rsx! {
        div {
            class: "pimble-discussions__row",
            onclick: move || open_beside(store, pane, s, n),
            div {
                class: "pimble-discussions__topic",
                {row.topic.clone()}
                {resolved}
                {unread}
                span { class: "pimble-discussions__count", {format!("{}", row.count)} }
            }
            div {
                class: "pimble-discussions__latest",
                {row.latest.clone()}
                {when}
            }
        }
    }
}

// ── Discussions in text (contract section 6, wave 2) ────────────────────

/// The embed attributes naming a discussion shown inline: the node as a
/// `pimble:` link (any reader of the content counts it as a link to it),
/// its kind, which says this view draws it, and its topic when it was put
/// there (shown until the node arrives).
pub(crate) const NODE_ATTR: &str = "data-node";
pub(crate) const KIND_ATTR: &str = "data-kind";
pub(crate) const KIND_DISCUSSION: &str = "discussion";
pub(crate) const TITLE_ATTR: &str = "data-title";

/// The discussion an embed's attributes name, if they name one.
pub(crate) fn embedded_discussion(attrs: &rinch_editor_core::Attrs) -> Option<(StoreId, NodeId)> {
    if attrs.get_str(KIND_ATTR) != Some(KIND_DISCUSSION) {
        return None;
    }
    let url = pimble_core::PimbleUrl::parse(attrs.get_str(NODE_ATTR)?)?;
    Some((url.store, url.node))
}

/// The attributes of an inline discussion of node `(store_id, node_id)`.
pub(crate) fn embed_attrs(store: AppStore, store_id: StoreId, node_id: NodeId) -> rinch_editor_core::Attrs {
    rinch_editor_core::Attrs::new()
        .with(NODE_ATTR, pimble_core::PimbleUrl::node(store_id, node_id).to_string())
        .with(KIND_ATTR, KIND_DISCUSSION)
        .with(TITLE_ATTR, store.display_label(store_id, node_id))
}

thread_local! {
    /// Each pane's editor's node view for discussions, registered once.
    static REGISTERED: RefCell<[Option<crate::rinch_editor::NodeViewRegistration>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
}

/// Draw discussions inline in pane `pane`'s editor (once per editor; the
/// editors live as long as the window).
pub(crate) fn register(store: AppStore, pane: PaneId) {
    if REGISTERED.with(|r| r.borrow()[pane.index()].is_some()) {
        return;
    }
    let spec = crate::rinch_editor::NodeViewSpec::new("embed", move |__scope: &mut RenderScope, ctx: crate::rinch_editor::NodeViewCtx| render_inline(__scope, store, pane, ctx))
        .matches(|attrs: &rinch_editor_core::Attrs| embedded_discussion(attrs).is_some());
    let registration = crate::editor::editor(pane).register_node_view(spec);
    REGISTERED.with(|r| r.borrow_mut()[pane.index()] = Some(registration));
}

/// An inline discussion: the same view a discussion pane has, its thread in
/// a box of its own, holding the node for as long as the atom is shown.
fn render_inline(__scope: &mut RenderScope, store: AppStore, pane: PaneId, ctx: crate::rinch_editor::NodeViewCtx) -> NodeHandle {
    let attrs = untracked(|| ctx.attrs.get());
    let Some((store_id, node_id)) = embedded_discussion(&attrs) else {
        return rsx! { div { class: "pimble-discussion__empty", "This discussion could not be read." } };
    };
    let slot = Slot::Inline(pane, store_id, node_id);
    let ui = DiscussionUi::new();
    inline_opened(store, slot, ui);
    __scope.on_cleanup(move || inline_closed(store, slot));
    let view = discussion_view(__scope, store, slot, ui, || true);
    rsx! {
        div {
            class: "pimble-discussion-inline",
            contenteditable: "false",
            onmousedown: move || crate::pane_view::press_in_view(),
            {view}
        }
    }
}

// ── View > "Discussions..." (contract section 8) ────────────────────────

/// The window listing every discussion with unread messages in the open
/// stores, newest first; a line opens its discussion in the focused pane.
pub(crate) fn render_unread_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let rows = move || unread_rows(store);
    rsx! {
        Modal {
            opened_fn: move || store.discussions_modal.get(),
            onclose: move || store.discussions_modal.set(false),
            title: "Discussions",
            size: "md",
            div {
                class: "pimble-discussions pimble-discussions--modal",
                if rows().is_empty() {
                    div { class: "pimble-discussion__empty", "Nothing unread." }
                }
                for row in rows() {
                    div {
                        key: format!("{:?}", row),
                        onclick: {
                            let (s, n) = (row.store_id, row.node_id);
                            move || {
                                store.discussions_modal.set(false);
                                let pane = untracked(|| store.focused_pane.get());
                                store.link_open.set(Some(LinkOpen { value: format!("node_{s}_{n}"), store_id: s, node_id: n, anchor: None, pane: Some(pane) }));
                            }
                        },
                        {host_row_static(__scope, store, row.clone())}
                    }
                }
            }
        }
    }
}

/// A row of the unread list: as a host's row, with what it is on.
fn host_row_static(__scope: &mut RenderScope, store: AppStore, row: DiscussionRow) -> NodeHandle {
    let on = store
        .get_node_signal(row.store_id, row.node_id)
        .and_then(|sig| untracked(|| sig.with(|n| n.attached_to)))
        .map(|host| format!(" · on {}", store.display_label(row.store_id, host)))
        .unwrap_or_default();
    rsx! {
        div {
            class: "pimble-discussions__row",
            div {
                class: "pimble-discussions__topic",
                {row.topic.clone()}
                span { class: "pimble-discussions__unread", {format!("{} new", row.unread)} }
            }
            div {
                class: "pimble-discussions__latest",
                {format!("{}{on}", row.latest)}
            }
        }
    }
}
