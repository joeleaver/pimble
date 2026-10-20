//! Stored by identity, shown by name: `to_stored` and `to_shown`, round
//! trips, renames, deletions, ambiguity and unknown names.

mod formula_support;

use formula_support::*;
use pimble_sheet::formula::{
    note_token, to_shown, to_stored, Engine, Evaluated, NameErrorKind, Value,
};
use pimble_sheet::{ColumnType, Format};

fn formulas_of(b: &TestBook, note: u128) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();
    for n in &b.notes {
        if n.node != node(note) {
            continue;
        }
        for t in &n.sheets.tables {
            if let Some(src) = &t.source {
                out.push((None, src.clone()));
            }
            for c in &t.columns {
                if let ColumnType::Formula { formula, .. } = &c.ty {
                    out.push((Some(t.id.clone()), formula.clone()));
                }
            }
        }
    }
    out
}

#[test]
fn examples_round_trip() {
    let b = examples();
    for note in [TRIP, BUDGET, TIME] {
        let view = b.at(note);
        for (table, f) in formulas_of(&b, note) {
            let stored =
                to_stored(&view, table.as_deref(), &f).unwrap_or_else(|e| panic!("{f}: {e:?}"));
            let back = to_shown(&view, table.as_deref(), &stored);
            assert_eq!(back, f, "stored {stored}");
            assert_eq!(to_stored(&view, table.as_deref(), &back).unwrap(), stored);
        }
    }
}

#[test]
fn stored_forms() {
    let b = examples();
    let st = |table: Option<&str>, f: &str| {
        to_stored(&b, table, f).unwrap_or_else(|e| panic!("{f}: {e:?}"))
    };
    assert_eq!(
        st(Some("trips"), "=2*[@miles]/[@vehicle].mpg*[@[gas price]]"),
        "=2*[@{c:trips.miles}]/[@{c:trips.vehicle}].{c:veh.mpg}*[@{c:trips.gas_price}]"
    );
    assert_eq!(
        st(Some("veh"), "=COUNTIF(Trips[vehicle], [@name])"),
        "=COUNTIF({t:trips}[{c:trips.vehicle}], [@{c:veh.name}])"
    );
    // This note's own tables carry no note token, however they were named.
    assert_eq!(
        st(None, "='Trip cost'!Vehicles[mpg]"),
        "={t:veh}[{c:veh.mpg}]"
    );
    assert_eq!(
        st(None, "=SUM(Vehicles[[mpg]])"),
        "=SUM({t:veh}[{c:veh.mpg}])"
    );
    // Other notes do.
    let budget = note_token(&node(BUDGET));
    assert_eq!(
        st(None, "=SUM('Household budget'!Categories[spent])"),
        format!("=SUM({budget}!{{t:cats}}[{{c:cats.spent}}])")
    );
    let client = note_token(&node(CLIENT));
    assert_eq!(
        st(None, "='Client website'!rate*2"),
        format!("={client}!rate*2")
    );
    // Multi-part references keep their brackets.
    assert_eq!(
        st(Some("trips"), "=Trips[[#This Row],[gas price]]"),
        "={t:trips}[[#This Row],[{c:trips.gas_price}]]"
    );
    let time = b.at(TIME);
    assert_eq!(
        to_stored(&time, None, "=SUM(Entries[[hours]:[billable]])").unwrap(),
        "=SUM({t:entries}[[{c:entries.hours}]:[{c:entries.billable}]])"
    );
    // A node link's fields and a summary's result columns keep their names.
    assert_eq!(
        to_stored(&time, Some("entries"), "=[@hours]*[@project].rate").unwrap(),
        "=[@{c:entries.hours}]*[@{c:entries.project}].rate"
    );
    let home = b.at(BUDGET);
    assert_eq!(
        to_stored(&home, Some("bymonth"), "=[@category].[monthly budget]").unwrap(),
        "=[@{r:1}].{c:cats.monthly_budget}"
    );
    assert_eq!(
        to_stored(&home, Some("bymonth"), "=[@budget]-[@spent]").unwrap(),
        "=[@{c:bymonth.budget}]-[@{r:2}]"
    );
    // LET names are not tables, and their links are followed.
    assert_eq!(
        st(
            Some("trips"),
            "=LET(v, [@vehicle], miles, [@miles], miles/v.mpg)"
        ),
        "=LET(v, [@{c:trips.vehicle}], miles, [@{c:trips.miles}], miles/v.{c:veh.mpg})"
    );
    // So is a lookup's result.
    assert_eq!(
        st(
            Some("trips"),
            "=XLOOKUP(\"Van\", Vehicles[name], Vehicles).mpg"
        ),
        "=XLOOKUP(\"Van\", {t:veh}[{c:veh.name}], {t:veh}).{c:veh.mpg}"
    );
    // A formula column that is a link is followed too.
    let mut b2 = b.clone();
    b2.note(TRIP).sheets.tables[1] = TB::new("trips", "Trips")
        .link("vehicle", "veh")
        .formula("car", "=[@vehicle]", Format::Auto)
        .build();
    assert_eq!(
        to_stored(&b2, Some("trips"), "=[@car].mpg").unwrap(),
        "=[@{c:trips.car}].{c:veh.mpg}"
    );
    // Brackets that are only optional are normalized.
    assert_eq!(
        to_shown(&b, Some("trips"), &st(Some("trips"), "=[@[miles]]")),
        "=[@miles]"
    );
    // Whitespace and everything else stay as typed.
    assert_eq!(
        st(Some("trips"), "  =  [@miles] *  2 "),
        "  =  [@{c:trips.miles}] *  2 "
    );
}

