//! "Sheet formulas", the guide an LLM reads before writing a formula
//! (docs/SHEETS_CONTRACT.md section 10, "How the MCP writes formulas"):
//! published as an MCP resource and as the `sheet_formula_guide` tool.

use pimble_sheet::formula::FUNCTIONS;

pub const URI: &str = "pimble://guides/sheet-formulas";
pub const NAME: &str = "Sheet formulas";

const BODY: &str = r#"# Sheet formulas

A Pimble sheet is a small named table in a note: typed columns, ordinary rows, and formula
columns whose one formula runs for every row. The language is Excel's, as it works inside an
Excel Table (structured references). Write formulas exactly as you would there, starting with =.

## Column types

text, number, money, date, hours, link (each cell names a row of another table in the same
note) and formula. Values go in as a person types them: "1,234.5", "$12.50", "2026-10-14",
"Oct 14", "10/14", "today", "yesterday", "1:30", "90m", "1h 30m", and for a link the linked
row's first-column value ("Civic"). Dates compute as Excel serial numbers, so TODAY()-7 and
">"&EOMONTH(TODAY(),-1) work as in Excel. Hours are plain numbers of hours: [@hours]*rate
needs no *24.

## References (Excel Table syntax)

- `[@miles]`, `[@[gas price]]`: this row's value of a column of the formula's own table
  (inner brackets when the name has spaces or symbols).
- `Transactions[amount]`: a whole column of a table in this note.
- `Entries[[hours]:[billable]]`: a range of columns.
- `Vehicles`: a whole table.
- A bare table name means this note only; there are no global table names.
- `Trips[[#Totals],[cost]]`: the cost cell of the table's first summary row (Excel's Total
  Row); `Trips[[#Average],[cost]]`: the summary row named Average.

## Pimble's extension: a dot follows a link

`[@vehicle].mpg` reads the mpg of the row the link column vehicle points at (it means an
XLOOKUP on the linked row). `[@category].[monthly budget]` with brackets for a name with
spaces. It is the one thing that does not paste back into Excel.

## Other notes

Excel's form for another workbook, with a note as the workbook:

- `'Household budget'!Transactions[amount]`: a table in another note.
- `Budget!Transactions[amount]`: a title without spaces needs no quotes.
- `'Home/Budget'!Transactions[amount]`: two notes with one title are told apart by the
  shortest unique path of titles. An ambiguous name is refused with the candidates.
- `'Family/Household budget'!...`: a path can start at a store's name.

## Summary rows and groups

A plain table can sum itself up without a second table: add_summary_row puts a named row
("Total") under its rows, each column showing sum, average, count, min, max or a formula.
In a summary row's formula, `[amount]` (no @) is the rows it sums up: `=SUM([amount])/SUM([nights])`.
group_table groups the rows by a column (a formula column too, like
`=TEXT([@date],"yyyy-mm")` for months), and the summary rows then also show under each
group as subtotals. Prefer these to a summary table for totals and per-group totals.

## Summaries

A summary is a table whose rows come from one dynamic-array formula (set_summary): GROUPBY,
FILTER, SORT, UNIQUE, HSTACK, LET. Its result columns can be renamed, and it can have formula
columns of its own that read the result's columns with [@column].

## Checking

add_column, update_column, set_summary and create_table refuse a formula that does not
parse, names something that does not exist, gives a list where one value goes (#SPILL!), or
depends on itself (#CIRC!); when they accept one they answer the first computed values.
evaluate runs a formula without saving anything, in a table's context and optionally one
row's: use it to check a formula, or to answer a question from the data.

## Errors

Excel's: #N/A, #VALUE!, #REF!, #NAME?, #DIV/0!, #NUM!, #SPILL!, #CALC!, plus #CIRC! (a
column that depends on itself) and Loading... (a referenced note that has not arrived). Each
comes with a sentence saying why; get_table lists them. IFERROR catches the Excel ones, never
#CIRC! or Loading.

## What differs from Excel

- No A1 references (B2, C2:C9), array constants or LAMBDA: name columns instead.
- A whole table row returned by XLOOKUP is a link to that row, so .column can follow it.
- SORT puts blanks last.
- GROUPBY adds no totals row unless asked.
- Formulas are stored by identity: renaming a note, table or column never breaks a formula.

## Worked examples

Trip cost calculator, a note with tables Vehicles (name, mpg) and Trips (name, miles,
vehicle as a link to Vehicles, gas price as money, nights as number, hotel rate as money):

- Trips gas (money): `=2*[@miles]/[@vehicle].mpg*[@[gas price]]`
- Trips cost (money): `=[@gas]+[@nights]*[@[hotel rate]]`
- Vehicles trips (number): `=COUNTIF(Trips[vehicle], [@name])`

Household budget, with Categories (name, monthly budget) and Transactions (date, category
as a link to Categories, amount as money):

- Categories spent (money): `=SUMIFS(Transactions[amount], Transactions[category], [@name], Transactions[date], ">"&EOMONTH(TODAY(),-1))`
- Categories left (money): `=[@[monthly budget]]-[@spent]`
- Summary ByMonth: `=GROUPBY(HSTACK(TEXT(Transactions[date],"yyyy-mm"), Transactions[category]), Transactions[amount], SUM)`,
  columns named month, category, spent, with its own columns
  budget `=[@category].[monthly budget]` and left `=[@budget]-[@spent]`.
- A question, through evaluate: `=SUMIFS(Transactions[amount], Transactions[category], "Gas", Transactions[date], ">="&DATE(2026,9,1), Transactions[date], "<"&DATE(2026,10,1))`

Time tracker, Entries (date, project, hours as hours):

- Last week by project (summary): `=LET(recent, Entries[date]>TODAY()-7, GROUPBY(FILTER(Entries[project], recent), FILTER(Entries[hours], recent), SUM))`
"#;

/// The guide, with the functions Pimble has today.
pub fn text() -> String {
    let mut out = BODY.to_string();
    out.push_str("\n## Functions Pimble has\n\n");
    for (category, names) in FUNCTIONS {
        out.push_str(&format!("- {category}: {}\n", names.join(", ")));
    }
    out.push_str(
        "\nAggregates (SUM, AVERAGE, COUNT, MIN, MAX, ...) can be passed by name to GROUPBY. A \
function that is not listed answers #NAME? with a sentence naming what to use instead.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_guide_lists_the_functions() {
        let t = super::text();
        assert!(t.contains("XLOOKUP"));
        assert!(t.contains("GROUPBY"));
        assert!(t.contains("[@vehicle].mpg"));
        assert!(t.contains("'Household budget'!"));
    }
}
