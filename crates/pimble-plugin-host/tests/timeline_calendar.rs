//! Calendars of one's own (docs/TIMELINE_CONTRACT.md wave 2), end to end: the
//! built `timeline_viewer` over a real in-process server. A calendar node is
//! made up in the calendar viewer's form (started from a template, a month
//! renamed, an era's way of writing changed, a date tried in it), each input
//! writing only its own place in the node's data; a timeline set to read in
//! it places a node by a date in that calendar, says when it is the way the
//! calendar writes it, and follows the calendar's edits.
#![cfg(feature = "wasm-host")]

mod common;

use common::{built_plugin, fixture, mount, Fixture, Running};
use pimble_api::Addr;
use pimble_core::NodeId;
use pimble_plugin_abi::{NodeAddr, Props};
use pimble_plugin_host::{Approval, Approved};
use rinch_core::dom::{DomDocument, NodeId as DomId};
use rinch_core::events::{dispatch_input_event, EventHandlerId};
use serde_json::json;

fn has_class(d: &rinch_core::dom::mock::MockDomDocument, n: DomId, class: &str) -> bool {
    d.get_attribute(n, "class").is_some_and(|c| c.split_whitespace().any(|w| w == class))
}

/// The board's data under `key`, as the server holds it.
fn data(f: &Fixture, node: NodeId, key: &str) -> serde_json::Value {
    f.rt.block_on(f.app.doc(Addr::new(f.store, node))).unwrap().data_json().get(key).cloned().unwrap_or(serde_json::Value::Null)
}

/// Hand the plugin the board's settings for `viewer` again, as the app does
/// when they change.
fn push_settings(r: &Running, f: &Fixture, viewer: &str) {
    let settings = data(f, f.board, viewer);
    let props = Props { metanode: NodeAddr::from(Addr::new(f.store, f.board)), settings: (!settings.is_null()).then(|| settings.to_string()), remembered: None };
    r.instance.props_changed(&props);
}

/// Commit `value` into the input `which` finds, as Enter or leaving it does.
fn change(r: &Running, which: &dyn Fn(&rinch_core::dom::mock::MockDomDocument, DomId) -> bool, value: &str) {
    let node = r.find(which).unwrap_or_else(|| panic!("no such input in: {}", r.text()));
    let handler = r.handler(node, "data-onchange");
    assert!(dispatch_input_event(EventHandlerId(handler), value.into()));
}

fn input_valued(value: &'static str) -> impl Fn(&rinch_core::dom::mock::MockDomDocument, DomId) -> bool {
    move |d, n| d.tag_name(n).as_deref() == Some("input") && d.get_attribute(n, "value").as_deref() == Some(value)
}

#[test]
fn a_calendar_is_made_up_in_its_form_and_a_timeline_reads_in_it() {
    if built_plugin("timeline_viewer").is_none() {
        return;
    }
    // The board is a calendar node.
    let f = fixture("calendar");
    let running = mount(&f, Approval { stores: Approved::All, write: true }, "timeline_viewer", "calendar");
    let r = &running;
    r.pump_until(&f, "the starters", |r| r.text().contains("Start an invented calendar"));

    // Started from the invented template: the whole definition written once.
    r.click("span", "Start an invented calendar");
    r.pump_until(&f, "the template written", |_| data(&f, f.board, "calendar").is_object());
    push_settings(r, &f, "calendar");
    r.pump_until(&f, "the form", |r| r.text().contains("Today reads"));
    assert!(r.find(&input_valued("First Month")).is_some(), "{}", r.text());

    // A month renamed and an era's short name changed: each one place.
    change(r, &input_valued("First Month"), "Afteryule");
    r.pump_until(&f, "the month renamed", |_| data(&f, f.board, "calendar")["year"][0]["month"] == "Afteryule");
    change(r, &input_valued("YF"), "AF");
    r.pump_until(&f, "the era written", |_| data(&f, f.board, "calendar")["eras"][0]["short"] == "AF");
    let cal = data(&f, f.board, "calendar");
    assert_eq!(cal["year"][1]["month"], "Second Month", "the rest is as it was");
    assert_eq!(cal["year"].as_array().unwrap().len(), 12);
    push_settings(r, &f, "calendar");

    // Not a number: said, and nothing written.
    let days = r.find(&|d, n| has_class(d, n, "calendar__number") && d.get_attribute(n, "value").as_deref() == Some("30")).unwrap();
    assert!(dispatch_input_event(EventHandlerId(r.handler(days, "data-onchange")), "lots".into()));
    r.pump_until(&f, "the refusal", |r| r.text().contains("\u{201c}lots\u{201d} is not a whole number."));

    // A date tried in it reads, and says where it falls in the Gregorian calendar.
    let tried = r.find(&|d, n| has_class(d, n, "calendar__try")).unwrap();
    assert!(dispatch_input_event(EventHandlerId(r.handler(tried, "data-oninput")), "Afteryule 3 412 AF".into()));
    r.pump_until(&f, "the sample", |r| r.text().contains("reads as Fir 3, 412 AF, which is"));
    r.instance.unmount();
}

