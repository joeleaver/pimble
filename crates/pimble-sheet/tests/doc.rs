//! The sheet document against the contract (docs/SHEETS_CONTRACT.md sections
//! 2, 3 and 5), through `NodeDoc` the way the app and the server hold it.

use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_sheet::doc::*;
use pimble_sheet::*;

const T0: &str = "2026-10-19T10:00:00.000Z";

fn node() -> NodeDoc {
    let mut doc = NodeDoc::new();
    doc.init("sheet", "Budget", None, T0).unwrap();
    doc
}

fn peer(of: &NodeDoc) -> NodeDoc {
    NodeDoc::load(&of.save()).unwrap()
}

fn sync(a: &mut NodeDoc, b: &mut NodeDoc) {
    let (asv, bsv) = (a.state_vector(), b.state_vector());
    let to_b = a.diff_since(&bsv).unwrap();
    let to_a = b.diff_since(&asv).unwrap();
    b.apply_update(&to_b).unwrap();
    a.apply_update(&to_a).unwrap();
}

fn send(update: Option<Vec<u8>>, to: &mut NodeDoc) {
    to.apply_update(&update.expect("an update")).unwrap();
}

/// A node with one table "Trips" with columns name and price, made on `a` and
/// held by a peer.
struct Fixture {
    a: NodeDoc,
    b: NodeDoc,
    table: String,
    name: String,
    price: String,
}

fn fixture() -> Fixture {
    let mut a = node();
    let s = Sheets::of_node(&a);
    let ((table, name, price), update) = s
        .edit(|t| {
            let table = t.create_table("Trips", Placement::Last)?;
            let name = t.add_column(&table, "name", ColumnType::Text, Placement::Last)?;
            let price = t.add_column(&table, "price", ColumnType::Money, Placement::Last)?;
            Ok((table, name, price))
        })
        .into_result()
        .unwrap();
    let mut b = peer(&a);
    send(update, &mut b);
    sync(&mut a, &mut b);
    Fixture { a, b, table, name, price }
}

fn num(n: f64) -> Stored {
    Stored::Number(n)
}

fn text(s: &str) -> Stored {
    Stored::Text(s.into())
}

fn names(snap: &TableSnap) -> Vec<&str> {
    snap.columns.iter().map(|c| c.name.as_str()).collect()
}

#[test]
fn a_node_has_no_tables_until_one_is_made() {
    let doc = node();
    let s = Sheets::of_node(&doc);
    assert!(!s.exists());
    assert!(s.read().tables.is_empty());
    assert!(s.table("nope").is_none());
    let (id, update) = s.edit(|t| t.create_table("Expenses", Placement::Last)).into_result().unwrap();
    assert!(update.is_some());
    assert!(s.exists());
    let snap = s.read();
    assert_eq!(snap.tables.len(), 1);
    assert_eq!(snap.tables[0].id, id);
    assert_eq!(snap.tables[0].name, "Expenses");
    assert!(snap.tables[0].columns.is_empty() && snap.tables[0].rows.is_empty() && snap.tables[0].source.is_none());
    assert_eq!(s.table(&id).unwrap().name, "Expenses");
}

#[test]
fn standalone_and_node_handles_share_the_layout() {
    let s = Sheets::standalone();
    let (id, update) = s.edit(|t| t.create_table("T", Placement::Last)).into_result().unwrap();
    let mut doc = node();
    send(update, &mut doc);
    assert_eq!(Sheets::of_node(&doc).table(&id).unwrap().name, "T");
}

#[test]
fn nothing_changed_answers_no_update() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    let e = s.edit(|t| t.rename_table("nope", "X"));
    assert!(e.result.is_err());
    assert!(e.update.is_none());
    let e = s.edit(|_| Ok(()));
    assert!(e.update.is_none());
}

#[test]
fn tables_read_in_order_then_id() {
    let doc = node();
    let s = Sheets::of_node(&doc);
    let ((a, b, c, d), _) = s
        .edit(|t| {
            let a = t.create_table("A", Placement::Last)?;
            let b = t.create_table("B", Placement::Last)?;
            let c = t.create_table("C", Placement::First)?;
            let d = t.create_table("D", Placement::After(a.clone()))?;
            Ok((a, b, c, d))
        })
        .into_result()
        .unwrap();
    let order: Vec<_> = s.read().tables.iter().map(|t| t.id.clone()).collect();
    assert_eq!(order, vec![c.clone(), a.clone(), d.clone(), b.clone()]);
    s.edit(|t| t.move_table(&b, Placement::Before(a.clone()))).into_result().unwrap();
    let order: Vec<_> = s.read().tables.iter().map(|t| t.id.clone()).collect();
    assert_eq!(order, vec![c.clone(), b.clone(), a.clone(), d.clone()]);
    s.edit(|t| t.move_table(&c, Placement::Last)).into_result().unwrap();
    let order: Vec<_> = s.read().tables.iter().map(|t| t.id.clone()).collect();
    assert_eq!(order, vec![b.clone(), a.clone(), d.clone(), c.clone()]);
    s.edit(|t| t.move_table(&c, Placement::After("gone".into()))).result.unwrap_err();
    // set_table_order writes a key directly.
    s.edit(|t| t.set_table_order(&c, "")).into_result().unwrap();
    assert_eq!(s.read().tables[0].id, c);
}

#[test]
fn tables_with_equal_orders_read_by_id() {
    let doc = node();
    let s = Sheets::of_node(&doc);
    let ((a, b), _) = s
        .edit(|t| {
            let a = t.create_table("A", Placement::Last)?;
            let b = t.create_table("B", Placement::Last)?;
            t.set_table_order(&a, "a5")?;
            t.set_table_order(&b, "a5")?;
            Ok((a, b))
        })
        .into_result()
        .unwrap();
    let mut want = vec![a, b];
    want.sort();
    let got: Vec<_> = s.read().tables.iter().map(|t| t.id.clone()).collect();
    assert_eq!(got, want);
}

#[test]
fn rename_and_delete_a_table() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    s.edit(|t| t.rename_table(&f.table, "Journeys")).into_result().unwrap();
    assert_eq!(s.table(&f.table).unwrap().name, "Journeys");
    // The same name in another case is the table's own.
    s.edit(|t| t.rename_table(&f.table, "journeys")).into_result().unwrap();
    assert_eq!(s.table(&f.table).unwrap().name, "journeys");
    let (_, update) = s.edit(|t| t.delete_table(&f.table)).into_result().unwrap();
    assert!(s.table(&f.table).is_none());
    let mut b = f.b;
    send(update, &mut b);
    assert!(Sheets::of_node(&b).read().tables.is_empty());
    assert_eq!(s.edit(|t| t.delete_table(&f.table)).result, Err(SheetError::NoTable(f.table.clone())));
}

