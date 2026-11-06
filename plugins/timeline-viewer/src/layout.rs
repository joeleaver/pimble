//! A timeline as data: which nodes are on the line and where, the lanes they
//! fall in, the rows they are packed into so nothing overlaps, and the ticks
//! of the axis at a zoom. Nothing here draws or writes.

use std::collections::HashMap;

use pimble_plugin_sdk::pimble_core::Node;
use pimble_plugin_sdk::prelude::{Addr, ForestEntry};

use crate::calendar::{Calendar, Date, Moment, Precision, Said};

/// The fields a timeline reads by default (docs/TIMELINE_CONTRACT.md
/// decision 5): its start, then `date` when it has no start, and its end.
pub const START: &str = "start";
pub const DATE: &str = "date";
pub const END: &str = "end";

/// Which fields a timeline reads: its own setting, `start`, `date` and `end`
/// unless it says otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fields {
    pub start: String,
    pub date: String,
    pub end: String,
}

impl Default for Fields {
    fn default() -> Self {
        Fields { start: START.into(), date: DATE.into(), end: END.into() }
    }
}

/// A node on the line.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub addr: Addr,
    pub label: String,
    pub color: Option<String>,
    pub background: Option<String>,
    pub start: i64,
    /// Where its span ends; `None` for a point (one moment, a day or finer).
    pub end: Option<i64>,
    /// When it is, as its calendar writes it: what a hover says.
    pub when: String,
    /// The lane it is in: a top node that has no moment of its own.
    pub lane: Option<Addr>,
    pub writable: bool,
    /// The field its start was read from (or its one moment's), and the moment.
    pub from: Source,
    /// The field its end was read from, when it has two moments.
    pub to: Option<Source>,
    /// Which of the timeline's calendars it reads in (0: its own; more when
    /// it shows several, docs/TIMELINE_CONTRACT.md wave 4).
    pub cal: usize,
    /// Its ends are only roughly where they are drawn ("c. 1066", "early
    /// 1400s"): drawn fading.
    pub soft: bool,
    /// Its two moments are written in one field ("412-415 AF").
    pub joined: bool,
    /// A rough date ("early 1400s", "spring of 412") that no moment is
    /// written for, so a drag cannot write it.
    pub rough: bool,
}

/// A moment of an item and the field it was read from: what a drag rewrites.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub field: String,
    pub moment: Moment,
    /// Written "c." (about).
    pub about: bool,
}

/// A node whose fields do not read as moments here: listed beside the line,
/// as typed, never guessed at.
#[derive(Debug, Clone, PartialEq)]
pub struct Unread {
    pub addr: Addr,
    pub label: String,
    /// What is wrong, in a sentence.
    pub why: String,
}

/// A lane: a top node of the source grouping the nodes under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub addr: Addr,
    pub label: String,
}

/// What a forest puts on the line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Placed {
    pub items: Vec<Item>,
    pub lanes: Vec<Lane>,
    pub unread: Vec<Unread>,
}

