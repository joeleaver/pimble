//! The timeline viewer (docs/TIMELINE_CONTRACT.md wave 1): a metanode's
//! source on a horizontal line of time, each node by its own fields (`start`,
//! else `date`, and `end`): a point for one moment a day or finer, a bar for a
//! span. Wave 1 reads the Gregorian calendar (BC and AD, to the second),
//! defined as data like any calendar to come (`calendar`).
//!
//! It zooms (Ctrl+wheel, a pinch, + and -) from seconds to billions of years
//! around the pointer, pans (a sideways swipe, Shift+wheel, a drag of the
//! background, the arrow keys), fits what it shows, and jumps to now. A top
//! node with no moment of its own is a lane for the nodes under it. It opens
//! things as the kanban does (a click opens beside, which keeps the keyboard;
//! Ctrl+click in place; double-click or F2 renames) and looks like it
//! (`pimble_ui::viewer`). Where it is and how close is this device's own
//! (`Host::remember`). Nodes whose fields do not read as moments are listed
//! below the line, as typed, never guessed at.
//!
//! Wave 3: a drag moves a node's moments (its bar whole, or an end), each kept
//! to the precision it was typed with (`layout::drag`), Alt+arrows step the
//! chosen one, and a double-click on the line makes a node at that moment.
//!
//! Wave 2: a calendar node (the `calendar` viewer, `calendar_view`) holds a
//! calendar of one's own as its settings; a timeline's settings (the gear)
//! name the calendar it reads in and the fields it reads.

mod calendar;
mod calendar_view;
mod tools;
mod layout;

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use pimble_plugin_sdk::pimble_core::custom_keys;
use pimble_plugin_sdk::pimble_ui::appearance::{display_color, row_background, IconGlyph};
use pimble_plugin_sdk::pimble_ui::presence::{more_label, PersonChip};
use pimble_plugin_sdk::pimble_ui::viewer as look;
use pimble_plugin_sdk::prelude::*;
use rinch::core::WheelEvent;

use calendar::{Calendar, Moment};
use layout::{drag, label_px, pack, place, precision_at, ticks, Dragged, Fields, Grip, Item, Placed, View};
use pimble_plugin_sdk::pimble_api::{Affected, Change};
use pimble_plugin_sdk::pimble_core::PimbleUrl;
use pimble_plugin_sdk::pimble_crdt::{DataEdit, DataStep};

/// The viewer's id, as a metanode names it in `metanode:viewer`.
pub const VIEWER: &str = "timeline";
/// A calendar node's viewer: the calendar is its settings (`data.calendar`,
/// docs/TIMELINE_CONTRACT.md section 3), made up in a form.
pub const CALENDAR_VIEWER: &str = "calendar";

const AXIS_H: f64 = 46.0;
/// The axis of each other calendar shown, under the timeline's own.
const ALSO_H: f64 = 40.0;

/// How tall the axes are with `also` other calendars under the own one.
fn axes_h(also: usize) -> f64 {
    AXIS_H + ALSO_H * also as f64
}
const ROW_H: f64 = 30.0;
const LANE_HEAD_H: f64 = 24.0;
/// How far past the panel's right edge the line is drawn, so a pane made
/// wider shows ticks before the next gesture reads its width again.
const OVERDRAW: f64 = 2400.0;

fn describe() -> Manifest {
    manifest("timeline-viewer", "Timeline", env!("CARGO_PKG_VERSION"))
        .with(viewer(VIEWER, "Timeline", &[SourceShape::Any]))
        .with(viewer(CALENDAR_VIEWER, "Calendar", &[SourceShape::Any]))
        .with(recipe("new-timeline", "New Timeline", "Timeline", Some("timeline-event"), VIEWER, 2, &[]))
        .with(recipe("new-calendar", "New Calendar", "Calendar", Some("calendar"), CALENDAR_VIEWER, 1, &[]))
        .writes(StoreAsk::Chosen)
}

/// A timeline's own settings (its `data` under `timeline`, shared with
/// everyone who sees it): the calendar node it reads in, as a pimble: link
/// (none: the Gregorian calendar), and which fields it reads.
#[derive(Debug, Clone, PartialEq, Default)]
struct Settings {
    calendar: Option<Addr>,
    /// Other calendars shown under its own, lined up (docs/TIMELINE_CONTRACT.md
    /// wave 4): an axis each, and a field that does not read in its own
    /// calendar is read in them.
    also: Vec<CalRef>,
    fields: Fields,
}

/// A calendar a timeline names: the built-in Gregorian, or a calendar node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalRef {
    Gregorian,
    Node(Addr),
}

impl CalRef {
    fn parse(text: &str) -> Option<CalRef> {
        if text.trim().eq_ignore_ascii_case("gregorian") {
            return Some(CalRef::Gregorian);
        }
        PimbleUrl::parse(text).map(|u| CalRef::Node(Addr::new(u.store, u.node)))
    }

    fn json(self) -> serde_json::Value {
        match self {
            CalRef::Gregorian => serde_json::Value::String("gregorian".into()),
            CalRef::Node(a) => serde_json::Value::String(PimbleUrl::node(a.store, a.node).to_string()),
        }
    }

    fn of(calendar: Option<Addr>) -> CalRef {
        calendar.map_or(CalRef::Gregorian, CalRef::Node)
    }
}

fn parse_settings(text: Option<&str>) -> Settings {
    let v = text.and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok()).unwrap_or_default();
    let named = |key: &str, default: &str| {
        v.get(key).and_then(|f| f.as_str()).map(str::trim).filter(|f| !f.is_empty()).unwrap_or(default).to_string()
    };
    let calendar = v.get("calendar").and_then(|c| c.as_str()).and_then(PimbleUrl::parse).map(|u| Addr::new(u.store, u.node));
    let mut also: Vec<CalRef> = Vec::new();
    for c in v.get("also").and_then(|a| a.as_array()).into_iter().flatten().filter_map(|c| c.as_str()).filter_map(CalRef::parse) {
        if c != CalRef::of(calendar) && !also.contains(&c) {
            also.push(c);
        }
    }
    Settings { calendar, also, fields: Fields { start: named("start", layout::START), date: named("date", layout::DATE), end: named("end", layout::END) } }
}

/// Whether `change` touched the document of `addr` (a calendar node).
fn names(change: &Change, addr: Addr) -> bool {
    let (store, affected) = match change {
        Change::Store(c) => (c.store, Affected::of(&c.kind)),
        Change::Resynced { store, changed } => (*store, Affected::documents(changed.clone())),
        Change::Rejudged { .. } => return false,
    };
    store == addr.store && (affected.nodes.contains(&addr.node) || affected.gone.contains(&addr.node))
}

/// Write one of a metanode's viewer settings (`data[viewer][key]`), or take
/// it away (`None`), leaving the rest as they are.
async fn set_setting(api: &Pimble, metanode: Addr, viewer: &'static str, key: &'static str, value: Option<serde_json::Value>) -> Result<(), ApiError> {
    api.edit(metanode, move |doc, _now| -> Result<(), ApiError> {
        if !doc.data_json().get(viewer).is_some_and(|v| v.is_object()) {
            doc.set_data(viewer, &serde_json::json!({}))?;
        }
        let edit = match &value {
            Some(v) => DataEdit::Set(v),
            None => DataEdit::Remove,
        };
        doc.edit_data(&[DataStep::Key(viewer), DataStep::Key(key)], edit)?;
        Ok(())
    })
    .await
    .map(|_| ())
}

/// Where the person was on the line, remembered on this device.
fn parse_view(text: &str) -> Option<View> {
    let v = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let start = v.get("start")?.as_i64()?;
    let scale = v.get("scale")?.as_f64()?;
    (scale.is_finite() && scale > 0.0).then_some(View { start, scale: scale.clamp(layout::MIN_SCALE, layout::MAX_SCALE) })
}

