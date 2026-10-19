//! Joe's three examples computed end to end, cross-note references and
//! their states, cycles and the error cases.

mod formula_support;

use formula_support::*;
use pimble_sheet::formula::{Engine, ErrorCode, Evaluated, Value};
use pimble_sheet::{Book, ColumnType, Format};

fn both_forms() -> Vec<(&'static str, TestBook)> {
    let shown = examples();
    let mut stored = examples();
    stored.store_formulas();
    vec![("shown", shown), ("stored", stored)]
}

#[test]
fn trip_cost() {
    for (form, b) in both_forms() {
        let en = Engine::new(&b);
        let tables = en.compute_node(&node(TRIP)).unwrap();
        let trips = table(&tables, "Trips");
        let gas = |row: &str| num(cell(trips, row, "gas"));
        let cost = |row: &str| num(cell(trips, row, "cost"));
        assert!(close(gas("chicago"), 2.0 * 410.0 / 36.0 * 3.29), "{form}");
        assert!(
            close(cost("chicago"), 2.0 * 410.0 / 36.0 * 3.29 + 129.0),
            "{form}"
        );
        assert!(close(gas("tahoe"), 2.0 * 620.0 / 22.0 * 3.89), "{form}");
        assert!(
            close(cost("tahoe"), 2.0 * 620.0 / 22.0 * 3.89 + 3.0 * 189.0),
            "{form}"
        );
        assert!(close(cost("grandma"), 2.0 * 140.0 / 17.0 * 3.45), "{form}");
        // As the cells show them.
        assert_eq!(
            shown(&en, cell(trips, "chicago", "gas"), Format::Money),
            "$74.94"
        );
        assert_eq!(
            shown(&en, cell(trips, "chicago", "cost"), Format::Money),
            "$203.94"
        );
        assert_eq!(
            shown(&en, cell(trips, "tahoe", "cost"), Format::Money),
            "$786.25"
        );
        assert_eq!(
            shown(&en, cell(trips, "grandma", "cost"), Format::Money),
            "$56.82"
        );
        // A link shows its row's label.
        assert_eq!(
            shown(&en, cell(trips, "tahoe", "vehicle"), Format::Auto),
            "Van"
        );

        let vehicles = table(&tables, "Vehicles");
        for v in ["truck", "civic", "van"] {
            assert_num(cell(vehicles, v, "trips"), 1.0);
        }
    }
}

#[test]
fn household_budget() {
    for (form, b) in both_forms() {
        let en = Engine::new(&b);
        let tables = en.compute_node(&node(BUDGET)).unwrap();
        let cats = table(&tables, "Categories");
        let expect = [
            ("groceries", 357.13, 292.87),
            ("gas", 97.30, 122.70),
            ("eating", 66.15, 113.85),
            ("utilities", 212.40, 97.60),
            ("kids", 64.00, 86.00),
        ];
        for (row, spent, left) in expect {
            assert!(close(num(cell(cats, row, "spent")), spent), "{form} {row}");
            assert!(close(num(cell(cats, row, "left")), left), "{form} {row}");
        }
        assert_eq!(
            shown(&en, cell(cats, "groceries", "left"), Format::Money),
            "$292.87"
        );

        let bym = table(&tables, "ByMonth");
        assert!(bym.error.is_none(), "{:?}", bym.error);
        let names: Vec<&str> = bym.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["month", "category", "spent", "budget", "left"]);
        assert_eq!(bym.columns[2].format, Format::Money);
        assert!(bym.columns[0].id.is_none() && bym.columns[3].id.is_some());
        let expect = [
            ("2026-09", "Eating out", 72.15, 180.0),
            ("2026-09", "Gas", 61.30, 220.0),
            ("2026-09", "Groceries", 319.64, 650.0),
            ("2026-09", "Kids", 45.00, 150.0),
            ("2026-09", "Utilities", 205.90, 310.0),
            ("2026-10", "Eating out", 66.15, 180.0),
            ("2026-10", "Gas", 97.30, 220.0),
            ("2026-10", "Groceries", 357.13, 650.0),
            ("2026-10", "Kids", 64.00, 150.0),
            ("2026-10", "Utilities", 212.40, 310.0),
        ];
        assert_eq!(bym.rows.len(), expect.len(), "{form}");
        for (row, (month, cat, spent, budget)) in bym.rows.iter().zip(expect) {
            assert_eq!(row.values[0], Value::Text(month.into()));
            assert_eq!(en.format_value(&row.values[1], Format::Auto), cat);
            assert!(
                matches!(row.values[1], Value::Row(_)),
                "the category stays a link"
            );
            assert!(close(num(&row.values[2]), spent), "{form} {month} {cat}");
            assert!(close(num(&row.values[3]), budget), "{form} {month} {cat}");
            assert!(
                close(num(&row.values[4]), budget - spent),
                "{form} {month} {cat}"
            );
        }
    }
}

