//! Golden values and a differential check against IronCalc (the sheet engine).
use super::*;
use super::compile::compile_full;
use duckdb::Connection;
use ironcalc::base::{cell::CellValue, types::CellType, Model};

const COLUMNS: [(&str, &str); 5] = [("n", "DOUBLE"), ("i", "BIGINT"), ("s", "VARCHAR"), ("b", "BOOLEAN"), ("d", "DATE")];

#[derive(Clone, Debug)]
struct Row {
    n: Option<f64>,
    i: Option<i64>,
    s: Option<&'static str>,
    b: Option<bool>,
    /// Days after 1899-12-30 (a sheet serial).
    d: Option<i64>,
}

fn rows() -> Vec<Row> {
    let ns = [Some(0.0), Some(1.0), Some(-2.5), Some(3.0), Some(0.1), Some(1000.0), None, Some(2.675), Some(-0.5), Some(1e-3)];
    let is = [Some(0), Some(2), Some(-7), None, Some(12), Some(1)];
    let ss = [Some("2"), Some(" 3 "), Some("abc"), Some(""), None, Some("TRUE"), Some("Ärger"), Some("a b  c"), Some("1e2"), Some("ABC"), Some("-4.5")];
    let bs = [Some(true), Some(false), None];
    let ds = [Some(45351), Some(36525), None, Some(61), Some(45322)];
    // A deterministic spread over the combinations.
    (0..60usize).map(|k| Row {
        n: ns[k % ns.len()], i: is[(k * 7 + 1) % is.len()], s: ss[(k * 5 + 3) % ss.len()], b: bs[(k / 3) % bs.len()], d: ds[(k * 3) % ds.len()],
    }).collect()
}

fn table(conn: &Connection, rows: &[Row]) {
    conn.execute_batch("CREATE TABLE t (k INTEGER, n DOUBLE, i BIGINT, s VARCHAR, b BOOLEAN, d DATE)").unwrap();
    let mut app = conn.appender("t").unwrap();
    for (k, r) in rows.iter().enumerate() {
        let d = r.d.map(chrono_like);
        app.append_row(duckdb::params![k as i32, r.n, r.i, r.s, r.b, d]).unwrap();
    }
}

/// `YYYY-MM-DD` of a serial, through DuckDB-independent arithmetic (civil calendar).
fn chrono_like(serial: i64) -> String {
    // Days from 1970-01-01: serial 25569 is 1970-01-01.
    let z = serial - 25569 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

#[derive(Debug, Clone, PartialEq)]
enum Out {
    Blank,
    Number(f64),
    Text(String),
    Bool(bool),
    Error(String),
}

fn run(conn: &Connection, formula: &str) -> (Ty, Vec<Out>) {
    let cols: Vec<(String, String)> = COLUMNS.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let c = compile(formula, &cols, "_rf_t_").unwrap_or_else(|e| panic!("{formula}: {e}"));
    let value = match c.ty {
        Ty::Date => format!("CAST(date_diff('day', DATE '1899-12-30', {}) AS VARCHAR)", c.value),
        _ => format!("CAST({} AS VARCHAR)", c.value),
    };
    let sql = format!("SELECT {value}, {} FROM ({}) ORDER BY k", c.error, c.wrap("SELECT * FROM t"));
    let mut stmt = conn.prepare(&sql).unwrap_or_else(|e| panic!("{formula}: {e}\n{sql}"));
    let out = stmt
        .query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))
        .unwrap_or_else(|e| panic!("{formula}: {e}"))
        .map(|r| {
            let (s, e) = r.unwrap_or_else(|e| panic!("{formula}: {e}"));
            if let Some(e) = e { return Out::Error(e); }
            match (c.ty, s) {
                (_, None) => Out::Blank,
                (Ty::Number | Ty::Date, Some(s)) => Out::Number(s.parse().unwrap()),
                (Ty::Bool, Some(s)) => Out::Bool(s == "true"),
                (_, Some(s)) => Out::Text(s),
            }
        })
        .collect();
    (c.ty, out)
}

