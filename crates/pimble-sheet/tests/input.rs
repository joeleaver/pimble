//! Cell input parsing against the contract (docs/SHEETS_CONTRACT.md section 3).

use chrono::NaiveDate;
use pimble_core::{NodeId, StoreId};
use pimble_sheet::input::*;
use pimble_sheet::*;

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()
}

fn no_link(_: &str) -> Option<Stored> {
    None
}

fn parse(ty: &ColumnType, s: &str) -> Option<Stored> {
    parse_input(ty, s, today(), &no_link)
}

fn n(ty: &ColumnType, s: &str) -> f64 {
    match parse(ty, s) {
        Some(Stored::Number(v)) => v,
        other => panic!("{s:?} parsed as {other:?}"),
    }
}

fn date(s: &str) -> Option<Stored> {
    parse(&ColumnType::Date, s)
}

fn d(s: &str) -> Option<Stored> {
    Some(Stored::Date(s.to_string()))
}

fn unparsed(s: &str) -> Option<Stored> {
    Some(Stored::Unparsed(s.to_string()))
}

#[test]
fn blank_after_trimming_is_none_for_every_type() {
    let tys = [
        ColumnType::Text,
        ColumnType::Number,
        ColumnType::Money,
        ColumnType::Date,
        ColumnType::Hours,
        ColumnType::RowLink { table: "t".into() },
        ColumnType::NodeLink { under: None },
    ];
    for ty in &tys {
        assert_eq!(parse(ty, ""), None);
        assert_eq!(parse(ty, " \t\n "), None);
    }
}

#[test]
fn text_is_as_typed() {
    assert_eq!(parse(&ColumnType::Text, "  Lake Tahoe "), Some(Stored::Text("Lake Tahoe".into())));
    assert_eq!(parse(&ColumnType::Text, "=1+1"), Some(Stored::Text("=1+1".into())));
    assert_eq!(parse(&ColumnType::Text, "12"), Some(Stored::Text("12".into())));
}

#[test]
fn numbers() {
    for ty in [ColumnType::Number, ColumnType::Money] {
        assert_eq!(n(&ty, "1234.5"), 1234.5);
        assert_eq!(n(&ty, "1,234.5"), 1234.5);
        assert_eq!(n(&ty, "$1,234.50"), 1234.5);
        assert_eq!(n(&ty, "-3"), -3.0);
        assert_eq!(n(&ty, ".5"), 0.5);
        assert_eq!(n(&ty, "  42  "), 42.0);
        assert_eq!(n(&ty, "0"), 0.0);
        assert_eq!(n(&ty, "-$5"), -5.0);
        assert_eq!(n(&ty, "$ 1 200"), 1200.0);
        assert_eq!(n(&ty, "1,000,000"), 1_000_000.0);
        assert_eq!(n(&ty, "-.25"), -0.25);
        assert_eq!(n(&ty, "007"), 7.0);
    }
}

#[test]
fn numbers_are_strict() {
    for bad in ["12abc", "abc", "1.2.3", "1,2,3", "12,34", "1,2345", ",5", "5.", "--3", "$", "-", ".", "1e5", "NaN", "inf", "$$5", "1 2 3 a", "12%"] {
        for ty in [ColumnType::Number, ColumnType::Money] {
            assert_eq!(parse(&ty, bad), unparsed(bad), "{bad}");
        }
    }
}

#[test]
fn hours() {
    let h = ColumnType::Hours;
    for (s, want) in [
        ("1:30", 1.5),
        ("0:45", 0.75),
        ("10:05", 10.0 + 5.0 / 60.0),
        ("1.5", 1.5),
        ("1.5h", 1.5),
        ("2", 2.0),
        (".5", 0.5),
        ("3h", 3.0),
        ("90m", 1.5),
        ("45M", 0.75),
        ("1h30", 1.5),
        ("1h30m", 1.5),
        ("1h 30m", 1.5),
        ("1 H 30 M", 1.5),
        ("  2h  ", 2.0),
        ("-1:30", -1.5),
    ] {
        assert!((n(&h, s) - want).abs() < 1e-9, "{s} -> {}", n(&h, s));
    }
    for bad in ["abc", "1:", ":30", "1:75", "1h75m", "h", "m", "1.5m", "1:2:3", "1x", "1,5", "1h30x", "1.5h30"] {
        assert_eq!(parse(&h, bad), unparsed(bad), "{bad}");
    }
}