#[test]
fn time_tracker() {
    for (form, b) in both_forms() {
        let b = b.at(TIME);
        let en = Engine::new(&b);
        let tables = en.compute_node(&node(TIME)).unwrap();
        let entries = table(&tables, "Entries");
        assert_num(cell(entries, "e2", "billable"), 285.0);
        assert_num(cell(entries, "e3", "billable"), 142.5);
        assert_num(cell(entries, "e4", "billable"), 0.0);
        assert_eq!(
            shown(&en, cell(entries, "e3", "hours"), Format::Hours),
            "1h 30m"
        );
        assert_eq!(
            shown(&en, cell(entries, "e8", "hours"), Format::Hours),
            "1h 45m"
        );
        assert_eq!(
            shown(&en, cell(entries, "e7", "date"), Format::Date),
            "Mon, Oct 19"
        );
        assert_eq!(
            shown(&en, cell(entries, "e7", "project"), Format::Auto),
            "Client website"
        );

        let week = table(&tables, "LastWeek");
        assert!(week.error.is_none(), "{form}: {:?}", week.error);
        let names: Vec<&str> = week.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["project", "hours", "billed"]);
        assert_eq!(week.columns[1].format, Format::Hours);
        assert_eq!(week.columns[2].format, Format::Money);
        let rows: Vec<(String, f64, f64)> = week
            .rows
            .iter()
            .map(|r| {
                (
                    en.format_value(&r.values[0], Format::Auto),
                    num(&r.values[1]),
                    num(&r.values[2]),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                ("Client website".to_string(), 8.0, 760.0),
                ("Garden shed".to_string(), 3.0, 0.0),
                ("Pimble".to_string(), 3.75, 0.0),
            ],
            "{form}"
        );

        let rates = table(&tables, "Rates");
        assert_num(cell(rates, "one", "value"), 190.0);
        assert_eq!(
            cell(rates, "one", "client"),
            &Value::Text("Lakeview Dental".into())
        );
    }
}

#[test]
fn one_off_evaluation() {
    let mut b = examples();
    b.store_formulas();
    let b = b.at(BUDGET);
    let en = Engine::new(&b);
    let home = node(BUDGET);
    // "What did we spend on gas in September?"
    let v = en
        .evaluate(
            &home,
            None,
            None,
            "=SUMIFS(Transactions[amount], Transactions[category], \"Gas\", Transactions[date], \">=\"&DATE(2026,9,1), Transactions[date], \"<\"&DATE(2026,10,1))",
        )
        .unwrap();
    let Evaluated::Value(v) = v else { panic!() };
    assert_num(&v, 61.30);
    // In a row's context.
    let v = en
        .evaluate(&home, Some("cats"), Some("kids"), "=[@[monthly budget]]*12")
        .unwrap();
    assert_eq!(v, Evaluated::Value(Value::Number(1800.0)));
    // A list answers as a table.
    let v = en
        .evaluate(
            &home,
            None,
            None,
            "=FILTER(Transactions[[date]:[amount]], Transactions[amount]>200)",
        )
        .unwrap();
    let Evaluated::Table(t) = v else {
        panic!("{v:?}")
    };
    assert_eq!(t.columns.len(), 2);
    assert_eq!(t.rows.len(), 2);
    assert_eq!(t.columns[1].name, "amount");
    // Names that do not resolve are refused with a sentence.
    let e = en
        .evaluate(&home, None, None, "=SUM(Transactions[price])")
        .unwrap_err();
    assert!(
        e.message.contains("Transactions has no column \"price\""),
        "{}",
        e.message
    );
    assert!(e.message.contains("amount"), "{}", e.message);
    let e = en
        .evaluate(&home, None, None, "=SUM(Expenses[amount])")
        .unwrap_err();
    assert!(
        e.message.contains("no table called Expenses"),
        "{}",
        e.message
    );
}

