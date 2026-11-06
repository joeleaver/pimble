//! The todo list viewer (docs/TODO_CONTRACT.md waves 1 and 2): a metanode whose
//! source is its own subtree, drawn as a checklist in the tree's own order;
//! an item's children are its sub-todos.
//!
//! Done is a timestamp in the node field `done` (decision 1): checking writes
//! when, unchecking removes it, one edit of that node each. Everything else it
//! changes is an ordinary edit of real nodes through the Pimble API, judged by
//! core as the explorer's edits and the kanban's are: a rename is the one
//! write with the explicit-title rule, a new item is a create (then named in
//! place), a move is planned and judged in the metanode first, a delete is
//! asked of `can_delete` first. It opens things as the kanban does (beside the
//! list, which keeps the focus), looks like the kanban (`pimble_ui::viewer`),
//! and follows every change the API hears.

mod due;
mod outline;

use std::sync::Arc;

use pimble_plugin_sdk::pimble_core::custom_keys;
use pimble_plugin_sdk::pimble_ui::appearance::{display_color, row_background, IconGlyph};
use pimble_plugin_sdk::pimble_ui::presence::{more_label, PersonChip};
use pimble_plugin_sdk::pimble_ui::viewer as look;
use pimble_plugin_sdk::prelude::*;
use rinch::core::dom::NodeId as DomId;

use outline::{drop_target, key_of, Item, Outline, Step, Target, DONE};

/// The sortable group of the rows: a drag lands only among rows.
const ITEMS: &str = "items";

/// The viewer's id, as a metanode names it in `metanode:viewer`.
pub const VIEWER: &str = "todo";

/// How deep "New Todo List" reads its subtree: sub-todos to this many levels.
pub const DEPTH: u32 = 16;

/// How far each level of sub-todos is indented, in pixels.
const INDENT: u32 = 22;

fn describe() -> Manifest {
    manifest("todo-viewer", "Todo List", env!("CARGO_PKG_VERSION"))
        .with(viewer(VIEWER, "Todo list", &[SourceShape::Subtree { depth: 1 }]))
        .with(recipe("new-todo-list", "New Todo List", "Todo List", Some("list-check"), VIEWER, DEPTH, &[]))
        .writes(StoreAsk::Chosen)
}

/// How the person has the list on this device: the items they folded (by
/// [`key_of`]) and whether done items are hidden. Kept by the host
/// (`Host::remember`), never in the store.
#[derive(Debug, Clone, Default, PartialEq)]
struct Look {
    folded: Vec<String>,
    hide_done: bool,
}

impl Look {
    fn parse(text: &str) -> Look {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else { return Look::default() };
        let folded = value
            .get("folded")
            .and_then(|f| f.as_array())
            .map(|f| f.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let hide_done = value.get("hide_done").and_then(|h| h.as_bool()).unwrap_or(false);
        Look { folded, hide_done }
    }

    fn to_json(&self) -> String {
        serde_json::json!({ "folded": self.folded, "hide_done": self.hide_done }).to_string()
    }
}

/// A move that `plan` or the metanode warned about, waiting for "Move anyway".
#[derive(Debug, Clone, PartialEq)]
struct Waiting {
    node: Addr,
    target: Target,
    sentence: String,
}

/// The list's state and what acts on it.
#[derive(Clone)]
struct Todo {
    api: Arc<Pimble>,
    host: Host,
    metanode: Addr,
    outline: Signal<Outline>,
    /// The forest has been read once.
    loaded: Signal<bool>,
    chosen: Signal<Option<Addr>>,
    /// The item whose name is being typed.
    renaming: Signal<Option<Addr>>,
    draft: Signal<String>,
    /// An item just made here, still unnamed: left without a name, it is
    /// deleted again.
    fresh: Signal<Option<Addr>>,
    status: Signal<String>,
    waiting: Signal<Option<Waiting>>,
    /// The list's element, which holds the keyboard.
    root: Signal<Option<NodeHandle>>,
    /// The name field while one is open.
    field: Signal<Option<NodeHandle>>,
    look: Signal<Look>,
    /// The item the pointer is over: it offers its trash can.
    hovered: Signal<Option<Addr>>,
}

fn dom_id(addr: Addr) -> String {
    format!("todo-{}", addr.node)
}

/// The node a row's key in the sortable list (its [`key_of`]) names.
fn addr_of(key: &str) -> Option<Addr> {
    let (store, node) = key.split_once('/')?;
    use pimble_plugin_sdk::pimble_core::{NodeId, StoreId};
    Some(Addr::new(StoreId::parse(store).ok()?, NodeId::parse(node).ok()?))
}

impl Todo {
    /// Read the forest into the outline: the forest read, which says what a
    /// change must touch to be read again.
    async fn read(&self) -> Option<Forest> {
        match self.api.metanode_source(self.metanode).await {
            Ok(forest) => {
                let outline = Outline::of(self.metanode, forest.node.access.allows_write(), &forest.entries, display_label);
                // Something chosen that is no longer listed is let go,
                // unless it is being named: an item just made is chosen
                // before the list has read it.
                let naming = untracked(|| self.renaming.get());
                if let Some(chosen) = untracked(|| self.chosen.get()).filter(|c| Some(*c) != naming) {
                    if outline.get(chosen).is_none() {
                        self.choose(None);
                    }
                }
                if untracked(|| self.outline.get()) != outline {
                    self.outline.set(outline);
                }
                self.loaded.set(true);
                Some(forest)
            }
            Err(e) => {
                self.say(e.sentence());
                None
            }
        }
    }

