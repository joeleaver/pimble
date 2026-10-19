//! The sheet tools' operations over in-memory node documents: values as
//! typed, links by label, formula checks, names, rendering, and that a cell
//! edit is one cell's entry (it merges with a concurrent edit of another).

use chrono::NaiveDate;
use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_sheet::doc::Sheets;
use pimble_sheet::{NodeRef, SheetsSnap, Stored};

use super::*;
use crate::sheets::book::McpBook;

fn r(n: u128) -> NodeRef {
    NodeRef { store: StoreId(uuid::Uuid::from_u128(5)), node: NodeId(uuid::Uuid::from_u128(n)) }
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()
}

fn col(name: &str, ty: &str) -> ColumnSpec {
    ColumnSpec { name: name.into(), ty: Some(ty.into()), ..Default::default() }
}

fn link(name: &str, target: &str) -> ColumnSpec {
    ColumnSpec { name: name.into(), ty: Some("link".into()), target: Some(target.into()), ..Default::default() }
}

fn formula_col(name: &str, f: &str, format: &str) -> ColumnSpec {
    ColumnSpec { name: name.into(), formula: Some(f.into()), format: Some(format.into()), ..Default::default() }
}

fn row(pairs: &[(&str, &str)]) -> RowValues {
    pairs.iter().map(|(k, v)| (k.to_string(), serde_json::json!(v))).collect()
}

struct Note {
    doc: NodeDoc,
    book: McpBook,
}

impl Note {
    fn new() -> Self {
        Note { doc: NodeDoc::new(), book: McpBook::new(r(1), SheetsSnap::default(), today()) }
    }

    fn work<R>(&mut self, f: impl FnOnce(&mut Work) -> R) -> R {
        let mut w = Work::new(Sheets::of_node(&self.doc), &mut self.book);
        f(&mut w)
    }

    fn snap(&self) -> SheetsSnap {
        Sheets::of_node(&self.doc).read()
    }

    fn render(&mut self, table: &str) -> String {
        self.work(|w| {
            let id = find_table(w.snap(), table).unwrap().id.clone();
            render_table(w, &id, &Filter { equals: Vec::new(), limit: 100 }).unwrap()
        })
    }
}

/// Joe's trip cost calculator (docs/SHEETS_CONTRACT.md).
fn trips() -> Note {
    let mut n = Note::new();
    n.work(|w| {
        create_table(
            w,
            "Vehicles",
            &[col("name", "text"), col("mpg", "number")],
            &[row(&[("name", "Civic"), ("mpg", "36")]), row(&[("name", "Van"), ("mpg", "22")])],
            false,
            None,
        )
        .unwrap();
        create_table(
            w,
            "Trips",
            &[
                col("name", "text"),
                col("miles", "number"),
                link("vehicle", "Vehicles"),
                col("gas price", "money"),
                col("nights", "number"),
                col("hotel rate", "money"),
                formula_col("gas", "=2*[@miles]/[@vehicle].mpg*[@[gas price]]", "money"),
                formula_col("cost", "=[@gas]+[@nights]*[@[hotel rate]]", "money"),
            ],
            &[
                row(&[("name", "Chicago"), ("miles", "410"), ("vehicle", "civic"), ("gas price", "$3.29"), ("nights", "1"), ("hotel rate", "$129")]),
                row(&[("name", "Tahoe"), ("miles", "620"), ("vehicle", "Van"), ("gas price", "3.89"), ("nights", "3"), ("hotel rate", "189")]),
            ],
            false,
            None,
        )
        .unwrap();
    });
    n
}

#[test]
fn the_trip_calculator_computes_as_the_app_does() {
    let mut n = trips();
    let out = n.render("Trips");
    assert!(out.contains("| Chicago | 410 | Civic | $3.29 | 1 | $129.00 | $74.94 | $203.94 |"), "{out}");
    assert!(out.contains("$786.25"), "{out}");
    assert!(out.contains("- vehicle: link to a row of Vehicles"), "{out}");
    assert!(out.contains("- cost: formula =[@gas]+[@nights]*[@[hotel rate]], format money"), "{out}");
    assert!(out.contains("row id"), "{out}");
    // Formulas are stored by id, not by name.
    let snap = n.snap();
    let trips = snap.tables.iter().find(|t| t.name == "Trips").unwrap();
    let ColumnType::Formula { formula, .. } = &trips.columns.iter().find(|c| c.name == "cost").unwrap().ty else { panic!() };
    assert!(formula.contains("{c:"), "{formula}");
    assert!(!formula.contains("gas"), "{formula}");
}