fn view_json(v: View) -> String {
    serde_json::json!({ "start": v.start, "scale": v.scale }).to_string()
}

/// Rows and lanes at the current zoom: each item's top in pixels, and each
/// lane's band.
#[derive(Debug, Clone, Default, PartialEq)]
struct Bands {
    top: HashMap<Addr, f64>,
    /// `(lane, its label, top, height)`; the items in no lane first, unnamed.
    lanes: Vec<(Option<Addr>, String, f64, f64)>,
    height: f64,
}

fn bands(placed: &Placed, view: View) -> Bands {
    let (rows, counts) = pack(&placed.items, view);
    let mut out = Bands::default();
    let mut y = 6.0;
    let mut order: Vec<(Option<Addr>, String)> = Vec::new();
    if counts.contains_key(&None) {
        order.push((None, String::new()));
    }
    order.extend(placed.lanes.iter().map(|l| (Some(l.addr), l.label.clone())));
    for (lane, label) in order {
        let head = if lane.is_some() { LANE_HEAD_H } else { 0.0 };
        let n = counts.get(&lane).copied().unwrap_or(0).max(1) as f64;
        let height = head + n * ROW_H + 6.0;
        for item in placed.items.iter().filter(|i| i.lane == lane) {
            out.top.insert(item.addr, y + head + rows.get(&item.addr).copied().unwrap_or(0) as f64 * ROW_H);
        }
        out.lanes.push((lane, label, y, height));
        y += height;
    }
    out.height = y + 6.0;
    out
}

/// A press on an item, and once the pointer has moved, where the drag
/// would leave it.
#[derive(Debug, Clone, PartialEq)]
struct Drag {
    item: Item,
    grip: Grip,
    x0: f32,
    /// Moved far enough to be a drag rather than a click.
    live: bool,
    result: Option<Result<Dragged, String>>,
    /// The click the press made, held until the release says it was not a
    /// drag (the desktop clicks on the press; Ctrl held: open in place).
    click: Option<bool>,
    /// Let go and written: drawn where it went until the change comes back.
    done: bool,
}

/// How far the pointer moves before a press is a drag.
const DRAG_PX: f32 = 3.0;

#[derive(Clone)]
struct Timeline {
    api: Arc<Pimble>,
    host: Host,
    metanode: Addr,
    /// The calendar it reads and writes in: the one its settings name, or
    /// the Gregorian.
    cal: Signal<Rc<Calendar>>,
    /// The other calendars it shows, in the order its settings name them,
    /// each with what is wrong with it when it cannot be shown.
    also: Signal<Vec<Rc<Calendar>>>,
    settings: Signal<Settings>,
    /// Why the calendar named is not the one in use, when it is not.
    cal_note: Signal<Option<String>>,
    /// The settings strip is open, and the calendars of the store it offers.
    settings_open: Signal<bool>,
    calendars: Signal<Vec<(Addr, String)>>,
    placed: Signal<Placed>,
    loaded: Signal<bool>,
    view: Signal<View>,
    /// Whether the view came from somewhere (remembered, a gesture): until
    /// it has, the first read fits it to what is on the line.
    placed_view: Signal<bool>,
    /// The panel's width, read at each gesture.
    width: Signal<f64>,
    chosen: Signal<Option<Addr>>,
    renaming: Signal<Option<Addr>>,
    draft: Signal<String>,
    status: Signal<String>,
    root: Signal<Option<NodeHandle>>,
    area: Signal<Option<NodeHandle>>,
    field: Signal<Option<NodeHandle>>,
    /// A drag of the background: where it was pressed and the view then.
    pan: Signal<Option<(f32, View)>>,
    /// The background was dragged since it was pressed: its click is no click.
    panned: Signal<bool>,
    /// A drag of an item (docs/TIMELINE_CONTRACT.md wave 3).
    drag: Signal<Option<Drag>>,
    /// The item a drag just let go of, and when: the click that ends the
    /// drag opens nothing.
    dropped: Signal<Option<(Addr, std::time::Instant)>>,
    /// Bumped by every change of view, so remembering it waits for a pause.
    remembering: Signal<u64>,
}

impl Timeline {
    async fn read(&self) -> Option<Forest> {
        match self.api.metanode_source(self.metanode).await {
            Ok(forest) => {
                let (cals, fields) = untracked(|| (self.cals(), self.settings.get().fields));
                let refs: Vec<&Calendar> = cals.iter().map(|c| c.as_ref()).collect();
                let placed = place(&refs, &fields, &forest.entries, display_label);
                if let Some(c) = untracked(|| self.chosen.get()) {
                    if untracked(|| self.renaming.get()) != Some(c) && !placed.items.iter().any(|i| i.addr == c) {
                        self.choose(None);
                    }
                }
                if untracked(|| self.placed.get()) != placed {
                    self.placed.set(placed);
                }
                if untracked(|| self.drag.get()).is_some_and(|d| d.done) {
                    self.drag.set(None);
                }
                self.loaded.set(true);
                if !untracked(|| self.placed_view.get()) {
                    let t = self.clone();
                    let _ = set_timeout(0, move || t.fit());
                }
                Some(forest)
            }
            Err(e) => {
                self.say(e.sentence());
                None
            }
        }
    }

    /// Every calendar it reads in: its own first, then the others it shows.
    fn cals(&self) -> Vec<Rc<Calendar>> {
        let mut out = vec![self.cal.get()];
        out.extend(self.also.get());
        out
    }

    /// The calendar `item` reads in.
    fn cal_of(&self, item: &Item) -> Rc<Calendar> {
        untracked(|| self.cals()).get(item.cal).cloned().unwrap_or_else(|| untracked(|| self.cal.get()))
    }

    /// The calendar `named` names, or why it cannot be used.
    async fn calendar_named(&self, named: CalRef) -> Result<Calendar, String> {
        let CalRef::Node(addr) = named else { return Ok(Calendar::gregorian()) };
        match self.api.doc(addr).await {
            Ok(doc) => {
                let title = doc.fields().map(|f| f.title).unwrap_or_default();
                match doc.data_json().get(CALENDAR_VIEWER).map(Calendar::from_json) {
                    Some(Ok(cal)) => Ok(cal),
                    Some(Err(why)) => Err(format!("\u{201c}{title}\u{201d} can't be used yet: {why}")),
                    None => Err(format!("\u{201c}{title}\u{201d} is not a calendar yet.")),
                }
            }
            Err(e) => Err(format!("A calendar this timeline names can't be read: {}", e.sentence())),
        }
    }

    /// Read the calendars the settings name (none: the Gregorian), then the
    /// line again in them.
    async fn load_calendar(&self) {
        let settings = untracked(|| self.settings.get());
        let (cal, mut note) = match self.calendar_named(CalRef::of(settings.calendar)).await {
            Ok(cal) => (cal, None),
            Err(why) => (Calendar::gregorian(), Some(format!("{why} Showing the Gregorian calendar meanwhile."))),
        };
        let mut also: Vec<Rc<Calendar>> = Vec::new();
        for named in settings.also {
            match self.calendar_named(named).await {
                Ok(c) => also.push(Rc::new(c)),
                Err(why) => {
                    note.get_or_insert_with(String::new);
                    if let Some(n) = note.as_mut() {
                        if !n.is_empty() {
                            n.push(' ');
                        }
                        n.push_str(&format!("{why} It is not shown."));
                    }
                }
            }
        }
        if *untracked(|| self.cal.get()) != cal {
            self.cal.set(Rc::new(cal));
        }
        if untracked(|| self.also.get()) != also {
            self.also.set(also);
        }
        if untracked(|| self.cal_note.get()) != note {
            self.cal_note.set(note);
        }
    }