    /// A click on an item: it opens beside the list, or with Ctrl (Cmd)
    /// held in the list's place; a second click within
    /// [`pimble_plugin_sdk::DOUBLE_CLICK`] renames it instead. The list keeps
    /// the keyboard.
    fn click_item(&self, addr: Addr, modifiers: rinch::core::ModifierState) {
        let again = clicked_again(addr);
        self.choose(Some(addr));
        if untracked(|| self.renaming.get()) == Some(addr) {
            return;
        }
        let Some(item) = untracked(|| self.outline.with(|o| o.get(addr).cloned())) else { return };
        if again {
            if item.writable {
                self.start_rename(addr, item.label.clone());
            }
            return;
        }
        if modifiers.ctrl || modifiers.meta {
            self.host.open(addr);
        } else {
            self.host.open_beside(addr);
        }
        self.take_keyboard();
    }

    /// Choose `chosen`, and tell the host the person is on it.
    fn choose(&self, chosen: Option<Addr>) {
        if untracked(|| self.chosen.get()) == chosen {
            return;
        }
        self.chosen.set(chosen);
        self.host.focus(chosen);
    }

    fn take_keyboard(&self) {
        if let Some(root) = untracked(|| self.root.get()) {
            root.focus();
        }
    }

    fn say(&self, line: impl Into<String>) {
        self.status.set(line.into());
    }

    fn relook(&self, f: impl FnOnce(&mut Look)) {
        let mut look = untracked(|| self.look.get());
        f(&mut look);
        if untracked(|| self.look.get()) != look {
            self.host.remember(look.to_json());
            self.look.set(look);
        }
    }

    fn toggle_fold(&self, addr: Addr) {
        let key = key_of(addr);
        self.relook(|l| match l.folded.iter().position(|f| *f == key) {
            Some(i) => {
                l.folded.remove(i);
            }
            None => l.folded.push(key),
        });
    }

    fn set_folded(&self, addr: Addr, folded: bool) {
        let key = key_of(addr);
        self.relook(|l| {
            let at = l.folded.iter().position(|f| *f == key);
            match (folded, at) {
                (true, None) => l.folded.push(key),
                (false, Some(i)) => {
                    l.folded.remove(i);
                }
                _ => {}
            }
        });
    }

    /// The rows shown now, tracked.
    fn rows(&self) -> Vec<Item> {
        let (folded, hide_done) = self.look.with(|l| (l.folded.clone(), l.hide_done));
        self.outline.with(|o| o.visible(&folded, hide_done))
    }

    /// The rows shown now, each with the key that redraws it (it, and its
    /// progress), tracked.
    fn shown(&self) -> Vec<(String, Item)> {
        let rows = self.rows();
        self.outline.with(|o| rows.into_iter().map(|r| { let (d, a) = o.progress(r.addr); (format!("{r:?} {d}/{a}"), r) }).collect())
    }

