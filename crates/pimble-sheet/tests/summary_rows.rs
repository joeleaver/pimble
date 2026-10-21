//! Summary rows and groups on a plain table (docs/SHEETS_CONTRACT.md
//! section 4b): the menu's aggregates, a formula over its scope, subtotals
//! per group, and other formulas reading a summary row Excel's way.

mod formula_support;

use formula_support::*;
use pimble_sheet::formula::{self, Engine, ErrorCode, Value};
use pimble_sheet::{Format, NodeRef};

const NOTE: u128 = 1;

fn spending(summary: bool) -> TestBook {
    let mut b = TestBook::new(NOTE);
    let mut t = TB::new("ex", "Expenses")
        .text("category")
        .money("amount")
        .number("nights")
        .formula("share", "[@amount]/Expenses[[#Totals],[amount]]", Format::Percent)
        .row("a", vec![("category", s("Hotel")), ("amount", n(300.0)), ("nights", n(2.0))])
        .row("b", vec![("category", s("Food")), ("amount", n(60.0))])
        .row("c", vec![("category", s("Hotel")), ("amount", n(150.0)), ("nights", n(1.0))])
        .row("d", vec![("amount", n(90.0))]);
    if summary {
        t = t
            .summary_row(
                "tot",
                "Total",
                &[
                    (&cid("ex", "amount"), "sum"),
                    (&cid("ex", "nights"), "sum"),
                    (&cid("ex", "category"), "count"),
                ],
            )
            .summary_row("avg", "Average", &[(&cid("ex", "amount"), "average")])
            .summary_row("pn", "Per night", &[(&cid("ex", "amount"), "=SUM([amount])/SUM([nights])")]);
    }
    b.add(NOTE, "Trip", None).sheets.tables.push(t.build());
    b
}

fn both() -> Vec<(&'static str, TestBook)> {
    let shown = spending(true);
    let mut stored = spending(true);
    stored.store_formulas();
    vec![("shown", shown), ("stored", stored)]
}

fn here() -> NodeRef {
    node(NOTE)
}

fn summary<'a>(t: &'a formula::ComputedTable, name: &str) -> &'a formula::ComputedSummaryRow {
    t.summary_rows.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no summary row {name}"))
}

fn col(t: &formula::ComputedTable, name: &str) -> usize {
    t.column_index(name).unwrap()
}

#[test]
fn summary_rows_sum_up_the_whole_table() {
    for (form, b) in both() {
        let en = Engine::new(&b);
        let t = en.compute_table(&here(), "ex").unwrap();
        let total = summary(&t, "Total");
        assert_eq!(total.values[col(&t, "amount")], Value::Number(600.0), "{form}");
        assert_eq!(total.values[col(&t, "nights")], Value::Number(3.0), "{form}");
        assert_eq!(total.values[col(&t, "category")], Value::Number(3.0), "{form}: count skips the blank");
        assert_eq!(total.values[col(&t, "share")], Value::Blank, "{form}: a column with no choice shows nothing");
        assert_eq!(total.formats[col(&t, "amount")], Format::Money, "a sum keeps the column's format");
        assert_eq!(total.formats[col(&t, "category")], Format::Number, "a count is a plain number");
        assert_eq!(summary(&t, "Average").values[col(&t, "amount")], Value::Number(150.0), "{form}");
        assert_eq!(summary(&t, "Per night").values[col(&t, "amount")], Value::Number(200.0), "{form}: a formula over the column");
        // Summary rows are not rows.
        assert_eq!(t.rows.len(), 4);
        assert!(t.groups.is_empty());
    }
}

#[test]
fn other_formulas_read_a_summary_row() {
    for (form, b) in both() {
        let en = Engine::new(&b);
        let t = en.compute_table(&here(), "ex").unwrap();
        assert_eq!(t.rows[0].values[col(&t, "share")], Value::Number(0.5), "{form}: [[#Totals],[amount]] is the first summary row");
        let average = match en.evaluate(&here(), None, None, "Expenses[[#Average],[amount]]").unwrap() {
            formula::Evaluated::Value(v) => v,
            other => panic!("{other:?}"),
        };
        assert_eq!(average, Value::Number(150.0), "{form}: a summary row by name");
        let row = match en.evaluate(&here(), None, None, "Expenses[[#Totals],[amount]:[nights]]").unwrap() {
            formula::Evaluated::Table(t) => t.rows,
            other => panic!("{other:?}"),
        };
        assert_eq!(row, vec![vec![Value::Number(600.0), Value::Number(3.0)]], "{form}: two columns of it");
    }
}