/// The same formula in IronCalc, one row per sheet row; columns A..E hold n, i, s, b, d.
fn ironcalc(rows: &[Row], formula: &str) -> Vec<Out> {
    let mut m = Model::new_empty("diff", "en", "UTC", "en").unwrap();
    let cell_formula = |r: usize| {
        let row = r + 1;
        formula.replace("[@n]", &format!("A{row}")).replace("[@i]", &format!("B{row}")).replace("[@s]", &format!("C{row}"))
            .replace("[@b]", &format!("D{row}")).replace("[@d]", &format!("E{row}"))
    };
    for (r, row) in rows.iter().enumerate() {
        let rr = r as i32 + 1;
        if let Some(n) = row.n { m.set_user_input(0, rr, 1, format!("{n}")).unwrap(); }
        if let Some(i) = row.i { m.set_user_input(0, rr, 2, format!("{i}")).unwrap(); }
        if let Some(s) = row.s { m.set_user_input(0, rr, 3, format!("'{s}")).unwrap(); }
        if let Some(b) = row.b { m.set_user_input(0, rr, 4, if b { "TRUE".into() } else { "FALSE".to_string() }).unwrap(); }
        if let Some(d) = row.d { m.set_user_input(0, rr, 5, format!("{d}")).unwrap(); }
        m.set_user_input(0, rr, 6, cell_formula(r)).unwrap();
    }
    m.evaluate();
    (0..rows.len())
        .map(|r| {
            let rr = r as i32 + 1;
            if m.get_cell_type(0, rr, 6).unwrap() == CellType::ErrorValue {
                return Out::Error(m.get_formatted_cell_value(0, rr, 6).unwrap());
            }
            match m.get_cell_value_by_index(0, rr, 6).unwrap() {
                CellValue::None => Out::Blank,
                CellValue::Number(n) => Out::Number(n),
                CellValue::String(s) => Out::Text(s),
                CellValue::Boolean(b) => Out::Bool(b),
            }
        })
        .collect()
}

fn same(a: &Out, b: &Out) -> bool {
    match (a, b) {
        (Out::Number(x), Out::Number(y)) => (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0),
        // A blank result stays blank in tables; a sheet shows 0 (documented).
        (Out::Blank, Out::Number(y)) => *y == 0.0,
        (Out::Blank, Out::Text(y)) => y.is_empty(),
        _ => a == b,
    }
}

/// Formulas both engines must agree on, row by row.
const CORPUS: &[&str] = &[
    "=[@n]+[@i]", "=[@n]-[@i]*2", "=[@n]/[@i]", "=[@i]/[@n]", "=-[@n]^2", "=[@n]^[@i]", "=[@i]^0.5", "=50%*[@n]", "=[@n]+[@s]",
    "=[@s]*1", "=[@b]+1", "=[@d]+1", "=[@d]-[@d]", "=[@n]&\"x\"", "=[@i]&[@s]", "=[@b]&\"\"", "=\"a\"&[@n]*2",
    "=[@n]=[@i]", "=[@n]<[@i]", "=[@s]=\"abc\"", "=[@s]>\"b\"", "=[@s]=[@n]", "=[@s]>[@n]", "=[@b]=TRUE", "=[@b]>[@n]", "=[@s]<[@b]",
    "=[@n]=0", "=[@s]=\"\"", "=0.1+0.2=0.3", "=[@d]>45000", "=[@n]<>[@s]",
    "=IF([@n]>1,\"big\",\"small\")", "=IF([@b],[@n],[@i])", "=IF([@s],1,2)", "=IF([@n],1/0,2)", "=IFS([@n]>2,1,[@n]>0,2)",
    "=IFERROR([@n]/[@i],-1)", "=IFERROR([@s]*1,0)", "=IFNA([@n]/[@i],7)", "=IFERROR(1/[@i],\"\")",
    "=AND([@b],[@n]>0)", "=OR([@b],[@n])", "=AND([@s],TRUE)", "=NOT([@n])", "=NOT([@b])", "=NOT([@s])",
    "=ISBLANK([@s])", "=ISNUMBER([@n])", "=ISTEXT([@s])", "=ISLOGICAL([@b])", "=ISERROR([@n]/[@i])", "=ISNA([@n]/[@i])", "=ISERR([@s]*1)",
    "=ABS([@n])", "=INT([@n])", "=ROUND([@n],2)", "=ROUND([@n],0)", "=ROUND([@n]*1000,-2)", "=ROUNDUP([@n],1)", "=ROUNDDOWN([@n],0)",
    "=ROUND(2.675,2)", "=ROUND(1.005,2)", "=MOD([@i],3)", "=MOD([@n],[@i])", "=MOD(-7,[@i])",
    "=SUM([@n],[@i],[@s])", "=SUM([@n],\"2\",TRUE)", "=MIN([@n],[@i])", "=MAX([@n],[@i],[@b])", "=AVERAGE([@n],[@i])", "=AVERAGE([@s],[@b])",
    "=LEFT([@s],2)", "=LEFT([@s])", "=RIGHT([@s],[@i])", "=MID([@s],2,2)", "=MID([@s],[@i],1)", "=LEN([@s])", "=LEN([@n])",
    "=TRIM([@s])", "=UPPER([@s])", "=LOWER([@s])", "=SUBSTITUTE([@s],\"b\",\"X\")", "=SUBSTITUTE([@s],\" \",\"_\",2)", "=CONCAT([@s],\"-\",[@i])",
    "=YEAR([@d])", "=MONTH([@d])", "=DAY([@d])", "=YEAR([@i]+40000)", "=DATE(2024,[@i],1)", "=DATE(2024,1,[@i])", "=DATE([@i],1,1)",
    "=EDATE([@d],[@i])", "=EOMONTH([@d],1)", "=DAY(EOMONTH([@d],-1))",
];

