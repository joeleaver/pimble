//! The calendar viewer (docs/TIMELINE_CONTRACT.md wave 2): a calendar node's
//! definition (its `data.calendar`, the viewer's own settings) made up in a
//! form, never as raw JSON: its name, how its day divides, its months and
//! days of no month, its week, its leap rule, where it starts on the line,
//! and its eras, with a live sample ("Today reads ...", and any date tried in
//! it). Every input writes the one place it shows (`NodeDoc::edit_data`), so
//! two people making up the same calendar each keep what they wrote.

use pimble_plugin_sdk::pimble_crdt::{DataEdit, DataStep};
use pimble_plugin_sdk::pimble_ui::appearance::IconGlyph;
use pimble_plugin_sdk::pimble_ui::viewer as look;
use pimble_plugin_sdk::prelude::*;
use serde_json::{json, Value};

use crate::calendar::{Calendar, Moment, Precision};
use crate::CALENDAR_VIEWER;

/// A step into the calendar's JSON.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    K(&'static str),
    I(u32),
}

/// What a write does at the place its path names.
#[derive(Debug, Clone)]
enum Write {
    Set(Value),
    Insert(Value),
    Remove,
    /// Swap the array items at the path's end and the one before it (a row
    /// moved up).
    Up,
}

fn at<'a>(v: &'a Value, path: &[Step]) -> Option<&'a Value> {
    path.iter().try_fold(v, |v, step| match step {
        Step::K(k) => v.get(*k),
        Step::I(i) => v.get(*i as usize),
    })
}

/// A value as an input shows it: text as it is, a number as digits, nothing
/// as empty.
fn shown(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Array(items)) => items.iter().filter_map(|i| i.as_str()).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

/// What a number input holds: a whole number, or empty.
enum Number {
    Whole(i64),
    Empty,
    Not,
}

fn number(text: &str) -> Number {
    let t = text.trim().replace(',', "");
    if t.is_empty() {
        Number::Empty
    } else {
        t.parse().map_or(Number::Not, Number::Whole)
    }
}

#[derive(Clone)]
struct Form {
    api: std::sync::Arc<Pimble>,
    metanode: Addr,
    /// The definition as stored (it may be unfinished: a form shows what is
    /// there).
    json: Signal<Value>,
    status: Signal<String>,
    writable: Signal<bool>,
    /// A date tried in the calendar.
    tried: Signal<String>,
}

impl Form {
    fn say(&self, line: impl Into<String>) {
        self.status.set(line.into());
    }

    /// One write at `path` inside the calendar, as one edit of the node.
    fn write(&self, path: Vec<Step>, write: Write) {
        let f = self.clone();
        spawn_local(async move {
            let edited = f
                .api
                .edit(f.metanode, move |doc, _now| -> Result<(), ApiError> {
                    let mut steps = vec![DataStep::Key(CALENDAR_VIEWER)];
                    steps.extend(path.iter().map(|s| match s {
                        Step::K(k) => DataStep::Key(k),
                        Step::I(i) => DataStep::Index(*i),
                    }));
                    // A part the calendar does not have yet (its seasons, how
                    // it writes) is made, empty, on the way to the place.
                    if matches!(write, Write::Set(_) | Write::Insert(_)) {
                        for n in 1..path.len() {
                            let data = doc.data_json();
                            if data.get(CALENDAR_VIEWER).and_then(|c| at(c, &path[..n])).is_none() {
                                let empty = if matches!(path[n], Step::I(_)) { json!([]) } else { json!({}) };
                                doc.edit_data(&steps[..=n], DataEdit::Set(&empty))?;
                            }
                        }
                    }
                    match &write {
                        Write::Set(v) => doc.edit_data(&steps, DataEdit::Set(v)).map(|_| ())?,
                        Write::Insert(v) => doc.edit_data(&steps, DataEdit::Insert(v)).map(|_| ())?,
                        Write::Remove => doc.edit_data(&steps, DataEdit::Remove).map(|_| ())?,
                        Write::Up => {
                            let Some(DataStep::Index(i)) = steps.last().copied() else { return Ok(()) };
                            if i == 0 {
                                return Ok(());
                            }
                            let data = doc.data_json();
                            let item = data.get(CALENDAR_VIEWER).and_then(|c| at(c, &path)).cloned().unwrap_or(Value::Null);
                            doc.edit_data(&steps, DataEdit::Remove)?;
                            let n = steps.len();
                            steps[n - 1] = DataStep::Index(i - 1);
                            doc.edit_data(&steps, DataEdit::Insert(&item))?;
                        }
                    }
                    Ok(())
                })
                .await;
            match edited {
                Ok(_) => f.say(""),
                Err(e) => f.say(e.sentence()),
            }
        });
    }