#[test]
fn source_and_headers() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    s.edit(|t| {
        t.set_source(&f.table, Some("=SORT(x)"))?;
        t.set_header(&f.table, &ResultKey::Pos(0), Some("Month"))?;
        t.set_header(&f.table, &ResultKey::Pos(2), Some("Spent"))
    })
    .into_result()
    .unwrap();
    let t = s.table(&f.table).unwrap();
    assert!(t.is_summary());
    assert_eq!(t.source.as_deref(), Some("=SORT(x)"));
    assert_eq!(t.headers.get(&ResultKey::Pos(0)).map(String::as_str), Some("Month"));
    assert_eq!(t.headers.get(&ResultKey::Pos(2)).map(String::as_str), Some("Spent"));
    s.edit(|t| {
        t.set_source(&f.table, None)?;
        t.set_header(&f.table, &ResultKey::Pos(0), None)
    })
    .into_result()
    .unwrap();
    let t = s.table(&f.table).unwrap();
    assert!(!t.is_summary());
    assert_eq!(t.headers.len(), 1);
}

#[test]
fn columns_every_type_roundtrips() {
    let doc = node();
    let s = Sheets::of_node(&doc);
    let target = NodeRef { store: StoreId::new(), node: NodeId::new() };
    let (table, _) = s.edit(|t| t.create_table("T", Placement::Last)).into_result().unwrap();
    let types = vec![
        ColumnType::Text,
        ColumnType::Number,
        ColumnType::Money,
        ColumnType::Date,
        ColumnType::Hours,
        ColumnType::RowLink { table: "tbl".into() },
        ColumnType::NodeLink { under: Some(target) },
        ColumnType::NodeLink { under: None },
        ColumnType::Formula { formula: "=[@a]+1".into(), format: Format::Money },
        ColumnType::Formula { formula: "x".into(), format: Format::Percent },
    ];
    s.edit(|t| {
        for (i, ty) in types.iter().enumerate() {
            t.add_column(&table, &format!("c{i}"), ty.clone(), Placement::Last)?;
        }
        Ok(())
    })
    .into_result()
    .unwrap();
    let snap = s.table(&table).unwrap();
    let got: Vec<_> = snap.columns.iter().map(|c| c.ty.clone()).collect();
    assert_eq!(got, types);
    assert_eq!(names(&snap), (0..10).map(|i| format!("c{i}")).collect::<Vec<_>>());
}

#[test]
fn set_column_type_removes_stale_fields() {
    let doc = node();
    let s = Sheets::of_node(&doc);
    let ((table, col), _) = s
        .edit(|t| {
            let table = t.create_table("T", Placement::Last)?;
            let col = t.add_column(&table, "f", ColumnType::Formula { formula: "=1".into(), format: Format::Money }, Placement::Last)?;
            Ok((table, col))
        })
        .into_result()
        .unwrap();
    s.edit(|t| t.set_column_type(&table, &col, ColumnType::RowLink { table: "x".into() })).into_result().unwrap();
    assert_eq!(s.table(&table).unwrap().columns[0].ty, ColumnType::RowLink { table: "x".into() });
    s.edit(|t| t.set_column_type(&table, &col, ColumnType::Number)).into_result().unwrap();
    assert_eq!(s.table(&table).unwrap().columns[0].ty, ColumnType::Number);
    // Stale fields are gone from the stored map, not merely ignored.
    {
        use yrs::{Map, Out, Transact};
        let txn = s.doc().transact();
        let t = s.table_map(&txn, &table).unwrap();
        let Some(Out::YMap(cols)) = t.get(&txn, "columns") else { panic!() };
        let Some(Out::YMap(c)) = cols.get(&txn, &col) else { panic!() };
        for key in ["target", "under", "formula", "format"] {
            assert!(c.get(&txn, key).is_none(), "{key} should be removed");
        }
    }
    s.edit(|t| t.set_column_type(&table, "nope", ColumnType::Text)).result.unwrap_err();
}

#[test]
fn columns_order_move_rename_delete() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    assert_eq!(names(&s.table(&f.table).unwrap()), vec!["name", "price"]);
    let (extra, _) = s.edit(|t| t.add_column(&f.table, "Qty", ColumnType::Number, Placement::First)).into_result().unwrap();
    assert_eq!(names(&s.table(&f.table).unwrap()), vec!["Qty", "name", "price"]);
    s.edit(|t| t.move_column(&f.table, &extra, Placement::After(f.name.clone()))).into_result().unwrap();
    assert_eq!(names(&s.table(&f.table).unwrap()), vec!["name", "Qty", "price"]);
    s.edit(|t| t.move_column(&f.table, &extra, Placement::Before(f.name.clone()))).into_result().unwrap();
    assert_eq!(names(&s.table(&f.table).unwrap()), vec!["Qty", "name", "price"]);
    s.edit(|t| t.move_column(&f.table, &f.name, Placement::Last)).into_result().unwrap();
    assert_eq!(names(&s.table(&f.table).unwrap()), vec!["Qty", "price", "name"]);
    s.edit(|t| t.rename_column(&f.table, &extra, "Quantity")).into_result().unwrap();
    assert_eq!(s.table(&f.table).unwrap().columns[0].name, "Quantity");
    // Spaces collapse, forbidden characters go.
    s.edit(|t| t.rename_column(&f.table, &extra, "  Hotel   [rate] @ night #  ")).into_result().unwrap();
    assert_eq!(s.table(&f.table).unwrap().columns[0].name, "Hotel rate night");
    s.edit(|t| t.delete_column(&f.table, &extra)).into_result().unwrap();
    assert_eq!(names(&s.table(&f.table).unwrap()), vec!["price", "name"]);
    assert_eq!(s.edit(|t| t.delete_column(&f.table, &extra)).result, Err(SheetError::NoColumn(extra.clone())));
    s.edit(|t| t.add_column("nope", "x", ColumnType::Text, Placement::Last)).result.unwrap_err();
    s.edit(|t| t.add_column(&f.table, "x", ColumnType::Text, Placement::After("gone".into()))).result.unwrap_err();
}

#[test]
fn rows_cells_and_every_stored_kind() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    let node_ref = NodeRef { store: StoreId::new(), node: NodeId::new() };
    let (link_col, _) = s.edit(|t| t.add_column(&f.table, "link", ColumnType::Text, Placement::Last)).into_result().unwrap();
    let kinds = vec![
        num(12.5),
        num(-0.0),
        text("Lake Tahoe"),
        text(""),
        Stored::Date("2026-10-14".into()),
        Stored::Row("rowid".into()),
        Stored::Node(node_ref),
        Stored::Unparsed("12abc".into()),
    ];
    for k in &kinds {
        let (row, update) = s.edit(|t| t.add_row(&f.table, Placement::Last, &[(f.name.clone(), text("x")), (link_col.clone(), k.clone())])).into_result().unwrap();
        let mut b = peer(&f.a);
        send(update, &mut b);
        let seen = Sheets::of_node(&b).table(&f.table).unwrap();
        let r = seen.row(&row).unwrap();
        let got = r.cells.get(&link_col).unwrap();
        match (k, got) {
            (Stored::Number(a), Stored::Number(b)) => assert_eq!(a, b),
            (a, b) => assert_eq!(a, b),
        }
        assert_eq!(r.cells.get(&f.name), Some(&text("x")));
        assert!(!r.cells.contains_key(&f.price));
    }
    assert_eq!(s.table(&f.table).unwrap().rows.len(), kinds.len());
}