#[test]
fn compiled_formulas_agree_with_the_sheet_engine() {
    let rows = rows();
    let conn = Connection::open_in_memory().unwrap();
    table(&conn, &rows);
    let mut failures = Vec::new();
    for f in CORPUS {
        let (_, ours) = run(&conn, f);
        let theirs = ironcalc(&rows, f);
        for (k, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            if !same(a, b) {
                failures.push(format!("{f} row {k} {:?}: table {a:?}, sheet {b:?}", rows[k]));
            }
        }
    }
    assert!(failures.is_empty(), "{} differences:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn golden_values() {
    let rows = vec![Row { n: Some(7.0), i: Some(0), s: Some(" a  b "), b: None, d: Some(45351) }, Row { n: None, i: None, s: None, b: Some(true), d: None }];
    let conn = Connection::open_in_memory().unwrap();
    table(&conn, &rows);
    let n = |x: f64| Out::Number(x);
    let t = |x: &str| Out::Text(x.into());
    let e = |x: &str| Out::Error(x.into());
    for (f, want) in [
        ("=[@n]/[@i]", vec![e("#DIV/0!"), e("#DIV/0!")]),
        ("=IFERROR([@n]/[@i],\"\")", vec![Out::Blank, Out::Blank]),
        ("=[@n]+[@s]", vec![e("#VALUE!"), n(0.0)]),
        ("=TRIM([@s])&\"|\"", vec![t("a b|"), t("|")]),
        ("=[@s]", vec![t(" a  b "), Out::Blank]),
        ("=[@n]", vec![n(7.0), Out::Blank]),
        ("=IF([@b],\"yes\",\"no\")", vec![t("no"), t("yes")]),
        ("=YEAR([@d])", vec![n(2024.0), e("#NUM!")]),
        ("=[@d]+1", vec![n(45352.0), n(1.0)]),
        ("=-2^2", vec![n(4.0), n(4.0)]),
        ("=2^3^2", vec![n(64.0), n(64.0)]),
        ("=\"1\"=1", vec![Out::Bool(false), Out::Bool(false)]),
        ("=\"a\">5", vec![Out::Bool(true), Out::Bool(true)]),
        ("=1e308*10", vec![e("#NUM!"), e("#NUM!")]),
        ("=(-8)^(1/3)", vec![e("#NUM!"), e("#NUM!")]),
        ("=0^-1", vec![e("#DIV/0!"), e("#DIV/0!")]),
        ("=#N/A", vec![e("#N/A"), e("#N/A")]),
        ("=IFNA(#DIV/0!,1)", vec![e("#DIV/0!"), e("#DIV/0!")]),
        ("=ISERROR(#REF!)", vec![Out::Bool(true), Out::Bool(true)]),
        ("=LEFT(\"abc\",[@n]-10)", vec![e("#VALUE!"), e("#VALUE!")]),
        ("=MID(\"abc\",1e300,2)", vec![t(""), t("")]),
        ("=DATE(10000,1,1)", vec![e("#NUM!"), e("#NUM!")]),
        ("=EDATE(DATE(2024,1,31),1)", vec![n(45351.0), n(45351.0)]),
        ("=ROUND(1e300,-400)", vec![n(0.0), n(0.0)]),
        ("=ROUNDUP(5,-400)", vec![e("#NUM!"), e("#NUM!")]),
    ] {
        let (_, got) = run(&conn, f);
        assert_eq!(got, want, "{f}");
    }
}

#[test]
fn refusals_explain_and_point_at_the_problem() {
    let cols: Vec<(String, String)> = [("amount", "DOUBLE"), ("tags", "VARCHAR[]"), ("when", "TIMESTAMP")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let err = |f: &str| compile(f, &cols, "_t").unwrap_err();
    assert!(err("=[@missing]").message.contains("no column named"));
    assert!(err("=[@tags]").message.contains("cannot use yet"));
    assert!(err("=IF([@amount]>1,1,\"one\")").message.contains("one type"));
    assert!(err("=IF([@amount]>1,1)").message.contains("value for when the condition is false"));
    assert!(err("=TODAY()").message.contains("deterministic"));
    assert!(err("=Orders[amount]").message.contains("whole column"));
    assert!(err("=XLOOKUP([@amount],T[a],T[b])").message.contains("no table named"));
    assert!(err("=VLOOKUPX(1)").message.contains("not available in table formulas"));
    assert!(err("=ROUND(1)").message.contains("takes 2 arguments"));
    assert!(err("=AND(\"x\")").message.contains("not a condition"));
    assert!(err("=T[[#This Row],[amount]]").message.contains("[@column]"));
    // A bare name reads as a table; used as a value it still points at [@name].
    assert!(err("=amount*2").message.contains("[@amount]"));
    assert!(err("=VLOOKUP([@amount],T[a],2,FALSE)").message.contains("whole table"));
    assert!(err("=VLOOKUP([@amount],T,2)").message.contains("approximate"));
    assert!(err("=VLOOKUP([@amount],T,2,TRUE)").message.contains("approximate"));
    assert!(err("=VLOOKUP([@amount],T,[@amount],FALSE)").message.contains("written in the formula"));
    assert!(err("=XLOOKUP(1,T[a]=[@amount],T[b])").message.contains("several keys"));
    assert!(err("=XLOOKUP(2,(T[a]=[@amount])*(T[b]=1),T[b])").message.contains("several keys"));
    assert!(err("=XLOOKUP(1,(T[a]=T[b])*(T[b]=1),T[b])").message.contains("several keys"));
    let e = err("=1+[@missing]");
    assert_eq!((e.span.start, e.span.end), (3, 13));
    // Column names resolve case-insensitively and are reported as named in the table.
    let ok = compile("=[@AMOUNT]*2", &cols, "_t").unwrap();
    assert_eq!(ok.columns, vec!["amount".to_string()]);
    assert_eq!(ok.ty, Ty::Number);
    assert_eq!(referenced_columns("=IF([@a]>1,[b],[@[c d]])").unwrap().iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["a", "b", "c d"]);
}

struct Lk;
impl Relations for Lk {
    fn relation(&self, table: &str) -> Result<Option<Relation>, String> {
        // `lt` is the same rows with the text key first, for VLOOKUP by text.
        Ok(match table {
            "lk" => Some(Relation {
                sql: "SELECT kn, kt, r, o AS _rf_ord FROM lk".into(),
                columns: vec![("kn".into(), "DOUBLE".into()), ("kt".into(), "VARCHAR".into()), ("r".into(), "VARCHAR".into())],
                errors: vec![],
                reads: vec![],
            }),
            "lt" => Some(Relation {
                sql: "SELECT kt, kn, r, o AS _rf_ord FROM lk".into(),
                columns: vec![("kt".into(), "VARCHAR".into()), ("kn".into(), "DOUBLE".into()), ("r".into(), "VARCHAR".into())],
                errors: vec![],
                reads: vec![],
            }),
            "ag" => Some(Relation {
                sql: "SELECT kn, kt, kb, kd, r, o AS _rf_ord FROM ag".into(),
                columns: vec![("kn".into(), "DOUBLE".into()), ("kt".into(), "VARCHAR".into()), ("kb".into(), "BOOLEAN".into()), ("kd".into(), "DATE".into()), ("r".into(), "DOUBLE".into())],
                errors: vec![],
                reads: vec![],
            }),
            _ => None,
        })
    }
}

/// Lookups over another table versus the same lookups over sheet ranges: `lk` is H1:J7
/// (kn, kt, r) and `lt` is L1:N7 (kt, kn, r).
fn check_lookups(formulas: &[&str]) {
    let rows = rows();
    let conn = Connection::open_in_memory().unwrap();
    table(&conn, &rows);
    // Lookup table: duplicates, a blank key, 0, "", case variants; the last row is filled
    // so the sheet range is not clipped.
    let lk: [(Option<f64>, Option<&str>, &str); 7] = [
        (Some(2.0), Some("x"), "a"), (Some(1.0), Some("X"), "b"), (Some(2.0), Some(""), "c"), (None, None, "d"),
        (Some(0.0), Some("abc"), "e"), (Some(-2.5), Some("ärger"), "f"), (Some(3.0), Some("x"), "g"),
    ];
    conn.execute_batch("CREATE TABLE lk (kn DOUBLE, kt VARCHAR, r VARCHAR, o BIGINT)").unwrap();
    for (o, (kn, kt, r)) in lk.iter().enumerate() {
        conn.execute("INSERT INTO lk VALUES (?, ?, ?, ?)", duckdb::params![kn, kt, r, (o as i64) * 10]).unwrap();
    }
    let cols: Vec<(String, String)> = COLUMNS.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let mut failures = Vec::new();
    for f in formulas {
        let c = compile_full(f, &cols, &[], &Lk, "_rf_t_").unwrap_or_else(|e| panic!("{f}: {e}"));
        let sql = format!("SELECT CAST({} AS VARCHAR), {} FROM ({}) ORDER BY k", c.value, c.error, c.wrap("SELECT * FROM t"));
        let ours: Vec<Out> = conn.prepare(&sql).unwrap_or_else(|e| panic!("{f}: {e}\n{sql}")).query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))
            .unwrap().map(|r| match r.unwrap() {
                (_, Some(e)) => Out::Error(e),
                (Some(s), None) if c.ty == Ty::Number => Out::Number(s.parse().unwrap()),
                (Some(s), None) => Out::Text(s),
                (None, None) => Out::Blank,
            }).collect();
        assert_eq!(ours.len(), rows.len(), "{f}: one result per row");
        let sheet_formula = f.replace("lk[kn]", "$H$1:$H$7").replace("lk[kt]", "$I$1:$I$7").replace("lk[r]", "$J$1:$J$7")
            .replace(",lk,", ",$H$1:$J$7,").replace(",lt,", ",$L$1:$N$7,");
        let theirs = ironcalc_with(&rows, &sheet_formula, &lk);
        for (k, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            if !same(a, b) { failures.push(format!("{f} row {k} {:?}: table {a:?}, sheet {b:?}", rows[k])); }
        }
    }
    assert!(failures.is_empty(), "{} differences:\n{}", failures.len(), failures.join("\n"));
}