#[test]
fn cross_note_references() {
    let mut b = examples();
    let trip = b.note(TRIP);
    trip.sheets.tables.push(
        TB::new("x", "Cross")
            .text("what")
            .formula(
                "spent",
                "=SUM('Household budget'!Categories[spent])",
                Format::Money,
            )
            .formula("rate", "='Client website'!rate", Format::Money)
            .formula("count", "=ROWS('Time tracker'!LastWeek)", Format::Number)
            .row("r", vec![("what", s("all"))])
            .build(),
    );
    b.store_formulas();
    // The stored form names the other notes by id.
    let stored = match &b.note(TRIP).sheets.tables[2].columns[1].ty {
        ColumnType::Formula { formula, .. } => formula.clone(),
        _ => unreachable!(),
    };
    assert!(stored.starts_with("=SUM({n:"), "{stored}");
    assert!(!stored.contains("Household"), "{stored}");

    let en = Engine::new(&b);
    let tables = en.compute_node(&node(TRIP)).unwrap();
    let cross = table(&tables, "Cross");
    assert_num(
        cell(cross, "r", "spent"),
        357.13 + 97.30 + 66.15 + 212.40 + 64.00,
    );
    assert_num(cell(cross, "r", "rate"), 95.0);
    assert_num(cell(cross, "r", "count"), 3.0);

    // The referenced note's states.
    for (state, code, sentence) in [
        (R::Deleted, "#REF!", "Household budget was deleted"),
        (R::NoAccess, "#REF!", "You can't read Household budget"),
        (
            R::Loading,
            "Loading…",
            "Waiting for Household budget to arrive",
        ),
    ] {
        let mut b2 = b.clone();
        b2.note(BUDGET).reach = state;
        let en = Engine::new(&b2);
        let tables = en.compute_node(&node(TRIP)).unwrap();
        let v = cell(table(&tables, "Cross"), "r", "spent");
        let msg = assert_err(v, code);
        // The deleted note's title is still known; an unreadable one is
        // named by what the book can show.
        if state == R::Deleted {
            assert_eq!(msg, sentence);
        } else {
            assert!(
                msg.contains("Household budget") || msg.contains("That note"),
                "{msg}"
            );
        }
        assert_eq!(en.format_value(v, Format::Money), code);
    }
    // Loading is not an answer: IFERROR cannot turn it into one.
    let mut b3 = b.clone();
    let cross = b3.table_mut(TRIP, "x");
    let stored_spent = match &cross.columns[1].ty {
        ColumnType::Formula { formula, .. } => formula.clone(),
        _ => unreachable!(),
    };
    cross.columns[1].ty = ColumnType::Formula {
        formula: format!("=IFERROR({}, 0)", stored_spent.trim_start_matches('=')),
        format: Format::Money,
    };
    b3.note(BUDGET).reach = R::Loading;
    let en = Engine::new(&b3);
    let tables = en.compute_node(&node(TRIP)).unwrap();
    assert_err(cell(table(&tables, "Cross"), "r", "spent"), "Loading…");
    // A missing note is #REF!, which IFERROR may catch.
    b3.notes.retain(|n| n.node != node(BUDGET));
    let en = Engine::new(&b3);
    let tables = en.compute_node(&node(TRIP)).unwrap();
    assert_num(cell(table(&tables, "Cross"), "r", "spent"), 0.0);
    let mut b5 = b.clone();
    b5.notes.retain(|n| n.node != node(BUDGET));
    let en = Engine::new(&b5);
    let tables = en.compute_node(&node(TRIP)).unwrap();
    let msg = assert_err(cell(table(&tables, "Cross"), "r", "spent"), "#REF!");
    assert_eq!(msg, "The note this points at is gone");
    // Computing a note that cannot be read says why.
    let mut b4 = b.clone();
    b4.note(TIME).reach = R::NoAccess;
    let en = Engine::new(&b4);
    assert_eq!(
        en.compute_node(&node(TIME)).unwrap_err().code,
        ErrorCode::Ref
    );
}