#[test]
fn renames_show_the_new_names() {
    let mut b = examples();
    b.store_formulas();
    let gas = match &b.note(TRIP).sheets.tables[1].columns[6].ty {
        ColumnType::Formula { formula, .. } => formula.clone(),
        _ => unreachable!(),
    };
    let cross = "=SUM('Household budget'!Categories[spent])+COUNT(Vehicles[mpg])";
    let cross = to_stored(&b, None, cross).unwrap();

    let mut r = b.clone();
    let veh = r.table_mut(TRIP, "veh");
    veh.name = "Cars".into();
    veh.columns[1].name = "miles per gallon".into();
    let trips = r.table_mut(TRIP, "trips");
    trips.columns[3].name = "fuel".into();
    r.note(BUDGET).title = "Budget".into();
    r.table_mut(BUDGET, "cats").columns[2].name = "spent so far".into();

    assert_eq!(
        to_shown(&r, Some("trips"), &gas),
        "=2*[@miles]/[@vehicle].[miles per gallon]*[@fuel]"
    );
    assert_eq!(
        to_shown(&r, None, &cross),
        "=SUM(Budget!Categories[spent so far])+COUNT(Cars[miles per gallon])"
    );
    // The new names are stable, and they compute what the old ones did.
    for (table, f) in [(Some("trips"), &gas), (None, &cross)] {
        let shown = to_shown(&r, table, f);
        assert_eq!(&to_stored(&r, table, &shown).unwrap(), f);
    }
    let en = Engine::new(&r);
    let tables = en.compute_node(&node(TRIP)).unwrap();
    assert_num(
        cell(table(&tables, "Trips"), "chicago", "gas"),
        2.0 * 410.0 / 36.0 * 3.29,
    );

    // Names that are not plain words are quoted or bracketed.
    r.table_mut(TRIP, "veh").name = "My cars".into();
    r.note(BUDGET).title = "Bob's budget".into();
    r.table_mut(TRIP, "trips").columns[1].name = "a[1]".into();
    let all = to_stored(
        &b,
        Some("trips"),
        "=COUNT(Vehicles)+SUM(Vehicles[mpg])+[@miles]",
    )
    .unwrap();
    let shown = to_shown(&r, Some("trips"), &all);
    assert_eq!(
        shown,
        "=COUNT('My cars'[#All])+SUM('My cars'[miles per gallon])+[@[a'[1']]]"
    );
    assert_eq!(to_stored(&r, Some("trips"), &shown).unwrap(), all);
    assert_eq!(
        to_shown(&r, None, &cross),
        "=SUM('Bob''s budget'!Categories[spent so far])+COUNT('My cars'[miles per gallon])"
    );
    // A table named like an aggregate is never mistaken for one.
    r.table_mut(TRIP, "veh").name = "Sum".into();
    let shown = to_shown(&r, None, &to_stored(&b, None, "=ROWS(Vehicles)").unwrap());
    assert_eq!(shown, "=ROWS('Sum'[#All])");
}