    /// Whether a drop may land where it is over, asked for every place a drag
    /// offers, synchronously: `plan`'s judgement over what the list holds (a
    /// row dropped into its own sub-todos, an item the person may only read).
    /// A warning is not a refusal: [`Todo::move_to`] judges the drop again in
    /// full and holds it for "Move anyway".
    fn judge_drop(&self, m: &SortMove) -> SortVerdict {
        let Some((node, target)) = self.drop_of(m) else { return SortVerdict::Refuse(String::new()) };
        let parent = (target.parent.store, Some(target.parent.node));
        match self.api.view(|v| plan::plan_move(v, &[(node.store, node.node)], parent, target.place)) {
            Ok(_) => SortVerdict::Accept,
            Err(refusal) => SortVerdict::Refuse(refusal.sentence().to_string()),
        }
    }

    /// The node a drop carries and where it lands.
    fn drop_of(&self, m: &SortMove) -> Option<(Addr, Target)> {
        let [key] = &m.keys[..] else { return None };
        let node = addr_of(key)?;
        let after = match m.place.after.as_deref() {
            Some(k) => Some(addr_of(k)?),
            None => None,
        };
        let rows = untracked(|| self.rows());
        let target = untracked(|| self.outline.with(|o| drop_target(o, &rows, after)))?;
        Some((node, target))
    }

    /// An accepted drop: the move, through the one function, and the row
    /// chosen so the keyboard follows it.
    fn drop_move(&self, m: SortMove) {
        let Some((node, target)) = self.drop_of(&m) else { return };
        self.choose(Some(node));
        self.take_keyboard();
        // A row dropped first under a folded one would vanish: show it.
        if target.place == Place::First && target.parent != self.metanode {
            self.set_folded(target.parent, false);
        }
        self.move_to(node, target, false);
    }

    /// Check `addr` (write when, in its `done` field) or uncheck it (remove
    /// the field): one edit of that node.
    fn toggle_done(&self, addr: Addr) {
        let Some(item) = untracked(|| self.outline.with(|o| o.get(addr).cloned())) else { return };
        if !item.writable {
            return self.say(pimble_plugin_sdk::pimble_core::StoreAccess::READ_ONLY_REFUSAL);
        }
        let checking = item.done.is_none();
        let k = self.clone();
        spawn_local(async move {
            let edited = k
                .api
                .edit(addr, move |doc, now| -> Result<(), ApiError> {
                    doc.set_field(DONE, if checking { Some(now) } else { None }, now)?;
                    Ok(())
                })
                .await;
            match edited {
                Ok(_) => k.say(""),
                Err(e) => k.say(e.sentence()),
            }
        });
    }

    /// The one way anything moves in the list, judged in the metanode first
    /// as the kanban's moves are: a refusal is the explorer's sentence and
    /// nothing is sent; a warning waits for "Move anyway" unless `confirmed`.
    fn move_to(&self, node: Addr, target: Target, confirmed: bool) {
        let k = self.clone();
        self.waiting.set(None);
        spawn_local(async move {
            let _ = k.api.stores().await;
            let edit = MetanodeEdit::Move { nodes: vec![node], target: target.parent, place: target.place };
            let judged = match k.api.judge_in_metanode(k.metanode, edit).await {
                Ok(judged) => judged,
                Err(e) => return k.say(e.sentence()),
            };
            let Some(plan) = judged.plan else { return };
            if plan.stays() {
                return k.say("");
            }
            if !judged.warnings.is_empty() && !confirmed {
                k.waiting.set(Some(Waiting { node, target, sentence: judged.warnings.join(" ") }));
                return;
            }
            match k.api.move_nodes(&plan).await {
                Ok(moved) => {
                    k.say("");
                    if let Some(to) = moved.first() {
                        if untracked(|| k.chosen.get()) == Some(node) && to.addr != node {
                            k.choose(Some(to.addr));
                        }
                    }
                }
                Err(e) => k.say(e.sentence()),
            }
        });
    }

    fn step(&self, step: Step) {
        let Some(chosen) = untracked(|| self.chosen.get()) else { return };
        let target = untracked(|| self.outline.with(|o| o.step(chosen, step)));
        if let Some(target) = target {
            // An item indented under a folded one would vanish: show it.
            if step == Step::Indent {
                self.set_folded(target.parent, false);
            }
            self.move_to(chosen, target, false);
        }
    }

    fn start_rename(&self, addr: Addr, now: String) {
        self.draft.set(now);
        self.renaming.set(Some(addr));
    }