fn field(node: &Node, name: &str) -> Option<String> {
    node.metadata.fields.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// "the Gregorian calendar", "the Gregorian calendar or the Kepler calendar".
fn called_all(cals: &[&Calendar]) -> String {
    let names: Vec<String> = cals.iter().map(|c| c.called()).collect();
    match names.len() {
        0 | 1 => names.into_iter().next().unwrap_or_default(),
        n => format!("{} or {}", names[..n - 1].join(", "), names[n - 1]),
    }
}

/// Where an item drawn from moments `from` (and `to`) starts and ends, each
/// `(moment, about)`: a point for one exact moment a day or finer, the span
/// of one given only to a month or a year, wider and soft for an "about".
/// `None` when it would end before it starts.
pub fn extent(cal: &Calendar, from: (Moment, bool), to: Option<(Moment, bool)>) -> Option<(i64, Option<i64>)> {
    let start = if from.1 { cal.about_span(from.0).0 } else { from.0.at };
    let end = match to {
        Some((e, about)) => {
            if cal.end_of(e) <= from.0.at {
                return None;
            }
            Some(if about { cal.about_span(e).1 } else { cal.end_of(e) })
        }
        None if from.1 => Some(cal.about_span(from.0).1),
        None => (from.0.precision <= Precision::Month).then(|| cal.end_of(from.0)),
    };
    Some((start, end))
}

/// When an item is, as `cal` writes it.
fn when_of(cal: &Calendar, from: &Source, to: Option<&Source>, joined: bool) -> String {
    match to {
        Some(t) if joined => format!("{}{}", if from.about { "c. " } else { "" }, cal.write_between(from.moment, t.moment)),
        Some(t) => format!("{} to {}", cal.write_about(from.moment, from.about), cal.write_about(t.moment, t.about)),
        None => cal.write_about(from.moment, from.about),
    }
}

/// What an item's fields say, put together in one calendar: `Err` with a
/// sentence when they cannot be.
struct Parts {
    from: Source,
    to: Option<Source>,
    start: i64,
    end: Option<i64>,
    when: String,
    soft: bool,
    joined: bool,
    rough: bool,
}

fn parts(cal: &Calendar, start: Option<(&str, &str, Said)>, end: Option<(&str, &str, Said)>) -> Result<Parts, String> {
    let too_much = |which: &str, text: &str| format!("Its {which} \u{201c}{text}\u{201d} is a span of its own, so it cannot have another field saying where it starts or ends.");
    let one = |field: &str, said: Said, text: &str| -> Result<Parts, String> {
        match said {
            Said::At { moment, about } => {
                let from = Source { field: field.to_string(), moment, about };
                let (start, end) = extent(cal, (moment, about), None).ok_or("It ends before it starts.")?;
                Ok(Parts { when: when_of(cal, &from, None, false), from, to: None, start, end, soft: about, joined: false, rough: false })
            }
            Said::Between { from: f, to: t, about } => {
                let from = Source { field: field.to_string(), moment: f, about };
                let to = Source { field: field.to_string(), moment: t, about };
                let (start, end) = extent(cal, (f, about), Some((t, about))).ok_or("It ends before it starts.")?;
                Ok(Parts { when: when_of(cal, &from, Some(&to), true), from, to: Some(to), start, end, soft: about, joined: true, rough: false })
            }
            Said::Rough { from, to, soft } => Ok(Parts {
                from: Source { field: field.to_string(), moment: Moment { at: from, precision: Precision::Year }, about: false },
                to: None,
                start: from,
                end: Some(to),
                when: text.to_string(),
                soft,
                joined: false,
                rough: true,
            }),
        }
    };
    match (start, end) {
        (Some((f, text, said)), None) | (None, Some((f, text, said))) => one(f, said, text),
        (Some((sf, st, s)), Some((ef, et, e))) => {
            let (Said::At { moment: sm, about: sa }, Said::At { moment: em, about: ea }) = (&s, &e) else {
                return Err(if matches!(s, Said::At { .. }) { too_much("end", et) } else { too_much("start", st) });
            };
            let from = Source { field: sf.to_string(), moment: *sm, about: *sa };
            let to = Source { field: ef.to_string(), moment: *em, about: *ea };
            let (start, end) = extent(cal, (*sm, *sa), Some((*em, *ea))).ok_or("It ends before it starts.")?;
            Ok(Parts { when: when_of(cal, &from, Some(&to), false), from, to: Some(to), start, end, soft: *sa || *ea, joined: false, rough: false })
        }
        (None, None) => Err(String::new()),
    }
}

/// The nodes of a forest on the line, read from `fields` in the first of
/// `cals` (the timeline's own calendar, then any others it shows) that reads
/// them, in the forest's order.
pub fn place(cals: &[&Calendar], fields: &Fields, entries: &[ForestEntry], label: impl Fn(&Node) -> String) -> Placed {
    // Whether a node has a moment field at all.
    let dated = |node: &Node| [&fields.start, &fields.date, &fields.end].iter().any(|f| field(node, f).is_some());
    let by_addr: HashMap<Addr, &ForestEntry> = entries.iter().map(|e| (e.addr, e)).collect();
    // A top with no moment of its own whose subtree has one is a lane.
    fn top_in(by_addr: &HashMap<Addr, &ForestEntry>, e: &ForestEntry) -> Addr {
        let mut at = e;
        while let Some(p) = at.parent.and_then(|p| by_addr.get(&p).copied()) {
            at = p;
        }
        at.addr
    }
    let top_of = |e: &ForestEntry| top_in(&by_addr, e);
    let mut lanes: Vec<Lane> = Vec::new();
    for e in entries.iter().filter(|e| e.parent.is_none() && !e.found && !dated(&e.node)) {
        let has_dated = entries.iter().any(|d| d.addr != e.addr && dated(&d.node) && top_of(d) == e.addr);
        if has_dated {
            lanes.push(Lane { addr: e.addr, label: label(&e.node) });
        }
    }
    let Some(main) = cals.first() else { return Placed::default() };
    let mut placed = Placed { lanes, ..Placed::default() };
    for e in entries {
        if !dated(&e.node) {
            continue;
        }
        let name = label(&e.node);
        let unread = |why: String| Unread { addr: e.addr, label: name.clone(), why };
        let start_field = if field(&e.node, &fields.start).is_some() { &fields.start } else { &fields.date };
        let start_text = field(&e.node, start_field);
        let end_text = field(&e.node, &fields.end);
        // The first calendar that reads every field there is.
        let found = cals.iter().enumerate().find_map(|(ci, cal)| {
            let s = match &start_text {
                Some(t) => Some((start_field.as_str(), t.as_str(), cal.read_said(t)?)),
                None => None,
            };
            let en = match &end_text {
                Some(t) => Some((fields.end.as_str(), t.as_str(), cal.read_said(t)?)),
                None => None,
            };
            Some((ci, s, en))
        });
        let Some((ci, s, en)) = found else {
            let bad = match (&start_text, &end_text) {
                (Some(t), _) if cals.iter().all(|c| c.read_said(t).is_none()) => ("start", t.clone()),
                (_, Some(t)) => ("end", t.clone()),
                (Some(t), None) => ("start", t.clone()),
                (None, None) => continue,
            };
            placed.unread.push(unread(format!("Its {} \u{201c}{}\u{201d} is not a date in {}.", bad.0, bad.1, called_all(cals))));
            continue;
        };
        let cal = cals[ci];
        let p = match parts(cal, s, en) {
            Ok(p) => p,
            Err(why) if why.is_empty() => continue,
            Err(why) => {
                placed.unread.push(unread(why));
                continue;
            }
        };
        // Read in another calendar: where that is in the timeline's own.
        let when = if ci == 0 {
            p.when
        } else {
            format!("{}, which is {} in {}", p.when, main.write(Moment { at: p.start, precision: p.from.moment.precision.max(Precision::Day) }), main.called())
        };
        let top = top_of(e);
        let lane = placed.lanes.iter().any(|l| l.addr == top).then_some(top);
        placed.items.push(Item {
            addr: e.addr,
            label: name,
            color: e.node.metadata.color().map(str::to_string),
            background: e.node.metadata.background().map(str::to_string),
            start: p.start,
            end: p.end,
            when,
            lane,
            writable: e.node.access.allows_write(),
            from: p.from,
            to: p.to,
            cal: ci,
            soft: p.soft,
            joined: p.joined,
            rough: p.rough,
        });
    }
    placed
}

/// What part of an item a drag holds: all of it, or an end of its span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grip {
    Move,
    Start,
    End,
}

