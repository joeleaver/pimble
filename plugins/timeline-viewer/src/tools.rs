//! What the timeline does for an agent (docs/AGENT_GUIDES_CONTRACT.md section
//! 8): reading a date the way the timeline reads it, before it is written, and
//! checking a timeline's or a calendar's settings. Run by the MCP with no
//! window; every read goes through the API, bounded by the agent's grant.

use std::sync::Arc;

use pimble_plugin_sdk::pimble_core::PimbleUrl;
use pimble_plugin_sdk::prelude::*;

use crate::calendar::{Calendar, Entry, Moment, Precision, Said};
use crate::{parse_settings, CalRef, CALENDAR_VIEWER, VIEWER};

pub fn tools(request: ToolRequest, api: Arc<Pimble>) -> ToolFuture {
    Box::pin(async move { answer(&request, &api).await })
}

fn text(args: &serde_json::Value, key: &str) -> String {
    args.get(key).and_then(|v| v.as_str()).unwrap_or_default().trim().to_string()
}

fn addr_of(link: &str) -> Option<Addr> {
    PimbleUrl::parse(link).map(|u| Addr::new(u.store, u.node))
}

async fn answer(r: &ToolRequest, api: &Pimble) -> ToolAnswer {
    let a = &r.args;
    match r.tool.as_str() {
        "check_settings" => {
            let settings = a.get("settings").cloned().unwrap_or_default();
            match text(a, "viewer").as_str() {
                VIEWER => check_timeline(api, &settings).await,
                CALENDAR_VIEWER => check_calendar(&settings),
                other => ToolAnswer::no(format!("the timeline plugin has no {other} viewer")),
            }
        }
        "check_field" => check_field(api, a).await,
        "outline" => outline(api, a).await,
        "read_date" => {
            let named = text(a, "calendar");
            let cal = match CalRef::parse(if named.is_empty() { "gregorian" } else { &named }) {
                Some(c) => calendar_of(api, c).await,
                None => Err(format!("{named} is neither \"gregorian\" nor a calendar node's pimble: link")),
            };
            match cal {
                Ok(cal) => read_in(&[cal], &text(a, "text")),
                Err(why) => ToolAnswer::no(why),
            }
        }
        other => ToolAnswer::no(format!("the timeline plugin has no tool {other}")),
    }
}

/// The calendar `named` names, or why it cannot be used.
async fn calendar_of(api: &Pimble, named: CalRef) -> Result<Calendar, String> {
    let CalRef::Node(addr) = named else { return Ok(Calendar::gregorian()) };
    let doc = api.doc(addr).await.map_err(|e| format!("the calendar node cannot be read: {}", e.sentence()))?;
    let title = doc.fields().map(|f| f.title).unwrap_or_default();
    match doc.data_json().get(CALENDAR_VIEWER).map(Calendar::from_json) {
        Some(Ok(cal)) => Ok(cal),
        Some(Err(why)) => Err(format!("\u{201c}{title}\u{201d} cannot be used as a calendar yet: {why}")),
        None => Err(format!("\u{201c}{title}\u{201d} is not a calendar: it has no calendar definition (set_metanode with the calendar viewer, then its settings)")),
    }
}