    /// Enter in the name field (`next`): keep the name
    /// through the one write, with the explicit-title rule. With Enter a new
    /// item follows right after it, as an outliner's Enter makes one; an
    /// item made here and left without a name is deleted again instead.
    fn commit_rename(&self, next: bool) {
        let Some(addr) = untracked(|| self.renaming.get()) else { return };
        let title = untracked(|| self.draft.get()).trim().to_string();
        let fresh = untracked(|| self.fresh.get()) == Some(addr);
        self.renaming.set(None);
        self.fresh.set(None);
        self.take_keyboard();
        if title.is_empty() {
            if fresh {
                self.remove_fresh(addr);
            }
            return;
        }
        let k = self.clone();
        spawn_local(async move {
            let edited = k
                .api
                .edit(addr, move |doc, now| -> Result<(), ApiError> {
                    doc.set_title(&title, now)?;
                    doc.set_custom(custom_keys::EXPLICIT_TITLE, &serde_json::Value::Bool(true), now)?;
                    Ok(())
                })
                .await;
            match edited {
                Ok(_) if next => k.add_after(Some(addr)),
                Ok(_) => {}
                Err(e) => k.say(e.sentence()),
            }
        });
    }

    fn cancel_rename(&self) {
        let Some(addr) = untracked(|| self.renaming.get()) else { return };
        let fresh = untracked(|| self.fresh.get()) == Some(addr);
        self.renaming.set(None);
        self.fresh.set(None);
        self.take_keyboard();
        if fresh {
            self.remove_fresh(addr);
        }
    }

    /// An item made here and never named: deleted again, quietly.
    fn remove_fresh(&self, addr: Addr) {
        let k = self.clone();
        spawn_local(async move {
            if let Err(e) = k.api.delete(addr).await {
                k.say(e.sentence());
            }
        });
    }

    /// A new item right after `after` (beside it, under its parent), or last
    /// in the list; then its name typed in place.
    fn add_after(&self, after: Option<Addr>) {
        let k = self.clone();
        let (parent, place) = match after.and_then(|a| untracked(|| self.outline.with(|o| o.get(a).cloned()))) {
            Some(item) => (item.parent.unwrap_or(self.metanode), Place::After(item.addr.node)),
            None => (self.metanode, Place::Last),
        };
        spawn_local(async move {
            let init = match pimble_api::recipes::document("", None) {
                Ok(init) => init,
                Err(e) => return k.say(e.sentence()),
            };
            match k.api.create(parent, place, init).await {
                Ok(made) => {
                    k.say("");
                    k.choose(Some(made));
                    k.fresh.set(Some(made));
                    k.start_rename(made, String::new());
                }
                Err(e) => k.say(e.sentence()),
            }
        });
    }

    fn delete(&self, addr: Addr) {
        let k = self.clone();
        let label = untracked(|| self.outline.with(|o| o.get(addr).map(|i| i.label.clone()))).unwrap_or_default();
        spawn_local(async move {
            if let Err(e) = k.api.can_delete(&[addr]).await {
                return k.say(e.sentence());
            }
            match k.api.delete(addr).await {
                Ok(()) => {
                    if untracked(|| k.chosen.get()) == Some(addr) {
                        k.choose(None);
                    }
                    k.say(format!("Deleted \u{201c}{label}\u{201d}. View > Recently Deleted puts it back."));
                }
                Err(e) => k.say(e.sentence()),
            }
        });
        self.take_keyboard();
    }

