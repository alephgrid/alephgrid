//! Golden values and a differential check against IronCalc (the sheet engine).
use super::*;
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
    let steps: String = c.steps.iter().map(|(a, s)| format!(", {s} AS {}", crate::sql::ident(a))).collect();
    let value = match c.ty {
        Ty::Date => format!("CAST(date_diff('day', DATE '1899-12-30', {}) AS VARCHAR)", c.value),
        _ => format!("CAST({} AS VARCHAR)", c.value),
    };
    let sql = format!("SELECT {value}, {} FROM (SELECT *{steps} FROM t) ORDER BY k", c.error);
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
    assert!(err("=XLOOKUP([@amount],T[a],T[b])").message.contains("not available yet"));
    assert!(err("=VLOOKUPX(1)").message.contains("not available in table formulas"));
    assert!(err("=ROUND(1)").message.contains("takes 2 arguments"));
    assert!(err("=AND(\"x\")").message.contains("not a condition"));
    assert!(err("=T[[#This Row],[amount]]").message.contains("[@column]"));
    let e = err("=1+[@missing]");
    assert_eq!((e.span.start, e.span.end), (3, 13));
    // Column names resolve case-insensitively and are reported as named in the table.
    let ok = compile("=[@AMOUNT]*2", &cols, "_t").unwrap();
    assert_eq!(ok.columns, vec!["amount".to_string()]);
    assert_eq!(ok.ty, Ty::Number);
    assert_eq!(referenced_columns("=IF([@a]>1,[b],[@[c d]])").unwrap().iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["a", "b", "c d"]);
}
