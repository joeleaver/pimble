//! Moving a note from positional result columns to keyed ones (contract
//! section 4, "A result column's key").
//!
//! Formulas and headers written before result columns had keys name them by
//! position (`{r:<n>}`, a `headers` entry `"<n>"`). Those keep working until a
//! summary's formula changes; then a position would point at whatever column
//! the new formula puts there. So before any summary's formula changes, the
//! note's own legacy uses are re-stored by key, against the formula as it
//! still is, in the same edit as the change.

use crate::book::Book;
use crate::doc::SheetsTxn;
use crate::model::{ColumnType, ResultKey};

use super::engine::Engine;
use super::names::{to_shown_with, to_stored_with};

/// What re-storing a note's legacy result-column references changes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Rekey {
    /// `(table, new source)`.
    pub sources: Vec<(String, String)>,
    /// `(table, column, new type)` for formula columns.
    pub formulas: Vec<(String, String, ColumnType)>,
    /// `(table, old key, new key, header)`.
    pub headers: Vec<(String, ResultKey, ResultKey, String)>,
}

impl Rekey {
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.formulas.is_empty() && self.headers.is_empty()
    }

    /// Write it, in the caller's edit.
    pub fn apply(&self, t: &mut SheetsTxn<'_, '_>) -> crate::doc::Result<()> {
        for (table, source) in &self.sources {
            t.set_source(table, Some(source))?;
        }
        for (table, column, ty) in &self.formulas {
            t.set_column_type(table, column, ty.clone())?;
        }
        for (table, old, new, header) in &self.headers {
            t.set_header(table, old, None)?;
            t.set_header(table, new, Some(header))?;
        }
        Ok(())
    }
}

fn legacy(stored: &str) -> bool {
    stored.contains("{r:")
}

/// The changes that name the home note's result columns by key wherever its
/// own formulas and headers still name them by position. A formula whose
/// names cannot be worked out now is left as it is.
pub fn rekey_note(book: &dyn Book) -> Rekey {
    let en = Engine::new(book);
    let home = book.home();
    let mut out = Rekey::default();
    let Ok(snap) = en.snap(&home) else {
        return out;
    };
    let restore = |table: &str, stored: &str| -> Option<String> {
        let shown = to_shown_with(&en, &home, Some(table), stored);
        let again = to_stored_with(&en, &home, Some(table), &shown).ok()?;
        (again != stored).then_some(again)
    };
    for t in &snap.tables {
        if let Some(src) = t.source.as_deref().filter(|s| legacy(s)) {
            if let Some(new) = restore(&t.id, src) {
                out.sources.push((t.id.clone(), new));
            }
        }
        for c in &t.columns {
            if let ColumnType::Formula { formula, format } = &c.ty {
                if legacy(formula) {
                    if let Some(new) = restore(&t.id, formula) {
                        out.formulas.push((t.id.clone(), c.id.clone(), ColumnType::Formula { formula: new, format: *format }));
                    }
                }
            }
        }
        if t.is_summary() && t.headers.keys().any(|k| matches!(k, ResultKey::Pos(_))) {
            if let Ok(s) = &*en.summary(&home, t) {
                for (k, header) in &t.headers {
                    let ResultKey::Pos(j) = k else { continue };
                    let Some(key) = s.keys.get(*j) else { continue };
                    let new = ResultKey::Key(key.clone());
                    if !t.headers.contains_key(&new) {
                        out.headers.push((t.id.clone(), k.clone(), new, header.clone()));
                    }
                }
            }
        }
    }
    out
}