    /// The calendars of every store open here (the timeline's own store
    /// first, the others named after their store): what its settings offer.
    async fn find_calendars(&self) {
        let Ok(stores) = self.api.stores().await else { return };
        let mut found: Vec<(bool, Addr, String)> = Vec::new();
        for store in stores {
            let here = store.id == self.metanode.store;
            let Ok(all) = self.api.walk(&store).await else { continue };
            for w in all.into_iter().filter(|w| w.node.metadata.metanode.as_ref().is_some_and(|m| m.viewer == CALENDAR_VIEWER)) {
                let name = display_label(&w.node);
                found.push((!here, w.addr, if here { name } else { format!("{name} ({})", store.name) }));
            }
        }
        found.sort_by(|a, b| (a.0, a.2.to_lowercase()).cmp(&(b.0, b.2.to_lowercase())));
        self.calendars.set(found.into_iter().map(|(_, a, n)| (a, n)).collect());
    }

    /// Show `named` under the timeline's own calendar too, or stop showing it.
    fn toggle_also(&self, named: CalRef) {
        let mut also = untracked(|| self.settings.get().also);
        match also.iter().position(|c| *c == named) {
            Some(i) => {
                also.remove(i);
            }
            None => also.push(named),
        }
        let value = (!also.is_empty()).then(|| serde_json::Value::Array(also.into_iter().map(CalRef::json).collect()));
        let t = self.clone();
        spawn_local(async move {
            if let Err(e) = set_setting(&t.api, t.metanode, VIEWER, "also", value).await {
                t.say(e.sentence());
            }
        });
    }

    fn set_calendar(&self, calendar: Option<Addr>) {
        let value = calendar.map(|a| serde_json::Value::String(PimbleUrl::node(a.store, a.node).to_string()));
        let t = self.clone();
        spawn_local(async move {
            if let Err(e) = set_setting(&t.api, t.metanode, VIEWER, "calendar", value).await {
                t.say(e.sentence());
            }
        });
    }

    fn set_field_name(&self, key: &'static str, name: String) {
        let name = name.trim().to_string();
        let t = self.clone();
        spawn_local(async move {
            let value = (!name.is_empty()).then(|| serde_json::Value::String(name));
            if let Err(e) = set_setting(&t.api, t.metanode, VIEWER, key, value).await {
                t.say(e.sentence());
            }
        });
    }

    /// A new calendar beside the timeline, started from `template`, and
    /// the timeline set to read in it; it opens beside to be made up.
    fn new_calendar(&self, template: Calendar) {
        let t = self.clone();
        spawn_local(async move {
            let made = async {
                let parent = match t.api.node(t.metanode).await?.parent_id {
                    Some(p) => Addr::new(t.metanode.store, p),
                    None => t.metanode,
                };
                let place = if parent == t.metanode { Place::Last } else { Place::After(t.metanode.node) };
                let init = pimble_api::recipes::document(&template.name, None)?;
                let made = t.api.create(parent, place, init).await?;
                let json = serde_json::to_value(&template).unwrap_or_default();
                t.api
                    .edit(made, move |doc, now| -> Result<(), ApiError> {
                        doc.set_title(&template.name, now)?;
                        doc.set_custom(custom_keys::EXPLICIT_TITLE, &serde_json::Value::Bool(true), now)?;
                        doc.set_custom(custom_keys::ICON, &serde_json::Value::String("calendar".into()), now)?;
                        doc.set_metanode_viewer(Some(CALENDAR_VIEWER), now)?;
                        doc.set_data(CALENDAR_VIEWER, &json)?;
                        Ok(())
                    })
                    .await?;
                Ok::<Addr, ApiError>(made)
            }
            .await;
            match made {
                Ok(made) => {
                    t.set_calendar(Some(made));
                    t.host.open_beside(made);
                    t.find_calendars().await;
                }
                Err(e) => t.say(e.sentence()),
            }
        });
    }

    fn say(&self, line: impl Into<String>) {
        self.status.set(line.into());
    }

    /// The panel's left edge and width now, kept in `width`.
    fn measure(&self) -> (f64, f64) {
        let bounds = untracked(|| self.area.get()).and_then(|a| a.bounding_rect());
        let (x, w) = bounds.map_or((0.0, 0.0), |b| (f64::from(b.x), f64::from(b.width)));
        let w = if w > 10.0 { w } else { untracked(|| self.width.get()) };
        if (untracked(|| self.width.get()) - w).abs() > 0.5 {
            self.width.set(w);
        }
        (x, w)
    }

    fn set_view(&self, view: View) {
        self.placed_view.set(true);
        if untracked(|| self.view.get()) == view {
            return;
        }
        self.view.set(view);
        // Remembered once the gestures pause.
        let n = untracked(|| self.remembering.get()) + 1;
        self.remembering.set(n);
        let t = self.clone();
        let _ = set_timeout(400, move || {
            if untracked(|| t.remembering.get()) == n {
                t.host.remember(view_json(untracked(|| t.view.get())));
            }
        });
    }

    /// Everything on the line across the panel; now when there is nothing.
    fn fit(&self) {
        let (_, w) = self.measure();
        let placed = untracked(|| self.placed.get());
        let from = placed.items.iter().map(|i| i.start).min();
        let to = placed.items.iter().map(|i| i.end.unwrap_or(i.start)).max();
        let view = match (from, to) {
            (Some(a), Some(b)) => View::fit(a, b, w),
            _ => {
                let now = Calendar::now();
                View::fit(now - 15 * 86_400, now + 15 * 86_400, w)
            }
        };
        self.set_view(view);
    }

    fn today(&self) {
        let (_, w) = self.measure();
        let v = untracked(|| self.view.get());
        self.set_view(View { start: Calendar::now().saturating_sub((w / 2.0 / v.scale) as i64), ..v });
    }

    /// Zoom by `factor` around `x` (the pointer), or around the chosen item
    /// when it is in view, or the middle.
    fn zoom(&self, factor: f64, x: Option<f64>) {
        let (_, w) = self.measure();
        let v = untracked(|| self.view.get());
        let chosen_x = untracked(|| self.chosen.get())
            .and_then(|c| untracked(|| self.placed.with(|p| p.items.iter().find(|i| i.addr == c).map(|i| v.x(i.start)))))
            .filter(|x| (0.0..=w).contains(x));
        self.set_view(v.zoomed(factor, x.or(chosen_x).unwrap_or(w / 2.0)));
    }

    fn pan_by(&self, dx: f64) {
        let v = untracked(|| self.view.get());
        self.set_view(v.panned(dx));
    }

    /// The wheel over the line: Ctrl (or a pinch) zooms around the pointer; a
    /// sideways swipe or Shift pans; a plain vertical wheel scrolls the rows.
    fn wheel(&self, e: &WheelEvent) -> bool {
        let (left, _) = self.measure();
        if e.modifiers.ctrl || e.modifiers.meta {
            self.zoom((-f64::from(e.delta_y) * 0.0025).exp(), Some(f64::from(e.x) - left));
            return true;
        }
        let dx = if e.modifiers.shift { e.delta_y } else { e.delta_x };
        if dx.abs() > 0.0 && (e.modifiers.shift || e.delta_x.abs() >= e.delta_y.abs()) {
            self.pan_by(f64::from(dx));
            return true;
        }
        false
    }

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

    /// A press on an item (`Grip::Move`) or an end of its bar: a drag once
    /// the pointer moves.
    fn press_item(&self, addr: Addr, grip: Grip) {
        self.take_keyboard();
        let Some(item) = untracked(|| self.placed.with(|p| p.items.iter().find(|i| i.addr == addr).cloned())) else { return };
        if !item.writable {
            return;
        }
        self.choose(Some(addr));
        let c = rinch::core::get_click_context();
        self.drag.set(Some(Drag { item, grip, x0: c.mouse_x, live: false, result: None, click: None, done: false }));
    }