#[test]
fn cycles() {
    let mut b = TestBook::new(1);
    b.add(1, "Loops", None).sheets.tables = vec![
        TB::new("t", "T")
            .text("name")
            .formula("a", "=[@b]+1", Format::Auto)
            .formula("b", "=[@a]+1", Format::Auto)
            .formula("c", "=SUM([c])", Format::Auto)
            .formula("d", "=IFERROR([@e], 0)", Format::Auto)
            .formula("e", "=[@d]", Format::Auto)
            .formula("f", "=[@name]&\"!\"", Format::Auto)
            .formula("g", "=SUM([a])", Format::Auto)
            .formula("h", "=COUNT([a])", Format::Auto)
            .row("r1", vec![("name", s("one"))])
            .row("r2", vec![("name", s("two"))])
            .build(),
        TB::new("s1", "S1").summary("=SORT(S2[x])", &["x"]).build(),
        TB::new("s2", "S2").summary("=SORT(S1[x])", &["x"]).build(),
    ];
    for store in [false, true] {
        let mut b = b.clone();
        if store {
            b.store_formulas();
        }
        let en = Engine::new(&b);
        let tables = en.compute_node(&node(1)).unwrap();
        let t = table(&tables, "T");
        for row in ["r1", "r2"] {
            for col in ["a", "b", "c", "d", "e"] {
                let msg = assert_err(cell(t, row, col), "#CIRC!");
                assert!(msg.contains("depends on itself"), "{msg}");
            }
            assert_eq!(
                cell(t, row, "f"),
                &Value::Text(format!("{}!", if row == "r1" { "one" } else { "two" }))
            );
            // Reading a column with a cycle in it gets its errors, as any
            // error, but is not itself on the cycle.
            assert_err(cell(t, row, "g"), "#CIRC!");
            assert_num(cell(t, row, "h"), 0.0);
        }
        let s1 = table(&tables, "S1");
        assert_eq!(s1.error.as_ref().map(|e| e.code), Some(ErrorCode::Circ));
        let s2 = table(&tables, "S2");
        assert_eq!(s2.error.as_ref().map(|e| e.code), Some(ErrorCode::Circ));
    }
}