    /// The list's keys (docs/TODO_CONTRACT.md section 3). Answers whether it
    /// took the key.
    fn key(&self, key: &KeyEventData) -> bool {
        if untracked(|| self.renaming.get()).is_some() {
            return false;
        }
        let rows = untracked(|| self.rows());
        let outline = untracked(|| self.outline.get());
        let chosen = untracked(|| self.chosen.get());
        let at = chosen.and_then(|c| rows.iter().position(|r| r.addr == c));
        let item = chosen.and_then(|c| outline.get(c).cloned());
        let writable = item.as_ref().is_some_and(|i| i.writable);
        // Alt+Shift+arrows move, as in the explorer and the kanban; Tab and
        // Shift+Tab indent and outdent.
        let moving = match key.key.as_str() {
            "ArrowUp" if key.alt && key.shift => Some(Step::Up),
            "ArrowDown" if key.alt && key.shift => Some(Step::Down),
            "ArrowRight" if key.alt && key.shift => Some(Step::Indent),
            "ArrowLeft" if key.alt && key.shift => Some(Step::Outdent),
            "Tab" if key.shift => Some(Step::Outdent),
            "Tab" => Some(Step::Indent),
            _ => None,
        };
        if let Some(step) = moving {
            if writable {
                self.step(step);
            }
            return true;
        }
        match key.key.as_str() {
            "ArrowDown" => {
                let next = match at {
                    None => rows.first(),
                    Some(i) => rows.get(i + 1).or(rows.get(i)),
                };
                self.choose(next.map(|r| r.addr).or(chosen));
            }
            "ArrowUp" => {
                let prev = match at {
                    None => rows.first(),
                    Some(i) => rows.get(i.saturating_sub(1)),
                };
                self.choose(prev.map(|r| r.addr).or(chosen));
            }
            "ArrowLeft" => {
                let Some(item) = item else { return true };
                let folded = untracked(|| self.look.with(|l| l.folded.contains(&key_of(item.addr))));
                if !item.children.is_empty() && !folded {
                    self.set_folded(item.addr, true);
                } else if let Some(parent) = item.parent {
                    self.choose(Some(parent));
                }
            }
            "ArrowRight" => {
                if let Some(item) = item.filter(|i| !i.children.is_empty()) {
                    self.set_folded(item.addr, false);
                }
            }
            "Enter" => {
                let Some(c) = chosen else { return false };
                if key.ctrl || key.meta {
                    self.host.open(c);
                } else {
                    self.host.open_beside(c);
                }
            }
            " " => {
                let Some(c) = chosen else { return false };
                self.toggle_done(c);
            }
            "F2" if writable => {
                if let Some(item) = item {
                    self.start_rename(item.addr, item.label.clone());
                }
            }
            "Delete" if writable => {
                if let Some(c) = chosen {
                    self.delete(c);
                }
            }
            "Escape" if chosen.is_some() => self.choose(None),
            _ => return false,
        }
        true
    }
}

const LIST_STYLE: &str = "display: flex; flex-direction: column; gap: 8px; padding: 12px 16px; height: 100%; box-sizing: border-box; outline: none; overflow: auto;";
const PANEL_STYLE: &str = "display: flex; flex-direction: column; gap: 4px; padding: 8px; max-width: 760px; box-sizing: border-box;";
const SLOT: &str = "display: inline-flex; width: 16px; height: 16px; flex-shrink: 0; margin-top: 2px;";

/// The viewer.
pub fn view(__scope: &mut RenderScope, mount: Mount) -> NodeHandle {
    let Some(metanode) = mount.metanode() else {
        return rsx! { div { style: look::LINE_STYLE, "This viewer was shown without a metanode." } };
    };
    let t = Todo {
        api: mount.api.clone(),
        host: mount.host.clone(),
        metanode,
        outline: Signal::new(Outline::default()),
        loaded: Signal::new(false),
        chosen: Signal::new(None),
        renaming: Signal::new(None),
        draft: Signal::new(String::new()),
        fresh: Signal::new(None),
        status: Signal::new(String::new()),
        waiting: Signal::new(None),
        root: Signal::new(None),
        field: Signal::new(None),
        look: Signal::new(mount.remembered().map(|r| Look::parse(&r)).unwrap_or_default()),
        hovered: Signal::new(None),
    };

    // Read the forest, then keep it live from every change the API hears.
    {
        let t = t.clone();
        spawn_local(async move {
            let mut forest = t.read().await;
            while let Some(change) = t.api.next_change().await {
                if forest.as_ref().map_or(true, |f| f.touched_by(&change)) {
                    forest = t.read().await.or(forest);
                }
            }
        });
    }

    // A name field takes the keyboard when it is drawn, a turn later.
    {
        let (field, renaming) = (t.field, t.renaming);
        __scope.create_effect(move || {
            if renaming.get().is_none() {
                return;
            }
            if let Some(field) = field.get() {
                let _ = set_timeout(0, move || field.focus());
            }
        });
    }

    // Following someone: scroll to the item they are on.
    {
        let (t, doc) = (t.clone(), __scope.doc_weak());
        __scope.create_effect(move || {
            let Some(leader) = t.host.following() else { return };
            let items: Vec<Addr> = t.outline.with(|o| o.items.iter().map(|i| i.addr).collect());
            let Some(item) = items.into_iter().find(|a| t.host.people_in(*a).iter().any(|p| p.key == leader)) else { return };
            let Some(doc) = doc.upgrade() else { return };
            let found = doc.borrow().query_selector(&format!("#{}", dom_id(item)));
            if let Some(found) = found {
                let _ = set_timeout(0, {
                    let doc = std::rc::Rc::downgrade(&doc);
                    move || NodeHandle::new(DomId(found.0), doc.clone()).scroll_into_view()
                });
            }
        });
    }

    let (tk, tw1, tw2, ta, th, tr) = (t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone());
    let (outline, status, waiting, loaded, root, look_now) = (t.outline, t.status, t.waiting, t.loaded, t.root, t.look);
    let seen = t.host.seen();
    rsx! {
        div {
            class: "todo",
            style: {move || format!("{LIST_STYLE}{}", look::surfaces(seen.dark()))},
            tabindex: "0",
            ref: root,
            onkeydown: move |key: &KeyEventData| tk.key(key),
            if status.with(|s| !s.is_empty()) {
                div { class: "todo__status", style: look::LINE_STYLE, {move || status.get()} }
            }
            if let Some(waiting) = waiting.get() {
                div {
                    class: "todo__waiting",
                    style: "display: flex; align-items: center; gap: 8px; font-size: 12px;",
                    span { {waiting.sentence.clone()} }
                    Button {
                        size: "xs",
                        variant: "filled",
                        onclick: {
                            let (t, w) = (tw1.clone(), waiting.clone());
                            move || t.move_to(w.node, w.target, true)
                        },
                        "Move anyway"
                    }
                    Button { size: "xs", variant: "default", onclick: { let t = tw2.clone(); move || t.waiting.set(None) }, "Cancel" }
                }
            }
            div {
                class: "todo__panel",
                style: format!("{PANEL_STYLE} {}", look::PANEL),
                // How far along the list is, and done items hidden or shown:
                // nothing to say about a list with no items.
                div {
                    class: "todo__head",
                    style: {move || if outline.with(|o| o.items.is_empty()) { "display: none;" } else { "display: flex; align-items: center; gap: 6px; padding: 0 4px 4px 4px;" }},
                    span {
                        style: format!("{} flex: 1;", look::LINE_STYLE),
                        {move || { let (done, all) = outline.with(|o| o.counts()); if all == 0 { String::new() } else { format!("{done} of {all} done") } }}
                    }
                    span {
                        class: "todo__hide-done",
                        style: look::ICON_BUTTON,
                        title: {move || if look_now.with(|l| l.hide_done) { "Show done items" } else { "Hide done items" }},
                        onclick: move || th.relook(|l| l.hide_done = !l.hide_done),
                        span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: if look_now.with(|l| l.hide_done) { "eye-off".to_string() } else { "eye".to_string() } } }
                    }
                }
                if loaded.get() && outline.with(|o| o.items.is_empty()) {
                    div { style: format!("{} padding: 4px;", look::LINE_STYLE), "No items yet." }
                }
                // The rows as one sortable list; a row is drawn again only when
                // it or its progress changes, the list only when whether the
                // person may change it does (a list takes its `disabled` once).
                for writable in [outline.with(|o| o.writable)] {
                    div {
                        key: writable.to_string(),
                        {render_list(__scope, &tr, writable)}
                    }
                }
                if outline.with(|o| o.writable) {
                    div {
                        class: "todo__add",
                        style: "font-size: 13px; color: var(--rinch-color-dimmed); cursor: pointer; padding: 4px 6px;",
                        onclick: { let t = ta.clone(); move || t.add_after(None) },
                        "+ Add item"
                    }
                }
            }
        }
    }
}