#[test]
fn rows_keep_order_and_move() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    let cell = |v: &str| vec![(f.name.clone(), text(v))];
    let (ids, _) = s
        .edit(|t| t.add_rows(&f.table, Placement::Last, &[cell("a"), cell("b"), cell("c"), cell("d")]))
        .into_result()
        .unwrap();
    let col = |s: &Sheets| -> Vec<String> {
        s.table(&f.table).unwrap().rows.iter().map(|r| match &r.cells[&f.name] {
            Stored::Text(t) => t.clone(),
            _ => unreachable!(),
        }).collect()
    };
    assert_eq!(col(&s), vec!["a", "b", "c", "d"]);
    let (first, _) = s.edit(|t| t.add_row(&f.table, Placement::First, &cell("z"))).into_result().unwrap();
    assert_eq!(col(&s), vec!["z", "a", "b", "c", "d"]);
    let (mid, _) = s.edit(|t| t.add_row(&f.table, Placement::After(ids[1].clone()), &cell("m"))).into_result().unwrap();
    assert_eq!(col(&s), vec!["z", "a", "b", "m", "c", "d"]);
    s.edit(|t| t.add_rows(&f.table, Placement::Before(ids[3].clone()), &[cell("p"), cell("q"), cell("r")])).into_result().unwrap();
    assert_eq!(col(&s), vec!["z", "a", "b", "m", "c", "p", "q", "r", "d"]);
    s.edit(|t| t.move_row(&f.table, &first, Placement::Last)).into_result().unwrap();
    assert_eq!(col(&s), vec!["a", "b", "m", "c", "p", "q", "r", "d", "z"]);
    s.edit(|t| t.move_row(&f.table, &mid, Placement::First)).into_result().unwrap();
    assert_eq!(col(&s)[0], "m");
    s.edit(|t| t.delete_row(&f.table, &mid)).into_result().unwrap();
    assert_eq!(col(&s)[0], "a");
    assert_eq!(s.edit(|t| t.delete_row(&f.table, &mid)).result, Err(SheetError::NoRow(mid.clone())));
    // A placement is refused before anything is written.
    let e = s.edit(|t| t.add_row(&f.table, Placement::After("gone".into()), &cell("x")));
    assert!(e.result.is_err() && e.update.is_none());
    // And so is a cell of a column that does not exist.
    let e = s.edit(|t| t.add_row(&f.table, Placement::Last, &[("nope".into(), num(1.0))]));
    assert_eq!(e.result, Err(SheetError::NoColumn("nope".into())));
    assert!(e.update.is_none());
    assert_eq!(s.edit(|t| t.add_rows(&f.table, Placement::Last, &[])).into_result().unwrap().0.len(), 0);
}

#[test]
fn many_rows_stay_ordered() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    let rows: Vec<Vec<(String, Stored)>> = (0..200).map(|i| vec![(f.price.clone(), num(i as f64))]).collect();
    let (ids, _) = s.edit(|t| t.add_rows(&f.table, Placement::Last, &rows)).into_result().unwrap();
    let snap = s.table(&f.table).unwrap();
    let got: Vec<_> = snap.rows.iter().map(|r| r.id.clone()).collect();
    assert_eq!(got, ids);
    // Insert in the middle of them.
    let middle = Placement::After(ids[99].clone());
    let (more, _) = s.edit(|t| t.add_rows(&f.table, middle, &rows[..5])).into_result().unwrap();
    let got: Vec<_> = s.table(&f.table).unwrap().rows.iter().map(|r| r.id.clone()).collect();
    assert_eq!(&got[100..105], &more[..]);
}

#[test]
fn set_cell_sets_and_blanks() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    let (row, _) = s.edit(|t| t.add_row(&f.table, Placement::Last, &[])).into_result().unwrap();
    assert!(s.table(&f.table).unwrap().rows[0].cells.is_empty());
    s.edit(|t| t.set_cell(&f.table, &row, &f.price, Some(&num(9.5)))).into_result().unwrap();
    assert_eq!(s.table(&f.table).unwrap().rows[0].cells[&f.price], num(9.5));
    s.edit(|t| t.set_cell(&f.table, &row, &f.price, Some(&Stored::Unparsed("abc".into())))).into_result().unwrap();
    assert_eq!(s.table(&f.table).unwrap().rows[0].cells[&f.price], Stored::Unparsed("abc".into()));
    s.edit(|t| t.set_cell(&f.table, &row, &f.price, None)).into_result().unwrap();
    assert!(s.table(&f.table).unwrap().rows[0].cells.is_empty());
    assert!(s.edit(|t| t.set_cell(&f.table, "gone", &f.price, None)).result.is_err());
    assert!(s.edit(|t| t.set_cell(&f.table, &row, "gone", Some(&num(1.0)))).result.is_err());
    assert!(s.edit(|t| t.set_cell("gone", &row, &f.price, None)).result.is_err());
}

#[test]
fn deleting_a_column_deletes_its_cells() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    for i in 0..3 {
        s.edit(|t| t.add_row(&f.table, Placement::Last, &[(f.name.clone(), text("n")), (f.price.clone(), num(i as f64))])).into_result().unwrap();
    }
    let (_, update) = s.edit(|t| t.delete_column(&f.table, &f.price)).into_result().unwrap();
    for rows in [s.table(&f.table).unwrap().rows] {
        assert!(rows.iter().all(|r| r.cells.len() == 1 && r.cells.contains_key(&f.name)));
    }
    // The cells are removed from the document, not just hidden when read:
    // a column with this id making its way back finds nothing in the rows.
    let mut b = f.b;
    send(update, &mut b);
    use yrs::{Map, Out, Transact};
    let sb = Sheets::of_node(&b);
    let txn = sb.doc().transact();
    let t = sb.table_map(&txn, &f.table).unwrap();
    let Some(Out::YMap(rows)) = t.get(&txn, "rows") else { panic!() };
    for (_, r) in rows.iter(&txn) {
        let Out::YMap(r) = r else { panic!() };
        let Some(Out::YMap(cells)) = r.get(&txn, "cells") else { panic!() };
        assert!(cells.get(&txn, &f.price).is_none());
    }
}