#[test]
fn a_timeline_reads_and_writes_in_the_calendar_its_settings_name() {
    if built_plugin("timeline_viewer").is_none() {
        return;
    }
    let f = fixture("timeline");
    let store = f.store;
    // A calendar node beside the board: years of one 412-day month, from the founding.
    let calendar = f.rt.block_on(async {
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
    });
    let link = pimble_core::PimbleUrl::node(store, calendar).to_string();
    f.rt.block_on(f.app.edit(Addr::new(store, f.board), move |doc, _| -> Result<(), pimble_api::ApiError> {
        doc.set_data("timeline", &json!({ "calendar": link, "start": "when" }))?;
        Ok(())
    }))
    .unwrap();
    // Plan is dated by the timeline's own field name, in the calendar's words.
    f.rt.block_on(f.app.edit(Addr::new(store, f.plan), |doc, now| -> Result<(), pimble_api::ApiError> {
        doc.set_field("when", Some("Day 214, 3088 AF"), now)?;
        Ok(())
    }))
    .unwrap();
    let running = mount(&f, Approval { stores: Approved::All, write: true }, "timeline_viewer", "timeline");
    let r = &running;
    push_settings(r, &f, "timeline");
    let plan_title = |r: &Running| {
        r.find(&|d, n| has_class(d, n, "timeline__item") && d.text_content(n).unwrap_or_default().contains("Plan"))
            .and_then(|n| r.doc.borrow().get_attribute(n, "title"))
    };
    r.pump_until(&f, "Plan in the Kepler calendar", |r| plan_title(r).is_some_and(|t| t.contains("Day 214, 3088 AF")));

    // The calendar's era renamed: the timeline follows it.
    f.rt.block_on(f.app.edit(Addr::new(store, calendar), |doc, _| -> Result<(), pimble_api::ApiError> {
        doc.edit_data(
            &[pimble_crdt::DataStep::Key("calendar"), pimble_crdt::DataStep::Key("eras"), pimble_crdt::DataStep::Index(0), pimble_crdt::DataStep::Key("also")],
            pimble_crdt::DataEdit::Set(&json!(["A.F."])),
        )?;
        doc.edit_data(
            &[pimble_crdt::DataStep::Key("calendar"), pimble_crdt::DataStep::Key("eras"), pimble_crdt::DataStep::Index(0), pimble_crdt::DataStep::Key("short")],
            pimble_crdt::DataEdit::Set(&json!("PF")),
        )?;
        Ok(())
    }))
    .unwrap();
    // Plan's "AF" still reads (as "A.F.", another way of writing the era) and
    // is written the era's new way.
    r.pump_until(&f, "the calendar's edit followed", |r| plan_title(r).is_some_and(|t| t.contains("Day 214, 3088 PF")));

    // With no other way of writing it, "AF" is not a date there: below the line.
    f.rt.block_on(f.app.edit(Addr::new(store, calendar), |doc, _| -> Result<(), pimble_api::ApiError> {
        doc.edit_data(
            &[pimble_crdt::DataStep::Key("calendar"), pimble_crdt::DataStep::Key("eras"), pimble_crdt::DataStep::Index(0), pimble_crdt::DataStep::Key("also")],
            pimble_crdt::DataEdit::Remove,
        )?;
        Ok(())
    }))
    .unwrap();
    r.pump_until(&f, "Plan unread", |r| r.text().contains("\u{201c}Day 214, 3088 AF\u{201d} is not a date in the Kepler calendar"));

    // A calendar that cannot be used yet says why, and the Gregorian is used meanwhile.
    f.rt.block_on(f.app.edit(Addr::new(store, calendar), |doc, _| -> Result<(), pimble_api::ApiError> {
        doc.edit_data(
            &[pimble_crdt::DataStep::Key("calendar"), pimble_crdt::DataStep::Key("day"), pimble_crdt::DataStep::Key("hours")],
            pimble_crdt::DataEdit::Set(&json!(0)),
        )?;
        Ok(())
    }))
    .unwrap();
    r.pump_until(&f, "the note", |r| r.text().contains("\u{201c}Kepler\u{201d} can't be used yet: A day needs between 1 and 10,000 hours."));
    r.instance.unmount();
}