#[test]
fn cell_errors() {
    let mut b = examples();
    b.note(TRIP).sheets.tables.push(
        TB::new("e", "Errors")
            .text("name")
            .link("vehicle", "veh")
            .number("count")
            .formula("unknown", "=FOO(1)", Format::Auto)
            .formula("later", "=NETWORKDAYS(1,2)", Format::Auto)
            .formula("broken", "=1+", Format::Auto)
            .formula("list", "=Trips[miles]", Format::Auto)
            .formula(
                "one",
                "=FILTER(Trips[miles], Trips[destination]=\"Chicago\")",
                Format::Auto,
            )
            .formula("div", "=1/[@count]", Format::Auto)
            .formula("mpg", "=[@vehicle].mpg", Format::Auto)
            .formula("nofield", "=[@vehicle].price", Format::Auto)
            .formula("notlink", "=[@name].mpg", Format::Auto)
            .formula("sum", "=SUM", Format::Auto)
            .row(
                "bus",
                vec![
                    ("name", s("a")),
                    ("vehicle", u("Bus")),
                    ("count", u("lots")),
                ],
            )
            .row("blank", vec![("name", s("b"))])
            .row(
                "ok",
                vec![("name", s("c")), ("vehicle", r("civic")), ("count", n(4.0))],
            )
            .build(),
    );
    let en = Engine::new(&b);
    let tables = en.compute_node(&node(TRIP)).unwrap();
    let t = table(&tables, "Errors");
    let msg = assert_err(cell(t, "bus", "unknown"), "#NAME?");
    assert_eq!(msg, "There's no function called FOO() in Pimble yet");
    let msg = assert_err(cell(t, "bus", "later"), "#NAME?");
    assert!(msg.starts_with("There's no function called NETWORKDAYS() in Pimble yet; the date functions are TODAY, DATE"), "{msg}");
    assert_err(cell(t, "bus", "broken"), "#ERROR!");
    let msg = assert_err(cell(t, "bus", "list"), "#SPILL!");
    assert!(msg.contains("a cell holds one value"), "{msg}");
    assert_num(cell(t, "bus", "one"), 410.0);
    let msg = assert_err(cell(t, "bus", "vehicle"), "#N/A");
    assert_eq!(msg, "Vehicles has no row named \"Bus\"");
    assert_err(cell(t, "bus", "count"), "#VALUE!");
    assert_err(cell(t, "bus", "div"), "#VALUE!");
    assert_err(cell(t, "blank", "div"), "#DIV/0!");
    assert_err(cell(t, "bus", "mpg"), "#N/A");
    assert_eq!(cell(t, "blank", "mpg"), &Value::Blank);
    assert_err(cell(t, "bus", "nofield"), "#N/A");
    assert_err(cell(t, "bus", "notlink"), "#VALUE!");
    assert_num(cell(t, "ok", "mpg"), 36.0);
    assert_num(cell(t, "ok", "div"), 0.25);
    let msg = assert_err(cell(t, "ok", "nofield"), "#REF!");
    assert_eq!(
        msg,
        "Vehicles has no column \"price\"; its columns are name, mpg, trips"
    );
    assert_err(cell(t, "bus", "sum"), "#VALUE!");

    // A link to a row that was deleted.
    let mut b2 = b.clone();
    b2.table_mut(TRIP, "veh").rows.retain(|r| r.id != "van");
    let en = Engine::new(&b2);
    let tables = en.compute_node(&node(TRIP)).unwrap();
    let trips = table(&tables, "Trips");
    assert_err(cell(trips, "tahoe", "vehicle"), "#REF!");
    assert_err(cell(trips, "tahoe", "cost"), "#REF!");
    assert_num(
        cell(trips, "chicago", "cost"),
        2.0 * 410.0 / 36.0 * 3.29 + 129.0,
    );
}

#[test]
fn row_labels_and_formats() {
    let b = examples();
    let en = Engine::new(&b);
    let tables = en.compute_node(&node(BUDGET)).unwrap();
    let tx = table(&tables, "Transactions");
    let first = &tx.rows[0];
    assert_eq!(
        en.format_value(&first.values[0], Format::Date),
        "Fri, Sep 4"
    );
    assert_eq!(en.format_value(&first.values[1], Format::Money), "$188.20");
    assert_eq!(en.format_value(&first.values[2], Format::Auto), "Groceries");
    if let Value::Row(r) = &first.values[2] {
        assert_eq!(en.row_label(r), "Groceries");
    }
    // A Transactions row's label is its date, shown as a date.
    let row = pimble_sheet::formula::RowRef {
        node: node(BUDGET),
        table: "tx".into(),
        row: "t00".into(),
    };
    assert_eq!(en.row_label(&row), "Fri, Sep 4");
    assert_eq!(
        en.format_value(&Value::Number(46_000.0), Format::Date),
        "Tue, Dec 9, 2025"
    );
    assert_eq!(
        en.format_value(&Value::Number(0.256), Format::Percent),
        "25.6%"
    );
    assert_eq!(
        en.format_value(&Value::Number(1234.5), Format::Number),
        "1,234.5"
    );
    assert_eq!(en.format_value(&Value::Bool(true), Format::Auto), "TRUE");
    assert_eq!(en.format_value(&Value::Blank, Format::Money), "");
    assert_eq!(b.today(), TODAY);
}