#[test]
fn cells_of_a_deleted_column_are_not_read() {
    // Someone types in a column while another deletes it.
    let mut f = fixture();
    let (row, u) = Sheets::of_node(&f.a).edit(|t| t.add_row(&f.table, Placement::Last, &[])).into_result().unwrap();
    send(u, &mut f.b);
    let (_, del) = Sheets::of_node(&f.a).edit(|t| t.delete_column(&f.table, &f.price)).into_result().unwrap();
    let (_, typed) = Sheets::of_node(&f.b).edit(|t| t.set_cell(&f.table, &row, &f.price, Some(&num(5.0)))).into_result().unwrap();
    send(typed, &mut f.a);
    send(del, &mut f.b);
    for d in [&f.a, &f.b] {
        let t = Sheets::of_node(d).table(&f.table).unwrap();
        assert!(t.columns.iter().all(|c| c.id != f.price));
        assert!(t.rows[0].cells.is_empty());
    }
}

// ── Names ────────────────────────────────────────────────────────────────

#[test]
fn table_names_are_unique_case_insensitively() {
    let doc = node();
    let s = Sheets::of_node(&doc);
    s.edit(|t| t.create_table("Expenses", Placement::Last)).into_result().unwrap();
    let e = s.edit(|t| t.create_table("  expenses ", Placement::Last));
    assert_eq!(e.result, Err(SheetError::DuplicateTable("expenses".into())));
    assert!(e.update.is_none());
    assert_eq!(e.result.unwrap_err().to_string(), "This note already has a table called \"expenses\".");
    let (other, _) = s.edit(|t| t.create_table("Trips", Placement::Last)).into_result().unwrap();
    assert!(matches!(s.edit(|t| t.rename_table(&other, "EXPENSES")).result, Err(SheetError::DuplicateTable(_))));
    assert_eq!(s.table(&other).unwrap().name, "Trips");
}

#[test]
fn table_names_have_a_shape() {
    let doc = node();
    let s = Sheets::of_node(&doc);
    assert_eq!(s.edit(|t| t.create_table("   ", Placement::Last)).result, Err(SheetError::EmptyTableName));
    assert_eq!(s.edit(|t| t.create_table("", Placement::Last)).result.unwrap_err().to_string(), "A table needs a name.");
    for bad in ["two words", "1st", "a-b", "a.b", "a[b]", "x!"] {
        let e = s.edit(|t| t.create_table(bad, Placement::Last));
        assert!(matches!(e.result, Err(SheetError::InvalidTableName(_))), "{bad}");
        assert!(e.update.is_none());
    }
    for good in ["_x", "Trips2", "a_b_c", "Straße"] {
        s.edit(|t| t.create_table(good, Placement::Last)).into_result().unwrap();
    }
    assert!(s.edit(|t| t.create_table("x", Placement::After("gone".into()))).result.is_err());
}

#[test]
fn table_name_from_and_column_name_from() {
    assert_eq!(table_name_from("Household budget"), "Household_budget");
    assert_eq!(table_name_from("  Trips & costs! "), "Trips__costs");
    assert_eq!(table_name_from("a-b.c"), "abc");
    assert_eq!(table_name_from("1 st"), "1_st");
    assert_eq!(table_name_from("!!!"), "");
    assert_eq!(column_name_from("  hotel   rate "), "hotel rate");
    assert_eq!(column_name_from("[@price]#"), "price");
    assert_eq!(column_name_from("a\tb\nc"), "a b c");
    assert_eq!(column_name_from("@#"), "");
    assert!(is_table_name("Trips") && !is_table_name("2x") && !is_table_name(""));
}

#[test]
fn column_names_are_unique_within_their_table() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    let e = s.edit(|t| t.add_column(&f.table, " PRICE ", ColumnType::Number, Placement::Last));
    assert_eq!(e.result.clone().unwrap_err().to_string(), "Trips already has a column called \"PRICE\".");
    assert!(e.update.is_none());
    assert!(matches!(s.edit(|t| t.rename_column(&f.table, &f.name, "Price")).result, Err(SheetError::DuplicateColumn { .. })));
    // Its own name in another case is fine.
    s.edit(|t| t.rename_column(&f.table, &f.price, "PRICE")).into_result().unwrap();
    // Another table may reuse the name.
    let (t2, _) = s.edit(|t| t.create_table("Other", Placement::Last)).into_result().unwrap();
    s.edit(|t| t.add_column(&t2, "price", ColumnType::Number, Placement::Last)).into_result().unwrap();
    assert_eq!(s.edit(|t| t.add_column(&f.table, "  ", ColumnType::Text, Placement::Last)).result, Err(SheetError::EmptyColumnName));
    assert_eq!(s.edit(|t| t.add_column(&f.table, "[@]", ColumnType::Text, Placement::Last)).result.unwrap_err().to_string(), "A column needs a name.");
}

// ── Concurrency ──────────────────────────────────────────────────────────

#[test]
fn different_cells_rows_and_columns_merge() {
    let mut f = fixture();
    let (rows, u) = Sheets::of_node(&f.a)
        .edit(|t| t.add_rows(&f.table, Placement::Last, &[vec![(f.name.clone(), text("a"))], vec![(f.name.clone(), text("b"))]]))
        .into_result()
        .unwrap();
    send(u, &mut f.b);
    let (a, b) = (Sheets::of_node(&f.a), Sheets::of_node(&f.b));
    a.edit(|t| t.set_cell(&f.table, &rows[0], &f.price, Some(&num(1.0)))).into_result().unwrap();
    b.edit(|t| t.set_cell(&f.table, &rows[1], &f.price, Some(&num(2.0)))).into_result().unwrap();
    let (qty_a, _) = a.edit(|t| t.add_column(&f.table, "qty", ColumnType::Number, Placement::Last)).into_result().unwrap();
    let (note_b, _) = b.edit(|t| t.add_column(&f.table, "note", ColumnType::Text, Placement::Last)).into_result().unwrap();
    b.edit(|t| t.rename_table(&f.table, "Journeys")).into_result().unwrap();
    a.edit(|t| t.create_table("Other", Placement::Last)).into_result().unwrap();
    sync(&mut f.a, &mut f.b);
    let (ra, rb) = (a.read(), b.read());
    assert_eq!(ra, rb);
    assert_eq!(ra.tables.len(), 2);
    let t = ra.table(&f.table).unwrap();
    assert_eq!(t.name, "Journeys");
    assert_eq!(t.rows[0].cells[&f.price], num(1.0));
    assert_eq!(t.rows[1].cells[&f.price], num(2.0));
    assert!(t.column(&qty_a).is_some() && t.column(&note_b).is_some());
}