/// An item as a drag leaves it: where it is drawn while dragging, and the
/// fields written when it is let go.
#[derive(Debug, Clone, PartialEq)]
pub struct Dragged {
    pub start: i64,
    pub end: Option<i64>,
    pub when: String,
    /// `(field, value)`, in the calendar's writing, each to the precision it
    /// was typed with.
    pub writes: Vec<(String, String)>,
}

/// `item` with what `grip` holds moved by `dt` seconds (docs/TIMELINE_CONTRACT.md
/// wave 3), in `cal`, the calendar it reads in. Every moment keeps its
/// precision and lands on the start of its unit nearest where it was
/// dragged: a year moves by years, a day by days; "c." stays, and two
/// moments in one field are written back into it. Moving a span moves both
/// ends by what its start moved, so it keeps its length. An end of a
/// one-moment span (a month, a year) splits it into a start and an `end`,
/// each the unit it was. A point has no ends to drag. `Err` with a sentence
/// when it would end before it starts, or for a rough date.
pub fn drag(cal: &Calendar, fields: &Fields, item: &Item, grip: Grip, dt: i64) -> Result<Dragged, String> {
    if item.rough {
        return Err(format!("\u{201c}{}\u{201d} is a rough date: change it in its field to move it.", item.when));
    }
    let shift = |m: Moment, dt: i64| Moment { at: cal.round(m.at.saturating_add(dt), m.precision), precision: m.precision };
    // A one-moment span's ends: its moment, a start and an end.
    let (from, to) = match (&item.to, grip) {
        (None, Grip::Start | Grip::End) if item.end.is_some() => {
            let start_field = if item.from.field == fields.end { fields.start.clone() } else { item.from.field.clone() };
            (Source { field: start_field, ..item.from.clone() }, Some(Source { field: fields.end.clone(), ..item.from.clone() }))
        }
        _ => (item.from.clone(), item.to.clone()),
    };
    let (f, t) = match grip {
        Grip::Move => {
            let f = shift(from.moment, dt);
            let moved = f.at - from.moment.at;
            (f, to.as_ref().map(|t| shift(t.moment, moved)))
        }
        Grip::Start if to.is_some() => (shift(from.moment, dt), to.as_ref().map(|t| t.moment)),
        Grip::End if to.is_some() => (from.moment, to.as_ref().map(|t| shift(t.moment, dt))),
        Grip::Start | Grip::End => (from.moment, None),
    };
    let from = Source { moment: f, ..from };
    let to = to.zip(t).map(|(to, t)| Source { moment: t, ..to });
    let (start, end) = extent(cal, (from.moment, from.about), to.as_ref().map(|t| (t.moment, t.about))).ok_or_else(|| "It would end before it starts.".to_string())?;
    let when = when_of(cal, &from, to.as_ref(), item.joined);
    let writes = match &to {
        Some(_) if item.joined => vec![(from.field.clone(), when.clone())],
        Some(to) => vec![(from.field.clone(), cal.write_about(from.moment, from.about)), (to.field.clone(), cal.write_about(to.moment, to.about))],
        None => vec![(from.field.clone(), cal.write_about(from.moment, from.about))],
    };
    Ok(Dragged { start, end, when, writes })
}