#[test]
fn deleted_ids_show_ref() {
    let mut b = examples();
    b.store_formulas();
    let gas = match &b.note(TRIP).sheets.tables[1].columns[6].ty {
        ColumnType::Formula { formula, .. } => formula.clone(),
        _ => unreachable!(),
    };
    let cross = to_stored(&b, None, "=SUM('Household budget'!Categories[spent])").unwrap();

    let mut r = b.clone();
    r.table_mut(TRIP, "veh").columns.remove(1); // mpg
    let shown = to_shown(&r, Some("trips"), &gas);
    assert_eq!(shown, "=2*[@miles]/#REF!*[@[gas price]]");
    // It stays #REF! once committed, and computes as #REF!.
    let stored = to_stored(&r, Some("trips"), &shown).unwrap();
    assert_eq!(to_shown(&r, Some("trips"), &stored), shown);
    let en = Engine::new(&r);
    let v = en
        .evaluate(&node(TRIP), Some("trips"), Some("chicago"), &shown)
        .unwrap();
    let Evaluated::Value(v) = v else { panic!() };
    assert_err(&v, "#REF!");

    let mut r = b.clone();
    r.note(BUDGET).sheets.tables.retain(|t| t.id != "cats");
    assert_eq!(to_shown(&r, None, &cross), "=SUM(#REF!)");
    let mut r = b.clone();
    r.notes.retain(|n| n.node != node(BUDGET));
    assert_eq!(to_shown(&r, None, &cross), "=SUM(#REF!)");

    // A note that can't be read is named, and its ids stay.
    let mut r = b.clone();
    r.note(BUDGET).reach = R::NoAccess;
    let shown = to_shown(&r, None, &cross);
    assert_eq!(shown, "=SUM('Household budget'!{t:cats}[{c:cats.spent}])");
    assert_eq!(to_stored(&r, None, &shown).unwrap(), cross);
    // Typing a name into it can't be checked.
    let e = to_stored(&r, None, "=SUM('Household budget'!Categories[spent])").unwrap_err();
    assert_eq!(e.kind, NameErrorKind::Unreadable);
    assert!(
        e.message.starts_with("You can't read Household budget"),
        "{}",
        e.message
    );
}