/// A timeline's settings: its calendar and the ones beside it must be
/// calendars, its field names not empty.
async fn check_timeline(api: &Pimble, settings: &serde_json::Value) -> ToolAnswer {
    for key in ["start", "date", "end"] {
        if settings.get(key).and_then(|v| v.as_str()).is_some_and(|f| f.trim().is_empty()) {
            return ToolAnswer::no(format!("{key} names no field; leave it out to read the field {key}"));
        }
    }
    let mut named: Vec<(String, CalRef)> = Vec::new();
    if let Some(link) = settings.get("calendar").and_then(|v| v.as_str()) {
        match addr_of(link) {
            Some(addr) => named.push((link.to_string(), CalRef::Node(addr))),
            None => return ToolAnswer::no(format!("calendar must be a calendar node's pimble: link, not \u{201c}{link}\u{201d} (leave it out for the Gregorian)")),
        }
    }
    for c in settings.get("also").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|c| c.as_str()) {
        match CalRef::parse(c) {
            Some(r) => named.push((c.to_string(), r)),
            None => return ToolAnswer::no(format!("also takes \"gregorian\" or calendar nodes' pimble: links, not \u{201c}{c}\u{201d}")),
        }
    }
    let mut names = Vec::new();
    for (link, r) in named {
        match calendar_of(api, r).await {
            Ok(cal) => names.push(cal.called()),
            Err(why) => return ToolAnswer::no(format!("{link}: {why}")),
        }
    }
    let own = if settings.get("calendar").is_some() { names.first().cloned().unwrap_or_default() } else { "the Gregorian calendar".to_string() };
    let beside: Vec<String> = names.into_iter().skip(usize::from(settings.get("calendar").is_some())).collect();
    ToolAnswer::ok(if beside.is_empty() {
        format!("It counts in {own}.")
    } else {
        format!("It counts in {own}, with {} beside it.", beside.join(" and "))
    })
}

/// A calendar's definition: one the timeline can use, and whose seasons name
/// its own months. A good one is described back.
fn check_calendar(settings: &serde_json::Value) -> ToolAnswer {
    let cal = match Calendar::from_json(settings) {
        Ok(cal) => cal,
        Err(why) => return ToolAnswer::no(why),
    };
    let months: Vec<&str> = cal.year.iter().filter_map(|e| if let Entry::Month { month, .. } = e { Some(month.as_str()) } else { None }).collect();
    for s in &cal.seasons {
        for m in [&s.from, &s.to] {
            if !months.iter().any(|n| n.eq_ignore_ascii_case(m)) {
                return ToolAnswer::no(format!("the season {} names the month {m}, which the year does not have", s.name));
            }
        }
    }
    ToolAnswer::ok(describe(&cal))
}

/// A calendar in a few sentences an agent can check its sums against.
fn describe(cal: &Calendar) -> String {
    let gregorian = Calendar::gregorian();
    let leap_year = (0..400).map(|y| y + 1).find(|y| cal.leap.is_leap(*y));
    let days = cal.year_days(1);
    let mut out = format!("{} reads: {days} days in a year", cal.called());
    if let Some(y) = leap_year {
        let leap_days = cal.year_days(y);
        if leap_days != days {
            out.push_str(&format!(" ({leap_days} in a leap year, such as the year written {}, absolute {y})", cal.year_label(y)));
        }
    }
    let start = cal.year_start(0);
    let day = |at: i64| Moment { at, precision: Precision::Day };
    out.push_str(&format!(
        ". The absolute year 0 is written \u{201c}{}\u{201d} and -1 \u{201c}{}\u{201d}; year 0's first day, {}, is {} in the Gregorian calendar. Today reads as {}.",
        cal.year_label(0),
        cal.year_label(-1),
        cal.write(day(start)),
        gregorian.write(day(start)),
        cal.write(day(Calendar::now()))
    ));
    if cal.eras.iter().all(|e| !e.quiet) {
        out.push_str(" No era is quiet, so every date must name its era.");
    }
    out
}

