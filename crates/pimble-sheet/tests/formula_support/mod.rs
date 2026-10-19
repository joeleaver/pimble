//! A [`Book`] over plain values for the formula tests, and Joe's three
//! examples (docs/SHEETS_CONTRACT.md) built as notes in it.

#![allow(dead_code)]

use std::collections::BTreeMap;

use pimble_core::{NodeId, StoreId};
use pimble_sheet::formula::{self, Engine, Value};
use pimble_sheet::{
    Book, ColumnSnap, ColumnType, Format, NodeInfo, NodeRef, Reach, RowSnap, SheetsSnap, Stored,
    TableSnap,
};
use uuid::Uuid;

/// 2026-10-19, a Monday.
pub const TODAY: f64 = 46_314.0;

pub fn node(n: u128) -> NodeRef {
    NodeRef {
        store: StoreId(Uuid::from_u128(0xabc)),
        node: NodeId(Uuid::from_u128(n)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum R {
    Ready,
    Deleted,
    NoAccess,
    Loading,
}

#[derive(Debug, Clone)]
pub struct Note {
    pub node: NodeRef,
    pub title: String,
    pub parent: Option<NodeRef>,
    pub fields: Vec<(String, String)>,
    pub sheets: SheetsSnap,
    pub reach: R,
}

#[derive(Debug, Clone)]
pub struct TestBook {
    pub home: NodeRef,
    pub today: f64,
    pub notes: Vec<Note>,
}

fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl TestBook {
    pub fn new(home: u128) -> Self {
        TestBook {
            home: node(home),
            today: TODAY,
            notes: Vec::new(),
        }
    }

    pub fn add(&mut self, id: u128, title: &str, parent: Option<u128>) -> &mut Note {
        self.notes.push(Note {
            node: node(id),
            title: title.to_string(),
            parent: parent.map(node),
            fields: Vec::new(),
            sheets: SheetsSnap::default(),
            reach: R::Ready,
        });
        self.notes.last_mut().unwrap()
    }

    pub fn note(&mut self, id: u128) -> &mut Note {
        let n = node(id);
        self.notes.iter_mut().find(|x| x.node == n).expect("note")
    }

    pub fn at(&self, home: u128) -> TestBook {
        TestBook {
            home: node(home),
            ..self.clone()
        }
    }

    pub fn table_mut(&mut self, note: u128, table: &str) -> &mut TableSnap {
        self.note(note)
            .sheets
            .tables
            .iter_mut()
            .find(|t| t.id == table)
            .expect("table")
    }

    /// Turns every formula and summary source, written in shown form, into
    /// its stored form, note by note.
    pub fn store_formulas(&mut self) {
        let notes: Vec<NodeRef> = self
            .notes
            .iter()
            .filter(|n| n.reach == R::Ready)
            .map(|n| n.node)
            .collect();
        for n in notes {
            let view = TestBook {
                home: n,
                ..self.clone()
            };
            let mut out = Vec::new();
            let note = view.notes.iter().find(|x| x.node == n).unwrap();
            for t in &note.sheets.tables {
                let source = t.source.as_ref().map(|s| {
                    formula::to_stored(&view, None, s).unwrap_or_else(|e| panic!("{s}: {e:?}"))
                });
                let cols: Vec<Option<String>> = t
                    .columns
                    .iter()
                    .map(|c| match &c.ty {
                        ColumnType::Formula { formula: f, .. } => Some(
                            formula::to_stored(&view, Some(&t.id), f)
                                .unwrap_or_else(|e| panic!("{f}: {e:?}")),
                        ),
                        _ => None,
                    })
                    .collect();
                out.push((t.id.clone(), source, cols));
            }
            let note = self.notes.iter_mut().find(|x| x.node == n).unwrap();
            for (tid, source, cols) in out {
                let t = note.sheets.tables.iter_mut().find(|t| t.id == tid).unwrap();
                if source.is_some() {
                    t.source = source;
                }
                for (c, f) in t.columns.iter_mut().zip(cols) {
                    if let (ColumnType::Formula { formula, .. }, Some(f)) = (&mut c.ty, f) {
                        *formula = f;
                    }
                }
            }
        }
    }

    fn ancestors_match(&self, n: &Note, parts: &[&str]) -> bool {
        let mut cur = n.parent;
        for p in parts.iter().rev() {
            let Some(c) = cur else { return false };
            let Some(pn) = self.notes.iter().find(|x| x.node == c) else {
                return false;
            };
            if norm(&pn.title) != norm(p) {
                return false;
            }
            cur = pn.parent;
        }
        true
    }

    fn path(&self, n: &Note, depth: usize) -> String {
        let mut parts = vec![n.title.clone()];
        let mut cur = n.parent;
        for _ in 0..depth {
            let Some(c) = cur else { break };
            let Some(pn) = self.notes.iter().find(|x| x.node == c) else {
                break;
            };
            parts.insert(0, pn.title.clone());
            cur = pn.parent;
        }
        parts.join("/")
    }
}

impl Book for TestBook {
    fn home(&self) -> NodeRef {
        self.home
    }

    fn sheets(&self, node: &NodeRef) -> Reach<'_> {
        match self.notes.iter().find(|n| n.node == *node) {
            None => Reach::Missing,
            Some(n) => match n.reach {
                R::Ready => Reach::Ready(&n.sheets),
                R::Deleted => Reach::Deleted,
                R::NoAccess => Reach::NoAccess,
                R::Loading => Reach::Loading,
            },
        }
    }

    fn node(&self, node: &NodeRef) -> Option<NodeInfo> {
        let n = self.notes.iter().find(|n| n.node == *node)?;
        match n.reach {
            R::Ready | R::Deleted => Some(NodeInfo {
                title: n.title.clone(),
                fields: n.fields.clone(),
                parent: n.parent,
            }),
            _ => None,
        }
    }

    fn find_nodes(&self, name_or_path: &str) -> Vec<NodeRef> {
        let parts: Vec<&str> = name_or_path.split('/').collect();
        let (last, before) = parts.split_last().unwrap();
        self.notes
            .iter()
            .filter(|n| norm(&n.title) == norm(last) && self.ancestors_match(n, before))
            .map(|n| n.node)
            .collect()
    }

    fn display_name(&self, node: &NodeRef) -> String {
        let Some(n) = self.notes.iter().find(|x| x.node == *node) else {
            return String::new();
        };
        for depth in 0..8 {
            let p = self.path(n, depth);
            if self.find_nodes(&p).len() <= 1 {
                return p;
            }
        }
        n.title.clone()
    }

    fn today(&self) -> f64 {
        self.today
    }
}

// ── Building tables ─────────────────────────────────────────────────────

pub fn n(x: f64) -> Stored {
    Stored::Number(x)
}
pub fn s(x: &str) -> Stored {
    Stored::Text(x.to_string())
}
pub fn d(x: &str) -> Stored {
    Stored::Date(x.to_string())
}
pub fn r(id: &str) -> Stored {
    Stored::Row(id.to_string())
}
pub fn u(x: &str) -> Stored {
    Stored::Unparsed(x.to_string())
}

/// The id a builder gives a column: `<table id>.<name>`.
pub fn cid(table: &str, name: &str) -> String {
    format!("{table}.{}", name.replace(' ', "_"))
}

pub struct TB {
    t: TableSnap,
}

impl TB {
    pub fn new(id: &str, name: &str) -> TB {
        TB {
            t: TableSnap {
                id: id.to_string(),
                name: name.to_string(),
                order: String::new(),
                source: None,
                headers: BTreeMap::new(),
                columns: Vec::new(),
                rows: Vec::new(),
            },
        }
    }

    pub fn col(mut self, name: &str, ty: ColumnType) -> TB {
        let order = format!("{:03}", self.t.columns.len());
        self.t.columns.push(ColumnSnap {
            id: cid(&self.t.id, name),
            name: name.to_string(),
            order,
            ty,
        });
        self
    }

    pub fn text(self, name: &str) -> TB {
        self.col(name, ColumnType::Text)
    }
    pub fn number(self, name: &str) -> TB {
        self.col(name, ColumnType::Number)
    }
    pub fn money(self, name: &str) -> TB {
        self.col(name, ColumnType::Money)
    }
    pub fn date(self, name: &str) -> TB {
        self.col(name, ColumnType::Date)
    }
    pub fn hours(self, name: &str) -> TB {
        self.col(name, ColumnType::Hours)
    }
    pub fn link(self, name: &str, table: &str) -> TB {
        self.col(
            name,
            ColumnType::RowLink {
                table: table.to_string(),
            },
        )
    }
    pub fn node_link(self, name: &str, under: Option<NodeRef>) -> TB {
        self.col(name, ColumnType::NodeLink { under })
    }
    pub fn formula(self, name: &str, f: &str, format: Format) -> TB {
        self.col(
            name,
            ColumnType::Formula {
                formula: f.to_string(),
                format,
            },
        )
    }

    pub fn summary(mut self, source: &str, headers: &[&str]) -> TB {
        self.t.source = Some(source.to_string());
        for (i, h) in headers.iter().enumerate() {
            self.t.headers.insert(i, h.to_string());
        }
        self
    }

    /// A row; cells by column name.
    pub fn row(mut self, id: &str, cells: Vec<(&str, Stored)>) -> TB {
        let order = format!("{:03}", self.t.rows.len());
        let mut map = BTreeMap::new();
        for (name, v) in cells {
            map.insert(cid(&self.t.id, name), v);
        }
        self.t.rows.push(RowSnap {
            id: id.to_string(),
            order,
            cells: map,
        });
        self
    }

    pub fn build(self) -> TableSnap {
        self.t
    }
}

// ── Reading results ─────────────────────────────────────────────────────

pub fn table<'a>(tables: &'a [formula::ComputedTable], name: &str) -> &'a formula::ComputedTable {
    tables
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no table {name}"))
}

