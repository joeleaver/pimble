//! A summary's result columns have keys (contract section 4): what reads or
//! renames one keeps pointing at it when the summary's formula is reshaped,
//! and a note written before keys is moved to them before its first reshape.

mod formula_support;

use formula_support::*;
use pimble_sheet::formula::{rekey_note, to_shown, to_stored, Engine, Rekey, Value};
use pimble_sheet::{ColumnType, Format, ResultKey};

const TRIP: u128 = 1;

/// The trip: an itinerary, expenses linked to its stops, and a summary of
/// spend by stop with an own column reading the stop's day total.
fn trip() -> TestBook {
    let mut b = TestBook::new(TRIP);
    let itinerary = TB::new("itin", "Itinerary")
        .text("stop")
        .number("day total")
        .row("i1", vec![("stop", s("Moab")), ("day total", n(100.0))])
        .row("i2", vec![("stop", s("Zion")), ("day total", n(200.0))])
        .build();
    let expenses = TB::new("exp", "Expenses")
        .link("at", "itin")
        .date("date")
        .money("amount")
        .row("e1", vec![("at", r("i1")), ("date", d("2026-10-01")), ("amount", n(10.0))])
        .row("e2", vec![("at", r("i2")), ("date", d("2026-10-02")), ("amount", n(20.0))])
        .row("e3", vec![("at", r("i1")), ("date", d("2026-10-01")), ("amount", n(5.0))])
        .build();
    let spend = TB::new("spend", "Spend")
        .summary("=GROUPBY(Expenses[at], Expenses[amount], SUM)", &["stop", "extras"])
        .formula("budget", "=[@stop].[day total]", Format::Number)
        .build();
    b.add(TRIP, "Trip", None).sheets.tables = vec![itinerary, expenses, spend];
    b.store_formulas();
    b
}

fn budget_formula(b: &mut TestBook) -> &mut String {
    match &mut b.table_mut(TRIP, "spend").columns[0].ty {
        ColumnType::Formula { formula, .. } => formula,
        _ => unreachable!(),
    }
}

/// What `Rekey::apply` writes, applied to the test book's snapshot.
fn apply(b: &mut TestBook, rk: &Rekey) {
    for (table, source) in &rk.sources {
        b.table_mut(TRIP, table).source = Some(source.clone());
    }
    for (table, column, ty) in &rk.formulas {
        let t = b.table_mut(TRIP, table);
        t.columns.iter_mut().find(|c| &c.id == column).unwrap().ty = ty.clone();
    }
    for (table, old, new, header) in &rk.headers {
        let t = b.table_mut(TRIP, table);
        t.headers.remove(old);
        t.headers.insert(new.clone(), header.clone());
    }
}

#[derive(Debug, PartialEq)]
struct Computed {
    names: Vec<String>,
    rows: Vec<Vec<Value>>,
}

fn spend(b: &TestBook) -> Computed {
    let en = Engine::new(b);
    let tables = en.compute_node(&node(TRIP)).unwrap();
    let t = table(&tables, "Spend");
    Computed {
        names: t.columns.iter().map(|c| c.name.clone()).collect(),
        rows: t.rows.iter().map(|r| r.values.clone()).collect(),
    }
}

#[test]
fn new_formulas_name_result_columns_by_key() {
    let mut b = trip();
    assert_eq!(budget_formula(&mut b).as_str(), "=[@{rk:at}].{c:itin.day_total}");
}

/// The Spreadsheet session's repro: an own column reading `[@stop]`, then the
/// summary reshaped to put a date column first. Before keys the own column
/// was retargeted to the date and showed #VALUE!.
#[test]
fn a_reshape_keeps_own_columns_and_renames_on_their_columns() {
    let mut b = trip();
    // Written before keys: the own column by position, renames by position.
    *budget_formula(&mut b) = "=[@{r:0}].{c:itin.day_total}".into();
    let before = spend(&b);
    assert_eq!(before.names, ["stop", "extras", "budget"]);

    let rk = rekey_note(&b);
    assert_eq!(rk.formulas.len(), 1);
    assert_eq!(
        rk.headers,
        vec![
            ("spend".into(), ResultKey::Pos(0), ResultKey::Key("at".into()), "stop".into()),
            ("spend".into(), ResultKey::Pos(1), ResultKey::Key("amount".into()), "extras".into()),
        ]
    );
    apply(&mut b, &rk);
    assert_eq!(budget_formula(&mut b).as_str(), "=[@{rk:at}].{c:itin.day_total}");
    // Nothing reads differently yet.
    assert_eq!(spend(&b), before);
    // Nothing left to move.
    assert!(rekey_note(&b).is_empty());

    // The reshape, with the names date, stop, extras by the new keys.
    let source = to_stored(
        &b,
        None,
        "=GROUPBY(HSTACK(Expenses[date], Expenses[at]), Expenses[amount], SUM)",
    )
    .unwrap();
    let t = b.table_mut(TRIP, "spend");
    t.source = Some(source);
    t.headers.insert(ResultKey::Key("date".into()), "date".into());
    let after = spend(&b);
    assert_eq!(after.names, ["date", "stop", "extras", "budget"]);
    // Moab's day total, then Zion's: the own column still reads the stop.
    assert_eq!(after.rows.len(), 2);
    assert!(close(num(&after.rows[0][3]), 100.0));
    assert!(close(num(&after.rows[1][3]), 200.0));
    assert!(close(num(&after.rows[0][2]), 15.0));
    let shown = {
        let f = budget_formula(&mut b).clone();
        to_shown(&b, Some("spend"), &f)
    };
    assert_eq!(shown, "=[@stop].[day total]");
}

/// Without the move to keys, a reshape still moves a position: what this
/// change exists to prevent.
#[test]
fn positions_from_before_keys_still_read_until_moved() {
    let mut b = trip();
    *budget_formula(&mut b) = "=[@{r:0}].{c:itin.day_total}".into();
    let s = spend(&b);
    assert!(close(num(&s.rows[0][2]), 100.0));
    assert_eq!(s.names, ["stop", "extras", "budget"]);
    // The same reshape with no move first retargets it to the date.
    let source = to_stored(
        &b,
        None,
        "=GROUPBY(HSTACK(Expenses[date], Expenses[at]), Expenses[amount], SUM)",
    )
    .unwrap();
    b.table_mut(TRIP, "spend").source = Some(source);
    let s = spend(&b);
    assert_eq!(s.names, ["stop", "extras", "amount", "budget"]);
    assert!(matches!(s.rows[0][3], Value::Error(_)));
}

#[test]
fn a_header_by_key_wins_over_one_by_position() {
    let mut b = trip();
    let t = b.table_mut(TRIP, "spend");
    t.headers.insert(ResultKey::Key("at".into()), "place".into());
    assert_eq!(spend(&b).names, ["place", "extras", "budget"]);
}