#[test]
fn the_same_cell_is_one_whole_value() {
    let mut f = fixture();
    let (row, u) = Sheets::of_node(&f.a).edit(|t| t.add_row(&f.table, Placement::Last, &[])).into_result().unwrap();
    send(u, &mut f.b);
    Sheets::of_node(&f.a).edit(|t| t.set_cell(&f.table, &row, &f.name, Some(&text("Lake Tahoe")))).into_result().unwrap();
    Sheets::of_node(&f.b).edit(|t| t.set_cell(&f.table, &row, &f.name, Some(&text("Yosemite")))).into_result().unwrap();
    sync(&mut f.a, &mut f.b);
    let va = Sheets::of_node(&f.a).table(&f.table).unwrap().rows[0].cells[&f.name].clone();
    let vb = Sheets::of_node(&f.b).table(&f.table).unwrap().rows[0].cells[&f.name].clone();
    assert_eq!(va, vb);
    assert!(va == text("Lake Tahoe") || va == text("Yosemite"), "{va:?}");
}

#[test]
fn a_formula_is_one_whole_value() {
    let mut f = fixture();
    let (col, u) = Sheets::of_node(&f.a)
        .edit(|t| t.add_column(&f.table, "cost", ColumnType::Formula { formula: "=1".into(), format: Format::Auto }, Placement::Last))
        .into_result()
        .unwrap();
    send(u, &mut f.b);
    let fa = ColumnType::Formula { formula: "=[@price]*1000".into(), format: Format::Money };
    let fb = ColumnType::Formula { formula: "=[@price]+7".into(), format: Format::Number };
    Sheets::of_node(&f.a).edit(|t| t.set_column_type(&f.table, &col, fa.clone())).into_result().unwrap();
    Sheets::of_node(&f.b).edit(|t| t.set_column_type(&f.table, &col, fb.clone())).into_result().unwrap();
    sync(&mut f.a, &mut f.b);
    let ta = Sheets::of_node(&f.a).table(&f.table).unwrap().column(&col).unwrap().ty.clone();
    let tb = Sheets::of_node(&f.b).table(&f.table).unwrap().column(&col).unwrap().ty.clone();
    assert_eq!(ta, tb);
    assert!(ta == fa || ta == fb, "no blend of the two: {ta:?}");
}

#[test]
fn concurrent_add_row_at_the_end_keeps_both() {
    let mut f = fixture();
    let a = Sheets::of_node(&f.a);
    let b = Sheets::of_node(&f.b);
    let (r1, _) = a.edit(|t| t.add_row(&f.table, Placement::Last, &[(f.name.clone(), text("from a"))])).into_result().unwrap();
    let (r2, _) = b.edit(|t| t.add_row(&f.table, Placement::Last, &[(f.name.clone(), text("from b"))])).into_result().unwrap();
    sync(&mut f.a, &mut f.b);
    let (ra, rb) = (a.table(&f.table).unwrap(), b.table(&f.table).unwrap());
    assert_eq!(ra.rows.len(), 2);
    assert_eq!(ra, rb);
    let ids: Vec<_> = ra.rows.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&r1.as_str()) && ids.contains(&r2.as_str()));
}

#[test]
fn concurrent_inserts_in_the_same_gap_keep_both() {
    let mut f = fixture();
    let (ids, u) = Sheets::of_node(&f.a)
        .edit(|t| t.add_rows(&f.table, Placement::Last, &[vec![], vec![]]))
        .into_result()
        .unwrap();
    send(u, &mut f.b);
    let (a, b) = (Sheets::of_node(&f.a), Sheets::of_node(&f.b));
    a.edit(|t| t.add_row(&f.table, Placement::After(ids[0].clone()), &[])).into_result().unwrap();
    b.edit(|t| t.add_row(&f.table, Placement::After(ids[0].clone()), &[])).into_result().unwrap();
    sync(&mut f.a, &mut f.b);
    let (ra, rb) = (a.table(&f.table).unwrap(), b.table(&f.table).unwrap());
    assert_eq!(ra.rows.len(), 4);
    assert_eq!(ra, rb);
    assert_eq!(ra.rows[0].id, ids[0]);
    assert_eq!(ra.rows[3].id, ids[1]);
}

#[test]
fn concurrent_rename_to_one_name_both_survive_reading() {
    let mut f = fixture();
    let (t2, u) = Sheets::of_node(&f.a).edit(|t| t.create_table("Second", Placement::Last)).into_result().unwrap();
    send(u, &mut f.b);
    let (a, b) = (Sheets::of_node(&f.a), Sheets::of_node(&f.b));
    a.edit(|t| t.rename_table(&f.table, "Costs")).into_result().unwrap();
    b.edit(|t| t.rename_table(&t2, "Costs")).into_result().unwrap();
    sync(&mut f.a, &mut f.b);
    for s in [&a, &b] {
        let snap = s.read();
        assert_eq!(snap.tables.len(), 2);
        assert!(snap.tables.iter().all(|t| t.name == "Costs"));
    }
    assert_eq!(a.read(), b.read());
    // Until one is renamed, a third "Costs" is still refused.
    assert!(a.edit(|t| t.create_table("costs", Placement::Last)).result.is_err());
    a.edit(|t| t.rename_table(&t2, "Second")).into_result().unwrap();

    // The same for columns.
    let (c1, u) = a.edit(|t| t.add_column(&f.table, "x", ColumnType::Text, Placement::Last)).into_result().unwrap();
    send(u, &mut f.b);
    let (c2, _) = a.edit(|t| t.add_column(&f.table, "y", ColumnType::Text, Placement::Last)).into_result().unwrap();
    sync(&mut f.a, &mut f.b);
    a.edit(|t| t.rename_column(&f.table, &c1, "same")).into_result().unwrap();
    b.edit(|t| t.rename_column(&f.table, &c2, "same")).into_result().unwrap();
    sync(&mut f.a, &mut f.b);
    let t = a.table(&f.table).unwrap();
    assert_eq!(t.columns.iter().filter(|c| c.name == "same").count(), 2);
    assert_eq!(t, b.table(&f.table).unwrap());
}

#[test]
fn concurrent_moves_converge() {
    let mut f = fixture();
    let (rows, u) = Sheets::of_node(&f.a).edit(|t| t.add_rows(&f.table, Placement::Last, &[vec![], vec![], vec![]])).into_result().unwrap();
    send(u, &mut f.b);
    let (a, b) = (Sheets::of_node(&f.a), Sheets::of_node(&f.b));
    a.edit(|t| t.move_row(&f.table, &rows[0], Placement::Last)).into_result().unwrap();
    b.edit(|t| t.move_row(&f.table, &rows[0], Placement::After(rows[1].clone()))).into_result().unwrap();
    sync(&mut f.a, &mut f.b);
    assert_eq!(a.read(), b.read());
    assert_eq!(a.table(&f.table).unwrap().rows.len(), 3);
}