/// Who else is on `addr`: the app's own chips, up to three and "+n".
fn people(__scope: &mut RenderScope, t: &Todo, addr: Addr) -> NodeHandle {
    let (seen, follow) = (t.host.seen(), t.host.clone());
    rsx! {
        span {
            class: "pimble-pane__people",
            style: "display: inline-flex; align-items: center;",
            for person in seen.people_in(addr).into_iter().take(3) {
                PersonChip {
                    key: person.key.clone(),
                    person: person.clone(),
                    onfollow: { let f = follow.clone(); move |key: String| f.follow(&key) },
                }
            }
            span {
                class: "pimble-pane__more",
                style: {move || if seen.people_in(addr).len() > 3 { "" } else { "display: none;" }},
                {move || more_label(seen.people_in(addr).len().saturating_sub(3))}
            }
        }
    }
}

/// The rows, as one sortable list: a row drags whole (a press that never
/// moves 5 px stays a click), and a drop lands where [`drop_target`] says.
fn render_list(__scope: &mut RenderScope, t: &Todo, writable: bool) -> NodeHandle {
    let (td, tr) = (t.clone(), t.clone());
    let judge: SortJudge = {
        let t = t.clone();
        std::rc::Rc::new(move |m: &SortMove| t.judge_drop(m))
    };
    rsx! {
        SortableList {
            group: ITEMS,
            id: "todo".to_string(),
            label: "Todo list".to_string(),
            class: "todo__rows",
            style: format!("display: flex; flex-direction: column; gap: 4px; position: relative;{}", look::SORT_THEME),
            disabled: !writable,
            // Space checks an item off and Enter opens it, so neither lifts a
            // row; the keyboard moves rows with Alt+Shift+arrows and Tab.
            lift_keys: vec!["Insert".to_string()],
            indicator: SortIndicator::Gap,
            source: SortSource::Collapse,
            preview: SortPreview::Snapshot,
            judge: Some(judge),
            ondrop: move |m: SortMove| td.drop_move(m),
            for (key, item) in tr.shown() {
                SortableItem {
                    key: key.clone(),
                    value: key_of(item.addr),
                    disabled: !item.writable,
                    label: item.label.clone(),
                    class: "todo__row",
                    {render_item(__scope, &tr, item.clone())}
                }
            }
        }
    }
}