#[test]
fn values_go_in_as_typed() {
    let mut n = Note::new();
    n.work(|w| {
        create_table(
            w,
            "Log",
            &[col("what", "text"), col("when", "date"), col("cost", "money"), col("time", "hours")],
            &[row(&[("what", "lunch"), ("when", "yesterday"), ("cost", "$12.50"), ("time", "1h 30m")])],
            false,
            None,
        )
        .unwrap();
    });
    let snap = n.snap();
    let t = &snap.tables[0];
    let cells = &t.rows[0].cells;
    let by = |name: &str| cells.get(&t.columns.iter().find(|c| c.name == name).unwrap().id).cloned();
    assert_eq!(by("when"), Some(Stored::Date("2026-10-18".into())));
    assert_eq!(by("cost"), Some(Stored::Number(12.5)));
    assert_eq!(by("time"), Some(Stored::Number(1.5)));
    let out = n.render("Log");
    assert!(out.contains("| lunch | 2026-10-18 | $12.50 | 1h 30m |"), "{out}");
}

#[test]
fn values_that_do_not_parse_are_refused_unless_kept() {
    let mut n = trips();
    let e = n.work(|w| add_rows(w, "Trips", &[row(&[("name", "Bus trip"), ("vehicle", "Bus")])], false, None)).unwrap_err();
    assert!(e.contains("No row of Vehicles is called \"Bus\""), "{e}");
    assert!(e.contains("\"Civic\", \"Van\""), "{e}");
    assert!(e.contains("keep_as_typed"), "{e}");
    let e = n.work(|w| add_rows(w, "Trips", &[row(&[("miles", "far")])], false, None)).unwrap_err();
    assert!(e.contains("\"far\" is not a number, so it can't go in miles"), "{e}");
    let e = n.work(|w| add_rows(w, "Trips", &[row(&[("cost", "12")])], false, None)).unwrap_err();
    assert!(e.contains("cost is a formula column"), "{e}");
    let e = n.work(|w| add_rows(w, "Trips", &[row(&[("price", "12")])], false, None)).unwrap_err();
    assert!(e.contains("Trips has no column \"price\""), "{e}");
    // Kept as typed, it shows its error and what was typed.
    n.work(|w| add_rows(w, "Trips", &[row(&[("name", "Bus trip"), ("vehicle", "Bus")])], true, None)).unwrap();
    let out = n.render("Trips");
    assert!(out.contains("(typed \"Bus\")"), "{out}");
}

#[test]
fn rows_are_named_by_first_column_or_id() {
    let mut n = trips();
    let changed = n
        .work(|w| update_rows(w, "trips", &[RowUpdate { row: "chicago".into(), values: row(&[("nights", "2"), ("miles", "410")]) }], false))
        .unwrap();
    // miles was already 410: only nights is written.
    assert_eq!(changed, 1);
    assert!(n.render("Trips").contains("$332.94"));
    let id = n.snap().tables[1].rows[1].id.clone();
    n.work(|w| update_rows(w, "Trips", &[RowUpdate { row: id.clone(), values: row(&[("name", "Lake Tahoe")]) }], false)).unwrap();
    assert!(n.render("Trips").contains("| Lake Tahoe |"));
    // Two rows with one name: refused with their ids.
    n.work(|w| add_rows(w, "Trips", &[row(&[("name", "Dup")]), row(&[("name", "dup")])], false, None)).unwrap();
    let e = n.work(|w| delete_rows(w, "Trips", &["Dup".into()])).unwrap_err();
    assert!(e.contains("2 rows of Trips are called \"Dup\"; name one by id"), "{e}");
    let e = n.work(|w| delete_rows(w, "Trips", &["Nowhere".into()])).unwrap_err();
    assert!(e.contains("Trips has no row \"Nowhere\""), "{e}");
    assert_eq!(n.work(|w| delete_rows(w, "Trips", &["Lake Tahoe".into(), "Chicago".into()])).unwrap(), 2);
    assert_eq!(n.snap().tables[1].rows.len(), 2);
}

