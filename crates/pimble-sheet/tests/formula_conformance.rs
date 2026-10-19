//! Excel conformance: formulas and the values Excel gives for them
//! (operator precedence, coercion, rounding, dates, criteria, lookups,
//! dynamic arrays, text formats). Today is 2026-10-19.

mod formula_support;

use formula_support::*;
use pimble_sheet::formula::{Engine, Evaluated, Value};
use pimble_sheet::Format;

#[derive(Debug)]
enum X {
    N(f64),
    S(&'static str),
    B(bool),
    E(&'static str),
    Blank,
}
use X::*;

const LAB: u128 = 77;

fn lab() -> TestBook {
    let mut b = TestBook::new(LAB);
    let things = TB::new("things", "Things")
        .text("item")
        .number("qty")
        .money("price")
        .date("when")
        .text("kind")
        .row(
            "a",
            vec![
                ("item", s("apple")),
                ("qty", n(3.0)),
                ("price", n(1.5)),
                ("when", d("2026-10-01")),
                ("kind", s("fruit")),
            ],
        )
        .row(
            "b",
            vec![
                ("item", s("banana")),
                ("qty", n(0.0)),
                ("price", n(0.25)),
                ("when", d("2026-10-10")),
                ("kind", s("fruit")),
            ],
        )
        .row(
            "c",
            vec![
                ("item", s("carrot")),
                ("qty", n(7.0)),
                ("price", n(0.8)),
                ("when", d("2026-09-15")),
                ("kind", s("veg")),
            ],
        )
        .row(
            "d",
            vec![
                ("item", s("apricot")),
                ("price", n(2.0)),
                ("when", d("2026-10-18")),
                ("kind", s("fruit")),
            ],
        )
        .row(
            "e",
            vec![
                ("item", s("10")),
                ("qty", n(2.0)),
                ("price", n(1.0)),
                ("kind", s("misc")),
            ],
        )
        .build();
    let steps = TB::new("steps", "Steps")
        .number("n")
        .text("label")
        .row("1", vec![("n", n(1.0)), ("label", s("one"))])
        .row("3", vec![("n", n(3.0)), ("label", s("three"))])
        .row("5", vec![("n", n(5.0)), ("label", s("five"))])
        .row("7", vec![("n", n(7.0)), ("label", s("seven"))])
        .build();
    b.add(LAB, "Lab", None).sheets.tables = vec![things, steps];
    b
}

const CASES: &[(&str, X)] = &[
    // Operators and precedence.
    ("=1+2*3", N(7.0)),
    ("=(1+2)*3", N(9.0)),
    ("=-2^2", N(4.0)),
    ("=2^3^2", N(64.0)),
    ("=-2^-2", N(0.25)),
    ("=2*-3", N(-6.0)),
    ("=10%", N(0.1)),
    ("=50%*4", N(2.0)),
    ("=200%%", N(0.02)),
    ("=1e3", N(1000.0)),
    ("=.5+1.", N(1.5)),
    ("=1/0", E("#DIV/0!")),
    ("=0^0", E("#NUM!")),
    ("=(-8)^(1/3)", E("#NUM!")),
    ("=\"a\"&\"b\"", S("ab")),
    ("=1&2", S("12")),
    ("=1+2&3", S("33")),
    ("=\"3\"+1", N(4.0)),
    ("=\"$1,000\"*2", N(2000.0)),
    ("=\"abc\"+1", E("#VALUE!")),
    ("=\"\"+1", E("#VALUE!")),
    ("=TRUE+1", N(2.0)),
    ("=-\"2\"", N(-2.0)),
    ("=0.1+0.2=0.3", B(true)),
    ("=\"abc\"=\"ABC\"", B(true)),
    ("=\"a\"<\"b\"", B(true)),
    ("=1<\"a\"", B(true)),
    ("=\"a\"<TRUE", B(true)),
    ("=1=TRUE", B(false)),
    ("=3>2>1", B(true)),
    ("=\"a\"<>\"b\"", B(true)),
    ("=1/3&\"\"", S("0.333333333333333")),
    ("=\"say \"\"hi\"\"\"", S("say \"hi\"")),
    ("=#N/A", E("#N/A")),
    ("=SUM(1;2;3)", N(6.0)),
    ("=  1 +  1 ", N(2.0)),
    ("2+2", N(4.0)),
    // Numbers and rounding.
    ("=ROUND(2.675,2)", N(2.68)),
    ("=ROUND(-2.5,0)", N(-3.0)),
    ("=ROUND(1234.567,-2)", N(1200.0)),
    ("=ROUND(1.005,2)", N(1.01)),
    ("=ROUNDUP(3.2,0)", N(4.0)),
    ("=ROUNDUP(-3.2,0)", N(-4.0)),
    ("=ROUNDUP(1.1*100,0)", N(110.0)),
    ("=ROUNDDOWN(-3.7,0)", N(-3.0)),
    ("=ROUNDDOWN(4.56789,3)", N(4.567)),
    ("=INT(-3.5)", N(-4.0)),
    ("=INT(3.9)", N(3.0)),
    ("=MOD(-3,2)", N(1.0)),
    ("=MOD(3,-2)", N(-1.0)),
    ("=MOD(5.5,2)", N(1.5)),
    ("=MOD(5,0)", E("#DIV/0!")),
    ("=SQRT(16)", N(4.0)),
    ("=SQRT(-1)", E("#NUM!")),
    ("=POWER(2,10)", N(1024.0)),
    ("=ABS(-5)", N(5.0)),
    ("=SIGN(-2)", N(-1.0)),
    ("=SIGN(0)", N(0.0)),
    // Totals.
    ("=SUM(1,2,\"3\")", N(6.0)),
    ("=SUM(1,\"x\")", E("#VALUE!")),
    ("=SUM(1,,2)", N(3.0)),
    ("=AVERAGE(1,2,3,4)", N(2.5)),
    ("=AVERAGE(Things[qty])", N(3.0)),
    ("=MEDIAN(1,3,2,4)", N(2.5)),
    ("=MEDIAN(5,1,3)", N(3.0)),
    ("=MAX(1,5,3)", N(5.0)),
    ("=MIN(Things[price])", N(0.25)),
    ("=MAX(Things[item])", N(0.0)),
    ("=PRODUCT(2,3,4)", N(24.0)),
    ("=COUNT(1,\"2\",\"x\",TRUE)", N(3.0)),
    ("=COUNT(Things[qty])", N(4.0)),
    ("=COUNT(Things[when])", N(4.0)),
    ("=COUNTA(1,\"\",\"x\")", N(3.0)),
    ("=COUNTA(Things[qty])", N(4.0)),
    ("=COUNTBLANK(Things[qty])", N(1.0)),
    ("=SUM(Things[qty]*2)", N(24.0)),
    ("=SUMPRODUCT(Things[qty],Things[price])", N(12.1)),
    (
        "=SUMPRODUCT((Things[kind]=\"fruit\")*Things[price])",
        N(3.75),
    ),
    ("=SUMPRODUCT(Things[kind]=\"fruit\")", N(0.0)),
    ("=SUMPRODUCT(--(Things[kind]=\"fruit\"))", N(3.0)),
    ("=SUM(IF(Things[kind]=\"veg\",Things[qty],0))", N(7.0)),
    // Criteria.
    ("=COUNTIF(Things[item],\"a*\")", N(2.0)),
    ("=COUNTIF(Things[item],\"?pple\")", N(1.0)),
    ("=COUNTIF(Things[item],\"*A*\")", N(4.0)),
    ("=COUNTIF(Things[qty],\">2\")", N(2.0)),
    ("=COUNTIF(Things[qty],\">=3\")", N(2.0)),
    ("=COUNTIF(Things[qty],\"<>3\")", N(4.0)),
    ("=COUNTIF(Things[qty],3)", N(1.0)),
    ("=COUNTIF(Things[qty],\"\")", N(1.0)),
    ("=COUNTIF(Things[qty],\"<5\")", N(3.0)),
    ("=COUNTIF(Things[item],10)", N(1.0)),
    ("=COUNTIF(Things[item],\"10\")", N(1.0)),
    ("=COUNTIF(Things[kind],\"<>fruit\")", N(2.0)),
    ("=COUNTIF(Things[when],\">=\"&DATE(2026,10,1))", N(3.0)),
    ("=COUNTIF(Things[when],\">=2026-10-01\")", N(3.0)),
    ("=COUNTIF(Things[item],\"<b\")", N(3.0)),
    ("=SUMIF(Things[kind],\"fruit\",Things[price])", N(3.75)),
    ("=SUMIF(Things[qty],\">1\")", N(12.0)),
    (
        "=SUMIFS(Things[price],Things[kind],\"fruit\",Things[when],\">\"&DATE(2026,10,5))",
        N(2.25),
    ),
    ("=AVERAGEIF(Things[kind],\"veg\",Things[qty])", N(7.0)),
    (
        "=AVERAGEIF(Things[kind],\"nope\",Things[qty])",
        E("#DIV/0!"),
    ),
    ("=AVERAGEIFS(Things[price],Things[kind],\"fruit\")", N(1.25)),
    ("=MAXIFS(Things[qty],Things[kind],\"fruit\")", N(3.0)),
    ("=MINIFS(Things[price],Things[kind],\"fruit\")", N(0.25)),
    ("=MAXIFS(Things[qty],Things[kind],\"nope\")", N(0.0)),
    (
        "=COUNTIFS(Things[kind],\"fruit\",Things[qty],\">0\")",
        N(1.0),
    ),
    (
        "=COUNTIFS(Things[kind],\"fruit\",Things[item],\"a~*\")",
        N(0.0),
    ),
    (
        "=SUMIFS(Things[price],Things[kind],\"fruit\",Things[qty],\"<>0\")",
        N(3.5),
    ),
    ("=SUM(COUNTIF(Things[kind],Things[kind]))", N(11.0)),
    // Lookups.
    ("=XLOOKUP(\"carrot\",Things[item],Things[qty])", N(7.0)),
    ("=XLOOKUP(\"CARROT\",Things[item],Things[qty])", N(7.0)),
    (
        "=XLOOKUP(\"kiwi\",Things[item],Things[qty],\"none\")",
        S("none"),
    ),
    ("=XLOOKUP(\"kiwi\",Things[item],Things[qty])", E("#N/A")),
    ("=XLOOKUP(\"a*\",Things[item],Things[price],,2)", N(1.5)),
    ("=XLOOKUP(\"a*\",Things[item],Things[price],,2,-1)", N(2.0)),
    ("=XLOOKUP(4,Steps[n],Steps[label],,-1)", S("three")),
    ("=XLOOKUP(4,Steps[n],Steps[label],,1)", S("five")),
    ("=XLOOKUP(8,Steps[n],Steps[label],,1)", E("#N/A")),
    ("=XLOOKUP(5,Steps[n],Steps).label", S("five")),
    ("=VLOOKUP(\"banana\",Things,3,FALSE)", N(0.25)),
    ("=VLOOKUP(\"b*\",Things,2,FALSE)", N(0.0)),
    ("=VLOOKUP(6,Steps,2)", S("five")),
    ("=VLOOKUP(6,Steps,2,TRUE)", S("five")),
    ("=VLOOKUP(0,Steps,2)", E("#N/A")),
    ("=VLOOKUP(1,Steps,3)", E("#REF!")),
    ("=INDEX(Things[item],2)", S("banana")),
    ("=INDEX(Things,3,2)", N(7.0)),
    ("=INDEX(Things[item],9)", E("#REF!")),
    ("=SUM(INDEX(Things,0,2))", N(12.0)),
    ("=MATCH(\"carrot\",Things[item],0)", N(3.0)),
    ("=MATCH(\"c*\",Things[item],0)", N(3.0)),
    ("=MATCH(5,Steps[n])", N(3.0)),
    ("=MATCH(6,Steps[n])", N(3.0)),
    ("=MATCH(0,Steps[n])", E("#N/A")),
    ("=XMATCH(7,Steps[n])", N(4.0)),
    ("=XMATCH(6,Steps[n],1)", N(4.0)),
    ("=XMATCH(6,Steps[n],-1)", N(3.0)),
    ("=XMATCH(\"c*\",Things[item])", E("#N/A")),
    ("=XMATCH(\"c*\",Things[item],2)", N(3.0)),
    ("=ROWS(Things)", N(5.0)),
    ("=COLUMNS(Things)", N(5.0)),
    // Dynamic arrays.
    ("=SUM(FILTER(Things[qty],Things[kind]=\"fruit\"))", N(3.0)),
    ("=ROWS(FILTER(Things,Things[kind]=\"fruit\"))", N(3.0)),
    ("=FILTER(Things[qty],Things[kind]=\"meat\")", E("#CALC!")),
    ("=FILTER(Things[qty],Things[kind]=\"meat\",0)", N(0.0)),
    ("=FILTER(Things[item],Things[qty]=7)", S("carrot")),
    ("=INDEX(SORT(Things[item]),1)", S("10")),
    ("=INDEX(SORT(Things[item]),5)", S("carrot")),
    ("=INDEX(SORT(Things[qty],1,-1),1)", N(7.0)),
    ("=INDEX(SORT(Things[qty]),5)", Blank),
    ("=INDEX(SORT(Things,3,-1),1,1)", S("apricot")),
    ("=ROWS(UNIQUE(Things[kind]))", N(3.0)),
    ("=ROWS(UNIQUE(Things[kind],,TRUE))", N(2.0)),
    ("=COLUMNS(HSTACK(Things[item],Things[qty]))", N(2.0)),
    ("=COLUMNS(HSTACK(1,2,3))", N(3.0)),
    ("=INDEX(HSTACK(Things[item],Steps[n]),5,2)", E("#N/A")),
    (
        "=INDEX(GROUPBY(Things[kind],Things[price],SUM),1,2)",
        N(3.75),
    ),
    (
        "=INDEX(GROUPBY(Things[kind],Things[price],SUM),3,1)",
        S("veg"),
    ),
    ("=ROWS(GROUPBY(Things[kind],Things[price],COUNT))", N(3.0)),
    (
        "=INDEX(GROUPBY(Things[kind],Things[qty],COUNT),1,2)",
        N(2.0),
    ),
    (
        "=INDEX(GROUPBY(Things[kind],Things[price],SUM,,1),4,1)",
        S("Total"),
    ),
    (
        "=INDEX(GROUPBY(Things[kind],Things[price],SUM,,1),4,2)",
        N(5.55),
    ),
    (
        "=INDEX(GROUPBY(Things[kind],Things[price],SUM,,,-2),1,1)",
        S("fruit"),
    ),
    (
        "=INDEX(GROUPBY(Things[kind],Things[price],MAX,,,-2),3,1)",
        S("veg"),
    ),
    (
        "=INDEX(GROUPBY(Things[kind],Things[price],AVERAGE),1,2)",
        N(1.25),
    ),
    (
        "=ROWS(GROUPBY(Things[kind],Things[price],SUM,,,,Things[qty]>0))",
        N(3.0),
    ),
    ("=GROUPBY(Things[kind],Things[price],42)", E("#VALUE!")),
    (
        "=TEXTJOIN(\",\",TRUE,Things[item])",
        S("apple,banana,carrot,apricot,10"),
    ),
    ("=TEXTJOIN(\"-\",FALSE,\"a\",\"\",\"b\")", S("a--b")),
    ("=AND(Things[qty]>=0)", B(true)),
    ("=OR(Things[qty]>5)", B(true)),
    ("=SUM(Things[item])", N(0.0)),
    ("=INDEX(SORT(Things[qty],1,-1),5)", Blank),
    ("=Things[qty]*0+1", E("#SPILL!")),
    // Conditions.
    ("=IF(1>2,\"y\",\"n\")", S("n")),
    ("=IF(FALSE,1)", B(false)),
    ("=IF(\"x\",1,2)", E("#VALUE!")),
    ("=IF(1,,2)", Blank),
    ("=IFS(1>2,\"a\",TRUE,\"b\")", S("b")),
    ("=IFS(FALSE,1)", E("#N/A")),
    ("=IFERROR(1/0,\"x\")", S("x")),
    ("=IFERROR(5,\"x\")", N(5.0)),
    ("=IFNA(1/0,\"x\")", E("#DIV/0!")),
    ("=IFNA(NA(),\"x\")", S("x")),
    ("=SWITCH(2,1,\"a\",2,\"b\",\"c\")", S("b")),
    ("=SWITCH(9,1,\"a\",\"c\")", S("c")),
    ("=SWITCH(9,1,\"a\")", E("#N/A")),
    ("=CHOOSE(2,\"a\",\"b\")", S("b")),
    ("=CHOOSE(3,\"a\",\"b\")", E("#VALUE!")),
    ("=AND(TRUE,1>0)", B(true)),
    ("=AND(TRUE,\"x\")", E("#VALUE!")),
    ("=OR(FALSE,0)", B(false)),
    ("=NOT(0)", B(true)),
    ("=LET(x,2,y,x*3,x+y)", N(8.0)),
    ("=LET(x,2,LET(x,5,x)+x)", N(7.0)),
    ("=ISBLANK(1)", B(false)),
    ("=ISBLANK(INDEX(Things[qty],4))", B(true)),
    ("=ISNUMBER(1)", B(true)),
    ("=ISNUMBER(\"1\")", B(false)),
    ("=ISTEXT(\"a\")", B(true)),
    ("=ISERROR(1/0)", B(true)),
    ("=ISNA(NA())", B(true)),
    ("=ISNA(1/0)", B(false)),
    ("=TRUE()", B(true)),
    // Dates.
    ("=DATE(2026,10,19)", N(46_314.0)),
    ("=DATE(2026,13,1)", N(46_388.0)),
    ("=DATE(2026,1,0)", N(46_022.0)),
    ("=DATE(2026,2,29)", N(46_082.0)),
    ("=DATE(2026,0,1)", N(45_992.0)),
    ("=YEAR(DATE(26,1,1))", N(1926.0)),
    ("=DATE(1900,1,1)", N(1.0)),
    ("=DATE(1900,3,1)", N(61.0)),
    ("=DATE(10000,1,1)", E("#NUM!")),
    ("=YEAR(46314)", N(2026.0)),
    ("=MONTH(46314)", N(10.0)),
    ("=DAY(46314.75)", N(19.0)),
    ("=YEAR(-1)", E("#NUM!")),
    ("=WEEKDAY(46314)", N(2.0)),
    ("=WEEKDAY(46314,2)", N(1.0)),
    ("=WEEKDAY(46314,3)", N(0.0)),
    ("=WEEKDAY(46314,16)", N(3.0)),
    ("=WEEKDAY(46314,9)", E("#NUM!")),
    ("=EOMONTH(46314,-1)", N(46_295.0)),
    ("=EOMONTH(46314,0)", N(46_326.0)),
    ("=EOMONTH(DATE(2024,1,31),1)", N(45_351.0)),
    ("=EDATE(DATE(2026,1,31),1)", N(46_081.0)),
    ("=EDATE(DATE(2026,3,31),-1)", N(46_081.0)),
    ("=DAYS(DATE(2026,12,25),DATE(2026,10,19))", N(67.0)),
    ("=DAYS(\"2026-12-25\",\"2026-10-19\")", N(67.0)),
    ("=TODAY()", N(46_314.0)),
    ("=TODAY()-7", N(46_307.0)),
    ("=\"2026-10-19\"+1", N(46_315.0)),
    ("=DATE(2026,10,19)-DATE(2026,1,1)", N(291.0)),
    ("=MONTH(EOMONTH(TODAY(),-1))", N(9.0)),
    ("=\">\"&EOMONTH(TODAY(),-1)", S(">46295")),
    ("=MAX(Things[when])", N(46_313.0)),
    // Text.
    ("=TEXT(46314,\"yyyy-mm\")", S("2026-10")),
    ("=TEXT(46314,\"dddd, mmmm d\")", S("Monday, October 19")),
    ("=TEXT(46314,\"ddd d mmm yy\")", S("Mon 19 Oct 26")),
    ("=TEXT(46314,\"mm/dd/yyyy\")", S("10/19/2026")),
    ("=TEXT(46314.5,\"h:mm AM/PM\")", S("12:00 PM")),
    ("=TEXT(0.75,\"hh:mm\")", S("18:00")),
    ("=TEXT(1234.5,\"$#,##0.00\")", S("$1,234.50")),
    ("=TEXT(1234.5,\"#,##0\")", S("1,235")),
    ("=TEXT(0.256,\"0.0%\")", S("25.6%")),
    ("=TEXT(5,\"000\")", S("005")),
    ("=TEXT(-1234.5,\"#,##0.00;(#,##0.00)\")", S("(1,234.50)")),
    ("=TEXT(0,\"0;-0;\"\"zero\"\"\")", S("zero")),
    ("=TEXT(1234567,\"#,##0.0,,\\M\")", S("1.2M")),
    ("=TEXT(12345.678,\"0.00E+00\")", S("1.23E+04")),
    ("=TEXT(\"abc\",\"0.00\")", S("abc")),
    ("=TEXT(\"12\",\"0.00\")", S("12.00")),
    ("=TEXT(TRUE,\"0\")", S("TRUE")),
    ("=TEXT(Things[when],\"mmm\")", E("#SPILL!")),
    ("=INDEX(TEXT(Things[when],\"mmm\"),3)", S("Sep")),
    ("=LEFT(\"Pimble\",3)", S("Pim")),
    ("=LEFT(\"abc\")", S("a")),
    ("=LEFT(\"abc\",10)", S("abc")),
    ("=LEFT(\"abc\",-1)", E("#VALUE!")),
    ("=RIGHT(\"Pimble\",2)", S("le")),
    ("=MID(\"Pimble\",2,3)", S("imb")),
    ("=MID(\"Pimble\",10,3)", S("")),
    ("=MID(\"Pimble\",0,3)", E("#VALUE!")),
    ("=LEN(\"héllo\")", N(5.0)),
    ("=LEN(12.5)", N(4.0)),
    ("=UPPER(\"abc\")", S("ABC")),
    ("=LOWER(\"ABC\")", S("abc")),
    (
        "=PROPER(\"hello wORLD o'neil 2-way\")",
        S("Hello World O'Neil 2-Way"),
    ),
    ("=TRIM(\"  a   b  \")", S("a b")),
    ("=VALUE(\"$1,234.50\")", N(1234.5)),
    ("=VALUE(\"12%\")", N(0.12)),
    ("=VALUE(\"x\")", E("#VALUE!")),
    ("=CONCAT(\"a\",1,TRUE)", S("a1TRUE")),
    ("=CONCAT(Steps[label])", S("onethreefiveseven")),
    ("=CONCATENATE(\"a\",\"b\",\"c\")", S("abc")),
    ("=INDEX(CONCATENATE(Steps[label],\"!\"),2)", S("three!")),
    ("=TEXTJOIN(\", \",TRUE,\"a\",\"\",\"b\")", S("a, b")),
    ("=UPPER(Things[item])&\"\"", E("#SPILL!")),
    // Unknown names.
    ("=FOO(1)", E("#NAME?")),
    ("=NOW()", E("#NAME?")),
];

fn check(v: &Value, want: &X) -> bool {
    match (v, want) {
        (Value::Number(x), N(y)) => close(*x, *y),
        (Value::Text(a), S(b)) => a == b,
        (Value::Bool(a), B(b)) => a == b,
        (Value::Error(e), E(c)) => e.code.code() == *c,
        (Value::Blank, Blank) => true,
        _ => false,
    }
}

#[test]
fn excel_conformance() {
    let b = lab();
    let en = Engine::new(&b);
    let mut failures = Vec::new();
    for (f, want) in CASES {
        let got = match en.evaluate(&node(LAB), None, None, f) {
            Ok(Evaluated::Value(v)) => v,
            Ok(Evaluated::Table(t)) => {
                // A list where one value goes, as a cell would show it.
                if matches!(want, E("#SPILL!")) && t.rows.len() > 1 {
                    continue;
                }
                failures.push(format!("{f}: a list of {} rows", t.rows.len()));
                continue;
            }
            Err(e) => {
                failures.push(format!("{f}: {e:?}"));
                continue;
            }
        };
        if !check(&got, want) {
            failures.push(format!("{f}: want {want:?}, got {got:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} failed:\n{}",
        failures.len(),
        CASES.len(),
        failures.join("\n")
    );
}

/// The same formulas as formula columns: a list where one value goes is
/// `#SPILL!` there.
#[test]
fn conformance_in_cells() {
    let mut b = lab();
    let mut t = TB::new("calc", "Calc").text("x");
    for (i, (f, _)) in CASES.iter().enumerate() {
        t = t.formula(&format!("f{i}"), f, Format::Auto);
    }
    b.note(LAB).sheets.tables.push(t.row("r", vec![]).build());
    let en = Engine::new(&b);
    let tables = en.compute_node(&node(LAB)).unwrap();
    let calc = table(&tables, "Calc");
    let mut failures = Vec::new();
    for (i, (f, want)) in CASES.iter().enumerate() {
        let v = cell(calc, "r", &format!("f{i}"));
        if !check(v, want) {
            failures.push(format!("{f}: want {want:?}, got {v:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn errors_carry_sentences() {
    let b = lab();
    let en = Engine::new(&b);
    let sentence = |f: &str| match en.evaluate(&node(LAB), None, None, f).unwrap() {
        Evaluated::Value(Value::Error(e)) => e.message,
        other => panic!("{f}: {other:?}"),
    };
    assert_eq!(sentence("=1/0"), "Divided by zero");
    assert_eq!(sentence("=\"abc\"+1"), "\"abc\" isn't a number");
    assert_eq!(
        sentence("=XLOOKUP(\"kiwi\",Things[item],Things[qty])"),
        "\"kiwi\" isn't in that list"
    );
    assert_eq!(
        sentence("=FILTER(Things[qty],Things[kind]=\"meat\")"),
        "Nothing matches the filter"
    );
    assert_eq!(
        sentence("=SQRT(-1)"),
        "A negative number has no square root"
    );
    assert_eq!(
        sentence("=IF(1)"),
        "IF() takes a condition, a value if true and a value if false"
    );
    assert_eq!(sentence("=ROUND()"), "ROUND() needs 2 inputs");
    assert_eq!(sentence("=TODAY(1)"), "TODAY() takes at most 0 inputs");
    assert!(sentence("=NOW()").starts_with(
        "There's no function called NOW() in Pimble yet; the date functions are TODAY"
    ));
    assert!(sentence("=SUBSTITUTE(\"a\",\"b\",\"c\")")
        .contains("the text functions are TEXT, TEXTJOIN"));
    assert_eq!(sentence("=SUM"), "SUM is a function; call it like SUM(…)");
}

#[test]
fn deep_and_long_formulas_are_refused_not_crashed() {
    let b = lab();
    let en = Engine::new(&b);
    let deep = format!("={}1{}", "(".repeat(500), ")".repeat(500));
    assert!(en.evaluate(&node(LAB), None, None, &deep).is_err());
    let long = format!("={}", vec!["1"; 2000].join("+"));
    assert!(en.evaluate(&node(LAB), None, None, &long).is_err());
    // Within the limits they compute.
    let ok = format!("={}1{}", "(".repeat(60), ")".repeat(60));
    assert_eq!(
        en.evaluate(&node(LAB), None, None, &ok).unwrap(),
        Evaluated::Value(Value::Number(1.0))
    );
    let ok = format!("={}", vec!["1"; 200].join("+"));
    assert_eq!(
        en.evaluate(&node(LAB), None, None, &ok).unwrap(),
        Evaluated::Value(Value::Number(200.0))
    );
    let nested = format!("={}1{}", "ABS(".repeat(60), ")".repeat(60));
    assert_eq!(
        en.evaluate(&node(LAB), None, None, &nested).unwrap(),
        Evaluated::Value(Value::Number(1.0))
    );
    // Garbage never panics.
    for junk in [
        "=[",
        "=]",
        "={",
        "='",
        "=\"",
        "=@",
        "=#",
        "=1..2",
        "=[@[",
        "=Table[[a]:[b]",
        "=a!b!c",
        "=f(,,,",
        "=.",
        "=1%%%",
        "=--",
        "={n:x}!a",
        "={t:",
        "=[[#Nope]]",
        "=é(1)",
        "=日本[x]",
    ] {
        let _ = en.evaluate(&node(LAB), None, None, junk);
    }
}