#[test]
fn dates_by_word() {
    assert_eq!(date("today"), d("2026-10-19"));
    assert_eq!(date("Today"), d("2026-10-19"));
    assert_eq!(date("yesterday"), d("2026-10-18"));
    assert_eq!(date("tomorrow"), d("2026-10-20"));
    let new_year = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    assert_eq!(parse_input(&ColumnType::Date, "yesterday", new_year, &no_link), d("2025-12-31"));
}

#[test]
fn dates_numeric() {
    assert_eq!(date("2026-10-14"), d("2026-10-14"));
    assert_eq!(date("2026-1-5"), d("2026-01-05"));
    assert_eq!(date("10/14"), d("2026-10-14"));
    assert_eq!(date("10/14/26"), d("2026-10-14"));
    assert_eq!(date("10/14/2026"), d("2026-10-14"));
    assert_eq!(date("1/2/2025"), d("2025-01-02"));
    assert_eq!(date(" 10/14 "), d("2026-10-14"));
}

#[test]
fn dates_with_month_names() {
    assert_eq!(date("Oct 14"), d("2026-10-14"));
    assert_eq!(date("October 14"), d("2026-10-14"));
    assert_eq!(date("oct 14"), d("2026-10-14"));
    assert_eq!(date("Oct 14 2026"), d("2026-10-14"));
    assert_eq!(date("Oct 14, 2026"), d("2026-10-14"));
    assert_eq!(date("14 Oct 2026"), d("2026-10-14"));
    assert_eq!(date("14 October"), d("2026-10-14"));
    assert_eq!(date("Sept 3"), d("2026-09-03"));
    assert_eq!(date("March 3rd 2025"), d("2025-03-03"));
    assert_eq!(date("Dec 31 1999"), d("1999-12-31"));
    assert_eq!(date("Feb 29 2028"), d("2028-02-29"));
}

#[test]
fn dates_that_do_not_parse_stay_as_typed() {
    for bad in [
        "soon", "13/40", "2/30", "Feb 29 2027", "Octember 3", "Oct", "14", "2026-13-01", "2026-10", "10/14/202", "10/14/2026/1", "Oct 32", "Oct 14 26",
        "Oct 14 2026 5", "0/5", "-", "ma 4",
    ] {
        assert_eq!(date(bad), unparsed(bad), "{bad}");
    }
}

#[test]
fn links_are_decided_by_the_caller() {
    let row = ColumnType::RowLink { table: "t".into() };
    let node = ColumnType::NodeLink { under: None };
    let target = Stored::Node(NodeRef { store: StoreId::new(), node: NodeId::new() });
    let want = target.clone();
    let link = move |s: &str| (s == "Deck repair").then(|| want.clone());
    assert_eq!(parse_input(&node, "  Deck repair ", today(), &link), Some(target));
    assert_eq!(parse_input(&node, "Nope", today(), &link), unparsed("Nope"));
    let seen = std::cell::RefCell::new(Vec::new());
    let spy = |s: &str| {
        seen.borrow_mut().push(s.to_string());
        Some(Stored::Row("r1".into()))
    };
    assert_eq!(parse_input(&row, " Civic ", today(), &spy), Some(Stored::Row("r1".into())));
    assert_eq!(*seen.borrow(), vec!["Civic".to_string()]);
    // Blank never asks.
    assert_eq!(parse_input(&row, "  ", today(), &spy), None);
    assert_eq!(seen.borrow().len(), 1);
}