#[test]
fn formulas_are_checked_before_they_are_kept() {
    let mut n = trips();
    // A name that does not resolve.
    let e = n.work(|w| add_column(w, "Trips", &formula_col("double", "=[@price]*2", "auto"))).unwrap_err();
    assert!(e.contains("has no column \"price\""), "{e}");
    assert!(e.contains("miles") && !e.contains("double"), "{e}");
    // A list where one value goes.
    let e = n.work(|w| add_column(w, "Trips", &formula_col("all", "=Trips[miles]", "auto"))).unwrap_err();
    assert!(e.contains("#SPILL!"), "{e}");
    assert!(e.contains("set_summary"), "{e}");
    // A column that reads itself.
    let e = n.work(|w| update_column(w, "Trips", "cost", None, &ColumnSpec { formula: Some("=[@cost]+1".into()), ..Default::default() })).unwrap_err();
    assert!(e.contains("#CIRC!"), "{e}");
    let e = n.work(|w| add_column(w, "Trips", &formula_col("again", "=[@again]+1", "auto"))).unwrap_err();
    assert!(e.contains("#CIRC!"), "{e}");
    // A function Pimble does not have.
    let e = n.work(|w| add_column(w, "Trips", &formula_col("d", "=NETWORKDAYS(1,2)", "auto"))).unwrap_err();
    assert!(e.contains("#NAME?"), "{e}");
    // Not a formula at all.
    let e = n.work(|w| add_column(w, "Trips", &formula_col("p", "=(1+", "auto"))).unwrap_err();
    assert!(e.contains("sheet_formula_guide"), "{e}");
    // An accepted one answers its first values.
    let mut n = trips();
    let out = n.work(|w| add_column(w, "Trips", &formula_col("round trip", "=[@miles]*2", "number"))).unwrap();
    assert!(out.contains("- Chicago: 820"), "{out}");
    assert!(out.contains("- Tahoe: 1,240"), "{out}");
}

#[test]
fn a_type_change_carries_values_over_as_typed() {
    let mut n = Note::new();
    n.work(|w| {
        create_table(w, "T", &[col("name", "text"), col("n", "text")], &[row(&[("name", "a"), ("n", "12")]), row(&[("name", "b"), ("n", "x")])], false, None)
            .unwrap();
        update_column(w, "T", "n", Some("count"), &ColumnSpec { ty: Some("number".into()), ..Default::default() }).unwrap();
    });
    let snap = n.snap();
    let t = &snap.tables[0];
    let c = t.columns.iter().find(|c| c.name == "count").unwrap();
    assert_eq!(c.ty, ColumnType::Number);
    assert_eq!(t.rows[0].cells.get(&c.id), Some(&Stored::Number(12.0)));
    assert_eq!(t.rows[1].cells.get(&c.id), Some(&Stored::Unparsed("x".into())));
}

#[test]
fn summaries_and_their_column_names() {
    let mut n = trips();
    let out = n
        .work(|w| set_summary(w, "ByVehicle", "=GROUPBY(Trips[vehicle], Trips[miles], SUM)", Some(&["vehicle".to_string(), "miles".to_string()])))
        .unwrap();
    assert!(out.contains("Made the summary ByVehicle"), "{out}");
    assert!(out.contains("| vehicle | miles |"), "{out}");
    assert!(out.contains("| Civic | 410 |"), "{out}");
    assert!(out.contains("summary of: =GROUPBY(Trips[vehicle], Trips[miles], SUM)"), "{out}");
    let e = n.work(|w| set_summary(w, "ByVehicle", "=GROUPBY(Trips[car], Trips[miles], SUM)", None)).unwrap_err();
    assert!(e.contains("no column \"car\""), "{e}");
    let e = n.work(|w| set_summary(w, "Trips", "=FILTER(Trips, Trips[miles]>1)", None)).unwrap_err();
    assert!(e.contains("Trips is a plain table"), "{e}");
    let e = n.work(|w| add_rows(w, "ByVehicle", &[row(&[("vehicle", "x")])], false, None)).unwrap_err();
    assert!(e.contains("is a summary"), "{e}");
    let listed = n.work(|w| {
        let t = find_table(w.snap(), "ByVehicle").unwrap().clone();
        table_summary(w.book, w.snap(), &t)
    });
    assert_eq!(listed, "ByVehicle (summary, 2 rows): vehicle (from the formula), miles (from the formula)");
}