/// Exact XLOOKUP over another table versus the same lookup over a sheet range.
#[test]
fn table_lookups_agree_with_the_sheet_engine() {
    check_lookups(&[
        "=XLOOKUP([@n],lk[kn],lk[r])", "=XLOOKUP([@i],lk[kn],lk[r],\"none\")", "=XLOOKUP([@s],lk[kt],lk[r])",
        "=XLOOKUP([@s],lk[kt],lk[r],\"none\",0,-1)", "=XLOOKUP([@s],lk[kn],lk[r],\"none\")", "=XLOOKUP([@n],lk[kt],lk[r],\"none\")",
        "=XLOOKUP([@b],lk[kn],lk[r],\"none\")", "=XLOOKUP([@i]*1,lk[kn],lk[r],\"none\",0,-1)", "=XLOOKUP([@n]/[@i],lk[kn],lk[r])",
        "=IFNA(XLOOKUP(UPPER([@s]),lk[kt],lk[r]),\"?\")&\"!\"",
    ]);
}

/// Exact VLOOKUP over a whole table, including column numbers outside it.
#[test]
fn table_vlookups_agree_with_the_sheet_engine() {
    check_lookups(&[
        "=VLOOKUP([@n],lk,3,FALSE)", "=VLOOKUP([@i],lk,2,FALSE)", "=VLOOKUP([@s],lt,3,FALSE)", "=VLOOKUP([@s],lt,2,0)",
        "=VLOOKUP([@b],lk,3,FALSE)", "=VLOOKUP([@n]/[@i],lk,3,FALSE)", "=IFNA(VLOOKUP([@s],lt,3,FALSE),\"none\")",
        "=VLOOKUP([@n],lk,1,FALSE)", "=VLOOKUP([@n],lk,0,FALSE)", "=VLOOKUP([@n],lk,4,FALSE)", "=VLOOKUP([@n],lk,2.9,FALSE)",
    ]);
}