pub fn cell<'a>(t: &'a formula::ComputedTable, row: &str, col: &str) -> &'a Value {
    let c = t
        .column_index(col)
        .unwrap_or_else(|| panic!("{} has no column {col}", t.name));
    let r = t
        .rows
        .iter()
        .find(|r| r.id == row)
        .unwrap_or_else(|| panic!("{} has no row {row}", t.name));
    &r.values[c]
}

pub fn num(v: &Value) -> f64 {
    match v {
        Value::Number(x) => *x,
        other => panic!("expected a number, got {other:?}"),
    }
}

pub fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9 * a.abs().max(b.abs()).max(1.0)
}

#[track_caller]
pub fn assert_num(v: &Value, x: f64) {
    let got = num(v);
    assert!(close(got, x), "expected {x}, got {got}");
}

#[track_caller]
pub fn assert_err(v: &Value, code: &str) -> String {
    match v {
        Value::Error(e) => {
            assert_eq!(e.code.code(), code, "{}", e.message);
            e.message.clone()
        }
        other => panic!("expected {code}, got {other:?}"),
    }
}

pub fn shown(en: &Engine, v: &Value, f: Format) -> String {
    en.format_value(v, f)
}

// ── Joe's examples ──────────────────────────────────────────────────────

pub const TRIP: u128 = 10;
pub const BUDGET: u128 = 20;
pub const TIME: u128 = 30;
pub const PROJECTS: u128 = 31;
pub const PIMBLE: u128 = 32;
pub const CLIENT: u128 = 33;
pub const SHED: u128 = 34;