#[test]
fn evaluate_answers_without_writing() {
    let mut n = trips();
    let before = n.snap();
    let v = n.work(|w| evaluate(w, None, None, "=SUM(Trips[miles])")).unwrap();
    assert_eq!(v, "1,030 (1030)");
    let v = n.work(|w| evaluate(w, Some("Trips"), Some("Tahoe"), "=[@miles]/[@vehicle].mpg")).unwrap();
    assert!(v.starts_with("28.18"), "{v}");
    let v = n.work(|w| evaluate(w, None, None, "=FILTER(Trips[[name]:[miles]], Trips[miles]>500)")).unwrap();
    assert!(v.contains("| name | miles |") && v.contains("| Tahoe | 620 |"), "{v}");
    let v = n.work(|w| evaluate(w, None, None, "=1/0")).unwrap();
    assert!(v.starts_with("#DIV/0!"), "{v}");
    let e = n.work(|w| evaluate(w, None, None, "=SUM(Expenses[amount])")).unwrap_err();
    assert!(e.contains("Expenses"), "{e}");
    assert_eq!(n.snap(), before);
}

#[test]
fn an_ambiguous_note_is_refused_with_its_paths() {
    let mut n = trips();
    n.book.add_node(r(10), "Family", None);
    n.book.add_node(r(11), "Home", Some(r(10)));
    n.book.add_node(r(12), "Work", Some(r(10)));
    n.book.add_node(r(13), "Budget", Some(r(11)));
    n.book.add_node(r(14), "Budget", Some(r(12)));
    n.book.add_sheets(r(13), SheetsSnap::default());
    n.book.add_sheets(r(14), SheetsSnap::default());
    let e = n.work(|w| evaluate(w, None, None, "=Budget!rate")).unwrap_err();
    assert!(e.contains("'Home/Budget'!") && e.contains("'Work/Budget'!"), "{e}");
}

#[test]
fn filters_and_limits() {
    let mut n = trips();
    n.work(|w| add_rows(w, "Trips", &[row(&[("name", "Grandma"), ("miles", "140"), ("vehicle", "Civic")])], false, None)).unwrap();
    let out = n.work(|w| {
        let id = find_table(w.snap(), "Trips").unwrap().id.clone();
        render_table(w, &id, &Filter { equals: vec![("vehicle".into(), "civic".into())], limit: 1 }).unwrap()
    });
    assert!(out.contains("| Chicago |"), "{out}");
    assert!(!out.contains("| Grandma |") && !out.contains("| Tahoe |"), "{out}");
    assert!(out.contains("showing 1 of 2 rows"), "{out}");
    let out = n.work(|w| {
        let id = find_table(w.snap(), "Trips").unwrap().id.clone();
        render_table(w, &id, &Filter { equals: vec![("miles".into(), "620".into())], limit: 10 }).unwrap()
    });
    assert!(out.contains("| Tahoe |") && !out.contains("| Chicago |"), "{out}");
    // Grandma has no gas price or nights: her cells compute, no error.
    let e = n.work(|w| {
        let id = find_table(w.snap(), "Trips").unwrap().id.clone();
        render_table(w, &id, &Filter { equals: vec![("price".into(), "1".into())], limit: 10 })
    });
    assert!(e.unwrap_err().contains("no column \"price\""));
}

#[test]
fn errors_are_listed_with_their_sentences() {
    let mut n = Note::new();
    n.work(|w| {
        create_table(w, "T", &[col("name", "text"), col("a", "number"), col("b", "number")], &[row(&[("name", "x"), ("a", "1"), ("b", "0")])], false, None).unwrap();
        add_column(w, "T", &formula_col("ratio", "=[@a]/[@b]", "auto")).unwrap();
    });
    let out = n.render("T");
    assert!(out.contains("| x | 1 | 0 | #DIV/0! |"), "{out}");
    assert!(out.contains("- row \"x\", ratio: #DIV/0! Divided by zero."), "{out}");
}