/// Several keys, written as spreadsheets do: 1 against a product of comparisons, or TRUE
/// against one comparison.
#[test]
fn multi_key_lookups_agree_with_the_sheet_engine() {
    check_lookups(&[
        "=XLOOKUP(1,(lk[kn]=[@n])*(lk[kt]=[@s]),lk[r],\"none\")", "=XLOOKUP(1,(lk[kt]=[@s])*(lk[kn]=[@i]),lk[r],\"none\",0,-1)",
        "=XLOOKUP(1,([@i]=lk[kn])*(lk[kt]=[@s]),lk[r])", "=XLOOKUP(1,(lk[kn]=[@b])*(lk[kt]=[@s])*(lk[r]=\"c\"),lk[r],\"none\")",
        "=XLOOKUP(TRUE,lk[kn]=[@n],lk[r],\"none\")", "=XLOOKUP(TRUE,lk[kt]=UPPER([@s]),lk[r],\"none\",0,-1)",
    ]);
}

fn ironcalc_with(rows: &[Row], formula: &str, lk: &[(Option<f64>, Option<&str>, &str)]) -> Vec<Out> {
    let mut m = Model::new_empty("diff", "en", "UTC", "en").unwrap();
    for (r, (kn, kt, v)) in lk.iter().enumerate() {
        let rr = r as i32 + 1;
        if let Some(kn) = kn { m.set_user_input(0, rr, 8, format!("{kn}")).unwrap(); }
        match kt { Some("") => m.set_user_input(0, rr, 9, "=\"\"".into()).unwrap(), Some(t) => m.set_user_input(0, rr, 9, format!("'{t}")).unwrap(), None => {} }
        m.set_user_input(0, rr, 10, format!("'{v}")).unwrap();
        // The same rows with the text key first (L:N), for VLOOKUP by text.
        match kt { Some("") => m.set_user_input(0, rr, 12, "=\"\"".into()).unwrap(), Some(t) => m.set_user_input(0, rr, 12, format!("'{t}")).unwrap(), None => {} }
        if let Some(kn) = kn { m.set_user_input(0, rr, 13, format!("{kn}")).unwrap(); }
        m.set_user_input(0, rr, 14, format!("'{v}")).unwrap();
    }
    let mut out = Vec::new();
    // Rows go below the lookup block so ranges do not overlap.
    let base = 20;
    for (r, row) in rows.iter().enumerate() {
        let rr = base + r as i32;
        if let Some(n) = row.n { m.set_user_input(0, rr, 1, format!("{n}")).unwrap(); }
        if let Some(i) = row.i { m.set_user_input(0, rr, 2, format!("{i}")).unwrap(); }
        if let Some(s) = row.s { m.set_user_input(0, rr, 3, format!("'{s}")).unwrap(); }
        if let Some(b) = row.b { m.set_user_input(0, rr, 4, if b { "TRUE".into() } else { "FALSE".to_string() }).unwrap(); }
        let f = formula.replace("[@n]", &format!("A{rr}")).replace("[@i]", &format!("B{rr}")).replace("[@s]", &format!("C{rr}")).replace("[@b]", &format!("D{rr}"));
        m.set_user_input(0, rr, 6, f).unwrap();
    }
    m.evaluate();
    for r in 0..rows.len() {
        let rr = base + r as i32;
        out.push(if m.get_cell_type(0, rr, 6).unwrap() == CellType::ErrorValue {
            Out::Error(m.get_formatted_cell_value(0, rr, 6).unwrap())
        } else {
            match m.get_cell_value_by_index(0, rr, 6).unwrap() {
                CellValue::None => Out::Blank,
                CellValue::Number(n) => Out::Number(n),
                CellValue::String(s) => Out::Text(s),
                CellValue::Boolean(b) => Out::Bool(b),
            }
        });
    }
    out
}

