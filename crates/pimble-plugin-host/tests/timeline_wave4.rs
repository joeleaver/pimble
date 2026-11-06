//! The timeline's wave 4 (docs/TIMELINE_CONTRACT.md section 9), end to end:
//! the built `timeline_viewer` over a real in-process server. Another
//! calendar shown under the timeline's own gets an axis of its own, and a
//! date that does not read in the timeline's calendar reads in it; rough and
//! "about" dates are drawn fading; who else is on a node shows on its bar,
//! and a click on them follows; and the calendar form
//! writes how its dates are written and its seasons into a calendar made
//! before it had either.
#![cfg(feature = "wasm-host")]

mod common;

use common::{built_plugin, fixture, mount, Fixture, Running};
use pimble_api::Addr;
use pimble_core::NodeId;
use pimble_plugin_abi::{HostRequest, HostState, NodeAddr, NodePeople, PresentPerson, Props};
use pimble_plugin_host::{Approval, Approved};
use rinch_core::dom::{DomDocument, NodeId as DomId};
use rinch_core::events::{dispatch_event, EventHandlerId};
use serde_json::json;

fn has_class(d: &rinch_core::dom::mock::MockDomDocument, n: DomId, class: &str) -> bool {
    d.get_attribute(n, "class").is_some_and(|c| c.split_whitespace().any(|w| w == class))
}

fn data(f: &Fixture, node: NodeId, key: &str) -> serde_json::Value {
    f.rt.block_on(f.app.doc(Addr::new(f.store, node))).unwrap().data_json().get(key).cloned().unwrap_or(serde_json::Value::Null)
}

fn push_settings(r: &Running, f: &Fixture, viewer: &str) {
    let settings = data(f, f.board, viewer);
    let props = Props { metanode: NodeAddr::from(Addr::new(f.store, f.board)), settings: (!settings.is_null()).then(|| settings.to_string()), remembered: None };
    r.instance.props_changed(&props);
}

fn set(f: &Fixture, node: NodeId, name: &'static str, value: &'static str) {
    f.rt.block_on(f.app.edit(Addr::new(f.store, node), move |doc, now| -> Result<(), pimble_api::ApiError> {
        doc.set_field(name, Some(value), now)?;
        Ok(())
    }))
    .unwrap();
}

fn style_of(r: &Running, class: &str, label: &str) -> String {
    let n = r
        .find(&|d, n| has_class(d, n, class) && d.text_content(n).unwrap_or_default().contains(label))
        .unwrap_or_else(|| panic!("no {class} {label:?} in: {}", r.text()));
    r.doc.borrow().get_attribute(n, "style").unwrap_or_default()
}

/// A calendar node "Kepler" made before calendars had seasons or a way of
/// writing: years of one 412-day month, from the founding.
fn kepler(f: &Fixture) -> NodeId {
    let store = f.store;
    f.rt.block_on(async {
        let c = f.service.create_node(store, None, "document", "Kepler").await.unwrap();
        f.app.set_metanode(Addr::new(store, c), Some("calendar"), None).await.unwrap();
        let def = json!({
            "name": "Kepler",
            "day": { "hours": 26, "minutes": 60, "seconds": 60 },
            "year": [{ "month": "Day", "days": 412 }],
            "epoch": 63_000_000_000_i64,
            "eras": [{ "name": "After Founding", "short": "AF", "from": 0, "counts": "up" }]
        });
        f.app.edit(Addr::new(store, c), move |doc, _| -> Result<(), pimble_api::ApiError> { doc.set_data("calendar", &def)?; Ok(()) }).await.unwrap();
        c
    })
}