#[test]
fn formula_columns_take_no_typed_input() {
    let f = ColumnType::Formula { formula: "=1".into(), format: Format::Auto };
    assert_eq!(parse(&f, "=1+1"), None);
    assert_eq!(parse(&f, "12"), None);
}

fn label(s: &Stored) -> String {
    match s {
        Stored::Row(id) => format!("row {id}"),
        Stored::Node(_) => "A node".into(),
        _ => unreachable!(),
    }
}

fn edit(v: Stored, ty: &ColumnType) -> String {
    edit_text(&v, ty, &label)
}

#[test]
fn edit_text_forms() {
    let num = ColumnType::Number;
    assert_eq!(edit(Stored::Number(1234.5), &num), "1234.5");
    assert_eq!(edit(Stored::Number(1234.0), &ColumnType::Money), "1234");
    assert_eq!(edit(Stored::Number(-0.0), &num), "0");
    assert_eq!(edit(Stored::Number(0.1 + 0.2), &num), "0.3");
    assert_eq!(edit(Stored::Number(1.0 / 3.0), &num), "0.3333333333");
    assert_eq!(edit(Stored::Number(1e15), &num), "1000000000000000");
    assert_eq!(edit(Stored::Number(-2.5), &num), "-2.5");
    assert_eq!(edit(Stored::Number(1e-12), &num), "0");
    assert_eq!(edit(Stored::Date("2026-10-14".into()), &ColumnType::Date), "2026-10-14");
    assert_eq!(edit(Stored::Number(2.5), &ColumnType::Hours), "2h 30m");
    assert_eq!(edit(Stored::Number(0.75), &ColumnType::Hours), "45m");
    assert_eq!(edit(Stored::Number(3.0), &ColumnType::Hours), "3h");
    assert_eq!(edit(Stored::Row("r1".into()), &ColumnType::RowLink { table: "t".into() }), "row r1");
    assert_eq!(edit(Stored::Node(NodeRef { store: StoreId::new(), node: NodeId::new() }), &ColumnType::NodeLink { under: None }), "A node");
    assert_eq!(edit(Stored::Unparsed("12abc".into()), &num), "12abc");
    assert_eq!(edit(Stored::Text("hi there".into()), &ColumnType::Text), "hi there");
    assert_eq!(edit(Stored::Number(4.0), &ColumnType::Text), "4");
}

#[test]
fn hours_text_forms() {
    assert_eq!(hours_text(2.5), "2h 30m");
    assert_eq!(hours_text(0.75), "45m");
    assert_eq!(hours_text(3.0), "3h");
    assert_eq!(hours_text(0.0), "0m");
    assert_eq!(hours_text(1.0 / 60.0), "1m");
    assert_eq!(hours_text(1.999), "2h");
    assert_eq!(hours_text(0.999), "1h");
    assert_eq!(hours_text(-1.5), "-1h 30m");
    assert_eq!(hours_text(-0.0001), "0m");
    assert_eq!(hours_text(100.25), "100h 15m");
    assert_eq!(hours_text(f64::NAN), "");
}

#[test]
fn editing_text_parses_back_to_the_same_cell() {
    let cases: Vec<(ColumnType, Stored)> = vec![
        (ColumnType::Number, Stored::Number(1234.5)),
        (ColumnType::Money, Stored::Number(-0.25)),
        (ColumnType::Number, Stored::Number(1e12)),
        (ColumnType::Date, Stored::Date("2026-10-14".into())),
        (ColumnType::Hours, Stored::Number(2.5)),
        (ColumnType::Hours, Stored::Number(0.75)),
        (ColumnType::Hours, Stored::Number(3.0)),
        (ColumnType::Hours, Stored::Number(-1.5)),
        (ColumnType::Text, Stored::Text("hello".into())),
        (ColumnType::Number, Stored::Unparsed("12abc".into())),
    ];
    for (ty, v) in cases {
        let shown = edit(v.clone(), &ty);
        assert_eq!(parse(&ty, &shown), Some(v), "{shown}");
    }
}