    /// The calendar starts from `template` (the form's first write).
    fn start(&self, template: Calendar) {
        let f = self.clone();
        spawn_local(async move {
            let value = serde_json::to_value(&template).unwrap_or_default();
            let edited = f
                .api
                .edit(f.metanode, move |doc, _now| -> Result<(), ApiError> {
                    doc.set_data(CALENDAR_VIEWER, &value)?;
                    Ok(())
                })
                .await;
            if let Err(e) = edited {
                f.say(e.sentence());
            }
        });
    }

    /// A text input over the string at `path`.
    fn text(&self, __scope: &mut RenderScope, path: Vec<Step>, width: u32, placeholder: &'static str) -> NodeHandle {
        let (json, writable) = (self.json, self.writable);
        let shown_path = path.clone();
        let f = self.clone();
        rsx! {
            input {
                class: "calendar__input",
                style: format!("{FIELD} width: {width}px;"),
                placeholder: placeholder,
                disabled: {move || !writable.get()},
                value: {move || shown(at(&json.get(), &shown_path))},
                onchange: move |v: String| f.write(path.clone(), Write::Set(Value::String(v.trim().to_string()))),
            }
        }
    }

    /// A text input over the list of names at `path`, written with commas
    /// between them.
    fn names(&self, __scope: &mut RenderScope, path: Vec<Step>, width: u32, placeholder: &'static str) -> NodeHandle {
        let (json, writable) = (self.json, self.writable);
        let shown_path = path.clone();
        let f = self.clone();
        rsx! {
            input {
                class: "calendar__names",
                style: format!("{FIELD} width: {width}px;"),
                placeholder: placeholder,
                disabled: {move || !writable.get()},
                value: {move || shown(at(&json.get(), &shown_path))},
                onchange: move |v: String| {
                    let names: Vec<Value> = v.split(',').map(str::trim).filter(|n| !n.is_empty()).map(|n| Value::String(n.to_string())).collect();
                    f.write(path.clone(), Write::Set(Value::Array(names)))
                },
            }
        }
    }

    /// A number input over the whole number at `path`; empty takes it away
    /// when `optional`, else is zero.
    fn number(&self, __scope: &mut RenderScope, path: Vec<Step>, width: u32, optional: bool) -> NodeHandle {
        let (json, writable) = (self.json, self.writable);
        let shown_path = path.clone();
        let f = self.clone();
        rsx! {
            input {
                class: "calendar__number",
                style: format!("{FIELD} width: {width}px; text-align: right;"),
                disabled: {move || !writable.get()},
                value: {move || shown(at(&json.get(), &shown_path))},
                onchange: move |v: String| match number(&v) {
                    Number::Whole(n) => f.write(path.clone(), Write::Set(json!(n))),
                    Number::Empty if optional => f.write(path.clone(), Write::Remove),
                    Number::Empty => f.write(path.clone(), Write::Set(json!(0))),
                    Number::Not => f.say(format!("\u{201c}{}\u{201d} is not a whole number.", v.trim())),
                },
            }
        }
    }

    /// A chip that turns the boolean at `path` on and off, saying `on` or
    /// `off`.
    fn toggle(&self, __scope: &mut RenderScope, path: Vec<Step>, on: &'static str, off: &'static str) -> NodeHandle {
        let json = self.json;
        let (p1, p2, p3) = (path.clone(), path.clone(), path.clone());
        let f = self.clone();
        rsx! {
            span {
                class: "calendar__toggle",
                style: {move || chip(bool_at(&json.get(), &p1))},
                onclick: move || {
                    if untracked(|| f.writable.get()) {
                        f.write(path.clone(), Write::Set(Value::Bool(!bool_at(&untracked(|| json.get()), &p2))));
                    }
                },
                {move || if bool_at(&json.get(), &p3) { on } else { off }}
            }
        }
    }