#[test]
fn deleting_a_row_while_another_types_in_it() {
    let mut f = fixture();
    let (row, u) = Sheets::of_node(&f.a).edit(|t| t.add_row(&f.table, Placement::Last, &[])).into_result().unwrap();
    send(u, &mut f.b);
    let (a, b) = (Sheets::of_node(&f.a), Sheets::of_node(&f.b));
    a.edit(|t| t.delete_row(&f.table, &row)).into_result().unwrap();
    b.edit(|t| t.set_cell(&f.table, &row, &f.name, Some(&text("late")))).into_result().unwrap();
    sync(&mut f.a, &mut f.b);
    assert_eq!(a.read(), b.read());
    assert!(a.table(&f.table).unwrap().rows.is_empty());
}

#[test]
fn diff_since_brings_a_peer_up_to_date() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    let sv = Sheets::of_node(&f.b).doc().transact_state();
    s.edit(|t| t.add_row(&f.table, Placement::Last, &[(f.name.clone(), text("later"))])).into_result().unwrap();
    let diff = s.diff_since(&sv);
    let mut b = f.b;
    b.apply_update(&diff).unwrap();
    assert_eq!(Sheets::of_node(&b).read(), s.read());
}

trait StateVectorOf {
    fn transact_state(&self) -> yrs::StateVector;
}
impl StateVectorOf for yrs::Doc {
    fn transact_state(&self) -> yrs::StateVector {
        use yrs::{ReadTxn, Transact};
        self.transact().state_vector()
    }
}

// ── Undo, watch ──────────────────────────────────────────────────────────

#[test]
fn undo_takes_back_my_edits_and_never_a_collaborators() {
    let mut f = fixture();
    let mine = Sheets::of_node(&f.a).with_origin("joe-device");
    let mut undo = mine.undo_manager(yrs::undo::Options::default());
    let (my_row, u) = mine.edit(|t| t.add_row(&f.table, Placement::Last, &[(f.name.clone(), text("mine"))])).into_result().unwrap();
    send(u, &mut f.b);
    let (their_row, u) = Sheets::of_node(&f.b).edit(|t| t.add_row(&f.table, Placement::Last, &[(f.name.clone(), text("theirs"))])).into_result().unwrap();
    send(u, &mut f.a);
    let update = mine.undo(&mut undo, false).expect("something to undo");
    f.b.apply_update(&update).unwrap();
    for d in [&f.a, &f.b] {
        let t = Sheets::of_node(d).table(&f.table).unwrap();
        assert!(t.row(&my_row).is_none());
        assert!(t.row(&their_row).is_some());
    }
    let update = mine.undo(&mut undo, true).expect("something to redo");
    f.b.apply_update(&update).unwrap();
    assert_eq!(Sheets::of_node(&f.b).table(&f.table).unwrap().rows.len(), 2);
    assert!(mine.undo(&mut undo, true).is_none());
}

#[test]
fn undo_takes_back_a_cell_edit() {
    let f = fixture();
    let s = Sheets::of_node(&f.a).with_origin("me");
    let (row, _) = s.edit(|t| t.add_row(&f.table, Placement::Last, &[])).into_result().unwrap();
    let mut undo = s.undo_manager(Default::default());
    undo.reset();
    s.edit(|t| t.set_cell(&f.table, &row, &f.price, Some(&num(5.0)))).into_result().unwrap();
    s.undo(&mut undo, false).unwrap();
    assert!(s.table(&f.table).unwrap().rows[0].cells.is_empty());
}

#[test]
fn watch_names_the_tables_that_changed() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    let (t2, _) = s.edit(|t| t.create_table("Other", Placement::Last)).into_result().unwrap();
    let w = s.watch();
    assert!(w.take().is_empty());
    s.edit(|t| t.add_row(&f.table, Placement::Last, &[(f.name.clone(), text("x"))])).into_result().unwrap();
    let c = w.take();
    assert!(!c.all && c.tables == [f.table.clone()].into_iter().collect());
    assert!(w.take().is_empty());
    s.edit(|t| t.rename_table(&t2, "Renamed")).into_result().unwrap();
    assert_eq!(w.take().tables, [t2.clone()].into_iter().collect());
    s.edit(|t| t.delete_table(&t2)).into_result().unwrap();
    assert!(w.take().tables.contains(&t2));
    // A remote change is reported too.
    let (_, u) = Sheets::of_node(&f.b).edit(|t| t.set_header(&f.table, &ResultKey::Pos(0), Some("h"))).into_result().unwrap();
    let mut a = f.a;
    send(u, &mut a);
    assert_eq!(w.take().tables, [f.table.clone()].into_iter().collect());
}

// ── Tolerance ────────────────────────────────────────────────────────────