/// An ISO date `days` from [`TODAY`].
pub fn day(days: i64) -> Stored {
    let s = formula::dates::iso(TODAY + days as f64);
    Stored::Date(s)
}

/// The trip cost calculator, the household budget and the time tracker, as
/// three notes, with formulas in shown form (call
/// [`TestBook::store_formulas`] for the stored form).
pub fn examples() -> TestBook {
    let mut b = TestBook::new(TRIP);

    let vehicles = TB::new("veh", "Vehicles")
        .text("name")
        .number("mpg")
        .formula("trips", "=COUNTIF(Trips[vehicle], [@name])", Format::Number)
        .row("truck", vec![("name", s("Truck")), ("mpg", n(17.0))])
        .row("civic", vec![("name", s("Civic")), ("mpg", n(36.0))])
        .row("van", vec![("name", s("Van")), ("mpg", n(22.0))])
        .build();
    let trips = TB::new("trips", "Trips")
        .text("destination")
        .number("miles")
        .link("vehicle", "veh")
        .money("gas price")
        .number("nights")
        .money("hotel rate")
        .formula(
            "gas",
            "=2*[@miles]/[@vehicle].mpg*[@[gas price]]",
            Format::Money,
        )
        .formula("cost", "=[@gas]+[@nights]*[@[hotel rate]]", Format::Money)
        .row(
            "chicago",
            vec![
                ("destination", s("Chicago")),
                ("miles", n(410.0)),
                ("vehicle", r("civic")),
                ("gas price", n(3.29)),
                ("nights", n(1.0)),
                ("hotel rate", n(129.0)),
            ],
        )
        .row(
            "tahoe",
            vec![
                ("destination", s("Lake Tahoe")),
                ("miles", n(620.0)),
                ("vehicle", r("van")),
                ("gas price", n(3.89)),
                ("nights", n(3.0)),
                ("hotel rate", n(189.0)),
            ],
        )
        .row(
            "grandma",
            vec![
                ("destination", s("Grandma's")),
                ("miles", n(140.0)),
                ("vehicle", r("truck")),
                ("gas price", n(3.45)),
                ("nights", n(0.0)),
                ("hotel rate", n(0.0)),
            ],
        )
        .build();
    let trip = b.add(TRIP, "Trip cost", None);
    trip.sheets.tables = vec![vehicles, trips];

    let cats = TB::new("cats", "Categories")
        .text("name")
        .money("monthly budget")
        .formula(
            "spent",
            "=SUMIFS(Transactions[amount], Transactions[category], [@name], Transactions[date], \">\"&EOMONTH(TODAY(),-1))",
            Format::Money,
        )
        .formula("left", "=[@[monthly budget]]-[@spent]", Format::Money)
        .row("groceries", vec![("name", s("Groceries")), ("monthly budget", n(650.0))])
        .row("gas", vec![("name", s("Gas")), ("monthly budget", n(220.0))])
        .row("eating", vec![("name", s("Eating out")), ("monthly budget", n(180.0))])
        .row("utilities", vec![("name", s("Utilities")), ("monthly budget", n(310.0))])
        .row("kids", vec![("name", s("Kids")), ("monthly budget", n(150.0))])
        .build();
    let tx_rows: &[(&str, f64, &str, &str)] = &[
        ("2026-09-04", 188.20, "groceries", "Costco"),
        ("2026-09-08", 61.30, "gas", ""),
        ("2026-09-12", 205.90, "utilities", "Electric"),
        ("2026-09-20", 72.15, "eating", "Anniversary"),
        ("2026-09-25", 131.44, "groceries", ""),
        ("2026-09-27", 45.00, "kids", "Field trip"),
        ("2026-10-02", 142.18, "groceries", "Costco"),
        ("2026-10-03", 46.20, "gas", "Shell"),
        ("2026-10-05", 38.75, "eating", "Tacos"),
        ("2026-10-06", 212.40, "utilities", "Electric"),
        ("2026-10-09", 96.33, "groceries", "Kroger"),
        ("2026-10-11", 64.00, "kids", "Soccer fees"),
        ("2026-10-12", 51.10, "gas", "Costco gas"),
        ("2026-10-14", 27.40, "eating", "Pizza"),
        ("2026-10-16", 118.62, "groceries", ""),
    ];
    let mut tx = TB::new("tx", "Transactions")
        .date("date")
        .money("amount")
        .link("category", "cats")
        .text("note");
    for (i, (date, amount, cat, note)) in tx_rows.iter().enumerate() {
        let mut cells = vec![
            ("date", d(date)),
            ("amount", n(*amount)),
            ("category", r(cat)),
        ];
        if !note.is_empty() {
            cells.push(("note", s(note)));
        }
        tx = tx.row(&format!("t{i:02}"), cells);
    }
    let by_month = TB::new("bymonth", "ByMonth")
        .summary(
            "=GROUPBY(HSTACK(TEXT(Transactions[date],\"yyyy-mm\"), Transactions[category]), Transactions[amount], SUM)",
            &["month", "category", "spent"],
        )
        .formula("budget", "=[@category].[monthly budget]", Format::Money)
        .formula("left", "=[@budget]-[@spent]", Format::Money)
        .build();
    let budget = b.add(BUDGET, "Household budget", None);
    budget.sheets.tables = vec![cats, tx.build(), by_month];

    let projects = b.add(PROJECTS, "Projects", None);
    let _ = projects;
    let p = b.add(PIMBLE, "Pimble", Some(PROJECTS));
    p.fields = vec![
        ("rate".into(), "0".into()),
        ("kind".into(), "personal".into()),
    ];
    let c = b.add(CLIENT, "Client website", Some(PROJECTS));
    c.fields = vec![
        ("rate".into(), "95".into()),
        ("client".into(), "Lakeview Dental".into()),
    ];
    let g = b.add(SHED, "Garden shed", Some(PROJECTS));
    g.fields = vec![("rate".into(), "0".into()), ("kind".into(), "house".into())];

    let entry = |days: i64, project: u128, note: &str, hours: f64| {
        vec![
            ("date", day(days)),
            ("project", Stored::Node(node(project))),
            ("note", s(note)),
            ("hours", n(hours)),
        ]
    };
    let entries = TB::new("entries", "Entries")
        .date("date")
        .node_link("project", Some(node(PROJECTS)))
        .text("note")
        .hours("hours")
        .formula("billable", "=[@hours]*[@project].rate", Format::Money)
        .row("e1", entry(-9, SHED, "Foundation", 5.0))
        .row("e2", entry(-8, CLIENT, "Wireframes", 3.0))
        .row("e3", entry(-4, CLIENT, "Call with Dr. Ruiz", 1.5))
        .row("e4", entry(-3, PIMBLE, "Boards in text", 2.0))
        .row("e5", entry(-2, CLIENT, "Booking form", 4.0))
        .row("e6", entry(-1, SHED, "Framing", 3.0))
        .row("e7", entry(0, CLIENT, "Homepage layout", 2.5))
        .row("e8", entry(0, PIMBLE, "Sheets sketch", 1.75))
        .build();
    let week = TB::new("week", "LastWeek")
        .summary(
            "=LET(recent, Entries[date]>TODAY()-7, GROUPBY(FILTER(Entries[project], recent), FILTER(Entries[[hours]:[billable]], recent), SUM))",
            &["project", "hours", "billed"],
        )
        .build();
    let rates = TB::new("rates", "Rates")
        .text("what")
        .formula("value", "='Client website'!rate*2", Format::Money)
        .formula(
            "client",
            "=XLOOKUP(\"Client website\", Entries[project], Entries[project]).client",
            Format::Auto,
        )
        .row("one", vec![("what", s("double rate"))])
        .build();
    let time = b.add(TIME, "Time tracker", None);
    time.sheets.tables = vec![entries, week, rates];
    b
}