    /// The pointer moved over the line: a drag of an item, or of the background.
    fn pointer_moved(&self) {
        let c = rinch::core::get_click_context();
        if let Some(d) = untracked(|| self.drag.get()).filter(|d| !d.done) {
            let dx = c.mouse_x - d.x0;
            if !d.live && dx.abs() < DRAG_PX {
                return;
            }
            let v = untracked(|| self.view.get());
            let (cal, fields) = (self.cal_of(&d.item), untracked(|| self.settings.get().fields));
            let result = drag(&cal, &fields, &d.item, d.grip, (f64::from(dx) / v.scale) as i64);
            let next = Drag { live: true, result: Some(result), ..d };
            if untracked(|| self.drag.get()).as_ref() != Some(&next) {
                self.drag.set(Some(next));
            }
            return;
        }
        let Some((x0, v0)) = untracked(|| self.pan.get()) else { return };
        if (c.mouse_x - x0).abs() >= DRAG_PX {
            self.panned.set(true);
        }
        self.set_view(v0.panned(f64::from(x0 - c.mouse_x)));
    }

    /// The pointer let go: a drag of an item writes where it went.
    fn released(&self) {
        self.pan.set(None);
        let Some(d) = untracked(|| self.drag.get()).filter(|d| !d.done) else { return };
        if !d.live {
            self.drag.set(None);
            if let Some(in_place) = d.click {
                self.open_clicked(d.item.addr, in_place);
            }
            return;
        }
        self.dropped.set(Some((d.item.addr, std::time::Instant::now())));
        match d.result.clone() {
            Some(Ok(went)) if went.start != d.item.start || went.end != d.item.end => {
                self.drag.set(Some(Drag { done: true, ..d.clone() }));
                self.write_moments(d.item.addr, went.writes);
            }
            _ => self.drag.set(None),
        }
    }

    /// The pointer left the line: a drag in progress goes back.
    fn left(&self) {
        self.pan.set(None);
        if untracked(|| self.drag.get()).is_some_and(|d| !d.done) {
            self.drag.set(None);
        }
    }

    /// One edit of `addr`'s moment fields, each `(field, as written)`.
    fn write_moments(&self, addr: Addr, writes: Vec<(String, String)>) {
        let t = self.clone();
        spawn_local(async move {
            let edited = t
                .api
                .edit(addr, move |doc, now| -> Result<(), ApiError> {
                    for (field, value) in &writes {
                        doc.set_field(field, Some(value), now)?;
                    }
                    Ok(())
                })
                .await;
            if let Err(e) = edited {
                t.drag.set(None);
                t.say(e.sentence());
            }
        });
    }

    /// Alt+Left and Alt+Right: the chosen node a step of its own unit (a
    /// day, a year) earlier or later.
    fn nudge(&self, addr: Addr, later: bool) {
        let Some(item) = untracked(|| self.placed.with(|p| p.items.iter().find(|i| i.addr == addr).cloned())) else { return };
        let (cal, fields) = (self.cal_of(&item), untracked(|| self.settings.get().fields));
        let m = item.from.moment;
        let step = cal.end_of(m) - m.at;
        match drag(&cal, &fields, &item, Grip::Move, if later { step } else { -step }) {
            Ok(went) => {
                self.say(went.when.clone());
                self.write_moments(addr, went.writes);
            }
            Err(e) => self.say(e),
        }
    }

    /// A click on the line's background; the second of a double-click makes a
    /// node at that moment, in the lane clicked, named in place.
    fn click_background(&self) {
        if untracked(|| self.panned.get()) {
            self.panned.set(false);
            return;
        }
        if !clicked_again(self.metanode) {
            return;
        }
        let c = rinch::core::get_click_context();
        let (left, top) = untracked(|| self.area.get()).and_then(|a| a.bounding_rect()).map_or((0.0, 0.0), |b| (b.x, b.y));
        let v = untracked(|| self.view.get());
        let (cal, fields) = untracked(|| (self.cal.get(), self.settings.get().fields));
        let precision = precision_at(&cal, v.scale);
        let at = cal.floor(v.at(f64::from(c.mouse_x - left)), precision);
        let y = f64::from(c.mouse_y - top) - axes_h(untracked(|| self.also.with(Vec::len)));
        if y < 0.0 {
            return;
        }
        let bands = untracked(|| self.placed.with(|p| bands(p, v)));
        let lane = bands.lanes.iter().find(|(_, _, top, h)| y >= *top && y < top + h).and_then(|(lane, ..)| *lane);
        let field = (fields.date.clone(), cal.write(Moment { at, precision }));
        let t = self.clone();
        spawn_local(async move {
            let init = match pimble_api::recipes::document("", None) {
                Ok(init) => init,
                Err(e) => return t.say(e.sentence()),
            };
            let made = match t.api.create(lane.unwrap_or(t.metanode), Place::Last, init).await {
                Ok(made) => made,
                Err(e) => return t.say(e.sentence()),
            };
            let (name, value) = field;
            let dated = t.api.edit(made, move |doc, now| -> Result<(), ApiError> { doc.set_field(&name, Some(&value), now)?; Ok(()) }).await;
            if let Err(e) = dated {
                return t.say(e.sentence());
            }
            t.say("");
            t.choose(Some(made));
            t.start_rename(made, String::new());
        });
    }

    /// A click on an item: open beside, Ctrl in place, a second click renames.
    fn click_item(&self, addr: Addr, modifiers: rinch::core::ModifierState) {
        // The click that ends a drag opens nothing, whether it comes before
        // the release is heard (the desktop) or after.
        if untracked(|| self.drag.get()).is_some_and(|d| d.item.addr == addr && d.live) {
            return;
        }
        if untracked(|| self.dropped.get()).is_some_and(|(a, at)| a == addr && at.elapsed() < std::time::Duration::from_millis(400)) {
            self.dropped.set(None);
            return;
        }
        let in_place = modifiers.ctrl || modifiers.meta;
        // Pressed and not yet let go: what the click does waits for the release.
        if let Some(d) = untracked(|| self.drag.get()).filter(|d| d.item.addr == addr && !d.done) {
            self.drag.set(Some(Drag { click: Some(in_place), ..d }));
            return;
        }
        self.open_clicked(addr, in_place);
    }

    /// What a click on an item does once it is known not to be a drag.
    fn open_clicked(&self, addr: Addr, in_place: bool) {
        let again = clicked_again(addr);
        self.choose(Some(addr));
        if untracked(|| self.renaming.get()) == Some(addr) {
            return;
        }
        let Some(item) = untracked(|| self.placed.with(|p| p.items.iter().find(|i| i.addr == addr).cloned())) else { return };
        if again {
            if item.writable {
                self.start_rename(addr, item.label.clone());
            }
            return;
        }
        if in_place {
            self.host.open(addr);
        } else {
            self.host.open_beside(addr);
        }
        self.take_keyboard();
    }

    fn start_rename(&self, addr: Addr, now: String) {
        self.draft.set(now);
        self.renaming.set(Some(addr));
    }

    fn commit_rename(&self) {
        let Some(addr) = untracked(|| self.renaming.get()) else { return };
        let title = untracked(|| self.draft.get()).trim().to_string();
        self.renaming.set(None);
        self.take_keyboard();
        if title.is_empty() {
            return;
        }
        let t = self.clone();
        spawn_local(async move {
            let edited = t
                .api
                .edit(addr, move |doc, now| -> Result<(), ApiError> {
                    doc.set_title(&title, now)?;
                    doc.set_custom(custom_keys::EXPLICIT_TITLE, &serde_json::Value::Bool(true), now)?;
                    Ok(())
                })
                .await;
            if let Err(e) = edited {
                t.say(e.sentence());
            }
        });
    }

    fn cancel_rename(&self) {
        self.renaming.set(None);
        self.take_keyboard();
    }

