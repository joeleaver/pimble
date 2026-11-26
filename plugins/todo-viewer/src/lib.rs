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
mod keys;
mod outline;
mod rows;

use std::sync::Arc;

use pimble_plugin_sdk::pimble_core::custom_keys;
use pimble_plugin_sdk::pimble_ui::appearance::IconGlyph;
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
}

const LIST_STYLE: &str = "display: flex; flex-direction: column; gap: 8px; padding: 12px 16px; height: 100%; box-sizing: border-box; outline: none; overflow: auto;";
const PANEL_STYLE: &str = "display: flex; flex-direction: column; gap: 4px; padding: 8px; max-width: 760px; box-sizing: border-box;";

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
                        {rows::render_list(__scope, &tr, writable)}
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