#[test]
fn another_calendar_lines_up_under_the_timelines_own_and_rough_dates_fade() {
    if built_plugin("timeline_viewer").is_none() {
        return;
    }
    let f = fixture("timeline");
    let store = f.store;
    let calendar = kepler(&f);
    let link = pimble_core::PimbleUrl::node(store, calendar).to_string();
    f.rt.block_on(f.app.edit(Addr::new(store, f.board), move |doc, _| -> Result<(), pimble_api::ApiError> {
        doc.set_data("timeline", &json!({ "also": [link] }))?;
        Ok(())
    }))
    .unwrap();
    // Plan is in the colony's calendar, Write it roughly in ours.
    set(&f, f.plan, "date", "Day 214, 3088 AF");
    set(&f, f.write, "date", "c. 2400");
    let running = mount(&f, Approval { stores: Approved::All, write: true }, "timeline_viewer", "timeline");
    let r = &running;
    push_settings(r, &f, "timeline");

    // Kepler's own axis, named, under the Gregorian one.
    r.pump_until(&f, "Kepler's axis", |r| r.find(&|d, n| has_class(d, n, "timeline__also-name") && d.text_content(n).as_deref() == Some("Kepler")).is_some());
    // Plan reads in Kepler (not in ours) and says where that is in ours.
    let plan_title = |r: &Running| {
        r.find(&|d, n| has_class(d, n, "timeline__item") && d.text_content(n).unwrap_or_default().contains("Plan")).and_then(|n| r.doc.borrow().get_attribute(n, "title"))
    };
    r.pump_until(&f, "Plan in Kepler", |r| plan_title(r).is_some_and(|t| t.contains("Day 214, 3088 AF, which is") && t.ends_with("in the Gregorian calendar")));
    // About 2400: a soft bar, fading at both ends.
    r.pump_until(&f, "the soft bar", |r| r.text().contains("Write it"));
    let soft = style_of(r, "timeline__item", "Write it");
    assert!(soft.contains("linear-gradient(to right, transparent 0%"), "{soft}");
    assert!(soft.contains("dashed"), "{soft}");

    // A rough date is drawn as typed (a drag of it is refused: the unit test).
    set(&f, f.write, "date", "early 1400s");
    r.pump_until(&f, "the rough date", |r| {
        r.find(&|d, n| has_class(d, n, "timeline__item") && d.get_attribute(n, "title").is_some_and(|t| t == "Write it: early 1400s")).is_some()
    });

    // Not shown any more: its axis goes, and Plan no longer reads.
    f.rt.block_on(f.app.edit(Addr::new(store, f.board), |doc, _| -> Result<(), pimble_api::ApiError> {
        doc.set_data("timeline", &json!({}))?;
        Ok(())
    }))
    .unwrap();
    push_settings(r, &f, "timeline");
    r.pump_until(&f, "Plan unread", |r| r.text().contains("\u{201c}Day 214, 3088 AF\u{201d} is not a date in the Gregorian calendar"));
    assert!(r.find(&|d, n| has_class(d, n, "timeline__also-axis")).is_none());

    // Who else is on Write it: their chip on its bar; a click on it follows.
    let sam = PresentPerson { key: "sub-sam".into(), name: "Sam".into(), initials: "S".into(), color: "#aa3300".into(), title: "Sam, on laptop".into(), agent: false };
    let on_write = NodeAddr::from(Addr::new(store, f.write));
    assert!(r.instance.set_host_state(&HostState { dark: false, people: vec![NodePeople { node: on_write, people: vec![sam] }], following: Some("sub-sam".into()) }));
    r.pump_until(&f, "Sam's chip", |r| r.find(&|d, n| has_class(d, n, "pimble-pane__person")).is_some());
    let chip = r.find(&|d, n| has_class(d, n, "pimble-pane__person")).unwrap();
    assert!(dispatch_event(EventHandlerId(r.handler(chip, "data-rid"))));
    r.pump_until(&f, "the follow", |r| r.requests.borrow().contains(&HostRequest::Follow { person: "sub-sam".into() }));
    r.instance.unmount();
}