#[test]
fn ambiguous_and_unknown_names() {
    let mut b = examples();
    b.add(40, "Home", None);
    b.add(41, "Budget", Some(40)).sheets.tables = vec![TB::new("hb", "T").number("x").build()];
    b.add(42, "Work", None);
    b.add(43, "Budget", Some(42)).sheets.tables = vec![TB::new("wb", "T").number("x").build()];
    b.note(TRIP)
        .sheets
        .tables
        .push(TB::new("d1", "Dup").number("x").build());
    b.note(TRIP)
        .sheets
        .tables
        .push(TB::new("d2", "dup").number("x").build());

    let src = "=SUM(Budget!T[x])";
    let e = to_stored(&b, None, src).unwrap_err();
    assert_eq!(e.kind, NameErrorKind::Ambiguous);
    assert_eq!(&src[e.span.clone()], "Budget!");
    let names: Vec<&str> = e.candidates.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Home/Budget", "Work/Budget"]);
    assert_eq!(e.candidates[0].token, note_token(&node(41)));
    // The path picks one, and is how it shows.
    let stored = to_stored(&b, None, "=SUM('Home/Budget'!T[x])").unwrap();
    assert_eq!(
        stored,
        format!("=SUM({}!{{t:hb}}[{{c:hb.x}}])", note_token(&node(41)))
    );
    assert_eq!(to_shown(&b, None, &stored), "=SUM('Home/Budget'!T[x])");
    // Evaluating the bare name says so too.
    let en = Engine::new(&b);
    let e = en.evaluate(&node(TRIP), None, None, src).unwrap_err();
    assert_eq!(e.kind, NameErrorKind::Ambiguous);

    let e = to_stored(&b, None, "=SUM(Dup[x])").unwrap_err();
    assert_eq!(e.kind, NameErrorKind::Ambiguous);
    assert_eq!(e.candidates.len(), 2);

    let cases: &[(Option<&str>, &str, &str, &str)] = &[
        (Some("trips"), "=[@nope]*2", "nope", "Trips has no column \"nope\"; its columns are destination, miles, vehicle, gas price, nights, hotel rate, gas, cost"),
        (None, "=SUM(Nope[x])", "Nope", "There's no table called Nope in this note; its tables are Vehicles, Trips, Dup, dup"),
        (None, "=SUM('Nowhere'!T[x])", "'Nowhere'!", "No note is called \"Nowhere\""),
        (Some("trips"), "=[@vehicle].price", "price", "Vehicles has no column \"price\"; its columns are name, mpg, trips"),
        (None, "=foo+1", "foo", "Nothing here is called foo; the tables here are Vehicles, Trips, Dup, dup"),
        (None, "=[@miles]", "[@miles]", "A bare [column] only means something in a table's own formulas"),
        (Some("veh"), "=Trips[@miles]", "[@miles]", "[@…] reads this row, and this row isn't in Trips"),
        (None, "='Client website'!price", "price", "Client website has no table or field called price"),
    ];
    for (table, src, at, msg) in cases {
        let e = to_stored(&b, *table, src).unwrap_err();
        assert_eq!(e.kind, NameErrorKind::Unknown, "{src}");
        assert_eq!(&src[e.span.clone()], *at, "{src}");
        assert_eq!(e.message, *msg, "{src}");
    }
    let e = to_stored(&b, None, "=SUM(1,").unwrap_err();
    assert_eq!(e.kind, NameErrorKind::Parse);

    // Functions are not names: an unknown one is #NAME? when computed.
    let st = to_stored(&b, None, "=FOO(1)").unwrap();
    assert_eq!(st, "=FOO(1)");
    let en = Engine::new(&b);
    let Evaluated::Value(v) = en.evaluate(&node(TRIP), None, None, "=FOO(1)").unwrap() else {
        panic!()
    };
    assert_err(&v, "#NAME?");
    assert_eq!(
        v,
        Value::Error(pimble_sheet::formula::FormulaError::new(
            pimble_sheet::formula::ErrorCode::Name,
            "There's no function called FOO() in Pimble yet"
        ))
    );
}