    /// The small icons at the end of a row of the list at `list`: up, and
    /// remove.
    fn row_tools(&self, __scope: &mut RenderScope, list: &'static str, i: u32) -> NodeHandle {
        let (fu, fr) = (self.clone(), self.clone());
        rsx! {
            span {
                style: "display: inline-flex; gap: 2px; margin-left: auto;",
                if i > 0 {
                    span {
                        class: "calendar__up",
                        style: look::ICON_BUTTON,
                        title: "Move up",
                        onclick: move || fu.write(vec![Step::K(list), Step::I(i)], Write::Up),
                        span { style: ICON, IconGlyph { name: "arrow-up".to_string() } }
                    }
                }
                span {
                    class: "calendar__remove",
                    style: look::ICON_BUTTON,
                    title: "Remove",
                    onclick: move || fr.write(vec![Step::K(list), Step::I(i)], Write::Remove),
                    span { style: ICON, IconGlyph { name: "trash".to_string() } }
                }
            }
        }
    }
}

const ICON: &str = "display: inline-flex; width: 14px; height: 14px;";
/// An input of the form: the viewers' field, at the width it is given.
const FIELD: &str = "flex: 0 0 auto; box-sizing: border-box; font: inherit; font-size: 13px; padding: 2px 4px;";
const ROW: &str = "display: flex; align-items: center; gap: 6px; flex-wrap: wrap; font-size: 13px;";
/// A month's or a day's row: on one line, its tools at the end.
const ENTRY: &str = "display: flex; align-items: center; gap: 6px; flex-wrap: nowrap; font-size: 13px; white-space: nowrap;";
const SECTION: &str = "font-size: 12px; font-weight: 600; color: var(--rinch-color-dimmed); margin-top: 6px;";

fn bool_at(json: &Value, path: &[Step]) -> bool {
    at(json, path).and_then(Value::as_bool).unwrap_or(false)
}

fn chip(chosen: bool) -> String {
    format!(
        "padding: 2px 10px; border-radius: 12px; font-size: 12px; cursor: pointer; white-space: nowrap; border: 1px solid {}; {}",
        if chosen { "var(--rinch-primary-color)" } else { "var(--viewer-item-border)" },
        if chosen { "background: var(--viewer-item);" } else { "" }
    )
}

/// The year's entries and the eras, for drawing (index, value).
fn listed(json: &Value, list: &str) -> Vec<(u32, Value)> {
    json.get(list).and_then(Value::as_array).map(|a| a.iter().cloned().enumerate().map(|(i, v)| (i as u32, v)).collect()).unwrap_or_default()
}

/// What the sample says: today in the calendar, and a date tried in it.
fn sample(json: &Value, tried: &str) -> (String, String) {
    let Ok(cal) = Calendar::from_json(json) else { return (String::new(), String::new()) };
    let today = cal.write(Moment { at: Calendar::now(), precision: Precision::Day });
    let gregorian = Calendar::gregorian();
    let tried = tried.trim();
    let reads = if tried.is_empty() {
        String::new()
    } else {
        match cal.read(tried) {
            Some(m) => format!("reads as {}, which is {} in the Gregorian calendar.", cal.write(m), gregorian.write(Moment { at: m.at, precision: m.precision.max(Precision::Day) })),
            None => format!("\u{201c}{tried}\u{201d} does not read in this calendar (a year is always needed)."),
        }
    };
    (today, reads)
}

/// How the calendar's first year is written ("1", "1 YF"), for the week's
/// first day.
fn first_year(json: &Value) -> String {
    serde_json::from_value::<Calendar>(json.clone()).map(|c| c.year_label(0)).unwrap_or_else(|_| "year 0".into())
}

/// Today as the calendar would write it, the day first or not: what a
/// choice of how it writes shows.
fn written_as(json: &Value, day_first: bool) -> String {
    let Ok(mut cal) = Calendar::from_json(json) else { return if day_first { "14 Oct 1066".into() } else { "Oct 14, 1066".into() } };
    cal.writes.day_first = day_first;
    cal.writes.weekday = false;
    cal.write(Moment { at: Calendar::now(), precision: Precision::Day })
}