#[test]
fn malformed_data_is_skipped_never_a_panic() {
    use std::sync::Arc;
    use yrs::{Any, Map, MapPrelim, MapRef, Out, Transact};
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    {
        let mut txn = s.doc().transact_mut();
        let sheets = s.root().clone();
        // An entry that is not a table, a table with nothing in it, a table
        // whose parts have the wrong types.
        sheets.insert(&mut txn, "sheet:junk", 5.0f64);
        sheets.insert(&mut txn, "sheet:bare", MapPrelim::default());
        let weird: MapRef = sheets.insert(&mut txn, "sheet:weird", MapPrelim::default());
        weird.insert(&mut txn, "name", 7.0f64);
        weird.insert(&mut txn, "order", true);
        weird.insert(&mut txn, "columns", "not a map");
        weird.insert(&mut txn, "rows", Any::Array(Arc::from(vec![Any::Null])));
        weird.insert(&mut txn, "headers", MapPrelim::from([("x".to_string(), Any::from("no")), ("3".to_string(), Any::from("ok")), ("4".to_string(), Any::from(5.0f64))]));
        // The real table: a column with an unknown type, one without a name,
        // rows with bad cells.
        let Some(Out::YMap(t)) = sheets.get(&txn, &table_key(&f.table)) else { panic!() };
        let Some(Out::YMap(cols)) = t.get(&txn, "columns") else { panic!() };
        let c: MapRef = cols.insert(&mut txn, "oddtype", MapPrelim::default());
        c.insert(&mut txn, "name", "odd");
        c.insert(&mut txn, "type", "hologram");
        cols.insert(&mut txn, "nameless", MapPrelim::default());
        cols.insert(&mut txn, "scalar", 3.0f64);
        let rf: MapRef = cols.insert(&mut txn, "badlink", MapPrelim::default());
        rf.insert(&mut txn, "type", "node_link");
        rf.insert(&mut txn, "under", Any::Array(Arc::from(vec![Any::from("not-a-uuid"), Any::from("x")])));
        let Some(Out::YMap(rows)) = t.get(&txn, "rows") else { panic!() };
        rows.insert(&mut txn, "scalarrow", "nope");
        let r: MapRef = rows.insert(&mut txn, "badcells", MapPrelim::default());
        r.insert(&mut txn, "order", 5.0f64);
        let cells: MapRef = r.insert(&mut txn, "cells", MapPrelim::default());
        cells.insert(&mut txn, f.name.as_str(), true);
        cells.insert(&mut txn, f.price.as_str(), MapPrelim::from([("date".to_string(), Any::from(5.0f64))]));
        cells.insert(&mut txn, "oddtype", MapPrelim::from([("node".to_string(), Any::from("x"))]));
        let r2: MapRef = rows.insert(&mut txn, "cellsnotmap", MapPrelim::default());
        r2.insert(&mut txn, "cells", 5.0f64);
    }
    let snap = s.read();
    let weird = snap.table("weird").expect("a table with odd contents still reads");
    assert_eq!(weird.name, "");
    assert!(weird.columns.is_empty() && weird.rows.is_empty());
    assert_eq!(weird.headers.len(), 1);
    assert!(snap.table("junk").is_none() && snap.table("bare").is_some());
    let t = snap.table(&f.table).unwrap();
    assert_eq!(t.column("oddtype").unwrap().ty, ColumnType::Text);
    assert_eq!(t.column("badlink").unwrap().ty, ColumnType::NodeLink { under: None });
    assert!(t.column("scalar").is_none());
    assert!(t.row("scalarrow").is_none());
    assert!(t.row("badcells").unwrap().cells.is_empty());
    assert!(t.row("cellsnotmap").unwrap().cells.is_empty());
    // And writes still work against it.
    s.edit(|t| t.add_column("weird", "ok", ColumnType::Text, Placement::Last)).into_result().unwrap();
    s.edit(|t| t.add_row(&f.table, Placement::Last, &[(f.name.clone(), text("fine"))])).into_result().unwrap();
    s.edit(|t| t.add_row("bare", Placement::First, &[])).into_result().unwrap();
    assert_eq!(s.table("bare").unwrap().rows.len(), 1);
}

fn first_table(doc: &NodeDoc, name: &str, extra_col: &str) -> (String, String, Vec<u8>) {
    let s = Sheets::of_node(doc);
    let ((table, col), update) = s
        .edit(|t| {
            let table = t.create_table(name, Placement::Last)?;
            let col = t.add_column(&table, extra_col, ColumnType::Text, Placement::Last)?;
            t.add_row(&table, Placement::Last, &[(col.clone(), text(name))])?;
            Ok((table, col))
        })
        .into_result()
        .unwrap();
    (table, col, update.unwrap())
}

#[test]
fn first_tables_made_offline_on_two_devices_both_survive() {
    let mut a = node();
    let mut b = peer(&a);
    let (ta, ca, ua) = first_table(&a, "Alpha", "x");
    let (tb, cb, ub) = first_table(&b, "Beta", "y");
    // Both ways, by update and by full sync.
    a.apply_update(&ub).unwrap();
    b.apply_update(&ua).unwrap();
    sync(&mut a, &mut b);
    for d in [&a, &b] {
        let snap = Sheets::of_node(d).read();
        assert_eq!(snap.tables.len(), 2);
        let (x, y) = (snap.table(&ta).unwrap(), snap.table(&tb).unwrap());
        assert_eq!((x.name.as_str(), x.columns[0].id.as_str(), x.rows.len()), ("Alpha", ca.as_str(), 1));
        assert_eq!((y.name.as_str(), y.columns[0].id.as_str(), y.rows.len()), ("Beta", cb.as_str(), 1));
        assert_eq!(x.rows[0].cells[&ca], text("Alpha"));
    }
    assert_eq!(Sheets::of_node(&a).read(), Sheets::of_node(&b).read());
}

#[test]
fn a_first_table_and_a_second_made_apart_both_survive() {
    let mut a = node();
    // b already has one table; a does not and makes its own offline.
    let (t0, _, u0) = first_table(&a, "Zero", "z");
    let mut b = peer(&node());
    b.apply_update(&u0).unwrap();
    let mut c = node();
    let (t1, _, u1) = first_table(&c, "One", "o");
    let (t2, _, u2) = first_table(&b, "Two", "w");
    a.apply_update(&u1).unwrap();
    let _ = (&mut c, u2.len());
    b.apply_update(&u1).unwrap();
    a.apply_update(&u2).unwrap();
    sync(&mut a, &mut b);
    for d in [&a, &b] {
        let snap = Sheets::of_node(d).read();
        let ids: std::collections::HashSet<_> = snap.tables.iter().map(|t| t.id.clone()).collect();
        assert_eq!(ids, [t0.clone(), t1.clone(), t2.clone()].into_iter().collect());
        assert!(snap.tables.iter().all(|t| t.columns.len() == 1 && t.rows.len() == 1));
    }
}

#[test]
fn other_keys_of_the_data_root_are_left_alone() {
    use yrs::{Map, MapPrelim, Transact};
    let doc = node();
    let s = Sheets::of_node(&doc);
    {
        let mut txn = s.doc().transact_mut();
        s.root().insert(&mut txn, "board", MapPrelim::default());
        s.root().insert(&mut txn, "boards", MapPrelim::default());
        s.root().insert(&mut txn, "x", 5.0f64);
        s.root().insert(&mut txn, "sheetx", MapPrelim::default());
    }
    assert!(!s.exists());
    assert!(s.read().tables.is_empty());
    let w = s.watch();
    let (id, _) = s.edit(|t| t.create_table("T", Placement::Last)).into_result().unwrap();
    assert!(s.exists());
    assert_eq!(s.read().tables.len(), 1);
    assert_eq!(w.take().tables, [id.clone()].into_iter().collect());
    // A change to someone else's key is not a table change.
    {
        let mut txn = s.doc().transact_mut();
        s.root().insert(&mut txn, "x", 6.0f64);
    }
    assert!(w.take().is_empty());
    s.edit(|t| t.delete_table(&id)).into_result().unwrap();
    assert!(!s.exists());
    use yrs::Out;
    let txn = s.doc().transact();
    assert!(matches!(s.root().get(&txn, "board"), Some(Out::YMap(_))));
}

/// A table copied into another node (Convert to Sheet Node) keeps its ids,
/// columns, rows and summary, so its formulas still read.
#[test]
fn a_copied_table_keeps_its_ids_and_contents() {
    let f = fixture();
    let source = Sheets::of_node(&f.a).table(&f.table).unwrap();
    let dest = Sheets::standalone();
    let copied = dest.edit(|t| t.copy_table(&source));
    copied.result.unwrap();
    let back = dest.table(&f.table).expect("the copy, under the same id");
    assert_eq!(back.name, source.name);
    assert_eq!(back.columns, source.columns);
    assert_eq!(back.rows, source.rows);
    // Twice is refused: the id (and the name) are taken.
    assert!(dest.edit(|t| t.copy_table(&source)).result.is_err());
}