#[test]
fn stability_over_many_forms() {
    let b = examples();
    let forms: &[(Option<&str>, &str)] = &[
        (Some("trips"), "=[@[gas price]]*[@miles]"),
        (Some("trips"), "=Trips[@[gas price]]"),
        (Some("trips"), "=[[#This Row],[miles]]+1"),
        (Some("trips"), "=SUM([miles])"),
        (Some("trips"), "=SUM(Trips[[miles]:[nights]])"),
        (Some("trips"), "=ROWS(Trips[#Data])"),
        (
            Some("trips"),
            "=XLOOKUP([@vehicle], Vehicles[name], Vehicles[mpg])",
        ),
        (None, "=Trip!Vehicles[mpg]"),
        (None, "=COUNTA(Vehicles[#Headers])"),
        (None, "='Household budget'!Transactions[@amount]"),
        (None, "=\"Vehicles[mpg] in a string\"&Vehicles[name]"),
        (None, "=Budget!x"),
    ];
    let mut b2 = b.clone();
    b2.add(50, "Trip", None).sheets.tables = vec![TB::new("tv", "Vehicles").number("mpg").build()];
    b2.add(51, "Budget", None).fields = vec![("x".into(), "1".into())];
    for (table, f) in forms {
        let Ok(stored) = to_stored(&b2, *table, f) else {
            // Some forms are refused by design; that is tested elsewhere.
            continue;
        };
        let shown = to_shown(&b2, *table, &stored);
        assert_eq!(
            to_stored(&b2, *table, &shown).unwrap(),
            stored,
            "{f} -> {shown}"
        );
        assert_eq!(
            to_shown(&b2, *table, &to_stored(&b2, *table, &shown).unwrap()),
            shown
        );
        assert!(
            !stored.contains("Vehicles[mpg]") || f.contains("in a string"),
            "{stored}"
        );
    }
    // Strings are never rewritten.
    let st = to_stored(&b2, None, "=\"Vehicles[mpg] in a string\"&Vehicles[name]").unwrap();
    assert_eq!(st, "=\"Vehicles[mpg] in a string\"&{t:veh}[{c:veh.name}]");
    // Headers keep the reference whole.
    let st = to_stored(&b2, None, "=COUNTA(Vehicles[#Headers])").unwrap();
    assert_eq!(st, "=COUNTA({t:veh}[#Headers])");
    // A this-row reference into another note is refused.
    assert!(to_stored(&b2, None, "='Household budget'!Transactions[@amount]").is_err());
}