#[test]
fn the_form_writes_how_dates_are_written_and_seasons_into_an_older_calendar() {
    if built_plugin("timeline_viewer").is_none() {
        return;
    }
    // The board is a calendar node made before writing styles and seasons.
    let f = fixture("calendar");
    let def = json!({
        "name": "Shire",
        "day": { "hours": 24, "minutes": 60, "seconds": 60 },
        "week": ["Sterday", "Sunday", "Monday", "Trewsday", "Hevensday", "Mersday", "Highday"],
        "year": [{ "month": "Afteryule", "short": "Aft", "days": 30 }, { "month": "Solmath", "short": "Sol", "days": 30 }],
        "epoch": 0,
        "eras": [{ "name": "Shire Reckoning", "short": "S.R.", "from": 0, "counts": "up" }]
    });
    f.rt.block_on(f.app.edit(Addr::new(f.store, f.board), move |doc, _| -> Result<(), pimble_api::ApiError> { doc.set_data("calendar", &def)?; Ok(()) })).unwrap();
    let running = mount(&f, Approval { stores: Approved::All, write: true }, "timeline_viewer", "calendar");
    let r = &running;
    push_settings(r, &f, "calendar");
    r.pump_until(&f, "the form", |r| r.text().contains("Writing a date"));
    // Once it knows this person may change it, its inputs take edits.
    r.pump_until(&f, "writable", |r| {
        r.find(&|d, n| has_class(d, n, "calendar__input") && d.get_attribute(n, "disabled").map_or(true, |v| v == "false")).is_some()
    });

    // The week's first day: a chip per name.
    r.pump_until(&f, "the week's first day", |r| r.text().contains("The first day of 1 S.R. is a"));
    let mersday = r.find(&|d, n| has_class(d, n, "calendar__week-starts") && d.text_content(n).as_deref() == Some("Mersday")).unwrap();
    assert!(dispatch_event(EventHandlerId(r.handler(mersday, "data-rid"))));
    r.pump_until(&f, "week_starts written", |_| data(&f, f.board, "calendar")["week_starts"] == 5);
    push_settings(r, &f, "calendar");

    // Day first and with the day of the week: `writes` made on the way.
    let day_first = r.find(&|d, n| has_class(d, n, "calendar__day-first") && !d.text_content(n).unwrap_or_default().contains(',')).expect("the day-first choice");
    assert!(dispatch_event(EventHandlerId(r.handler(day_first, "data-rid"))));
    r.pump_until(&f, "day first", |_| data(&f, f.board, "calendar")["writes"]["day_first"] == true);
    push_settings(r, &f, "calendar");
    r.click("span", "without the day of the week");
    r.pump_until(&f, "with the weekday", |_| data(&f, f.board, "calendar")["writes"]["weekday"] == true);
    let cal = data(&f, f.board, "calendar");
    assert_eq!(cal["year"][1]["month"], "Solmath", "the rest is as it was");
    push_settings(r, &f, "calendar");
    // The sample writes today that way: a day name first, the day before the month.
    r.pump_until(&f, "the sample", |r| {
        let t = r.text();
        ["Sterday, ", "Sunday, ", "Monday, ", "Trewsday, ", "Hevensday, ", "Mersday, ", "Highday, "].iter().any(|w| t.contains(&format!("Today reads {w}")))
    });

    // A season: the list made on the way, starting at the first month.
    r.click("span", "+ Season");
    r.pump_until(&f, "the season", |_| data(&f, f.board, "calendar")["seasons"][0]["from"] == "Afteryule");
    push_settings(r, &f, "calendar");
    r.pump_until(&f, "its row", |r| r.find(&|d, n| has_class(d, n, "calendar__season")).is_some());
    let to = r.find(&|d, n| d.tag_name(n).as_deref() == Some("input") && d.get_attribute(n, "placeholder").as_deref() == Some("May")).unwrap();
    assert!(rinch_core::events::dispatch_input_event(EventHandlerId(r.handler(to, "data-onchange")), "Halimath".into()));
    r.pump_until(&f, "the season's end", |_| data(&f, f.board, "calendar")["seasons"][0]["to"] == "Halimath");
    push_settings(r, &f, "calendar");
    r.pump_until(&f, "the problem said", |r| r.text().contains("names \u{201c}Halimath\u{201d}, which is not a month of this year"));
    r.instance.unmount();
}