#[test]
fn a_summary_row_is_stored_by_id_and_shown_by_name() {
    let b = spending(true);
    let stored = formula::to_stored(&b, None, "Expenses[[#Per night],[amount]]").unwrap();
    assert!(stored.contains("#{s:pn}"), "{stored}");
    assert_eq!(formula::to_shown(&b, None, &stored), "Expenses[[#Per night],[amount]]");
    // `#Totals` stays as Excel writes it.
    let totals = formula::to_stored(&b, None, "Expenses[[#Totals],[amount]]").unwrap();
    assert!(totals.contains("#Totals"), "{totals}");
    // A renamed row is shown by its new name; a deleted one is #REF!.
    let mut renamed = spending(true);
    renamed.table_mut(NOTE, "ex").summary_rows.iter_mut().find(|s| s.id == "pn").unwrap().name = "Nightly".into();
    assert_eq!(formula::to_shown(&renamed, None, &stored), "Expenses[[#Nightly],[amount]]");
    let mut gone = spending(true);
    gone.table_mut(NOTE, "ex").summary_rows.retain(|s| s.id != "pn");
    assert_eq!(formula::to_shown(&gone, None, &stored), "#REF!");
    // A name it does not have is refused, with the ones it has.
    let refused = formula::to_stored(&b, None, "Expenses[[#Median],[amount]]").unwrap_err();
    assert!(refused.message.contains("its summary rows are Total, Average, Per night"), "{}", refused.message);
}

#[test]
fn a_table_without_summary_rows_has_no_totals() {
    let b = spending(false);
    let en = Engine::new(&b);
    let t = en.compute_table(&here(), "ex").unwrap();
    match &t.rows[0].values[col(&t, "share")] {
        Value::Error(e) => {
            assert_eq!(e.code, ErrorCode::Ref);
            assert!(e.message.contains("Expenses has no summary rows"), "{}", e.message);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn groups_have_subtotals_and_the_bottom_rows_sum_everything() {
    let mut b = spending(true);
    b.table_mut(NOTE, "ex").group_by = Some(cid("ex", "category"));
    for (form, b) in [("shown", b.clone()), ("stored", {
        let mut s = b.clone();
        s.store_formulas();
        s
    })] {
        let en = Engine::new(&b);
        let t = en.compute_table(&here(), "ex").unwrap();
        assert_eq!(t.group_by, Some(col(&t, "category")));
        let labels: Vec<&str> = t.groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["Food", "Hotel", ""], "{form}: ascending, blanks last");
        let hotel = &t.groups[1];
        assert_eq!(hotel.rows, ["a", "c"], "{form}: in their own order");
        let sub = |g: &formula::ComputedGroup, name: &str| g.subtotals.iter().find(|s| s.name == name).unwrap().values[col(&t, "amount")].clone();
        assert_eq!(sub(hotel, "Total"), Value::Number(450.0), "{form}");
        assert_eq!(sub(hotel, "Average"), Value::Number(225.0), "{form}");
        assert_eq!(sub(hotel, "Per night"), Value::Number(150.0), "{form}: the formula reads the group");
        assert_eq!(sub(&t.groups[0], "Total"), Value::Number(60.0), "{form}");
        assert_eq!(summary(&t, "Total").values[col(&t, "amount")], Value::Number(600.0), "{form}: the bottom rows sum every row");
        // `[[#Totals],…]` is the bottom row, never a subtotal.
        assert_eq!(t.rows[1].values[col(&t, "share")], Value::Number(0.1), "{form}");
    }
}

#[test]
fn a_summary_row_that_reads_itself_is_circ() {
    let mut b = spending(true);
    let t = b.table_mut(NOTE, "ex");
    t.summary_rows[0].cells.insert(cid("ex", "nights"), pimble_sheet::SummaryCell::Formula("Expenses[[#Totals],[nights]]+1".into()));
    let en = Engine::new(&b);
    let t = en.compute_table(&here(), "ex").unwrap();
    match &summary(&t, "Total").values[col(&t, "nights")] {
        Value::Error(e) => assert_eq!(e.code, ErrorCode::Circ),
        other => panic!("{other:?}"),
    }
    // The rest still computes.
    assert_eq!(summary(&t, "Total").values[col(&t, "amount")], Value::Number(600.0));
}