/// Spreadsheet VLOOKUP reads `*`, `?` and `~` in a text value as a pattern even in exact
/// mode; tables give #VALUE! for those rows until pattern matching exists. XLOOKUP's exact
/// mode compares them literally. Expected values by hand from the `lk` rows.
#[test]
fn vlookup_patterns_are_refused_per_row_and_xlookup_compares_literally() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE lk (kn DOUBLE, kt VARCHAR, r VARCHAR, o BIGINT);
         INSERT INTO lk VALUES (1, 'x', 'a', 0), (2, 'X', 'b', 10), (3, 'x*', 'c', 20), (4, 'xy', 'd', 30);
         CREATE TABLE w (k INTEGER, s VARCHAR);
         INSERT INTO w VALUES (0, 'X'), (1, 'x*'), (2, 'x?'), (3, '~x'), (4, 'none');",
    ).unwrap();
    let cols = vec![("k".to_string(), "INTEGER".to_string()), ("s".to_string(), "VARCHAR".to_string())];
    let run = |f: &str| -> Vec<(Option<String>, Option<String>)> {
        let c = compile_full(f, &cols, &[], &Lk, "_rf_t_").unwrap();
        let sql = format!("SELECT CAST({} AS VARCHAR), {} FROM ({}) ORDER BY k", c.value, c.error, c.wrap("SELECT * FROM w"));
        conn.prepare(&sql).unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
    };
    let (v, e) = (|s: &str| (Some(s.to_string()), None), |s: &str| (None, Some(s.to_string())));
    assert_eq!(run("=VLOOKUP([@s],lt,3,FALSE)"), [v("a"), e("#VALUE!"), e("#VALUE!"), e("#VALUE!"), e("#N/A")]);
    assert_eq!(run("=XLOOKUP([@s],lk[kt],lk[r],\"-\")"), [v("a"), v("c"), v("-"), v("-"), v("-")]);
}