/// One row: its fold chevron, checkbox, icon, title, progress, due date, who
/// is on it, and its trash can.
fn render_item(__scope: &mut RenderScope, t: &Todo, item: Item) -> NodeHandle {
    let addr = item.addr;
    let seen = t.host.seen();
    let (tc, tf, tchk, tr, tfield, tfield2, td, tenter, tleave) = (t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone());
    let (chosen, renaming, hovered, look_now) = (t.chosen, t.renaming, t.hovered, t.look);
    let writable = item.writable;
    let done = item.done.is_some();
    let has_children = !item.children.is_empty();
    let (done_now, all) = untracked(|| t.outline.with(|o| o.progress(addr)));
    let indent = (item.depth.saturating_sub(1)) * INDENT;
    let (color, color2, background) = (item.color.clone(), item.color.clone(), item.background.clone());
    let show_icon = item.icon != look::PLAIN_ICON;
    let icon = item.icon.clone();
    let label = item.label.clone();
    let due = item.due.as_deref().map(|typed| due::due_of(typed, due::today(), done));
    let done_title = item.done.as_ref().map(|when| format!("Done {when}")).unwrap_or_else(|| if writable { "Check off".to_string() } else { String::new() });
    rsx! {
        div {
            class: "todo__item",
            id: dom_id(addr),
            style: {|| {
                let leader = seen.following().and_then(|key| seen.people_in(addr).into_iter().find(|p| p.key == key).map(|p| p.color));
                format!(
                    "display: flex; align-items: flex-start; gap: 6px; margin-left: {indent}px; font-size: 14px; line-height: 1.45; {}",
                    look::item_style(chosen.get() == Some(addr), background.as_deref().map(|b| row_background(b, seen.dark())), leader, (6, 10)),
                )
            }},
            onclick: move || tc.click_item(addr, rinch::core::get_modifier_state()),
            onmouseenter: move || tenter.hovered.set(Some(addr)),
            onmouseleave: move || {
                if untracked(|| tleave.hovered.get()) == Some(addr) {
                    tleave.hovered.set(None);
                }
            },
            // A grip says the row drags (the whole row does).
            if writable {
                span {
                    class: "todo__grip",
                    style: format!("{SLOT} margin-left: -4px; color: var(--rinch-color-dimmed); opacity: 0.6;"),
                    IconGlyph { name: "grip-vertical".to_string() }
                }
            }
            // The fold chevron's slot is always there, so titles line up.
            span {
                class: "todo__fold",
                style: format!("{SLOT} color: var(--rinch-color-dimmed);{}", if has_children { " cursor: pointer;" } else { "" }),
                title: if has_children { "Fold or unfold its sub-todos" } else { "" },
                onclick: move || if has_children { tf.toggle_fold(addr) },
                if has_children {
                    IconGlyph { name: if look_now.with(|l| l.folded.contains(&key_of(addr))) { "chevron-right".to_string() } else { "chevron-down".to_string() } }
                }
            }
            span {
                class: {if done { "todo__check todo__check--done" } else { "todo__check" }},
                style: format!("{SLOT}{}{}", if done { " color: var(--rinch-primary-color);" } else { " color: var(--rinch-color-dimmed);" }, if writable { " cursor: pointer;" } else { "" }),
                title: done_title,
                onclick: move || tchk.toggle_done(addr),
                IconGlyph { name: if done { "square-check".to_string() } else { "square".to_string() } }
            }
            if show_icon {
                span {
                    style: {let color = color.clone(); move || format!("{SLOT}{}", color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default())},
                    IconGlyph { name: icon.clone() }
                }
            }
            if renaming.get() == Some(addr) {
                input {
                    class: "todo__field",
                    style: look::FIELD_STYLE,
                    ref: tfield.field,
                    placeholder: "Item name",
                    value: {move || tfield.draft.get()},
                    oninput: move |v: String| tfield2.draft.set(v),
                    onkeydown: {
                        let t = tr.clone();
                        move |key: &KeyEventData| match key.key.as_str() {
                            "Enter" => { t.commit_rename(true); true }
                            "Escape" => { t.cancel_rename(); true }
                            _ => false,
                        }
                    },
                }
            } else {
                span {
                    class: "todo__label",
                    style: {
                        let color = color2.clone();
                        move || format!(
                            "flex: 1; overflow-wrap: anywhere;{}{}",
                            if done { " text-decoration: line-through; color: var(--rinch-color-dimmed);" } else { "" },
                            match (&color, done) { (Some(c), false) => format!(" color: {};", display_color(c, seen.dark())), _ => String::new() },
                        )
                    },
                    {label.clone()}
                }
            }
            if all > 0 {
                span { class: "todo__progress", style: format!("{} margin-top: 2px;", look::LINE_STYLE), {format!("{done_now}/{all}")} }
            }
            if let Some(due) = due.clone() {
                span {
                    class: if due.overdue { "todo__due todo__due--overdue" } else { "todo__due" },
                    style: format!("{} margin-top: 2px; white-space: nowrap;{}", look::LINE_STYLE, if due.overdue { " color: var(--rinch-color-red-6);" } else { "" }),
                    title: due.title.clone(),
                    {due.text.clone()}
                }
            }
            {people(__scope, t, addr)}
            if writable {
                span {
                    class: "todo__trash-slot",
                    style: SLOT,
                    if (hovered.get() == Some(addr) || chosen.get() == Some(addr)) && renaming.get() != Some(addr) {
                        span {
                            class: "todo__trash",
                            title: "Delete (View > Recently Deleted puts it back)",
                            style: "display: inline-flex; width: 16px; height: 16px; cursor: pointer; color: var(--rinch-color-dimmed);",
                            onclick: { let t = td.clone(); move || t.delete(addr) },
                            IconGlyph { name: "trash".to_string() }
                        }
                    }
                }
            }
        }
    }
}