/// How closely a moment made by a double-click on the line is given at
/// `scale` (pixels a second): to the minute when a day is wide, to the day
/// when a day is a few pixels, else to the month or the year.
pub fn precision_at(cal: &Calendar, scale: f64) -> Precision {
    let day = cal.seconds_in_day() as f64 * scale;
    if day >= 600.0 {
        Precision::Minute
    } else if day >= 6.0 {
        Precision::Day
    } else if day * 30.0 >= 24.0 {
        Precision::Month
    } else {
        Precision::Year
    }
}

/// What part of the line shows: the moment at the left edge and how many
/// pixels a second is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub start: i64,
    pub scale: f64,
}

/// The closest and farthest a timeline zooms: a second across 2000 px, and
/// 30 billion years across 1000.
pub const MAX_SCALE: f64 = 2000.0;
pub const MIN_SCALE: f64 = 1000.0 / (30.0e9 * 365.25 * 86_400.0);

impl View {
    /// Where `at` is, in pixels from the left edge.
    pub fn x(&self, at: i64) -> f64 {
        (at as i128 - self.start as i128) as f64 * self.scale
    }

    /// The moment at `x` pixels from the left edge.
    pub fn at(&self, x: f64) -> i64 {
        self.start.saturating_add((x / self.scale) as i64)
    }

    /// The view zoomed by `factor` around `x` pixels from the left edge.
    pub fn zoomed(&self, factor: f64, x: f64) -> View {
        let scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let pinned = self.at(x);
        View { start: pinned.saturating_sub((x / scale) as i64), scale }
    }

    /// The view moved by `dx` pixels (positive: later).
    pub fn panned(&self, dx: f64) -> View {
        View { start: self.start.saturating_add((dx / self.scale) as i64), ..*self }
    }

    /// A view showing `from..to` across `width` pixels: a margin on the
    /// left, and room on the right for the labels after the last moment.
    pub fn fit(from: i64, to: i64, width: f64) -> View {
        let (left, right) = (40.0, 220.0_f64.min(width * 0.4));
        let usable = (width - left - right).max(60.0);
        let span = (to as i128 - from as i128).max(3600) as f64;
        let scale = (usable / span).clamp(MIN_SCALE, MAX_SCALE);
        View { start: (from as i128 - (left / scale) as i128) as i64, scale }
    }
}

/// How wide a label is drawn, roughly, in pixels: enough to pack rows by.
pub fn label_px(label: &str) -> f64 {
    16.0 + 7.2 * label.chars().count() as f64
}

/// Each item's row within its lane, packed first-fit in order of start so no
/// two overlap at `view` (a point or a narrow bar takes its label's width),
/// and how many rows each lane (`None`: the items in no lane) needs.
pub fn pack(items: &[Item], view: View) -> (HashMap<Addr, usize>, HashMap<Option<Addr>, usize>) {
    let mut order: Vec<&Item> = items.iter().collect();
    order.sort_by_key(|i| i.start);
    let mut ends: HashMap<Option<Addr>, Vec<f64>> = HashMap::new();
    let mut rows = HashMap::new();
    for item in order {
        let x0 = view.x(item.start);
        let bar = item.end.map_or(0.0, |e| view.x(e) - x0);
        let x1 = x0 + bar.max(label_px(&item.label) + 12.0);
        let lane = ends.entry(item.lane).or_default();
        let row = match lane.iter().position(|end| *end + 6.0 <= x0) {
            Some(r) => r,
            None => {
                lane.push(f64::NEG_INFINITY);
                lane.len() - 1
            }
        };
        lane[row] = x1;
        rows.insert(item.addr, row);
    }
    let counts = ends.into_iter().map(|(lane, rows)| (lane, rows.len())).collect();
    (rows, counts)
}