/// Source rows for conditional aggregates: numbers, numeric text, "", blank, logical text,
/// a literal `*`, dates (serials) and amounts.
/// (kn, kt, kb, kd as a serial, r).
type AgRow = (Option<f64>, Option<&'static str>, Option<bool>, Option<i64>, Option<f64>);
const AG: [AgRow; 7] = [
    (Some(2.0), Some("x"), Some(true), Some(45351), Some(10.0)),
    (Some(1.0), Some("2"), Some(false), Some(36526), Some(20.0)),
    (None, Some(""), None, None, None),
    (Some(0.0), Some("abc"), Some(true), Some(61), Some(5.0)),
    (Some(-2.5), Some("a*b"), Some(false), Some(45322), Some(7.5)),
    (Some(3.0), None, Some(true), Some(36525), Some(1.0)),
    (Some(2.0), Some("TRUE"), None, Some(45351), Some(2.0)),
];

/// SUMIFS/COUNTIFS/AVERAGEIFS over a table versus the same formulas over sheet ranges:
/// `ag` is P1:T7 (kn, kt, kb, kd, r).
#[test]
fn conditional_aggregates_agree_with_the_sheet_engine() {
    let rows = rows();
    let conn = Connection::open_in_memory().unwrap();
    table(&conn, &rows);
    conn.execute_batch("CREATE TABLE ag (kn DOUBLE, kt VARCHAR, kb BOOLEAN, kd DATE, r DOUBLE, o BIGINT)").unwrap();
    for (o, (kn, kt, kb, kd, r)) in AG.iter().enumerate() {
        conn.execute("INSERT INTO ag VALUES (?, ?, ?, ?, ?, ?)", duckdb::params![kn, kt, kb, kd.map(chrono_like), r, o as i64]).unwrap();
    }
    let cols: Vec<(String, String)> = COLUMNS.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let mut failures = Vec::new();
    for f in [
        "=COUNTIFS(ag[kn],[@n])", "=COUNTIFS(ag[kt],[@s])", "=COUNTIFS(ag[kb],[@b])", "=COUNTIFS(ag[kb],[@s])", "=COUNTIFS(ag[kn],[@s])",
        "=SUMIFS(ag[r],ag[kn],[@i])", "=SUMIFS(ag[r],ag[kn],\">\"&[@n])", "=SUMIFS(ag[r],ag[kt],\"<>\"&[@s])", "=SUMIFS(ag[r],ag[kt],\"<=\"&[@s])",
        "=AVERAGEIFS(ag[r],ag[kn],[@n])", "=AVERAGEIFS(ag[r],ag[kn],\"<\"&[@i])", "=AVERAGEIFS(ag[kt],ag[kn],[@n])",
        "=COUNTIFS(ag[kd],\">=\"&[@d])", "=SUMIFS(ag[r],ag[kd],[@d])", "=SUMIFS(ag[r],ag[kn],[@n],ag[kt],[@s])", "=SUMIFS(ag[r],ag[kn],[@i],ag[kb],[@b])",
        "=SUMIF(ag[kn],[@n],ag[r])", "=SUMIF(ag[kn],[@n])", "=COUNTIF(ag[kb],[@b])", "=AVERAGEIF(ag[kn],[@i],ag[r])",
        "=COUNTIFS(ag[kt],\"a*\")", "=COUNTIFS(ag[kt],\"<>\")", "=COUNTIFS(ag[kt],\"\")", "=COUNTIFS(ag[kt],\"<=b\")", "=COUNTIFS(ag[kt],\">=b\")",
        "=COUNTIFS(ag[kt],\"a~*b\")", "=COUNTIFS(ag[kt],\"<>a*\")", "=COUNTIFS(ag[kt],\"?\")", "=COUNTIFS(ag[kt],\"true\")", "=COUNTIFS(ag[kb],\"<>TRUE\")",
        "=COUNTIFS(ag[kd],\"<7/31/2023\")", "=COUNTIFS(ag[kd],\">=2000-01-01\")", "=COUNTIFS(ag[kd],\"=Feb/29/2024\")", "=COUNTIFS(ag[kn],\">=\"&\"1\")",
        "=AVERAGEIF(ag[kn],\">0\")", "=COUNTIFS(ag[kn],\"<\")", "=COUNTIFS(ag[kn],\"<>2\",ag[kt],\"<>\")", "=SUMIFS(ag[r],ag[kt],\"*\",ag[kn],\">\"&[@i])",
    ] {
        let c = compile_full(f, &cols, &[], &Lk, "_rf_t_").unwrap_or_else(|e| panic!("{f}: {e}"));
        let sql = format!("SELECT CAST({} AS VARCHAR), {} FROM ({}) ORDER BY k", c.value, c.error, c.wrap("SELECT * FROM t"));
        let ours: Vec<Out> = conn.prepare(&sql).unwrap_or_else(|e| panic!("{f}: {e}\n{sql}")).query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))
            .unwrap().map(|r| match r.unwrap() {
                (_, Some(e)) => Out::Error(e),
                (Some(s), None) => Out::Number(s.parse().unwrap()),
                (None, None) => Out::Blank,
            }).collect();
        assert_eq!(ours.len(), rows.len(), "{f}: one result per row");
        let sheet = f.replace("ag[kn]", "$P$1:$P$7").replace("ag[kt]", "$Q$1:$Q$7").replace("ag[kb]", "$R$1:$R$7").replace("ag[kd]", "$S$1:$S$7").replace("ag[r]", "$T$1:$T$7");
        let theirs = ironcalc_aggregates(&rows, &sheet);
        for (k, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            if !same(a, b) { failures.push(format!("{f} row {k} {:?}: table {a:?}, sheet {b:?}", rows[k])); }
        }
    }
    assert!(failures.is_empty(), "{} differences:\n{}", failures.len(), failures.join("\n"));
}