/// A field a timeline reads, read as it reads it: in its own calendar, then
/// the ones it shows beside it. Fields it does not read pass.
async fn check_field(api: &Pimble, a: &serde_json::Value) -> ToolAnswer {
    let field = text(a, "field");
    let Some(metanode) = addr_of(&text(a, "metanode")) else { return ToolAnswer::no("metanode must be the timeline's pimble: link") };
    let settings = match api.doc(metanode).await {
        Ok(doc) => doc.data_json().get(VIEWER).map(|v| v.to_string()),
        Err(e) => return ToolAnswer::no(format!("the timeline cannot be read: {}", e.sentence())),
    };
    let settings = parse_settings(settings.as_deref());
    let reads = [&settings.fields.start, &settings.fields.date, &settings.fields.end];
    if !reads.iter().any(|f| **f == field) {
        return ToolAnswer::ok(format!("The timeline does not read {field}."));
    }
    let mut cals = Vec::new();
    match calendar_of(api, CalRef::of(settings.calendar)).await {
        Ok(c) => cals.push(c),
        Err(why) => return ToolAnswer::no(format!("the timeline's calendar: {why}")),
    }
    for named in settings.also {
        if let Ok(c) = calendar_of(api, named).await {
            cals.push(c);
        }
    }
    let value = text(a, "value");
    let mut answer = read_in(&cals, &value);
    if !answer.ok || value.is_empty() {
        return answer;
    }
    let fields = &settings.fields;
    // An end said with a year or a month runs to its end, as the line draws it.
    if field == fields.end {
        if let Some((cal, (_, end))) = span_of(&cals, &value) {
            let last = Moment { at: end - cal.seconds_in_day(), precision: Precision::Day };
            answer.said.push_str(&format!(" As an end it runs through {}.", cal.write(last)));
        }
    }
    // A start and an end the wrong way round would draw no bar.
    let other = if field == fields.end { Some(&fields.start) } else if field == fields.start { Some(&fields.end) } else { None };
    if let (Some(other), Some(node)) = (other, addr_of(&text(a, "node"))) {
        let theirs = api.node(node).await.ok().and_then(|n| n.metadata.fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(other)).map(|(_, v)| v.trim().to_string()));
        if let (Some(theirs), Some((_, mine))) = (theirs.filter(|t| !t.is_empty()), span_of(&cals, &value)) {
            if let Some((_, theirs_span)) = span_of(&cals, &theirs) {
                let (start, end) = if field == fields.end { (theirs_span.0, mine.1) } else { (mine.0, theirs_span.1) };
                if end <= start {
                    return ToolAnswer::no(format!(
                        "the span would end before it starts: its {} is \u{201c}{}\u{201d} and its {} \u{201c}{}\u{201d}",
                        fields.start,
                        if field == fields.start { value.as_str() } else { theirs.as_str() },
                        fields.end,
                        if field == fields.end { value.as_str() } else { theirs.as_str() },
                    ));
                }
            }
        }
    }
    answer
}

/// Where `value` begins and ends on the line, in the first of `cals` that
/// reads it, as the line draws it.
fn span_of<'a>(cals: &'a [Calendar], value: &str) -> Option<(&'a Calendar, (i64, i64))> {
    cals.iter().find_map(|cal| {
        let span = match cal.read_said(value)? {
            Said::At { moment, .. } => (moment.at, cal.end_of(moment)),
            Said::Between { from, to, .. } => (from.at, cal.end_of(to)),
            Said::Rough { from, to, .. } => (from, to),
        };
        Some((cal, span))
    })
}

/// `value` read in the first of `cals` that reads it, said back in that
/// calendar and the Gregorian; or why it does not read, with forms that do.
fn read_in(cals: &[Calendar], value: &str) -> ToolAnswer {
    if value.is_empty() {
        return ToolAnswer::ok("An empty field puts nothing on the line.");
    }
    let gregorian = Calendar::gregorian();
    let in_greg = |at: i64| gregorian.write(Moment { at, precision: Precision::Day });
    for cal in cals {
        let Some(said) = cal.read_said(value) else { continue };
        let (reads, from) = match said {
            Said::At { moment, about } => (cal.write_about(moment, about), moment.at),
            Said::Between { from, to, about } => {
                let s = cal.write_between(from, to);
                (if about { format!("about {s}") } else { s }, from.at)
            }
            Said::Rough { from, to, .. } => (format!("roughly {} to {}", cal.write(Moment { at: from, precision: Precision::Day }), cal.write(Moment { at: to, precision: Precision::Day })), from),
        };
        let greg = if cal.name == gregorian.name { String::new() } else { format!(", from {} in the Gregorian calendar", in_greg(from)) };
        return ToolAnswer::ok(format!("\u{201c}{value}\u{201d} reads as {reads} in {}{greg}.", cal.called()));
    }
    let cal = &cals[0];
    let now = Calendar::now();
    let examples = [Precision::Day, Precision::Year].map(|precision| cal.write(Moment { at: now, precision }));
    ToolAnswer::no(format!(
        "\u{201c}{value}\u{201d} does not read as a date in {}. It reads dates such as \u{201c}{}\u{201d} or \u{201c}{}\u{201d}, \u{201c}c. {}\u{201d}, and two in one field joined by \u{201c}-\u{201d} or \u{201c}to\u{201d}.",
        cal.called(),
        examples[0],
        examples[1],
        examples[1]
    ))
}