/// A step of the axis.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Unit {
    Seconds(i64),
    Days(i64),
    Months(i64),
    Years(i64),
}

/// A tick of the axis: where, and what it says.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub at: i64,
    pub label: String,
}

/// The closest two ticks are drawn.
pub const TICK_PX: f64 = 84.0;
const MAX_TICKS: usize = 400;

/// The steps a calendar's axis may take, smallest first, each with roughly
/// how many seconds it is.
fn units(cal: &Calendar) -> Vec<(Unit, f64)> {
    let (sm, sh, sd) = (cal.day.seconds_in_minute(), cal.day.seconds_in_hour(), cal.seconds_in_day());
    let mut out: Vec<(Unit, f64)> = Vec::new();
    for s in [1, 5, 15, 30] {
        out.push((Unit::Seconds(s), s as f64));
    }
    for m in [1, 5, 15, 30] {
        out.push((Unit::Seconds(m * sm), (m * sm) as f64));
    }
    for h in [1, 3, 6, 12] {
        if h < i64::from(cal.day.hours) {
            out.push((Unit::Seconds(h * sh), (h * sh) as f64));
        }
    }
    for d in [1, 2, 7, 14] {
        out.push((Unit::Days(d), (d * sd) as f64));
    }
    let year = (0..400).map(|y| cal.year_days(y)).sum::<i64>() as f64 / 400.0 * sd as f64;
    let months = cal.year.iter().enumerate().filter(|(i, _)| cal.is_month(*i)).count().max(1);
    if months > 1 {
        for m in [1, 3, 6] {
            if (m as usize) < months {
                out.push((Unit::Months(m), year / months as f64 * m as f64));
            }
        }
    }
    let mut n: i64 = 1;
    while n <= 10_000_000_000 {
        for k in [1, 2, 5] {
            out.push((Unit::Years(n * k), year * (n * k) as f64));
        }
        n *= 10;
    }
    out
}