fn ironcalc_aggregates(rows: &[Row], formula: &str) -> Vec<Out> {
    let mut m = Model::new_empty("diff", "en", "UTC", "en").unwrap();
    for (r, (kn, kt, kb, kd, v)) in AG.iter().enumerate() {
        let rr = r as i32 + 1;
        if let Some(x) = kn { m.set_user_input(0, rr, 16, format!("{x}")).unwrap(); }
        match kt { Some("") => m.set_user_input(0, rr, 17, "=\"\"".into()).unwrap(), Some(s) => m.set_user_input(0, rr, 17, format!("'{s}")).unwrap(), None => {} }
        if let Some(b) = kb { m.set_user_input(0, rr, 18, if *b { "TRUE".into() } else { "FALSE".into() }).unwrap(); }
        if let Some(d) = kd { m.set_user_input(0, rr, 19, format!("{d}")).unwrap(); }
        if let Some(x) = v { m.set_user_input(0, rr, 20, format!("{x}")).unwrap(); }
    }
    let base = 20;
    for (r, row) in rows.iter().enumerate() {
        let rr = base + r as i32;
        if let Some(n) = row.n { m.set_user_input(0, rr, 1, format!("{n}")).unwrap(); }
        if let Some(i) = row.i { m.set_user_input(0, rr, 2, format!("{i}")).unwrap(); }
        if let Some(s) = row.s { m.set_user_input(0, rr, 3, format!("'{s}")).unwrap(); }
        if let Some(b) = row.b { m.set_user_input(0, rr, 4, if b { "TRUE".into() } else { "FALSE".to_string() }).unwrap(); }
        if let Some(d) = row.d { m.set_user_input(0, rr, 5, format!("{d}")).unwrap(); }
        let f = formula.replace("[@n]", &format!("A{rr}")).replace("[@i]", &format!("B{rr}")).replace("[@s]", &format!("C{rr}")).replace("[@b]", &format!("D{rr}")).replace("[@d]", &format!("E{rr}"));
        m.set_user_input(0, rr, 6, f).unwrap();
    }
    m.evaluate();
    (0..rows.len()).map(|r| {
        let rr = base + r as i32;
        if m.get_cell_type(0, rr, 6).unwrap() == CellType::ErrorValue {
            Out::Error(m.get_formatted_cell_value(0, rr, 6).unwrap())
        } else {
            match m.get_cell_value_by_index(0, rr, 6).unwrap() {
                CellValue::None => Out::Blank,
                CellValue::Number(n) => Out::Number(n),
                CellValue::String(s) => Out::Text(s),
                CellValue::Boolean(b) => Out::Bool(b),
            }
        }
    }).collect()
}



/// Expected values worked out by hand from the `AG` rows, independently of either engine.
#[test]
fn conditional_aggregates_by_hand() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE one (k INTEGER); INSERT INTO one VALUES (0); CREATE TABLE ag (kn DOUBLE, kt VARCHAR, kb BOOLEAN, kd DATE, r DOUBLE, o BIGINT)").unwrap();
    for (o, (kn, kt, kb, kd, r)) in AG.iter().enumerate() {
        conn.execute("INSERT INTO ag VALUES (?, ?, ?, ?, ?, ?)", duckdb::params![kn, kt, kb, kd.map(chrono_like), r, o as i64]).unwrap();
    }
    let cols = vec![("k".to_string(), "INTEGER".to_string())];
    let run = |f: &str| -> String {
        let c = compile_full(f, &cols, &[], &Lk, "_rf_t_").unwrap_or_else(|e| panic!("{f}: {e}"));
        let sql = format!("SELECT CAST({} AS VARCHAR), {} FROM ({})", c.value, c.error, c.wrap("SELECT * FROM one"));
        let (v, e): (Option<String>, Option<String>) = conn.query_row(&sql, [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        e.or(v).unwrap_or_default()
    };
    for (f, want) in [
        ("=COUNTIFS(ag[kt],\"<=b\")", "4.0"), // "2", "", "abc", "a*b"
        ("=COUNTIFS(ag[kt],\">=b\")", "2.0"), // "x", "TRUE"
        ("=COUNTIFS(ag[kt],\"a*\")", "2.0"),  // "abc", "a*b"
        ("=COUNTIFS(ag[kt],\"a~*b\")", "1.0"),
        ("=COUNTIFS(ag[kt],\"<>\")", "6.0"),  // every non-blank cell, "" included
        ("=COUNTIFS(ag[kt],\"\")", "2.0"),    // "" and the blank cell
        ("=COUNTIFS(ag[kn],2)", "2.0"),
        ("=COUNTIFS(ag[kt],2)", "1.0"),       // the text "2" reads as the number
        ("=SUMIFS(ag[r],ag[kn],\">0\")", "33.0"),
        ("=AVERAGEIFS(ag[r],ag[kn],\"<0\")", "7.5"),
        ("=AVERAGEIFS(ag[r],ag[kn],99)", "#DIV/0!"),
        ("=COUNTIFS(ag[kd],\">=2000-01-01\")", "4.0"),
        ("=SUMIFS(ag[r],ag[kn],2,ag[kb],TRUE)", "10.0"),
    ] {
        assert_eq!(run(f), want, "{f}");
    }
}