pimble_plugin_sdk::plugin!(describe: describe, view: view, guide: include_bytes!("../guide.json"));

#[cfg(test)]
mod tests {
    /// What it tells agents (docs/AGENT_GUIDES_CONTRACT.md) reads, names
    /// every viewer the manifest provides, and nothing else.
    #[test]
    fn the_guide_reads_and_names_its_viewers() {
        let guide = pimble_plugin_sdk::abi::guide::Guide::parse(include_bytes!("../guide.json")).unwrap();
        let mut guided: Vec<&str> = guide.viewers.iter().map(|v| v.viewer.as_str()).collect();
        let manifest = super::describe();
        let mut provided: Vec<&str> = manifest.viewers().map(|v| v.id.as_str()).collect();
        guided.sort();
        provided.sort();
        assert_eq!(guided, provided);
    }

    use super::*;

    #[test]
    fn it_provides_the_todo_viewer_and_new_todo_list() {
        let m = describe();
        assert_eq!(m.viewers().next().map(|v| v.id.as_str()), Some(VIEWER));
        let recipe = m.recipes().next().expect("New Todo List");
        assert_eq!(recipe.label, "New Todo List");
        assert_eq!(recipe.depth, DEPTH);
        assert!(recipe.children.is_empty());
        assert!(m.asks.write);
    }

    #[test]
    fn the_look_is_remembered_as_json_and_read_back() {
        let look = Look { folded: vec!["a/b".into()], hide_done: true };
        assert_eq!(Look::parse(&look.to_json()), look);
        assert_eq!(Look::parse("not json"), Look::default());
    }
}