/// The first month's name, for a new season.
fn first_month(json: &Value) -> String {
    json.get("year").and_then(Value::as_array).into_iter().flatten().find_map(|e| e.get("month").and_then(Value::as_str)).unwrap_or("").to_string()
}

/// A season naming a month the year does not have, as a sentence.
fn season_problem(json: &Value) -> Option<String> {
    let entries: Vec<&Value> = json.get("year").and_then(Value::as_array).into_iter().flatten().collect();
    let names: Vec<String> = entries.iter().flat_map(|e| ["month", "short"].map(|k| e.get(k).and_then(Value::as_str).map(str::to_lowercase))).flatten().collect();
    for s in json.get("seasons").and_then(Value::as_array).into_iter().flatten() {
        for key in ["from", "to"] {
            let typed = s.get(key).and_then(Value::as_str).unwrap_or("").trim();
            if !names.contains(&typed.to_lowercase()) {
                let name = s.get("name").and_then(Value::as_str).unwrap_or("A season");
                return Some(format!("{name} names \u{201c}{typed}\u{201d}, which is not a month of this year, so it does not read yet."));
            }
        }
    }
    None
}

/// Where the calendar's year 0 begins, as the Gregorian calendar writes it.
fn epoch_shown(json: &Value) -> String {
    let at = json.get("epoch").and_then(Value::as_i64).unwrap_or(0);
    let g = Calendar::gregorian();
    let precision = if g.floor(at, Precision::Day) == at { Precision::Day } else { Precision::Minute };
    g.write(Moment { at, precision })
}