/// The axis at `view` across `width` pixels: its ticks, and above them the
/// larger units they fall in (the month and year over days, the day over
/// hours), each at its start, the first one pinned to the left edge.
pub fn ticks(cal: &Calendar, view: View, width: f64) -> (Vec<Tick>, Vec<Tick>) {
    let (from, to) = (view.start, view.at(width));
    let unit = units(cal).into_iter().find(|(_, secs)| secs * view.scale >= TICK_PX).map(|(u, _)| u).unwrap_or(Unit::Years(10_000_000_000));
    let sd = cal.seconds_in_day();
    let mut ticks: Vec<Tick> = Vec::new();
    let mut context: Vec<Tick> = Vec::new();
    let push = |v: &mut Vec<Tick>, at: i64, label: String| {
        if at >= from - sd && at <= to && v.len() < MAX_TICKS {
            v.push(Tick { at, label });
        }
    };
    let first = cal.date_of(from);
    match unit {
        Unit::Seconds(step) => {
            let mut day = cal.moment_of(Date { second: 0, ..first });
            let with_seconds = step < cal.day.seconds_in_minute();
            while day <= to && ticks.len() < MAX_TICKS {
                let date = cal.date_of(day);
                push(&mut context, day.max(from), cal.day_label(date));
                // From the last step before the view, not from midnight.
                let mut s = ((from - day).max(0) / step - 1).max(0) * step;
                while s < sd && day + s <= to {
                    push(&mut ticks, day + s, cal.time_label(s, with_seconds));
                    s += step;
                }
                day += sd;
            }
        }
        Unit::Days(step) => {
            let first_day = (from - cal.epoch).div_euclid(sd);
            let mut d = first_day - first_day.rem_euclid(step);
            let mut month_seen: Option<(i64, usize)> = None;
            while cal.epoch + d * sd <= to && ticks.len() < MAX_TICKS {
                let at = cal.epoch + d * sd;
                let date = cal.date_of(at);
                if month_seen != Some((date.year, date.entry)) {
                    month_seen = Some((date.year, date.entry));
                    let start = cal.moment_of(Date { day: 1, second: 0, ..date });
                    push(&mut context, start.max(from), format!("{} {}", cal.entry_label(date.entry), cal.year_label(date.year)));
                }
                let label = if cal.is_month(date.entry) { format!("{} {}", cal.entry_label(date.entry), date.day) } else { cal.entry_label(date.entry) };
                push(&mut ticks, at, label);
                d += step;
            }
        }
        Unit::Months(step) => {
            let mut year = first.year;
            while cal.year_start(year) <= to && ticks.len() < MAX_TICKS {
                push(&mut context, cal.year_start(year).max(from), cal.year_label(year));
                let mut ordinal = 0;
                for entry in 0..cal.year.len() {
                    if !cal.is_month(entry) {
                        continue;
                    }
                    if ordinal % step == 0 {
                        push(&mut ticks, cal.moment_of(Date { year, entry, day: 1, second: 0 }), cal.entry_label(entry));
                    }
                    ordinal += 1;
                }
                year += 1;
            }
        }
        Unit::Years(step) => {
            let (y_from, y_to) = (first.year, cal.date_of(to).year);
            for era in &cal.eras {
                let lo = era.from.map_or(y_from, |f| f.max(y_from));
                let hi = era.to.map_or(y_to, |t| t.min(y_to));
                if lo > hi {
                    continue;
                }
                // The year an era would number 0, so ticks fall on round numbers in it.
                let zero = match era.counts {
                    crate::calendar::Counts::Up => era.from.unwrap_or(0) - 1,
                    crate::calendar::Counts::Down => era.to.unwrap_or(0) + 1,
                };
                // An era's first year is marked too (AD 1 after the BC
                // centuries), when it falls between round numbers.
                if let (crate::calendar::Counts::Up, Some(f)) = (era.counts, era.from) {
                    if step > 1 && f >= lo && f <= hi && (f - zero).rem_euclid(step) != 0 {
                        push(&mut ticks, cal.year_start(f), cal.year_label(f));
                    }
                }
                let mut y = lo + (zero - lo).rem_euclid(step);
                while y <= hi && ticks.len() < MAX_TICKS {
                    push(&mut ticks, cal.year_start(y), cal.year_label(y));
                    y += step;
                }
            }
            ticks.sort_by_key(|t| t.at);
        }
    }
    (ticks, context)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g() -> Calendar {
        Calendar::gregorian()
    }

    fn labels(t: &[Tick]) -> Vec<String> {
        t.iter().map(|t| t.label.clone()).collect()
    }

    #[test]
    fn the_axis_steps_from_seconds_to_ages_as_it_zooms_out() {
        let c = g();
        let at = c.read("Oct 14 1066 09:00").unwrap().at;
        // A minute across 1000 px: ticks every 15 seconds (5 would be closer than TICK_PX).
        let (t, ctx) = ticks(&c, View { start: at, scale: 1000.0 / 60.0 }, 1000.0);
        assert!(labels(&t).contains(&"09:00:15".to_string()), "{:?}", labels(&t));
        assert_eq!(ctx.first().map(|c| c.label.as_str()), Some("Oct 14, 1066"));
        // A month across 1000 px: weeks, on Mondays (the line's day 0, 1 January
        // AD 1, is a Monday); a week across 1000 px: days.
        let (t, ctx) = ticks(&c, View { start: c.read("Oct 1 1066").unwrap().at, scale: 1000.0 / (31.0 * 86_400.0) }, 1000.0);
        assert_eq!(labels(&t), ["Oct 1", "Oct 8", "Oct 15", "Oct 22", "Oct 29"]);
        let (days, _) = ticks(&c, View { start: c.read("Oct 10 1066").unwrap().at, scale: 1000.0 / (7.0 * 86_400.0) }, 1000.0);
        assert!(labels(&days).contains(&"Oct 14".to_string()), "{:?}", labels(&days));
        assert_eq!(ctx.first().map(|c| c.label.as_str()), Some("Oct 1066"));
        // Ten years: months or years.
        let (t, _) = ticks(&c, View { start: c.read("1060").unwrap().at, scale: 1000.0 / (10.0 * 365.25 * 86_400.0) }, 1000.0);
        assert!(labels(&t).iter().any(|l| l == "1066" || l == "Jan"), "{:?}", labels(&t));
        // Across BC and AD, by centuries: round numbers in each era, no year 0.
        let (t, _) = ticks(&c, View { start: c.read("500 BC").unwrap().at, scale: 1000.0 / (1000.0 * 365.25 * 86_400.0) }, 1000.0);
        let l = labels(&t);
        assert!(l.contains(&"200 BC".to_string()) && l.contains(&"200".to_string()), "{l:?}");
        assert!(!l.iter().any(|x| x == "0" || x == "0 BC"), "{l:?}");
        assert!(l.contains(&"1".to_string()), "AD 1 is marked: {l:?}");
        // Deep time: millions of years, and the ticks are still few.
        let (t, _) = ticks(&c, View { start: -c.read("2026").unwrap().at * 1000, scale: 1000.0 / (5.0e6 * 365.25 * 86_400.0) }, 1000.0);
        assert!(!t.is_empty() && t.len() < 30, "{}", t.len());
    }

    #[test]
    fn zooming_keeps_the_moment_under_the_pointer_in_place() {
        let v = View { start: 1_000_000, scale: 0.01 };
        let under = v.at(300.0);
        let z = v.zoomed(2.0, 300.0);
        assert!((z.at(300.0) - under).abs() <= 1, "{} vs {under}", z.at(300.0));
        assert!((z.scale - 0.02).abs() < 1e-12);
        let fit = View::fit(0, 86_400 * 10, 1000.0);
        assert!(fit.x(0) > 0.0 && fit.x(86_400 * 10) < 1000.0);
    }

    #[test]
    fn rows_are_packed_so_nothing_overlaps() {
        use pimble_plugin_sdk::pimble_core::{NodeId, StoreId};
        let store = StoreId::new();
        let item = |start: i64, end: Option<i64>, label: &str| Item {
            addr: Addr::new(store, NodeId::new()),
            label: label.into(),
            color: None,
            background: None,
            start,
            end,
            when: String::new(),
            lane: None,
            writable: true,
            from: Source { field: START.into(), moment: Moment { at: start, precision: Precision::Second }, about: false },
            to: None,
            cal: 0,
            soft: false,
            joined: false,
            rough: false,
        };
        let view = View { start: 0, scale: 1.0 };
        let items = vec![item(0, Some(500), "a"), item(100, Some(200), "b"), item(600, None, "c"), item(150, None, "d")];
        let (rows, counts) = pack(&items, view);
        assert_eq!(rows[&items[0].addr], 0);
        assert_eq!(rows[&items[1].addr], 1, "b overlaps a");
        assert_eq!(rows[&items[2].addr], 0, "c starts after a ends");
        assert_eq!(rows[&items[3].addr], 2, "d's label overlaps b's");
        assert_eq!(counts[&None], 3);
    }

    /// The one item `fields` put on the line.
    fn placed(fields: &[(&str, &str)]) -> Item {
        use pimble_plugin_sdk::pimble_core::{NodeId, StoreId};
        let mut node = Node::new("document");
        node.metadata.fields = fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let entry = ForestEntry { addr: Addr::new(StoreId::new(), NodeId::new()), node, parent: None, children: vec![], depth: 1, found: false };
        let p = place(&[&g()], &Fields::default(), &[entry], |n| n.metadata.title.clone());
        assert!(p.unread.is_empty(), "{:?}", p.unread);
        p.items.into_iter().next().unwrap()
    }

    #[test]
    fn a_drag_moves_moments_by_their_own_units_and_writes_them_as_typed() {
        let c = g();
        let day = 86_400;
        // A span of days moved by a day and a half lands two days later (the
        // nearest day), keeps its length, and writes both fields.
        let plan = placed(&[("start", "Oct 1 2026"), ("end", "Oct 20 2026")]);
        let d = drag(&c, &Fields::default(), &plan, Grip::Move, day * 3 / 2 + 3600 * 2).unwrap();
        assert_eq!(d.writes, [("start".to_string(), "Oct 3, 2026".to_string()), ("end".to_string(), "Oct 22, 2026".to_string())]);
        assert_eq!(d.end.unwrap() - d.start, plan.end.unwrap() - plan.start);
        // Its end edge moves only the end; dragged before the start it refuses.
        let d = drag(&c, &Fields::default(), &plan, Grip::End, -day * 4).unwrap();
        assert_eq!(d.writes, [("start".to_string(), "Oct 1, 2026".to_string()), ("end".to_string(), "Oct 16, 2026".to_string())]);
        assert_eq!(drag(&c, &Fields::default(), &plan, Grip::End, -day * 40).unwrap_err(), "It would end before it starts.");
        // A point in time moves to the minute when it was typed to the minute.
        let call = placed(&[("date", "Nov 3 2026 14:30")]);
        let d = drag(&c, &Fields::default(), &call, Grip::Move, 95).unwrap();
        assert_eq!(d.writes, [("date".to_string(), "Nov 3, 2026 14:32".to_string())]);
        assert_eq!(d.end, None);
        // A year moves by years, across BC and AD with no year 0.
        let reign = placed(&[("date", "2 BC")]);
        let d = drag(&c, &Fields::default(), &reign, Grip::Move, 365 * day * 2 + 3 * day).unwrap();
        assert_eq!(d.writes, [("date".to_string(), "1".to_string())]);
        // An end of a one-moment span (a month) splits it into a start and an end.
        let month = placed(&[("date", "Oct 1066")]);
        let d = drag(&c, &Fields::default(), &month, Grip::End, 31 * day * 2).unwrap();
        assert_eq!(d.writes, [("date".to_string(), "Oct 1066".to_string()), ("end".to_string(), "Dec 1066".to_string())]);
        assert_eq!(d.end, Some(c.read("Jan 1067").unwrap().at));
    }

    #[test]
    fn rounding_lands_on_the_nearest_start_of_a_unit() {
        let c = g();
        let at = c.read("Oct 14 1066 13:00").unwrap().at;
        assert_eq!(c.round(at, Precision::Day), c.read("Oct 15 1066").unwrap().at);
        assert_eq!(c.round(at, Precision::Month), c.read("Oct 1066").unwrap().at);
        assert_eq!(c.round(at, Precision::Year), c.read("1067").unwrap().at);
        assert_eq!(c.round(at + 29, Precision::Minute), at);
        assert_eq!(c.floor(at + 59, Precision::Minute), at);
    }

    #[test]
    fn rough_and_about_dates_are_drawn_soft_and_drag_as_written() {
        let c = g();
        let year = 365 * 86_400;
        // "c." stays when it moves.
        let about = placed(&[("date", "c. 1066")]);
        assert!(about.soft && !about.rough && about.end.is_some());
        let d = drag(&c, &Fields::default(), &about, Grip::Move, year * 4 + 86_400).unwrap();
        assert_eq!(d.writes, [("date".to_string(), "c. 1070".to_string())]);
        // Two moments in one field are written back into it.
        let war = placed(&[("date", "1939-1945")]);
        assert!(war.joined && !war.soft);
        assert_eq!(war.when, "1939\u{2013}1945");
        let d = drag(&c, &Fields::default(), &war, Grip::End, year).unwrap();
        assert_eq!(d.writes, [("date".to_string(), "1939\u{2013}1946".to_string())]);
        // A rough date is drawn, not dragged.
        let rough = placed(&[("date", "early 1400s")]);
        assert!(rough.rough && rough.soft);
        assert_eq!(drag(&c, &Fields::default(), &rough, Grip::Move, year).unwrap_err(), "\u{201c}early 1400s\u{201d} is a rough date: change it in its field to move it.");
    }

    #[test]
    fn a_date_that_does_not_read_in_the_timelines_calendar_reads_in_another_it_shows() {
        use pimble_plugin_sdk::pimble_core::{NodeId, StoreId};
        let colony: Calendar = serde_json::from_value(serde_json::json!({
            "name": "Kepler", "day": { "hours": 26, "minutes": 60, "seconds": 60 },
            "year": [{ "month": "Day", "days": 412 }], "epoch": 63_000_000_000_i64,
            "eras": [{ "name": "After Founding", "short": "AF", "from": 0, "counts": "up" }]
        }))
        .unwrap();
        let entry = |date: &str| {
            let mut node = Node::new("document");
            node.metadata.title = date.into();
            node.metadata.fields = vec![("date".into(), date.into())];
            ForestEntry { addr: Addr::new(StoreId::new(), NodeId::new()), node, parent: None, children: vec![], depth: 1, found: false }
        };
        let entries = [entry("Oct 14 1066"), entry("Day 214, 3088 AF"), entry("Stardate 41153")];
        let p = place(&[&g(), &colony], &Fields::default(), &entries, |n| n.metadata.title.clone());
        assert_eq!(p.items.iter().map(|i| i.cal).collect::<Vec<_>>(), [0, 1]);
        assert!(p.items[1].when.starts_with("Day 214, 3088 AF, which is "), "{}", p.items[1].when);
        assert!(p.items[1].when.ends_with("in the Gregorian calendar"), "{}", p.items[1].when);
        assert_eq!(p.unread[0].why, "Its start \u{201c}Stardate 41153\u{201d} is not a date in the Gregorian calendar or the Kepler calendar.");
    }
}