// ── Summary rows and groups (contract section 4b) ───────────────────────

#[test]
fn summary_rows_are_named_and_kept_apart_from_rows() {
    let mut f = fixture();
    let s = Sheets::of_node(&f.a);
    let ((first, second), update) = s
        .edit(|t| {
            let first = t.add_summary_row(&f.table, None, &[(f.price.clone(), SummaryCell::Sum)], Placement::Last)?;
            let second = t.add_summary_row(&f.table, None, &[], Placement::Last)?;
            Ok((first, second))
        })
        .into_result()
        .unwrap();
    send(update, &mut f.b);
    let snap = Sheets::of_node(&f.b).table(&f.table).unwrap();
    let names: Vec<&str> = snap.summary_rows.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Total", "Total 2"], "the next free Total");
    assert_eq!(snap.summary_rows[0].cells.get(&f.price), Some(&SummaryCell::Sum));
    assert!(snap.rows.is_empty(), "a summary row is not a row");

    let s = Sheets::of_node(&f.a);
    let refused = s.edit(|t| t.rename_summary_row(&f.table, &second, "total")).result.unwrap_err();
    assert!(matches!(refused, SheetError::DuplicateSummaryRow { .. }), "{refused}");
    let refused = s.edit(|t| t.rename_summary_row(&f.table, &second, "This Row")).result.unwrap_err();
    assert!(matches!(refused, SheetError::ReservedSummaryRowName(_)), "{refused}");
    let (_, update) = s
        .edit(|t| {
            t.rename_summary_row(&f.table, &second, "Average")?;
            t.set_summary_cell(&f.table, &second, &f.price, Some(&SummaryCell::Formula("SUM({c:x})".into())))?;
            t.move_summary_row(&f.table, &second, Placement::First)?;
            t.set_summary_cell(&f.table, &first, &f.price, None)
        })
        .into_result()
        .unwrap();
    send(update, &mut f.b);
    let snap = Sheets::of_node(&f.b).table(&f.table).unwrap();
    assert_eq!(snap.summary_rows[0].name, "Average", "moved first");
    assert_eq!(snap.summary_rows[0].cells.get(&f.price), Some(&SummaryCell::Formula("SUM({c:x})".into())));
    assert!(snap.summary_rows[1].cells.is_empty(), "a cell taken back shows nothing");

    let (_, update) = s.edit(|t| t.delete_summary_row(&f.table, &first)).into_result().unwrap();
    send(update, &mut f.b);
    assert_eq!(Sheets::of_node(&f.b).table(&f.table).unwrap().summary_rows.len(), 1);
}

#[test]
fn grouping_and_a_deleted_column() {
    let mut f = fixture();
    let s = Sheets::of_node(&f.a);
    let (_, update) = s
        .edit(|t| {
            t.set_group_by(&f.table, Some(&f.name))?;
            t.add_summary_row(&f.table, None, &[(f.name.clone(), SummaryCell::Count), (f.price.clone(), SummaryCell::Sum)], Placement::Last)
        })
        .into_result()
        .unwrap();
    send(update, &mut f.b);
    assert_eq!(Sheets::of_node(&f.b).table(&f.table).unwrap().group_by.as_deref(), Some(f.name.as_str()));
    // The grouping column goes, and its grouping and summary cells with it.
    let (_, update) = s.edit(|t| t.delete_column(&f.table, &f.name)).into_result().unwrap();
    send(update, &mut f.b);
    let snap = Sheets::of_node(&f.b).table(&f.table).unwrap();
    assert_eq!(snap.group_by, None);
    assert_eq!(snap.summary_rows[0].cells.keys().collect::<Vec<_>>(), [&f.price]);
    let (_, update) = s.edit(|t| t.set_group_by(&f.table, None)).into_result().unwrap();
    assert!(update.is_none() || Sheets::of_node(&f.a).table(&f.table).unwrap().group_by.is_none());
}

#[test]
fn two_devices_adding_a_tables_first_summary_rows_apart_keep_both() {
    let mut f = fixture();
    let (_, ua) = Sheets::of_node(&f.a).edit(|t| t.add_summary_row(&f.table, Some("Total"), &[(f.price.clone(), SummaryCell::Sum)], Placement::Last)).into_result().unwrap();
    let (_, ub) = Sheets::of_node(&f.b).edit(|t| t.add_summary_row(&f.table, Some("Average"), &[(f.price.clone(), SummaryCell::Average)], Placement::Last)).into_result().unwrap();
    send(ua, &mut f.b);
    send(ub, &mut f.a);
    for doc in [&f.a, &f.b] {
        let snap = Sheets::of_node(doc).table(&f.table).unwrap();
        let mut names: Vec<&str> = snap.summary_rows.iter().map(|s| s.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["Average", "Total"]);
    }
}

#[test]
fn a_copied_table_keeps_its_summary_rows_and_grouping() {
    let f = fixture();
    let s = Sheets::of_node(&f.a);
    s.edit(|t| {
        t.set_group_by(&f.table, Some(&f.name))?;
        t.add_summary_row(&f.table, None, &[(f.price.clone(), SummaryCell::Sum)], Placement::Last)
    })
    .into_result()
    .unwrap();
    let original = s.table(&f.table).unwrap();
    let other = node();
    Sheets::of_node(&other).edit(|t| t.copy_table(&original)).into_result().unwrap();
    let copy = Sheets::of_node(&other).table(&f.table).unwrap();
    assert_eq!(copy.summary_rows, original.summary_rows);
    assert_eq!(copy.group_by, original.group_by);
}

#[test]
fn a_table_is_rotated_and_turned_back() {
    let mut f = fixture();
    assert!(!Sheets::of_node(&f.b).table(&f.table).unwrap().rotated);
    let (_, update) = Sheets::of_node(&f.a).edit(|t| t.set_rotated(&f.table, true)).into_result().unwrap();
    send(update, &mut f.b);
    let rotated = Sheets::of_node(&f.b).table(&f.table).unwrap();
    assert!(rotated.rotated, "everyone sees it rotated");
    let other = node();
    Sheets::of_node(&other).edit(|t| t.copy_table(&rotated)).into_result().unwrap();
    assert!(Sheets::of_node(&other).table(&f.table).unwrap().rotated, "and a copy is too");
    let (_, update) = Sheets::of_node(&f.a).edit(|t| t.set_rotated(&f.table, false)).into_result().unwrap();
    send(update, &mut f.b);
    assert!(!Sheets::of_node(&f.b).table(&f.table).unwrap().rotated);
}