#[test]
fn summary_result_columns_are_stored_by_position() {
    let mut b = examples();
    b.store_formulas();
    let home = b.at(BUDGET);
    // Every way a result column is named.
    let forms: &[(Option<&str>, &str, &str)] = &[
        (Some("bymonth"), "=[@spent]*2", "=[@{r:2}]*2"),
        (
            Some("bymonth"),
            "=[@[spent]]+[@month]",
            "=[@{r:2}]+[@{r:0}]",
        ),
        (None, "=SUM(ByMonth[spent])", "=SUM({t:bymonth}[{r:2}])"),
        (
            None,
            "=SUM(ByMonth[[spent]:[budget]])",
            "=SUM({t:bymonth}[[{r:2}]:[{c:bymonth.budget}]])",
        ),
        (
            None,
            "=XLOOKUP(\"2026-10\", ByMonth[month], ByMonth).spent",
            "=XLOOKUP(\"2026-10\", {t:bymonth}[{r:0}], {t:bymonth}).{r:2}",
        ),
    ];
    for (table, shown, stored) in forms {
        let st = to_stored(&home, *table, shown).unwrap_or_else(|e| panic!("{shown}: {e:?}"));
        assert_eq!(&st, stored, "{shown}");
        assert_eq!(
            to_stored(&home, *table, &to_shown(&home, *table, &st)).unwrap(),
            st
        );
    }
    // An unknown result column is refused with the summary's columns.
    let e = to_stored(&home, Some("bymonth"), "=[@price]").unwrap_err();
    assert_eq!(
        e.message,
        "ByMonth has no column \"price\"; its columns are month, category, spent, budget, left"
    );

    // Renaming through headers leaves stored formulas working and shows the new names.
    let total = to_stored(&home, None, "=SUM(ByMonth[spent])").unwrap();
    let mut r = home.clone();
    r.table_mut(BUDGET, "bymonth")
        .headers
        .insert(2, "total".into());
    r.table_mut(BUDGET, "bymonth")
        .headers
        .insert(1, "what".into());
    assert_eq!(to_shown(&r, None, &total), "=SUM(ByMonth[total])");
    let left = match &r.table_mut(BUDGET, "bymonth").columns[1].ty {
        ColumnType::Formula { formula, .. } => formula.clone(),
        _ => unreachable!(),
    };
    assert_eq!(to_shown(&r, Some("bymonth"), &left), "=[@budget]-[@total]");
    let budget = match &r.table_mut(BUDGET, "bymonth").columns[0].ty {
        ColumnType::Formula { formula, .. } => formula.clone(),
        _ => unreachable!(),
    };
    assert_eq!(
        to_shown(&r, Some("bymonth"), &budget),
        "=[@what].[monthly budget]"
    );
    let en = Engine::new(&r);
    let tables = en.compute_node(&node(BUDGET)).unwrap();
    let bym = table(&tables, "ByMonth");
    let names: Vec<&str> = bym.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["month", "what", "total", "budget", "left"]);
    // September groceries: 650 - 319.64.
    assert!(close(num(&bym.rows[2].values[4]), 650.0 - 319.64));
    let Evaluated::Value(v) = en.evaluate(&node(BUDGET), None, None, &total).unwrap() else {
        panic!()
    };
    assert!(close(
        num(&v),
        188.20
            + 61.30
            + 205.90
            + 72.15
            + 131.44
            + 45.00
            + 142.18
            + 46.20
            + 38.75
            + 212.40
            + 96.33
            + 64.00
            + 51.10
            + 27.40
            + 118.62
    ));

    // A position past the result's width is #REF! with a sentence.
    let mut n = home.clone();
    n.table_mut(BUDGET, "bymonth").source = Some(
        to_stored(
            &home,
            None,
            "=GROUPBY(Transactions[category], Transactions[amount], SUM)",
        )
        .unwrap(),
    );
    assert_eq!(to_shown(&n, None, &total), "=SUM(#REF!)");
    let en = Engine::new(&n);
    let tables = en.compute_node(&node(BUDGET)).unwrap();
    let bym = table(&tables, "ByMonth");
    let Evaluated::Value(v) = en
        .evaluate(
            &node(BUDGET),
            Some("bymonth"),
            Some(&bym.rows[0].id),
            "=[@{r:2}]",
        )
        .unwrap()
    else {
        panic!()
    };
    let msg = assert_err(&v, "#REF!");
    assert_eq!(
        msg,
        "This formula reads column 3 of ByMonth's result, and its formula now gives 2"
    );
    let Evaluated::Value(v) = en
        .evaluate(&node(BUDGET), None, None, "=SUM({t:bymonth}[{r:5}])")
        .unwrap()
    else {
        panic!()
    };
    assert_err(&v, "#REF!");
    // A summary that fails keeps its tokens: they come back when it does.
    let mut f = home.clone();
    f.table_mut(BUDGET, "bymonth").source = Some("=1+".into());
    assert_eq!(to_shown(&f, None, &total), "=SUM(ByMonth[{r:2}])");
    assert_eq!(to_stored(&f, None, "=SUM(ByMonth[{r:2}])").unwrap(), total);
}

/// What a table reads of its own note (contract section 1, "Convert to Sheet
/// Node"): a row link's table and the tables its formulas name without a
/// note; another note's tables are not its note's.
#[test]
fn tables_read_names_this_notes_tables_only() {
    let mut b = examples();
    b.store_formulas();
    let trip = b.notes.iter().find(|n| n.node == node(TRIP)).unwrap();
    let trips = trip.sheets.tables.iter().find(|t| t.id == "trips").unwrap();
    let veh = trip.sheets.tables.iter().find(|t| t.id == "veh").unwrap();
    assert!(trips.tables_read().contains("veh"), "a row link and the dot");
    assert!(veh.tables_read().contains("trips"), "COUNTIF(Trips[vehicle], ..)");
    let mut lone = TB::new("lone", "Lone").number("x").build();
    lone.columns.push(pimble_sheet::ColumnSnap {
        id: "other".into(),
        name: "other".into(),
        order: "z".into(),
        ty: ColumnType::Formula { formula: format!("=SUM({}!{{t:cats}}[{{c:cats.spent}}])+SUM({{t:lone}}[{{c:lone.x}}])", note_token(&node(BUDGET))), format: Format::Auto },
    });
    assert!(lone.tables_read().is_empty(), "another note's table, and itself: {:?}", lone.tables_read());
}