/// What a person opening the timeline sees, as text: every node on the line in
/// time order, with how its dates read and where they fall in the Gregorian
/// calendar, then the groups, then what is not on the line and why.
async fn outline(api: &Pimble, a: &serde_json::Value) -> ToolAnswer {
    let Some(metanode) = addr_of(&text(a, "timeline")) else { return ToolAnswer::no("timeline must be the timeline's pimble: link") };
    let forest = match api.metanode_source(metanode).await {
        Ok(f) => f,
        Err(e) => return ToolAnswer::no(format!("the timeline cannot be read: {}", e.sentence())),
    };
    let settings = parse_settings(api.doc(metanode).await.ok().and_then(|d| d.data_json().get(VIEWER).map(|v| v.to_string())).as_deref());
    let mut cals = Vec::new();
    match calendar_of(api, CalRef::of(settings.calendar)).await {
        Ok(c) => cals.push(c),
        Err(why) => return ToolAnswer::no(format!("the timeline's calendar: {why}")),
    }
    for named in settings.also {
        if let Ok(c) = calendar_of(api, named).await {
            cals.push(c);
        }
    }
    let gregorian = Calendar::gregorian();
    let day = |at: i64| Moment { at, precision: Precision::Day };
    let f = &settings.fields;
    let get = |node: &pimble_plugin_sdk::pimble_core::Node, name: &str| {
        node.metadata.fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty())
    };
    let mut on_line: Vec<(i64, String)> = Vec::new();
    let mut groups = Vec::new();
    let mut off = Vec::new();
    for entry in &forest.entries {
        let title = pimble_plugin_sdk::display_label(&entry.node);
        let (start, date, end) = (get(&entry.node, &f.start), get(&entry.node, &f.date), get(&entry.node, &f.end));
        let Some(first) = start.clone().or(date.clone()) else {
            if end.is_some() {
                off.push(format!("{title}: an {} with no {} or {}", f.end, f.start, f.date));
            } else {
                groups.push(title);
            }
            continue;
        };
        let Some((cal, (from, mut to))) = span_of(&cals, &first) else {
            off.push(format!("{title}: \u{201c}{first}\u{201d} does not read"));
            continue;
        };
        if let Some(end) = &end {
            match span_of(&cals, end) {
                Some((_, (_, e))) if e > from => to = e,
                Some(_) => off.push(format!("{title}: its {} \u{201c}{end}\u{201d} is before it starts, so it shows as a point", f.end)),
                None => off.push(format!("{title}: its {} \u{201c}{end}\u{201d} does not read, so it shows as a point", f.end)),
            }
        }
        let shape = if end.is_some() && to > from { "bar" } else if to - from > cal.seconds_in_day() { "span" } else { "point" };
        let mut line = if shape == "point" {
            format!("{} (point): {title}", cal.write(day(from)))
        } else {
            format!("{} to {} ({shape}): {title}", cal.write(day(from)), cal.write(day(to - cal.seconds_in_day())))
        };
        if cal.name != gregorian.name {
            line.push_str(&if shape == "point" {
                format!(" [Gregorian {}]", gregorian.write(day(from)))
            } else {
                format!(" [Gregorian {} to {}]", gregorian.write(day(from)), gregorian.write(day(to - 1)))
            });
        }
        on_line.push((from, line));
    }
    on_line.sort_by_key(|(at, _)| *at);
    let mut out = format!("{} on the line, in {}:\n", on_line.len(), cals[0].called());
    for (_, line) in &on_line {
        out.push_str(&format!("- {line}\n"));
    }
    if !groups.is_empty() {
        out.push_str(&format!("Group labels (no dates of their own): {}.\n", groups.join(", ")));
    }
    if !off.is_empty() {
        out.push_str("Not as meant:\n");
        for o in &off {
            out.push_str(&format!("- {o}\n"));
        }
    }
    ToolAnswer::ok(out.trim_end().to_string())
}