pub fn view(__scope: &mut RenderScope, mount: Mount) -> NodeHandle {
    let Some(metanode) = mount.metanode() else {
        return rsx! { div { style: look::LINE_STYLE, "This viewer was shown without a metanode." } };
    };
    let props = mount.props;
    let read = move || untracked(|| props.get()).settings.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
    let f = Form {
        api: mount.api.clone(),
        metanode,
        json: Signal::new(read()),
        status: Signal::new(String::new()),
        writable: Signal::new(false),
        tried: Signal::new(String::new()),
    };
    {
        let json = f.json;
        __scope.create_effect(move || {
            let next = props.get().settings.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
            if untracked(|| json.get()) != next {
                json.set(next);
            }
        });
    }
    {
        let f = f.clone();
        spawn_local(async move {
            if let Ok(node) = f.api.node(f.metanode).await {
                f.writable.set(node.access.allows_write());
            }
        });
    }
    let seen = mount.host.seen();
    let (json, status, tried, writable) = (f.json, f.status, f.tried, f.writable);
    let (fg, fi, fname, fday, fweek, fleap, fepoch, fyear, feras, fadd) = (f.clone(), f.clone(), f.clone(), f.clone(), f.clone(), f.clone(), f.clone(), f.clone(), f.clone(), f.clone());
    rsx! {
        div {
            class: "calendar",
            style: {move || format!("display: flex; flex-direction: column; gap: 8px; padding: 12px 16px; height: 100%; box-sizing: border-box; overflow-y: auto; {}", look::surfaces(seen.dark()))},
            if !json.get().is_object() {
                div {
                    class: "calendar__start",
                    style: format!("display: flex; flex-direction: column; gap: 10px; padding: 14px; {}", look::PANEL),
                    div { style: "font-size: 14px;", "A calendar to read timelines in: real or made up, its months, days, hours and eras your own." }
                    div {
                        style: ROW,
                        span { class: "calendar__start-gregorian", style: chip(false), onclick: move || {
                            let mut g = Calendar::gregorian();
                            g.name = "My calendar".into();
                            fg.start(g)
                        }, "Start from the Gregorian calendar" }
                        span { class: "calendar__start-invented", style: chip(false), onclick: move || fi.start(Calendar::invented()), "Start an invented calendar" }
                    }
                }
            } else {
                {form(__scope, &fname, &fday, &fweek, &fleap, &fepoch, &fyear, &feras, &fadd)}
            }
            div { class: "calendar__status", style: look::LINE_STYLE, {move || status.get()} }
            if json.get().is_object() {
                div {
                    class: "calendar__sample",
                    style: format!("display: flex; flex-direction: column; gap: 6px; padding: 10px 12px; {}", look::PANEL),
                    {move || match Calendar::from_json(&json.get()) {
                        Ok(_) => format!("Today reads {}.", sample(&json.get(), "").0),
                        Err(why) => why,
                    }}
                    div {
                        style: ROW,
                        span { style: look::LINE_STYLE, "Try a date:" }
                        input {
                            class: "calendar__try",
                            style: format!("{FIELD} width: 200px;"),
                            placeholder: "Day 214, 3088 AF",
                            value: {move || tried.get()},
                            oninput: move |v: String| tried.set(v),
                        }
                        span { class: "calendar__reads", style: look::LINE_STYLE, {move || sample(&json.get(), &tried.get()).1} }
                    }
                }
            }
            if !writable.get() && json.get().is_object() {
                div { style: look::LINE_STYLE, "You can read this calendar, not change it." }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn form(__scope: &mut RenderScope, fname: &Form, fday: &Form, fweek: &Form, fleap: &Form, fepoch: &Form, fyear: &Form, feras: &Form, fadd: &Form) -> NodeHandle {
    let json = fname.json;
    let (fm, fd, fe, fs) = (fadd.clone(), fadd.clone(), fadd.clone(), fadd.clone());
    let (fweek, fwrites, fwrites2, fseasons) = (fweek.clone(), fadd.clone(), fadd.clone(), fadd.clone());
    let (fyear, feras) = (fyear.clone(), feras.clone());
    let fepoch2 = fepoch.clone();
    let epoch_writable = fepoch.writable;
    rsx! {
        div {
            class: "calendar__form",
            style: format!("display: flex; flex-direction: column; gap: 6px; padding: 12px 14px; {}", look::PANEL),
            div { style: ROW, span { style: look::LINE_STYLE, "Name" } {fname.text(__scope, vec![Step::K("name")], 260, "Shire Reckoning")} }

            div { style: SECTION, "A day" }
            div {
                style: ROW,
                {fday.number(__scope, vec![Step::K("day"), Step::K("hours")], 48, false)}
                span { "hours of" }
                {fday.number(__scope, vec![Step::K("day"), Step::K("minutes")], 48, false)}
                span { "minutes of" }
                {fday.number(__scope, vec![Step::K("day"), Step::K("seconds")], 48, false)}
                span { title: "A second here is a real second, so timelines in different calendars line up.", "seconds" }
            }

            div { style: SECTION, "A year, in order" }
            for (i, entry) in listed(&json.get(), "year") {
                div {
                    key: format!("{i} {entry}"),
                    class: "calendar__entry",
                    style: ROW,
                    {entry_row(__scope, &fyear, i, &entry)}
                }
            }
            div {
                style: ROW,
                span { class: "calendar__add-month", style: chip(false), onclick: move || {
                    let n = listed(&untracked(|| fm.json.get()), "year").len() as u32;
                    fm.write(vec![Step::K("year"), Step::I(n)], Write::Insert(json!({ "month": "New month", "days": 30 })))
                }, "+ Month" }
                span { class: "calendar__add-day", style: chip(false), onclick: move || {
                    let n = listed(&untracked(|| fd.json.get()), "year").len() as u32;
                    fd.write(vec![Step::K("year"), Step::I(n)], Write::Insert(json!({ "day": "Festival", "leap": false })))
                }, "+ Day of no month" }
            }

            div { style: SECTION, "A week" }
            div {
                style: ROW,
                span { style: look::LINE_STYLE, "Day names, in order" }
                {fweek.names(__scope, vec![Step::K("week")], 420, "none: a calendar without weeks")}
            }
            if json.get().get("week").and_then(Value::as_array).is_some_and(|w| !w.is_empty()) {
                div {
                    style: ROW,
                    span { style: look::LINE_STYLE, {move || format!("The first day of {} is a", first_year(&json.get()))} }
                    for (i, name) in json.get().get("week").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().enumerate() {
                        span {
                            key: format!("{i} {name}"),
                            class: "calendar__week-starts",
                            style: {move || chip(json.get().get("week_starts").and_then(Value::as_u64).unwrap_or(0) == i as u64)},
                            onclick: { let f = fweek.clone(); move || if untracked(|| f.writable.get()) { f.write(vec![Step::K("week_starts")], Write::Set(json!(i))) } },
                            {name.as_str().unwrap_or_default().to_string()}
                        }
                    }
                }
            }

            div { style: SECTION, "Writing a date" }
            div {
                style: ROW,
                for day_first in [false, true] {
                    span {
                        key: format!("{day_first}"),
                        class: "calendar__day-first",
                        style: {move || chip(bool_at(&json.get(), &[Step::K("writes"), Step::K("day_first")]) == day_first)},
                        onclick: { let f = fwrites2.clone(); move || if untracked(|| f.writable.get()) { f.write(vec![Step::K("writes"), Step::K("day_first")], Write::Set(json!(day_first))) } },
                        {move || written_as(&json.get(), day_first)}
                    }
                }
                {fwrites.toggle(__scope, vec![Step::K("writes"), Step::K("full_months")], "whole month names", "short month names")}
                {fwrites.toggle(__scope, vec![Step::K("writes"), Step::K("weekday")], "with the day of the week", "without the day of the week")}
            }

            div { style: SECTION, "Seasons: parts of the year a date can name (\u{201c}spring of 412\u{201d})" }
            for (i, season) in listed(&json.get(), "seasons") {
                div {
                    key: format!("{i} {season}"),
                    class: "calendar__season",
                    style: ENTRY,
                    {fseasons.text(__scope, vec![Step::K("seasons"), Step::I(i), Step::K("name")], 110, "Spring")}
                    span { "from the start of" }
                    {fseasons.text(__scope, vec![Step::K("seasons"), Step::I(i), Step::K("from")], 110, "March")}
                    span { "to the end of" }
                    {fseasons.text(__scope, vec![Step::K("seasons"), Step::I(i), Step::K("to")], 110, "May")}
                    span { style: look::LINE_STYLE, "also" }
                    {fseasons.names(__scope, vec![Step::K("seasons"), Step::I(i), Step::K("also")], 80, "Fall")}
                    {fseasons.row_tools(__scope, "seasons", i)}
                }
            }
            if let Some(why) = season_problem(&json.get()) {
                div { class: "calendar__season-problem", style: format!("{} color: var(--rinch-color-orange-6);", look::LINE_STYLE), {why} }
            }
            div {
                style: ROW,
                span { class: "calendar__add-season", style: chip(false), onclick: move || {
                    let j = untracked(|| fs.json.get());
                    let n = listed(&j, "seasons").len() as u32;
                    let first = first_month(&j);
                    fs.write(vec![Step::K("seasons"), Step::I(n)], Write::Insert(json!({ "name": "New season", "from": first, "to": first })))
                }, "+ Season" }
            }

            div { style: SECTION, "Leap years" }
            div {
                style: ROW,
                span { "Every" }
                {fleap.number(__scope, vec![Step::K("leap"), Step::K("every")], 56, false)}
                span { "years, except every" }
                {fleap.number(__scope, vec![Step::K("leap"), Step::K("except_every")], 56, false)}
                span { ", except every" }
                {fleap.number(__scope, vec![Step::K("leap"), Step::K("except_except_every")], 56, false)}
                span { "after all; counted from year" }
                {fleap.number(__scope, vec![Step::K("leap"), Step::K("offset")], 56, false)}
                span { style: look::LINE_STYLE, "(0: never)" }
            }

            div { style: SECTION, "Where it starts" }
            div {
                style: ROW,
                span { "Its year 0 begins on" }
                input {
                    class: "calendar__epoch",
                    style: format!("{FIELD} width: 200px;"),
                    disabled: {move || !epoch_writable.get()},
                    value: {move || epoch_shown(&json.get())},
                    onchange: move |v: String| match Calendar::gregorian().read(&v) {
                        Some(m) => fepoch2.write(vec![Step::K("epoch")], Write::Set(json!(m.at))),
                        None => fepoch2.say(format!("\u{201c}{}\u{201d} is not a date in the Gregorian calendar (\u{201c}Oct 14, 1066\u{201d}, \u{201c}3000 BC\u{201d}).", v.trim())),
                    },
                }
                span { style: look::LINE_STYLE, "in the Gregorian calendar: where it lines up with every other" }
            }

            div { style: SECTION, "Eras: how years are numbered" }
            for (i, era) in listed(&json.get(), "eras") {
                div {
                    key: format!("{i} {era}"),
                    class: "calendar__era",
                    style: ROW,
                    {era_row(__scope, &feras, i)}
                }
            }
            div {
                style: ROW,
                span { class: "calendar__add-era", style: chip(false), onclick: move || {
                    let n = listed(&untracked(|| fe.json.get()), "eras").len() as u32;
                    fe.write(vec![Step::K("eras"), Step::I(n)], Write::Insert(json!({ "name": "New era", "short": "NE", "from": 0, "counts": "up" })))
                }, "+ Era" }
            }
        }
    }
}

/// A month (name, short name, days, leap days) or a day of no month (name,
/// only in leap years).
fn entry_row(__scope: &mut RenderScope, f: &Form, i: u32, entry: &Value) -> NodeHandle {
    let at = |k: &'static str| vec![Step::K("year"), Step::I(i), Step::K(k)];
    if entry.get("month").is_some() {
        rsx! {
            span {
                style: ENTRY,
                {f.text(__scope, at("month"), 110, "Month")}
                {f.text(__scope, at("short"), 48, "Short")}
                {f.number(__scope, at("days"), 48, false)}
                span { "days, +" }
                {f.number(__scope, at("leap_days"), 36, true)}
                span { style: look::LINE_STYLE, "in leap years" }
                {f.row_tools(__scope, "year", i)}
            }
        }
    } else {
        rsx! {
            span {
                style: ENTRY,
                {f.text(__scope, at("day"), 110, "Day")}
                span { style: look::LINE_STYLE, "a day of no month," }
                {f.toggle(__scope, at("leap"), "only in leap years", "every year")}
                {f.row_tools(__scope, "year", i)}
            }
        }
    }
}

/// An era: its name and short name, which way it counts, its first and last
/// year, whether it is left out when writing, and its other spellings.
fn era_row(__scope: &mut RenderScope, f: &Form, i: u32) -> NodeHandle {
    let at = |k: &'static str| vec![Step::K("eras"), Step::I(i), Step::K(k)];
    let json = f.json;
    let fc = f.clone();
    let counts_path = at("counts");
    let down = move || at_counts(&json.get(), i);
    let down2 = down.clone();
    rsx! {
        span {
            style: ROW,
            {f.text(__scope, at("name"), 150, "Anno Domini")}
            {f.text(__scope, at("short"), 50, "AD")}
            span {
                class: "calendar__counts",
                style: {move || chip(down())},
                title: "Which way its years count",
                onclick: move || {
                    if untracked(|| fc.writable.get()) {
                        let next = if untracked(&down2) { "up" } else { "down" };
                        fc.write(counts_path.clone(), Write::Set(json!(next)));
                    }
                },
                {move || if down() { "counts down (BC)" } else { "counts up" }}
            }
            span { "from year" }
            {f.number(__scope, at("from"), 70, true)}
            span { "to" }
            {f.number(__scope, at("to"), 70, true)}
            {f.toggle(__scope, at("quiet"), "assumed, left out", "always written")}
            span { style: look::LINE_STYLE, "also" }
            {f.names(__scope, at("also"), 90, "CE")}
            {f.row_tools(__scope, "eras", i)}
        }
    }
}

fn at_counts(json: &Value, i: u32) -> bool {
    at(json, &[Step::K("eras"), Step::I(i), Step::K("counts")]).and_then(Value::as_str) == Some("down")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_reads_what_is_stored_and_the_sample_reads_dates() {
        let g = serde_json::to_value(Calendar::gregorian()).unwrap();
        assert_eq!(shown(at(&g, &[Step::K("year"), Step::I(1), Step::K("month")])), "February");
        assert_eq!(shown(at(&g, &[Step::K("day"), Step::K("hours")])), "24");
        assert_eq!(shown(at(&g, &[Step::K("week")])).split(", ").count(), 7);
        assert_eq!(epoch_shown(&g), "Jan 1, 1");
        let (_, reads) = sample(&g, "Oct 14 1066");
        assert_eq!(reads, "reads as Oct 14, 1066, which is Oct 14, 1066 in the Gregorian calendar.");
        assert!(sample(&g, "whenever").1.contains("does not read"));
        assert!(matches!(number("1,000"), Number::Whole(1000)));
        assert!(matches!(number(" "), Number::Empty));
        assert!(matches!(number("x"), Number::Not));
    }
}
