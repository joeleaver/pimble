//! What search finds in a node's tables (docs/SHEETS_CONTRACT.md section 10):
//! the typed cells, as table rows with their column names, so "Lake Tahoe"
//! finds the trip. Computed values (formula columns, summaries) are derived
//! and would go stale, so they are not indexed.

use pimble_core::{IndexUnit, UnitKind};
use pimble_crdt::NodeDoc;

use crate::doc::Sheets;
use crate::input;
use crate::model::{ColumnType, SheetsSnap, Stored};

/// The index units of a node's tables, in table order: per table its name
/// (a field unit, which also ends the previous table's run of rows), a
/// header row of its typed columns' names, and one row per row.
pub fn index_units(doc: &NodeDoc) -> Vec<IndexUnit> {
    units_of(&Sheets::of_node(doc).read())
}

/// [`index_units`] of a snapshot.
pub fn units_of(snap: &SheetsSnap) -> Vec<IndexUnit> {
    let mut out = Vec::new();
    for t in &snap.tables {
        if t.source.is_some() {
            continue;
        }
        let typed: Vec<_> = t.columns.iter().filter(|c| !matches!(c.ty, ColumnType::Formula { .. })).collect();
        out.push(IndexUnit::new(UnitKind::Field("table".to_string()), format!("sheet:{}", t.id), t.name.clone()));
        if typed.is_empty() || t.rows.is_empty() {
            continue;
        }
        out.push(IndexUnit::new(
            UnitKind::TableRow,
            format!("sheet:{}/h", t.id),
            typed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(" | "),
        ));
        for r in &t.rows {
            let label = |s: &Stored| match s {
                // A linked row reads as its first column, as it shows.
                Stored::Row(id) => match &snap_link(snap, &typed, id) {
                    Some(text) => text.clone(),
                    None => String::new(),
                },
                _ => String::new(),
            };
            let cells: Vec<String> = typed
                .iter()
                .filter_map(|c| {
                    let v = r.cells.get(&c.id)?;
                    let text = input::edit_text(v, &c.ty, &label);
                    (!text.trim().is_empty()).then(|| format!("{}: {}", c.name, text))
                })
                .collect();
            if !cells.is_empty() {
                out.push(IndexUnit::new(UnitKind::TableRow, format!("sheet:{}/{}", t.id, r.id), cells.join(" | ")));
            }
        }
    }
    out
}

/// The nodes the typed cells of a node's tables link to (link-to-a-node
/// columns), each once: links of the node for the index, so a node's
/// backlinks include the rows that point at it (contract section 8).
pub fn linked_nodes(doc: &NodeDoc) -> Vec<crate::model::NodeRef> {
    let mut out = Vec::new();
    for t in Sheets::of_node(doc).read().tables {
        for r in &t.rows {
            for v in r.cells.values() {
                if let Stored::Node(n) = v {
                    if !out.contains(n) {
                        out.push(*n);
                    }
                }
            }
        }
    }
    out
}

/// The first cell of row `row` of whichever table a row link of `typed`
/// points at (a row id is unique, so the first match is it).
fn snap_link(snap: &SheetsSnap, typed: &[&crate::model::ColumnSnap], row: &str) -> Option<String> {
    for c in typed {
        let ColumnType::RowLink { table } = &c.ty else { continue };
        let t = snap.table(table)?;
        let r = t.row(row)?;
        let first = t.columns.first()?;
        return r.cells.get(&first.id).map(|v| input::edit_text(v, &first.ty, &|_| String::new()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Placement;

    #[test]
    fn typed_cells_index_as_rows_and_formulas_do_not() {
        let doc = NodeDoc::new();
        let sheets = Sheets::of_node(&doc);
        sheets
            .edit(|t| {
                let veh = t.create_table("Vehicles", Placement::Last)?;
                let name = t.add_column(&veh, "name", ColumnType::Text, Placement::Last)?;
                let civic = t.add_row(&veh, Placement::Last, &[(name, Stored::Text("Civic".into()))])?;
                let trips = t.create_table("Trips", Placement::Last)?;
                let to = t.add_column(&trips, "to", ColumnType::Text, Placement::Last)?;
                let miles = t.add_column(&trips, "miles", ColumnType::Number, Placement::Last)?;
                let car = t.add_column(&trips, "car", ColumnType::RowLink { table: veh }, Placement::Last)?;
                t.add_column(&trips, "cost", ColumnType::Formula { formula: "=1".into(), format: crate::Format::Auto }, Placement::Last)?;
                t.add_row(&trips, Placement::Last, &[(to, Stored::Text("Lake Tahoe".into())), (miles, Stored::Number(212.0)), (car, Stored::Row(civic))])?;
                Ok(())
            })
            .result
            .unwrap();
        let units = index_units(&doc);
        let texts: Vec<&str> = units.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(texts, ["Vehicles", "name", "name: Civic", "Trips", "to | miles | car", "to: Lake Tahoe | miles: 212 | car: Civic"]);
        assert_eq!(units[5].kind, UnitKind::TableRow);
    }
}