#[test]
fn names_tables_and_deletes() {
    let mut n = trips();
    let e = n.work(|w| create_table(w, "trips", &[], &[], false, None)).unwrap_err();
    assert!(e.contains("already has a table called"), "{e}");
    let out = n.work(|w| rename_table(w, "Vehicles", "My cars")).unwrap();
    assert!(out.contains("to My_cars"), "{out}");
    // The link column and formulas follow the rename.
    let out = n.render("Trips");
    assert!(out.contains("link to a row of My_cars"), "{out}");
    let out = n.work(|w| delete_column(w, "Trips", "gas")).unwrap();
    assert!(out.contains("Trips[cost]"), "{out}");
    assert!(n.render("Trips").contains("#REF!"));
    let out = n.work(|w| delete_table(w, "My_cars")).unwrap();
    assert!(out.contains("Trips[vehicle]"), "{out}");
    assert_eq!(n.snap().tables.len(), 1);
    let e = n.work(|w| add_column(w, "Trips", &col("who", "node_link"))).unwrap_err();
    assert!(e.contains("later wave"), "{e}");
}

/// The whole document as one update.
fn bytes_of(doc: &NodeDoc) -> Vec<u8> {
    Sheets::of_node(doc).diff_since(&yrs::StateVector::default())
}

#[test]
fn a_cell_edit_is_that_cell_alone() {
    let mut a = trips();
    let mut b = Note::new();
    b.doc.apply_update(&bytes_of(&a.doc)).unwrap();
    assert_eq!(a.snap(), b.snap());
    let sv_a = yrs::ReadTxn::state_vector(&yrs::Transact::transact(Sheets::of_node(&a.doc).doc()));
    let sv_b = yrs::ReadTxn::state_vector(&yrs::Transact::transact(Sheets::of_node(&b.doc).doc()));
    // Two people edit two cells of one row at once.
    a.work(|w| update_rows(w, "Trips", &[RowUpdate { row: "Chicago".into(), values: row(&[("nights", "4")]) }], false)).unwrap();
    b.work(|w| update_rows(w, "Trips", &[RowUpdate { row: "Chicago".into(), values: row(&[("miles", "500")]) }], false)).unwrap();
    let from_a = Sheets::of_node(&a.doc).diff_since(&sv_a);
    let from_b = Sheets::of_node(&b.doc).diff_since(&sv_b);
    // One cell's entry, not the table rewritten.
    assert!(from_a.len() < 80, "{} bytes", from_a.len());
    a.doc.apply_update(&from_b).unwrap();
    b.doc.apply_update(&from_a).unwrap();
    assert_eq!(a.snap(), b.snap());
    let out = a.render("Trips");
    assert!(out.contains("| Chicago | 500 | Civic | $3.29 | 4 |"), "{out}");
}

#[test]
fn a_new_sheet_is_made_in_one_update() {
    let content = new_sheet_content(
        "Budget",
        &[col("category", "text"), col("amount", "money"), formula_col("yearly", "=[@amount]*12", "money")],
        &[row(&[("category", "Food"), ("amount", "$400")])],
        false,
        r(77),
        today(),
    )
    .unwrap();
    let mut doc = NodeDoc::new();
    doc.apply_update(&content).unwrap();
    let mut book = McpBook::new(r(2), SheetsSnap::default(), today());
    let w = Work::new(Sheets::of_node(&doc), &mut book);
    let out = render_table(&w, &w.snap().tables[0].id.clone(), &Filter { equals: Vec::new(), limit: 10 }).unwrap();
    assert!(out.contains("### Budget"), "{out}");
    assert!(out.contains("| Food | $400.00 | $4,800.00 |"), "{out}");
    // The defaults when no columns are given, as the app's New Sheet.
    let content = new_sheet_content("Table1", &[], &[], false, r(77), today()).unwrap();
    let mut doc = NodeDoc::new();
    doc.apply_update(&content).unwrap();
    let snap = Sheets::of_node(&doc).read();
    let names: Vec<&str> = snap.tables[0].columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["name", "amount"]);
    // A refused formula makes nothing.
    let e = new_sheet_content("T", &[formula_col("x", "=[@nope]", "auto")], &[], false, r(77), today()).unwrap_err();
    assert!(e.contains("nope"), "{e}");
}