    fn delete(&self, addr: Addr) {
        let t = self.clone();
        let label = untracked(|| self.placed.with(|p| p.items.iter().find(|i| i.addr == addr).map(|i| i.label.clone()))).unwrap_or_default();
        spawn_local(async move {
            if let Err(e) = t.api.can_delete(&[addr]).await {
                return t.say(e.sentence());
            }
            match t.api.delete(addr).await {
                Ok(()) => {
                    t.choose(None);
                    t.say(format!("Deleted \u{201c}{label}\u{201d}. View > Recently Deleted puts it back."));
                }
                Err(e) => t.say(e.sentence()),
            }
        });
    }

    /// Bring `addr` into view when it is off the panel.
    fn show(&self, addr: Addr) {
        let Some(item) = untracked(|| self.placed.with(|p| p.items.iter().find(|i| i.addr == addr).cloned())) else { return };
        let (_, w) = self.measure();
        let v = untracked(|| self.view.get());
        let x = v.x(item.start);
        if x < 20.0 || x > w - 60.0 {
            self.set_view(View { start: item.start.saturating_sub((w / 3.0 / v.scale) as i64), ..v });
        }
    }

    /// The timeline's keys. Answers whether it took the key.
    fn key(&self, key: &KeyEventData) -> bool {
        if untracked(|| self.renaming.get()).is_some() {
            return false;
        }
        let (_, w) = self.measure();
        let chosen = untracked(|| self.chosen.get());
        let mut order: Vec<Item> = untracked(|| self.placed.with(|p| p.items.clone()));
        order.sort_by_key(|i| i.start);
        let at = chosen.and_then(|c| order.iter().position(|i| i.addr == c));
        let writable = at.is_some_and(|i| order[i].writable);
        match key.key.as_str() {
            "ArrowLeft" | "ArrowRight" if key.alt => {
                let Some(c) = chosen.filter(|_| writable) else { return false };
                self.nudge(c, key.key == "ArrowRight");
            }
            "ArrowLeft" => self.pan_by(-w / if key.shift { 1.5 } else { 6.0 }),
            "ArrowRight" => self.pan_by(w / if key.shift { 1.5 } else { 6.0 }),
            "+" | "=" => self.zoom(1.6, None),
            "-" | "_" => self.zoom(1.0 / 1.6, None),
            "0" => self.fit(),
            "t" | "T" => self.today(),
            "ArrowDown" | "ArrowUp" => {
                let next = match (at, key.key.as_str()) {
                    (None, _) => order.first(),
                    (Some(i), "ArrowDown") => order.get(i + 1).or(order.get(i)),
                    (Some(i), _) => order.get(i.saturating_sub(1)),
                };
                if let Some(next) = next.map(|i| i.addr) {
                    self.choose(Some(next));
                    self.show(next);
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
            "F2" if writable => {
                if let Some(i) = at {
                    self.start_rename(order[i].addr, order[i].label.clone());
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

const ROOT_STYLE: &str = "display: flex; flex-direction: column; gap: 8px; padding: 12px 16px; height: 100%; box-sizing: border-box; outline: none; overflow-y: auto;";
const ICON: &str = "display: inline-flex; width: 14px; height: 14px;";

/// The plugin's viewers: a timeline, or a calendar being made up.
pub fn view(__scope: &mut RenderScope, mount: Mount) -> NodeHandle {
    if mount.viewer == CALENDAR_VIEWER {
        return calendar_view::view(__scope, mount);
    }
    timeline_view(__scope, mount)
}

/// The timeline viewer.
fn timeline_view(__scope: &mut RenderScope, mount: Mount) -> NodeHandle {
    let Some(metanode) = mount.metanode() else {
        return rsx! { div { style: look::LINE_STYLE, "This viewer was shown without a metanode." } };
    };
    let remembered = mount.remembered().and_then(|r| parse_view(&r));
    let props = mount.props;
    let t = Timeline {
        api: mount.api.clone(),
        host: mount.host.clone(),
        metanode,
        cal: Signal::new(Rc::new(Calendar::gregorian())),
        also: Signal::new(Vec::new()),
        settings: Signal::new(parse_settings(untracked(|| props.get()).settings.as_deref())),
        cal_note: Signal::new(None),
        settings_open: Signal::new(false),
        calendars: Signal::new(Vec::new()),
        placed: Signal::new(Placed::default()),
        loaded: Signal::new(false),
        view: Signal::new(remembered.unwrap_or(View { start: Calendar::now() - 15 * 86_400, scale: 1000.0 / (30.0 * 86_400.0) })),
        placed_view: Signal::new(remembered.is_some()),
        width: Signal::new(1000.0),
        chosen: Signal::new(None),
        renaming: Signal::new(None),
        draft: Signal::new(String::new()),
        status: Signal::new(String::new()),
        root: Signal::new(None),
        area: Signal::new(None),
        field: Signal::new(None),
        pan: Signal::new(None),
        panned: Signal::new(false),
        drag: Signal::new(None),
        dropped: Signal::new(None),
        remembering: Signal::new(0),
    };

    {
        let t = t.clone();
        spawn_local(async move {
            t.load_calendar().await;
            let mut forest = t.read().await;
            while let Some(change) = t.api.next_change().await {
                // The calendar it reads in changed (or went): the line is
                // read again in it.
                let calendar = untracked(|| {
                    let s = t.settings.get();
                    s.calendar.into_iter().chain(s.also.iter().filter_map(|c| match c {
                        CalRef::Node(a) => Some(*a),
                        CalRef::Gregorian => None,
                    })).any(|c| names(&change, c))
                });
                if calendar {
                    t.load_calendar().await;
                }
                if calendar || forest.as_ref().map_or(true, |f| f.touched_by(&change)) {
                    forest = t.read().await.or(forest);
                }
            }
        });
    }

    // New settings (another calendar, other fields): read again.
    {
        let t = t.clone();
        __scope.create_effect(move || {
            let next = parse_settings(props.get().settings.as_deref());
            let before = untracked(|| t.settings.get());
            if next == before {
                return;
            }
            t.settings.set(next.clone());
            let t = t.clone();
            spawn_local(async move {
                if next.calendar != before.calendar || next.also != before.also {
                    t.load_calendar().await;
                }
                if next.calendar != before.calendar {
                    // Another calendar: what was in view is shown again whole.
                    t.placed_view.set(false);
                }
                t.read().await;
            });
        });
    }

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

    // Following someone (docs/PRESENCE_CONTRACT.md section 7): the pane shows
    // this timeline because they are here; bring what they are on into view.
    {
        let t = t.clone();
        __scope.create_effect(move || {
            let Some(leader) = t.host.following() else { return };
            let items: Vec<Addr> = t.placed.with(|p| p.items.iter().map(|i| i.addr).collect());
            if let Some(on) = items.into_iter().find(|a| t.host.people_in(*a).iter().any(|p| p.key == leader)) {
                let t = t.clone();
                let _ = set_timeout(0, move || t.show(on));
            }
        });
    }

    let bands_now: Memo<Rc<Bands>> = {
        let (placed, view) = (t.placed, t.view);
        Memo::new(move || Rc::new(placed.with(|p| bands(p, view.get()))))
    };
    let seen = t.host.seen();
    let (tk, tw, tdown, tmove, tup, tleave) = (t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone());
    let (tzo, tzi, tfit, tnow) = (t.clone(), t.clone(), t.clone(), t.clone());
    let (ti, tu) = (t.clone(), t.clone());
    let (status, loaded, placed, view, width, root, area, dragging) = (t.status, t.loaded, t.placed, t.view, t.width, t.root, t.area, t.drag);
    let also = t.also;
    let tclick = t.clone();
    let (cal, cal2) = (t.cal, t.cal);
    let (tset, tsettings) = (t.clone(), t.clone());
    let (settings_open, cal_note) = (t.settings_open, t.cal_note);
    rsx! {
        div {
            class: "timeline",
            style: {move || format!("{ROOT_STYLE}{}", look::surfaces(seen.dark()))},
            tabindex: "0",
            ref: root,
            onkeydown: move |key: &KeyEventData| tk.key(key),
            div {
                class: "timeline__bar",
                style: "display: flex; align-items: center; gap: 2px;",
                span { class: "timeline__status", style: format!("{} flex: 1;", look::LINE_STYLE), {move || match dragging.get().filter(|d| d.live && !d.done).and_then(|d| d.result) {
                    Some(Ok(went)) => went.when,
                    Some(Err(why)) => why,
                    None => status.get(),
                }} }
                span { class: "timeline__zoom-out", style: look::ICON_BUTTON, title: "Zoom out (-)", onclick: move || tzo.zoom(1.0 / 1.6, None), span { style: ICON, IconGlyph { name: "zoom-out".to_string() } } }
                span { class: "timeline__zoom-in", style: look::ICON_BUTTON, title: "Zoom in (+)", onclick: move || tzi.zoom(1.6, None), span { style: ICON, IconGlyph { name: "zoom-in".to_string() } } }
                span { class: "timeline__fit", style: look::ICON_BUTTON, title: "Show everything (0)", onclick: move || tfit.fit(), span { style: ICON, IconGlyph { name: "arrows-horizontal".to_string() } } }
                span { class: "timeline__today", style: look::ICON_BUTTON, title: "Go to now (T)", onclick: move || tnow.today(), span { style: ICON, IconGlyph { name: "calendar-event".to_string() } } }
                span {
                    class: "timeline__settings-button",
                    style: look::ICON_BUTTON,
                    title: "Calendar and fields",
                    onclick: move || {
                        let open = !untracked(|| tset.settings_open.get());
                        tset.settings_open.set(open);
                        if open {
                            let t = tset.clone();
                            spawn_local(async move { t.find_calendars().await });
                        }
                    },
                    span { style: ICON, IconGlyph { name: "settings".to_string() } }
                }
            }
            if let Some(note) = cal_note.get() {
                div { class: "timeline__calendar-note", style: format!("{} color: var(--rinch-color-orange-6);", look::LINE_STYLE), {note} }
            }
            if settings_open.get() {
                {settings_strip(__scope, &tsettings)}
            }
            div {
                class: "timeline__panel",
                // As tall as its rows (the pane scrolls past a tall one).
                style: {move || format!("position: relative; flex-shrink: 0; height: {:.0}px; overflow: hidden; cursor: grab; {}", (axes_h(also.with(Vec::len)) + bands_now.get().height).max(160.0), look::PANEL)},
                ref: area,
                onwheel: move |e: &WheelEvent| tw.wheel(e),
                onmousedown: move || {
                    let c = rinch::core::get_click_context();
                    tdown.pan.set(Some((c.mouse_x, untracked(|| tdown.view.get()))));
                    tdown.panned.set(false);
                    tdown.take_keyboard();
                },
                onmousemove: move || tmove.pointer_moved(),
                onmouseup: move || tup.released(),
                onmouseleave: move || tleave.left(),
                onclick: move || tclick.click_background(),
                // The axis: the larger units on top, the ticks below.
                div {
                    class: "timeline__axis",
                    style: format!("position: absolute; left: 0; right: 0; top: 0; height: {AXIS_H}px; border-bottom: 1px solid var(--viewer-panel-border); overflow: hidden;"),
                    for tick in axis_ticks(cal, view, width) {
                        div {
                            key: format!("{} {} {}", tick.0, tick.1.at, tick.1.label),
                            class: {if tick.0 { "timeline__context" } else { "timeline__tick" }},
                            style: {
                                let (ctx, at) = (tick.0, tick.1.at);
                                move || {
                                    let x = view.get().x(at).clamp(-1.0e6, 1.0e6);
                                    if ctx {
                                        format!("position: absolute; top: 4px; left: {:.1}px; font-size: 12px; font-weight: 600; white-space: nowrap; padding-left: 4px;", x.max(0.0))
                                    } else {
                                        format!("position: absolute; top: 24px; left: {x:.1}px; font-size: 11px; color: var(--rinch-color-dimmed); white-space: nowrap; padding-left: 4px; border-left: 1px solid var(--viewer-panel-border); height: 22px; box-sizing: border-box;")
                                    }
                                }
                            },
                            {tick.1.label.clone()}
                        }
                    }
                }
                // The other calendars shown, lined up under it.
                for (i, other) in also.get().into_iter().enumerate() {
                    div {
                        key: format!("{i} {}", other.name),
                        {also_axis(__scope, other.clone(), AXIS_H + i as f64 * ALSO_H, view, width)}
                    }
                }
                // The rows, scrolled up and down under the axes.
                div {
                    class: "timeline__body",
                    style: {move || format!("position: absolute; left: 0; right: 0; top: {}px; bottom: 0; overflow: hidden;", axes_h(also.with(Vec::len)))},
                    div {
                        class: "timeline__content",
                        style: {move || format!("position: relative; height: {:.0}px; min-height: 100%;", bands_now.get().height)},
                        // A line down from every tick.
                        for (_, tick) in axis_ticks(cal2, view, width).into_iter().filter(|(ctx, _)| !ctx) {
                            div {
                                key: format!("{} {}", tick.at, tick.label),
                                class: "timeline__grid",
                                style: { let at = tick.at; move || format!("position: absolute; top: 0; bottom: 0; width: 1px; left: {:.1}px; background: var(--viewer-panel-border); opacity: 0.5;", view.get().x(at).clamp(-1.0e6, 1.0e6)) },
                            }
                        }
                        // Now.
                        div {
                            class: "timeline__now",
                            title: "Now",
                            style: {move || {
                                let x = view.get().x(Calendar::now());
                                if (-10.0..=width.get() + OVERDRAW).contains(&x) {
                                    format!("position: absolute; top: 0; bottom: 0; width: 2px; left: {x:.1}px; background: var(--rinch-color-red-6); opacity: 0.6;")
                                } else {
                                    "display: none;".to_string()
                                }
                            }},
                        }
                        // The lanes' bands and names.
                        for (lane, label, top, height) in bands_now.get().lanes.clone() {
                            div {
                                key: format!("{lane:?} {label} {top} {height}"),
                                class: "timeline__lane",
                                style: format!("position: absolute; left: 0; right: 0; top: {top:.0}px; height: {height:.0}px; {}", if lane.is_some() { "border-top: 1px solid var(--viewer-panel-border);" } else { "" }),
                                if lane.is_some() {
                                    span {
                                        style: "position: absolute; left: 8px; top: 4px; font-size: 12px; font-weight: 600; color: var(--rinch-color-dimmed);",
                                        {label.clone()}
                                    }
                                }
                            }
                        }
                        for item in placed.with(|p| p.items.clone()) {
                            div {
                                key: format!("{item:?}"),
                                {render_item(__scope, &ti, item.clone(), bands_now)}
                            }
                        }
                    }
                }
            }
            if loaded.get() && placed.with(|p| p.items.is_empty() && p.unread.is_empty()) {
                div { style: look::LINE_STYLE, "Nothing here has a start, end or date field yet. Give a node a start field (View > Properties) to put it on the line." }
            }
            // What does not read as a moment, as typed.
            if placed.with(|p| !p.unread.is_empty()) {
                div {
                    class: "timeline__unread",
                    style: "display: flex; flex-direction: column; gap: 2px;",
                    div { style: look::LINE_STYLE, "Not on the line:" }
                    for u in placed.with(|p| p.unread.clone()) {
                        div {
                            key: format!("{u:?}"),
                            class: "timeline__unread-row",
                            style: "font-size: 12px; cursor: pointer;",
                            onclick: { let (t, a) = (tu.clone(), u.addr); move || t.host.open_beside(a) },
                            span { style: "font-weight: 600;", {u.label.clone()} }
                            span { style: look::LINE_STYLE, {format!(" \u{2014} {}", u.why)} }
                        }
                    }
                }
            }
        }
    }
}

/// The timeline's settings, shared with everyone who sees it: the calendar it
/// reads in (the Gregorian, or a calendar node of its store, or a new one),
/// and the fields it reads.
fn settings_strip(__scope: &mut RenderScope, t: &Timeline) -> NodeHandle {
    let (settings, calendars) = (t.settings, t.calendars);
    let (tg, tc, tn, ti, ta, tal) = (t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone());
    let chip = |chosen: bool| {
        format!(
            "padding: 2px 10px; border-radius: 12px; font-size: 12px; cursor: pointer; border: 1px solid {}; {}",
            if chosen { "var(--rinch-primary-color)" } else { "var(--viewer-item-border)" },
            if chosen { "background: var(--viewer-item);" } else { "" }
        )
    };
    rsx! {
        div {
            class: "timeline__settings",
            style: format!("display: flex; flex-direction: column; gap: 8px; padding: 10px 12px; {}", look::PANEL),
            div {
                style: "display: flex; align-items: center; gap: 6px; flex-wrap: wrap;",
                span { style: look::LINE_STYLE, "Calendar:" }
                span {
                    class: "timeline__calendar-choice",
                    style: {move || chip(settings.get().calendar.is_none())},
                    onclick: move || tg.set_calendar(None),
                    "Gregorian"
                }
                for (addr, name) in calendars.get() {
                    span {
                        key: format!("{addr:?}"),
                        class: "timeline__calendar-choice",
                        style: {move || chip(settings.get().calendar == Some(addr))},
                        onclick: { let t = tc.clone(); move || t.set_calendar(Some(addr)) },
                        {name.clone()}
                    }
                }
                span {
                    class: "timeline__new-calendar",
                    style: chip(false),
                    title: "A calendar of your own, beside this timeline, made up in its own view",
                    onclick: move || tn.new_calendar(Calendar::invented()),
                    "+ New calendar"
                }
                span {
                    class: "timeline__copy-gregorian",
                    style: chip(false),
                    title: "A copy of the Gregorian calendar to change",
                    onclick: move || {
                        let mut g = Calendar::gregorian();
                        g.name = "My calendar".into();
                        ti.new_calendar(g)
                    },
                    "+ Copy of the Gregorian"
                }
            }
            div {
                style: "display: flex; align-items: center; gap: 6px; flex-wrap: wrap;",
                span { style: look::LINE_STYLE, title: "Other calendars drawn under its own, lined up; a date that does not read in its own calendar is read in them.", "Also show:" }
                span {
                    class: "timeline__also-choice",
                    style: {move || if settings.get().calendar.is_none() { "display: none;".to_string() } else { chip(settings.get().also.contains(&CalRef::Gregorian)) }},
                    onclick: move || ta.toggle_also(CalRef::Gregorian),
                    "Gregorian"
                }
                for (addr, name) in calendars.get() {
                    span {
                        key: format!("also {addr:?}"),
                        class: "timeline__also-choice",
                        style: {move || if settings.get().calendar == Some(addr) { "display: none;".to_string() } else { chip(settings.get().also.contains(&CalRef::Node(addr))) }},
                        onclick: { let t = tal.clone(); move || t.toggle_also(CalRef::Node(addr)) },
                        {name.clone()}
                    }
                }
            }
            div {
                style: "display: flex; align-items: center; gap: 14px; flex-wrap: wrap;",
                span { style: look::LINE_STYLE, "Reads the fields" }
                {field_input(__scope, "start", "start", t.clone())}
                {field_input(__scope, "end", "end", t.clone())}
                {field_input(__scope, "date", "or one date", t.clone())}
            }
        }
    }
}

/// An input naming one of the fields the timeline reads.
fn field_input(__scope: &mut RenderScope, key: &'static str, label: &'static str, t: Timeline) -> NodeHandle {
    let settings = t.settings;

    rsx! {
        span {
            style: "display: inline-flex; align-items: center; gap: 4px; font-size: 12px;",
            span { style: look::LINE_STYLE, {label} }
            input {
                class: format!("timeline__field-name timeline__field-name--{key}"),
                style: "width: 90px; font: inherit; font-size: 12px; padding: 1px 4px;",
                value: {move || {
                    let f = settings.get().fields;
                    match key {
                        "start" => f.start,
                        "date" => f.date,
                        _ => f.end,
                    }
                }},
                onchange: move |v: String| t.set_field_name(key, v),
            }
        }
    }
}

/// The axis's ticks (`false`) and the larger units over them (`true`) at the
/// view now, tracked.
fn axis_ticks(cal: Signal<Rc<Calendar>>, view: Signal<View>, width: Signal<f64>) -> Vec<(bool, layout::Tick)> {
    let (t, ctx) = ticks(&cal.get(), view.get(), width.get() + OVERDRAW);
    t.into_iter().map(|t| (false, t)).chain(ctx.into_iter().map(|t| (true, t))).collect()
}

/// Another calendar's axis under the timeline's own: its larger units over
/// its ticks, at the same moments on the line, and its name at the right.
fn also_axis(__scope: &mut RenderScope, cal: Rc<Calendar>, top: f64, view: Signal<View>, width: Signal<f64>) -> NodeHandle {
    let name = cal.name.clone();
    let shown = move || {
        let (t, ctx) = ticks(&cal, view.get(), width.get() + OVERDRAW);
        t.into_iter().map(|t| (false, t)).chain(ctx.into_iter().map(|t| (true, t))).collect::<Vec<_>>()
    };
    rsx! {
        div {
            class: "timeline__also-axis",
            style: format!("position: absolute; left: 0; right: 0; top: {top:.0}px; height: {ALSO_H}px; border-bottom: 1px solid var(--viewer-panel-border); overflow: hidden; background: var(--viewer-item);"),
            for tick in shown() {
                div {
                    key: format!("{} {} {}", tick.0, tick.1.at, tick.1.label),
                    class: {if tick.0 { "timeline__also-context" } else { "timeline__also-tick" }},
                    style: {
                        let (ctx, at) = (tick.0, tick.1.at);
                        move || {
                            let x = view.get().x(at).clamp(-1.0e6, 1.0e6);
                            if ctx {
                                format!("position: absolute; top: 2px; left: {:.1}px; font-size: 11px; font-weight: 600; white-space: nowrap; padding-left: 4px;", x.max(0.0))
                            } else {
                                format!("position: absolute; top: 20px; left: {x:.1}px; font-size: 11px; color: var(--rinch-color-dimmed); white-space: nowrap; padding-left: 4px; border-left: 1px solid var(--viewer-panel-border); height: 20px; box-sizing: border-box;")
                            }
                        }
                    },
                    {tick.1.label.clone()}
                }
            }
            span {
                class: "timeline__also-name",
                style: "position: absolute; right: 6px; top: 2px; font-size: 11px; font-style: italic; color: var(--rinch-color-dimmed); white-space: nowrap; background: var(--viewer-item); padding: 0 4px;",
                {name}
            }
        }
    }
}

/// One node on the line: a bar for a span, a dot for a moment, its name after.
fn render_item(__scope: &mut RenderScope, t: &Timeline, item: Item, bands_now: Memo<Rc<Bands>>) -> NodeHandle {
    let addr = item.addr;
    let seen = t.host.seen();
    let (view, chosen, renaming, dragging) = (t.view, t.chosen, t.renaming, t.drag);
    let (tc, tf, tf2, tr, tp, ts, te) = (t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone());
    let tpeople = t.clone();
    let grips = item.writable && item.end.is_some() && !item.rough;
    let soft = item.soft;
    let (start, end) = (item.start, item.end);
    let label = item.label.clone();
    let label_w = label_px(&label);
    let color = item.color.clone();
    let background = item.background.clone();
    let title = format!("{}: {}", item.label, item.when);
    rsx! {
        div {
            class: {if end.is_some() { "timeline__item timeline__item--span" } else { "timeline__item timeline__item--point" }},
            title: title,
            style: {move || {
                let v = view.get();
                let top = bands_now.get().top.get(&addr).copied().unwrap_or(0.0);
                // Where a drag would leave it, while it is dragged.
                let (start, end, lifted) = match dragging.get().filter(|d| d.item.addr == addr && d.live) {
                    Some(Drag { result: Some(Ok(went)), .. }) => (went.start, went.end.or(end), true),
                    Some(_) => (start, end, true),
                    None => (start, end, false),
                };
                let lift = if lifted { " box-shadow: 0 2px 8px rgba(0, 0, 0, 0.25); z-index: 2;" } else { "" };
                let x0 = v.x(start).clamp(-1.0e7, 1.0e7);
                let w = end.map_or(0.0, |e| (v.x(e).clamp(-1.0e7, 1.0e7) - x0).max(3.0));
                let is_chosen = chosen.get() == Some(addr);
                let ring = if is_chosen { " outline: 2px solid var(--rinch-primary-color); outline-offset: 1px;" } else { "" };
                let fill = background.as_deref().map(|b| row_background(b, seen.dark())).unwrap_or_else(|| "var(--viewer-item)".to_string());
                match end {
                    // Only roughly there: its ends fade.
                    Some(_) if soft => format!(
                        "position: absolute; top: {top:.0}px; left: {x0:.1}px; width: {w:.1}px; height: 22px; box-sizing: border-box; border-radius: 4px; border-top: 1px dashed var(--viewer-item-border); border-bottom: 1px dashed var(--viewer-item-border); background: linear-gradient(to right, transparent 0%, {fill} 30%, {fill} 70%, transparent 100%); cursor: pointer;{ring}{lift}"
                    ),
                    Some(_) => format!(
                        "position: absolute; top: {top:.0}px; left: {x0:.1}px; width: {w:.1}px; height: 22px; box-sizing: border-box; border-radius: 4px; border: 1px solid var(--viewer-item-border); border-left: 3px solid var(--rinch-primary-color); background: {fill}; cursor: pointer;{ring}{lift}"
                    ),
                    None => format!("position: absolute; top: {:.0}px; left: {:.1}px; height: 22px; width: {label_w:.0}px; cursor: pointer;{ring}{lift} border-radius: 4px;", top, x0 - 6.0),
                }
            }},
            onclick: move || tc.click_item(addr, rinch::core::get_modifier_state()),
            onmousedown: move || tp.press_item(addr, Grip::Move),
            // A bar's ends: dragged, they change its start or its end.
            if grips {
                div {
                    class: "timeline__grip timeline__grip--start",
                    title: "Drag to change when it starts",
                    style: {move || grip_style("left", view.get(), start, end)},
                    onmousedown: move || ts.press_item(addr, Grip::Start),
                }
                div {
                    class: "timeline__grip timeline__grip--end",
                    title: "Drag to change when it ends",
                    style: {move || grip_style("right", view.get(), start, end)},
                    onmousedown: move || te.press_item(addr, Grip::End),
                }
            }
            // Only roughly there: a band of the accent color fading at both ends.
            if soft {
                span {
                    class: "timeline__fade",
                    style: "position: absolute; left: 0; right: 0; top: 0; bottom: 0; border-radius: 4px; opacity: 0.45; background: linear-gradient(to right, transparent 0%, var(--rinch-primary-color) 30%, var(--rinch-primary-color) 70%, transparent 100%);",
                }
            }
            if end.is_none() {
                span {
                    class: "timeline__dot",
                    style: "position: absolute; left: 1px; top: 6px; width: 10px; height: 10px; border-radius: 5px; background: var(--rinch-primary-color);",
                }
            }
            if renaming.get() == Some(addr) {
                input {
                    class: "timeline__field",
                    style: format!("position: absolute; top: 0; left: {}px; width: 220px; height: 22px; box-sizing: border-box; font: inherit; font-size: 13px; padding: 1px 4px;", if end.is_some() { 4 } else { 16 }),
                    ref: tf.field,
                    value: {move || tf.draft.get()},
                    oninput: move |v: String| tf2.draft.set(v),
                    onkeydown: {
                        let t = tr.clone();
                        move |key: &KeyEventData| match key.key.as_str() {
                            "Enter" => { t.commit_rename(); true }
                            "Escape" => { t.cancel_rename(); true }
                            _ => false,
                        }
                    },
                }
            } else {
                span {
                    class: "timeline__label",
                    style: {
                        let color = color.clone();
                        move || format!(
                            "position: absolute; top: 0; height: 22px; display: inline-flex; align-items: center; left: {}px; font-size: 13px; white-space: nowrap;{}",
                            if end.is_some() { 6 } else { 16 },
                            color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default(),
                        )
                    },
                    {label.clone()}
                    {people(__scope, &tpeople, addr)}
                }
            }
        }
    }
}

/// Who else is on `addr`: the app's own chips, up to three and "+n"; a click
/// follows them.
fn people(__scope: &mut RenderScope, t: &Timeline, addr: Addr) -> NodeHandle {
    let (seen, follow) = (t.host.seen(), t.host.clone());
    rsx! {
        span {
            class: "pimble-pane__people",
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

/// A bar's end grip: on its `side`, hidden while the bar is too narrow to
/// hold both grips and still leave room to move it.
fn grip_style(side: &str, v: View, start: i64, end: Option<i64>) -> String {
    let wide = end.is_some_and(|e| v.x(e) - v.x(start) >= 24.0);
    if wide {
        format!("position: absolute; {side}: -4px; top: 0; width: 9px; height: 22px; cursor: ew-resize; z-index: 1;")
    } else {
        "display: none;".to_string()
    }
}

pimble_plugin_sdk::plugin!(describe: describe, view: view, guide: include_bytes!("../guide.json"), tools: tools::tools);

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

    /// The calendar's schema takes every calendar the plugin itself writes,
    /// and its example is a calendar the plugin reads.
    #[test]
    fn the_calendar_schema_fits_the_calendars_the_code_writes() {
        use pimble_plugin_sdk::abi::guide::{check, Guide};
        let guide = Guide::parse(include_bytes!("../guide.json")).unwrap();
        let settings = guide.viewer(super::CALENDAR_VIEWER).unwrap().settings.as_ref().unwrap();
        for cal in [crate::calendar::Calendar::gregorian(), crate::calendar::Calendar::invented()] {
            let written = serde_json::to_value(&cal).unwrap();
            check(&settings.schema, &written).unwrap_or_else(|e| panic!("{}: {e}", cal.name));
        }
        let example: crate::calendar::Calendar = serde_json::from_value(settings.example.clone().unwrap()).unwrap();
        assert_eq!(example.name, "Reckoning of the Founding");
        // The timeline's example names a calendar the way its settings do.
        let timeline = guide.viewer(super::VIEWER).unwrap().settings.as_ref().unwrap();
        let s = super::parse_settings(Some(&timeline.example.clone().unwrap().to_string()));
        assert!(s.calendar.is_some());
    }

    use super::*;

    #[test]
    fn it_provides_the_timeline_viewer_and_new_timeline() {
        let m = describe();
        assert_eq!(m.viewers().next().map(|v| v.id.as_str()), Some(VIEWER));
        assert_eq!(m.recipes().next().map(|r| r.label.as_str()), Some("New Timeline"));
        assert!(m.asks.write);
    }

    #[test]
    fn the_view_is_remembered_as_json_and_read_back() {
        let v = View { start: -1_234_567, scale: 0.5 };
        assert_eq!(parse_view(&view_json(v)), Some(v));
        assert_eq!(parse_view("nope"), None);
    }
}
