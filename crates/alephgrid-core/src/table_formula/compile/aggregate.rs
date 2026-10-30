//! Conditional aggregates over whole table columns: SUMIFS, COUNTIFS, AVERAGEIFS, MINIFS,
//! MAXIFS and the single-criterion SUMIF, COUNTIF, AVERAGEIF; and SUM, AVERAGE, MIN, MAX,
//! COUNT and COUNTA of whole columns.
//!
//! Criteria follow the sheet engine's rules (IronCalc `build_criteria`): a criterion value
//! is classified into a comparison (`op`) with a numeric (`n`) or text (`t`) operand, and a
//! source cell matches by that comparison. Classes:
//!
//! | op | Matches |
//! |---|---|
//! | `eqn` | numbers equal to `n`, and text that reads as that number |
//! | `ltn` `len` `gtn` `gen` | numbers only |
//! | `nen` | anything except numbers equal to `n` |
//! | `eqb` / `neb` | logical values equal / anything except them |
//! | `eqe` / `nee` | that error / anything except it |
//! | `eqs` | text equal to `t` (case-insensitive); `""` also matches blank cells |
//! | `nes` | text not equal to `t` (other types never match) |
//! | `lts` `les` `gts` `ges` | text only, by lowercase code points |
//! | `eqr` / `ner` | text matching the wildcard pattern / anything except it |
//! | `eq0` | a blank criterion: numbers equal to 0 |
//! | `nonempty` | `"<>"`: any non-blank cell |
//! | `none` | `"<"`, `">"`… with nothing after them: nothing |
//!
//! Plans: criteria written as literals filter the source once. Criteria from the row whose
//! type is not text only ever test equality, so the source is grouped once by its cells'
//! match keys and left-joined on the row's keys (cached like a lookup). Text criteria can
//! carry operators or wildcards from the data; distinct criterion values are then matched
//! against the source in one join, equality ones through the grouped keys.

use super::*;

const EPS: &str = "2.220446049250313e-16";
const ERRORS: &str = "('#REF!', '#NAME?', '#VALUE!', '#DIV/0!', '#N/A', '#NUM!', '#ERROR!', '#N/IMPL!', '#SPILL!', '#CALC!', '#CIRC!', '#NULL!')";
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

#[derive(Clone, Copy, PartialEq)]
enum Agg {
    Sum,
    Count,
    Average,
    Min,
    Max,
}

/// A criterion: its comparison and operands as SQL, and whether it can vary per row.
struct Criterion {
    op: String,
    n: String,
    t: String,
    /// Written as a literal: a filter on the source, the same for every row.
    constant: bool,
    /// Only equality classes are possible (row values of a non-text type).
    equality_only: bool,
}

/// One criteria column of the aggregated table: its value and error column as SQL.
struct Source {
    x: String,
    xe: Option<String>,
    ty: Ty,
}

/// Rust's `str::parse::<f64>` for finite numbers (what sheets use for numeric text).
fn number_text(x: &str) -> String {
    format!("(CASE WHEN regexp_full_match({x}, '[+-]?([0-9]+[.]?[0-9]*|[.][0-9]+)([eE][+-]?[0-9]+)?') AND isfinite(TRY_CAST({x} AS DOUBLE)) THEN TRY_CAST({x} AS DOUBLE) END)")
}

/// The day serial of date text as the sheet engine reads criteria (`parse_date`, English):
/// three parts split by `/`, `-` or `.`; `yyyy-mm-dd`, otherwise month first; 1–2 digit day,
/// month as 1–2 digits or an English month name, 2- or 4-digit year (below 30 is 20xx).
/// Each part is bound once, in its own layer, so the SQL stays linear in size.
fn date_text(me: &mut Compiler<'_>, x: &str) -> String {
    let bind = |me: &mut Compiler<'_>, s: String| me.bind(s);
    let sep = bind(me, format!("CASE WHEN contains({x}, '/') THEN '/' WHEN contains({x}, '-') THEN '-' WHEN contains({x}, '.') THEN '.' END"));
    me.layer();
    let parts = bind(me, format!("string_split({x}, {sep})"));
    me.layer();
    let p = |i: usize| format!("{parts}[{i}]");
    let iso = bind(me, format!("(length({}) = 4 AND regexp_full_match({}, '[0-9]*') AND regexp_full_match({}, '[0-9]*'))", p(1), p(2), p(3)));
    me.layer();
    let d = bind(me, format!("CASE WHEN {iso} THEN {} ELSE {} END", p(3), p(2)));
    let m = bind(me, format!("CASE WHEN {iso} THEN {} ELSE {} END", p(2), p(1)));
    let y = bind(me, format!("CASE WHEN {iso} THEN {} ELSE {} END", p(1), p(3)));
    let names: String = MONTHS.iter().enumerate()
        .map(|(i, name)| format!(" WHEN {m} = {} OR {m} = {} THEN {}", lit(&name[..3]), lit(name), i + 1))
        .collect();
    me.layer();
    let day = bind(me, format!("CASE WHEN strlen({d}) <= 2 AND regexp_full_match({d}, '[+]?[0-9]+') THEN CAST({d} AS INTEGER) END"));
    let month = bind(me, format!("CASE WHEN strlen({m}) <= 2 THEN CASE WHEN regexp_full_match({m}, '[+]?[0-9]+') THEN CAST({m} AS INTEGER) END {names} END"));
    let year = bind(me, format!(
        "CASE WHEN strlen({y}) IN (2, 4) AND regexp_full_match({y}, '[+-]?[0-9]+') THEN \
         CASE WHEN CAST({y} AS INTEGER) < 30 THEN 2000 + CAST({y} AS INTEGER) WHEN CAST({y} AS INTEGER) < 100 THEN 1900 + CAST({y} AS INTEGER) ELSE CAST({y} AS INTEGER) END END"
    ));
    me.layer();
    let valid = bind(me, format!(
        "({sep} IS NOT NULL AND len({parts}) = 3 AND {day} IS NOT NULL AND {month} BETWEEN 1 AND 12 AND {year} BETWEEN 1 AND 9999 \
         AND {day} BETWEEN 1 AND day(last_day(make_date({year}, {month}, 1))))"
    ));
    let out = format!("CASE WHEN {valid} THEN {} END", serial(&format!("make_date({year}, {month}, {day})")));
    me.layer();
    out
}

/// The same reading of date text, in Rust, for criteria written as literals.
fn parse_date(s: &str) -> Option<f64> {
    let sep = ['/', '-', '.'].into_iter().find(|c| s.contains(*c))?;
    let p: Vec<&str> = s.split(sep).collect();
    if p.len() != 3 {
        return None;
    }
    let iso = p[0].len() == 4 && p[1].chars().all(char::is_numeric) && p[2].chars().all(char::is_numeric);
    let (d, m, y) = if iso { (p[2], p[1], p[0]) } else { (p[1], p[0], p[2]) };
    let day: u32 = if d.len() <= 2 { d.parse().ok()? } else { return None };
    let month: u32 = if m.len() <= 2 {
        m.parse().ok()?
    } else {
        MONTHS.iter().position(|n| *n == m || &n[..3] == m)? as u32 + 1
    };
    if y.len() != 2 && y.len() != 4 {
        return None;
    }
    let y: i32 = y.parse().ok()?;
    let year = if y < 30 { 2000 + y } else if y < 100 { 1900 + y } else { y };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) || day == 0 || day > days_in_month[month as usize - 1] {
        return None;
    }
    // Days from 1970-01-01 (civil calendar), then the 1900 system's 1899-12-30 epoch.
    let (y, m) = if month <= 2 { (year as i64 - 1, month as i64 + 9) } else { (year as i64, month as i64 - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468 + 25569) as f64)
}

/// `from_wildcard_to_regex` in Rust, for criteria written as literals.
fn wildcard_regex_text(w: &str) -> String {
    let escaped: String = w.chars().flat_map(|c| if "\\.+*?()|[]{}^$#&-~".contains(c) { vec!['\\', c] } else { vec![c] }).collect();
    let steps = [("\\?", "."), ("\\*", ".*"), ("\\~\\~", "??"), ("\\~.*", "\\*"), ("\\~.", "\\?"), ("\\~", ""), ("??", "\\~")];
    let body = steps.iter().fold(escaped, |acc, (from, to)| acc.replace(from, to));
    format!("^{body}$")
}

/// A literal criterion classified now (op, number, text), as `build_criteria` would.
fn classify_literal(kind: &ExprKind) -> (String, Option<f64>, Option<String>) {
    let finite = |s: &str| s.parse::<f64>().ok().filter(|f| f.is_finite());
    let error = |s: &str| ERRORS.contains(&format!("'{s}'"));
    match kind {
        ExprKind::Number(n) => ("eqn".into(), Some(*n), None),
        ExprKind::Bool(b) => ("eqb".into(), Some(if *b { 1.0 } else { 0.0 }), None),
        ExprKind::Error(code) => ("eqe".into(), None, Some(code.code().to_string())),
        ExprKind::Text(s) => {
            let (pre, r) = if let Some(r) = s.strip_prefix("<=") { ("le", r) } else if let Some(r) = s.strip_prefix(">=") { ("ge", r) }
                else if let Some(r) = s.strip_prefix("<>") { ("ne", r) } else if let Some(r) = s.strip_prefix('<') { ("lt", r) }
                else if let Some(r) = s.strip_prefix('>') { ("gt", r) } else { ("eq", s.strip_prefix('=').unwrap_or(s)) };
            let lower = r.to_lowercase();
            let boolean = lower.parse::<bool>().ok();
            let wild = r.contains('*') || r.contains('?');
            match pre {
                "le" | "ge" | "lt" | "gt" => {
                    if let Some(f) = finite(r) { (format!("{pre}n"), Some(f), None) }
                    else if r.is_empty() { ("none".into(), None, None) }
                    else if let Some(f) = parse_date(r) { (format!("{pre}n"), Some(f), None) }
                    else { (format!("{pre}s"), None, Some(lower)) }
                }
                "ne" => {
                    if let Some(f) = finite(r) { ("nen".into(), Some(f), None) }
                    else if let Some(b) = boolean { ("neb".into(), Some(if b { 1.0 } else { 0.0 }), None) }
                    else if error(r) { ("nee".into(), None, Some(r.to_string())) }
                    else if wild { ("ner".into(), None, Some(wildcard_regex_text(&lower))) }
                    else if r.is_empty() { ("nonempty".into(), None, None) }
                    else if let Some(f) = parse_date(r) { ("nen".into(), Some(f), None) }
                    else { ("nes".into(), None, Some(lower)) }
                }
                _ => {
                    if let Some(f) = finite(r) { ("eqn".into(), Some(f), None) }
                    else if let Some(b) = boolean { ("eqb".into(), Some(if b { 1.0 } else { 0.0 }), None) }
                    else if error(r) { ("eqe".into(), None, Some(r.to_string())) }
                    else if wild { ("eqr".into(), None, Some(wildcard_regex_text(&lower))) }
                    else if let Some(f) = parse_date(r) { ("eqn".into(), Some(f), None) }
                    else { ("eqs".into(), None, Some(lower)) }
                }
            }
        }
        _ => ("eq0".into(), None, None),
    }
}

/// The sheet engine's wildcard-to-regex conversion (`from_wildcard_to_regex`, anchored):
/// `?` one character, `*` any run, `~` escapes the next character.
fn wildcard_regex(w: &str) -> String {
    // regex::escape: backslash before every meta character.
    let escaped = format!("regexp_replace({w}, '([\\\\.+*?()|\\[\\]{{}}^$#&\\-~])', '\\\\\\1', 'g')");
    let steps = [("\\?", "."), ("\\*", ".*"), ("\\~\\~", "??"), ("\\~.*", "\\*"), ("\\~.", "\\?"), ("\\~", ""), ("??", "\\~")];
    let body = steps.iter().fold(escaped, |acc, (from, to)| format!("replace({acc}, {}, {})", lit(from), lit(to)));
    format!("'^' || {body} || '$'")
}

/// The match key of an equality criterion (`None` for other classes), as SQL.
fn criterion_key(c: &Criterion) -> String {
    format!(
        "(CASE {op} WHEN 'eqn' THEN 'n' || CAST({n} + 0.0 AS VARCHAR) WHEN 'eqb' THEN 'b' || CAST({n} <> 0 AS VARCHAR) \
         WHEN 'eqe' THEN 'e' || {t} WHEN 'eqs' THEN 's' || {t} WHEN 'eq0' THEN 'z' END)",
        op = c.op, n = c.n, t = c.t
    )
}

/// The equality keys a source cell matches (its value's key and the blank equivalents).
fn source_keys(s: &Source) -> String {
    let err = s.xe.as_ref().map(|e| format!("WHEN {e} IS NOT NULL THEN list_value('e' || {e}) ")).unwrap_or_default();
    let x = &s.x;
    let value = match s.ty {
        Ty::Number | Ty::Date => {
            let n = if s.ty == Ty::Date { serial(x) } else { format!("CAST({x} AS DOUBLE)") };
            format!("WHEN {n} = 0 THEN list_value('n' || CAST({n} + 0.0 AS VARCHAR), 'z') ELSE list_value('n' || CAST({n} + 0.0 AS VARCHAR))")
        }
        Ty::Bool => format!("ELSE list_value('b' || CAST({x} AS VARCHAR))"),
        _ => {
            let n = number_text(x);
            format!("WHEN {n} IS NOT NULL THEN list_value('s' || lower({x}), 'n' || CAST({n} + 0.0 AS VARCHAR)) ELSE list_value('s' || lower({x}))")
        }
    };
    format!("(CASE {err}WHEN {x} IS NULL THEN list_value('s') {value} END)")
}

/// Whether source cell `s` matches criterion `c` (with `c`'s operands as SQL).
fn matches(c: &Criterion, s: &Source) -> String {
    let (op, n, t) = (&c.op, &c.n, &c.t);
    let x = &s.x;
    let err = s.xe.as_ref().map(|e| format!("({e} IS NOT NULL)")).unwrap_or_else(|| "FALSE".into());
    let ok = format!("(NOT {err})");
    let xe = s.xe.clone().unwrap_or_else(|| "NULL".into());
    let empty = format!("({x} IS NULL AND {ok})");
    // The cell as a number (numbers and dates), as a number read from text, as text.
    let (num, text, logical): (Option<String>, Option<String>, Option<String>) = match s.ty {
        Ty::Number => (Some(format!("CAST({x} AS DOUBLE)")), None, None),
        Ty::Date => (Some(serial(x)), None, None),
        Ty::Bool => (None, None, Some(x.clone())),
        _ => (None, Some(format!("lower({x})")), None),
    };
    let numeric = num.clone().map(|v| format!("({ok} AND {v} IS NOT NULL)")).unwrap_or_else(|| "FALSE".into());
    let is_text = text.clone().map(|_| format!("({ok} AND {x} IS NOT NULL)")).unwrap_or_else(|| "FALSE".into());
    let cmp_n = |sym: &str| num.as_ref().map(|v| format!("({numeric} AND {v} {sym} {n})")).unwrap_or_else(|| "FALSE".into());
    let cmp_t = |sym: &str| text.as_ref().map(|v| format!("({is_text} AND {v} {sym} {t})")).unwrap_or_else(|| "FALSE".into());
    let eqn = match (&num, &text) {
        (Some(v), _) => format!("({numeric} AND abs({v} - {n}) < {EPS})"),
        (_, Some(_)) => format!("({is_text} AND abs({} - {n}) < {EPS})", number_text(x)),
        _ => "FALSE".into(),
    };
    let nen = match &num {
        Some(v) => format!("(NOT {numeric} OR abs({v} - {n}) > {EPS})"),
        None => "TRUE".into(),
    };
    let eqb = logical.as_ref().map(|v| format!("({ok} AND {v} IS NOT NULL AND {v} = ({n} <> 0))")).unwrap_or_else(|| "FALSE".into());
    let neb = logical.as_ref().map(|v| format!("(NOT ({ok} AND {v} IS NOT NULL) OR {v} <> ({n} <> 0))")).unwrap_or_else(|| "TRUE".into());
    let eqs = match &text {
        Some(v) => format!("(({is_text} AND {v} = {t}) OR ({empty} AND {t} = ''))"),
        None => format!("({empty} AND {t} = '')"),
    };
    let eqr = text.as_ref().map(|v| format!("({is_text} AND regexp_full_match({v}, {t}))")).unwrap_or_else(|| "FALSE".into());
    let eq0 = num.as_ref().map(|v| format!("({numeric} AND {v} = 0)")).unwrap_or_else(|| "FALSE".into());
    format!(
        "(CASE {op} WHEN 'eqn' THEN {eqn} WHEN 'ltn' THEN {lt} WHEN 'len' THEN {le} WHEN 'gtn' THEN {gt} WHEN 'gen' THEN {ge} WHEN 'nen' THEN {nen} \
         WHEN 'eqb' THEN {eqb} WHEN 'neb' THEN {neb} WHEN 'eqe' THEN ({err} AND {xe} = {t}) WHEN 'nee' THEN (NOT {err} OR {xe} <> {t}) \
         WHEN 'eqs' THEN {eqs} WHEN 'nes' THEN {nes} WHEN 'lts' THEN {lts} WHEN 'les' THEN {les} WHEN 'gts' THEN {gts} WHEN 'ges' THEN {ges} \
         WHEN 'eqr' THEN {eqr} WHEN 'ner' THEN NOT {eqr} WHEN 'eq0' THEN {eq0} WHEN 'nonempty' THEN ({x} IS NOT NULL OR {err}) ELSE FALSE END)",
        lt = cmp_n("<"), le = cmp_n("<="), gt = cmp_n(">"), ge = cmp_n(">="),
        nes = cmp_t("<>"), lts = cmp_t("<"), les = cmp_t("<="), gts = cmp_t(">"), ges = cmp_t(">="),
    )
}

/// Length-prefixed concatenation, so distinct tuples never share an encoding.
fn combine(parts: &[String]) -> String {
    if parts.len() == 1 {
        return parts[0].clone();
    }
    parts.iter().map(|p| format!("CAST(length({p}) AS VARCHAR) || ':' || {p}")).collect::<Vec<_>>().join(" || ")
}

impl Compiler<'_> {
    /// Classify a criterion value of type `ty` (SQL `v`, error `e`) as the sheet engine does.
    fn criterion(&mut self, v: &str, e: Option<&str>, ty: Ty, constant: bool) -> Criterion {
        let bind = |me: &mut Self, sql: String| if constant { format!("({sql})") } else { me.bind(sql) };
        let (op, n, t) = match ty {
            Ty::Number | Ty::Date => {
                let n = if ty == Ty::Date { serial(v) } else { format!("CAST({v} AS DOUBLE)") };
                (format!("CASE WHEN {v} IS NULL THEN 'eq0' ELSE 'eqn' END"), n, "CAST(NULL AS VARCHAR)".to_string())
            }
            Ty::Bool => (
                format!("CASE WHEN {v} IS NULL THEN 'eq0' ELSE 'eqb' END"),
                format!("CASE WHEN {v} THEN CAST(1 AS DOUBLE) ELSE CAST(0 AS DOUBLE) END"),
                "CAST(NULL AS VARCHAR)".to_string(),
            ),
            Ty::Blank => ("'eq0'".to_string(), "CAST(NULL AS DOUBLE)".to_string(), "CAST(NULL AS VARCHAR)".to_string()),
            Ty::Text => {
                let pre = bind(self, format!(
                    "CASE WHEN starts_with({v}, '<=') THEN 'le' WHEN starts_with({v}, '>=') THEN 'ge' WHEN starts_with({v}, '<>') THEN 'ne' \
                     WHEN starts_with({v}, '<') THEN 'lt' WHEN starts_with({v}, '>') THEN 'gt' ELSE 'eq' END"
                ));
                let r = bind(self, format!(
                    "CASE WHEN {pre} IN ('le', 'ge', 'ne') THEN substr({v}, 3) WHEN {pre} IN ('lt', 'gt') THEN substr({v}, 2) \
                     WHEN starts_with({v}, '=') THEN substr({v}, 2) ELSE {v} END"
                ));
                self.layer();
                let rn = bind(self, number_text(&r));
                let rd = date_text(self, &r);
                let rd = bind(self, rd);
                self.layer();
                let rl = format!("lower({r})");
                let boolean = format!("{rl} IN ('true', 'false')");
                let error = format!("{r} IN {ERRORS}");
                let wild = format!("(contains({r}, '*') OR contains({r}, '?'))");
                let op = bind(self, format!(
                    "CASE WHEN {v} IS NULL THEN 'eq0' \
                     WHEN {pre} IN ('le', 'ge', 'lt', 'gt') THEN CASE WHEN {rn} IS NOT NULL THEN {pre} || 'n' WHEN {r} = '' THEN 'none' WHEN {rd} IS NOT NULL THEN {pre} || 'n' ELSE {pre} || 's' END \
                     WHEN {pre} = 'ne' THEN CASE WHEN {rn} IS NOT NULL THEN 'nen' WHEN {boolean} THEN 'neb' WHEN {error} THEN 'nee' WHEN {wild} THEN 'ner' WHEN {r} = '' THEN 'nonempty' WHEN {rd} IS NOT NULL THEN 'nen' ELSE 'nes' END \
                     ELSE CASE WHEN {rn} IS NOT NULL THEN 'eqn' WHEN {boolean} THEN 'eqb' WHEN {error} THEN 'eqe' WHEN {wild} THEN 'eqr' WHEN {rd} IS NOT NULL THEN 'eqn' ELSE 'eqs' END END"
                ));
                self.layer();
                let n = format!(
                    "CASE WHEN {op} IN ('eqn', 'nen', 'ltn', 'len', 'gtn', 'gen') THEN COALESCE({rn}, {rd}) \
                     WHEN {op} IN ('eqb', 'neb') THEN CASE WHEN {rl} = 'true' THEN CAST(1 AS DOUBLE) ELSE CAST(0 AS DOUBLE) END END"
                );
                let t = format!("CASE WHEN {op} IN ('eqr', 'ner') THEN {} WHEN {op} IN ('eqe', 'nee') THEN {r} ELSE {rl} END", wildcard_regex(&rl));
                (op, n, t)
            }
        };
        // An error value as the criterion matches that error.
        let (op, n, t) = match e {
            Some(e) => (format!("CASE WHEN {e} IS NOT NULL THEN 'eqe' ELSE {op} END"), n, format!("CASE WHEN {e} IS NOT NULL THEN {e} ELSE {t} END")),
            None => (op, n, t),
        };
        let (op, n, t) = (bind(self, op), bind(self, n), bind(self, t));
        if !constant { self.layer(); }
        Criterion { op, n, t, constant, equality_only: ty != Ty::Text }
    }

    /// A criterion argument: literal criteria are constants; `"op"&date` reads the date as
    /// its serial number, as text concatenation of a date cell does in sheets.
    fn criterion_arg(&mut self, arg: &Expr) -> Result<Criterion, FormulaError> {
        let literal = matches!(arg.kind, ExprKind::Number(_) | ExprKind::Text(_) | ExprKind::Bool(_) | ExprKind::Error(_) | ExprKind::Empty);
        if let ExprKind::Binary { op: BinaryOp::Concat, left, right } = &arg.kind {
            let (a, b) = (self.expr(left)?, self.expr(right)?);
            let as_text = |me: &mut Self, x: &V| -> Result<(String, Option<String>), FormulaError> {
                if x.ty == Ty::Date {
                    let n = format!("CASE WHEN {} IS NOT NULL THEN {} END", x.v, serial(&x.v));
                    Ok((format!("COALESCE({}, '')", super::number_text(&n)), None))
                } else {
                    me.text(x)
                }
            };
            let (x, xe) = as_text(self, &a)?;
            let (y, ye) = as_text(self, &b)?;
            let e = first_error([a.e.clone(), xe, b.e.clone(), ye]);
            let v = self.bind(format!("({x}) || ({y})"));
            let e = e.map(|e| self.bind(e));
            return Ok(self.criterion(&v, e.as_deref(), Ty::Text, false));
        }
        if literal {
            let (op, n, t) = classify_literal(&arg.kind);
            return Ok(Criterion {
                op: lit(&op),
                n: n.map(num_lit).unwrap_or_else(|| "CAST(NULL AS DOUBLE)".into()),
                t: t.map(|t| lit(&t)).unwrap_or_else(|| "CAST(NULL AS VARCHAR)".into()),
                constant: true,
                equality_only: true,
            });
        }
        let c = self.expr(arg)?;
        Ok(self.criterion(&c.v, c.e.as_deref(), c.ty, false))
    }

    /// SUMIFS, COUNTIFS, AVERAGEIFS and their single-criterion forms over whole columns of
    /// one table, evaluated for every row in one plan.
    pub(super) fn conditional(&mut self, name: &str, args: &[Expr], span: Span) -> Result<V, FormulaError> {
        // (aggregated column, [(criteria column, criterion)]).
        let (agg, value, pairs): (Agg, Option<&Expr>, Vec<(&Expr, &Expr)>) = match name {
            "SUMIFS" | "AVERAGEIFS" | "MINIFS" | "MAXIFS" => {
                if args.len() < 3 || args.len().is_multiple_of(2) {
                    return fail(format!("{name} takes a range to aggregate and then pairs of criteria range and criterion"), span);
                }
                let agg = match name { "SUMIFS" => Agg::Sum, "MINIFS" => Agg::Min, "MAXIFS" => Agg::Max, _ => Agg::Average };
                (agg, Some(&args[0]), args[1..].chunks(2).map(|p| (&p[0], &p[1])).collect())
            }
            "COUNTIFS" => {
                if args.len() < 2 || !args.len().is_multiple_of(2) {
                    return fail("COUNTIFS takes pairs of criteria range and criterion", span);
                }
                (Agg::Count, None, args.chunks(2).map(|p| (&p[0], &p[1])).collect())
            }
            "SUMIF" | "AVERAGEIF" => {
                Self::arity(name, args, 2, 3, span)?;
                let agg = if name == "SUMIF" { Agg::Sum } else { Agg::Average };
                (agg, Some(args.get(2).unwrap_or(&args[0])), vec![(&args[0], &args[1])])
            }
            _ => {
                Self::arity(name, args, 2, 2, span)?;
                (Agg::Count, None, vec![(&args[0], &args[1])])
            }
        };
        let column = |e: &Expr| match &e.kind {
            ExprKind::TableColumn { table, column } => Ok((table.clone(), column.clone())),
            _ => fail(format!("{name} needs whole columns of a table here, e.g. Orders[amount]"), e.span),
        };
        let mut ranges = Vec::new();
        if let Some(v) = value { ranges.push((column(v)?, v.span)); }
        for (r, _) in &pairs { ranges.push((column(r)?, r.span)); }
        let table = ranges[0].0 .0.clone();
        if let Some((_, at)) = ranges.iter().find(|((t, _), _)| !t.eq_ignore_ascii_case(&table)) {
            return fail("all ranges must be columns of the same table", *at);
        }
        let rel = self.lookup_relation(&table, ranges[0].1)?;
        let source = |name: &str, at: Span| -> Result<Source, FormulaError> {
            let (col, ty) = Self::lookup_column(&rel, &table, name, at)?;
            let xe = rel.errors.iter().find(|(n, _)| *n == col).map(|(_, e)| format!("s.{}", ident(e)));
            Ok(Source { x: format!("s.{}", ident(&col)), xe, ty })
        };
        let aggregated = match value { Some(v) => Some(source(&column(v)?.1, v.span)?), None => None };
        let mut criteria = Vec::new();
        for (r, k) in &pairs {
            let s = source(&column(r)?.1, r.span)?;
            let c = self.criterion_arg(k)?;
            criteria.push((s, c));
        }

        // The aggregate of the matched rows `s`, as (value, error) over SQL aggregates.
        let (sum_value, sum_error) = match &aggregated {
            Some(a) => {
                let n = match a.ty { Ty::Number => Some(format!("CAST({} AS DOUBLE)", a.x)), Ty::Date => Some(serial(&a.x)), _ => None };
                (n.unwrap_or_else(|| "CAST(NULL AS DOUBLE)".into()), a.xe.clone().unwrap_or_else(|| "CAST(NULL AS VARCHAR)".into()))
            }
            None => ("CAST(NULL AS DOUBLE)".into(), "CAST(NULL AS VARCHAR)".into()),
        };
        let outputs = |sum: &str, count: &str, numbers: &str, error: &str, min: &str, max: &str| -> (String, String) {
            match agg {
                Agg::Sum => (format!("COALESCE({sum}, 0)"), error.to_string()),
                Agg::Count => (format!("CAST({count} AS DOUBLE)"), "CAST(NULL AS VARCHAR)".to_string()),
                Agg::Average => (
                    format!("CASE WHEN {numbers} > 0 THEN {sum} / {numbers} END"),
                    format!("COALESCE({error}, CASE WHEN {numbers} = 0 THEN '#DIV/0!' END)"),
                ),
                // No matching number: 0, as in sheets.
                Agg::Min => (format!("COALESCE({min}, 0)"), error.to_string()),
                Agg::Max => (format!("COALESCE({max}, 0)"), error.to_string()),
            }
        };
        // Over matched source rows `q`: the aggregated cells' sum, the row count, how many of
        // those cells are numbers, and the first error in row order.
        let aggregates = |q: &str| format!(
            "sum({q}._rf_agg_v) AS sm, count(*) AS ct, count({q}._rf_agg_v) AS cn, min({q}._rf_agg_v) AS mn, max({q}._rf_agg_v) AS mx, \
             arg_min({q}._rf_agg_e, {q}._rf_agg_o) FILTER (WHERE {q}._rf_agg_e IS NOT NULL) AS er"
        );
        let (v_out, e_out) = outputs("sm", "ct", "cn", "er", "mn", "mx");
        // The source rows with their aggregated value, error and order, under alias `s`;
        // constant criteria filter it.
        let filter: Vec<String> = criteria.iter().filter(|(_, c)| c.constant).map(|(s, c)| matches(c, s)).collect();
        let filter = if filter.is_empty() { String::new() } else { format!(" WHERE {}", filter.join(" AND ")) };
        let source_rows = format!("SELECT {sum_value} AS _rf_agg_v, {sum_error} AS _rf_agg_e, s._rf_ord AS _rf_agg_o, s.* FROM ({}) s{filter}", rel.sql);
        let row: Vec<&(Source, Criterion)> = criteria.iter().filter(|(_, c)| !c.constant).collect();
        // Grouped by the match keys of the row criteria's columns.
        let lists: String = row.iter().enumerate().map(|(i, (s, _))| format!(", {} AS _rf_agg_l{i}", source_keys(s))).collect();
        let mut grouped = format!("SELECT s._rf_agg_v, s._rf_agg_e, s._rf_agg_o{lists} FROM ({source_rows}) s");
        for i in 0..row.len() {
            grouped = format!("SELECT * EXCLUDE (_rf_agg_l{i}), unnest(_rf_agg_l{i}) AS _rf_agg_u{i} FROM ({grouped})");
        }
        let keys: Vec<String> = (0..row.len()).map(|i| format!("g._rf_agg_u{i}")).collect();
        let grouped = if row.is_empty() {
            format!("SELECT '' AS k, {} FROM ({grouped}) g", aggregates("g"))
        } else {
            format!("SELECT {} AS k, {} FROM ({grouped}) g GROUP BY 1", combine(&keys), aggregates("g"))
        };
        let grouped = format!("SELECT k, {v_out} AS v, {e_out} AS e FROM ({grouped})");
        let alias = format!("{}j{}", self.prefix, self.stages.len());
        let no_match = match agg {
            Agg::Average => ("NULL".to_string(), "'#DIV/0!'".to_string()),
            _ => ("CAST(0 AS DOUBLE)".to_string(), "NULL".to_string()),
        };
        let row_keys: Vec<String> = row.iter().map(|(_, c)| criterion_key(c)).collect();
        if row.iter().all(|(_, c)| c.equality_only) {
            // Every row criterion tests equality: one grouped relation, joined on the row's keys.
            let probe = if row.is_empty() { self.bind("CAST('' AS VARCHAR)".into()) } else { self.bind(combine(&row_keys)) };
            let steps = std::mem::take(&mut self.steps);
            self.stages.push(Stage::Steps(steps));
            self.stages.push(Stage::Join { alias: alias.clone(), sql: grouped, key: probe, reads: rel.reads.clone() });
        } else {
            // Text criteria may carry operators or wildcards: match each distinct criterion
            // once, through the grouped keys when it is an equality, otherwise by comparison.
            let descriptor: Vec<String> = row.iter().flat_map(|(_, c)| {
                [c.op.clone(), format!("COALESCE(CAST({} AS VARCHAR), '')", c.n), format!("COALESCE({}, '')", c.t)]
            }).collect();
            let probe = self.bind(combine(&descriptor));
            let all_keys = row_keys.iter().map(|k| format!("{k} IS NOT NULL")).collect::<Vec<_>>().join(" AND ");
            let hashed = self.bind(format!("CASE WHEN {all_keys} THEN {} END", combine(&row_keys)));
            let input = ident(&format!("{alias}o"));
            let fields: Vec<String> = row.iter().enumerate().flat_map(|(i, (_, c))| {
                [format!("{} AS op{i}", c.op), format!("{} AS n{i}", c.n), format!("{} AS t{i}", c.t)]
            }).collect();
            let distinct = format!("SELECT DISTINCT {probe} AS k, {hashed} AS h, {} FROM {input}", fields.join(", "));
            let compared: Vec<String> = row.iter().enumerate().map(|(i, (s, _))| {
                let at = Criterion { op: format!("d.op{i}"), n: format!("d.n{i}"), t: format!("d.t{i}"), constant: false, equality_only: false };
                matches(&at, s)
            }).collect();
            let criteria_rows = ident(&format!("{alias}d"));
            let by_comparison = format!(
                "SELECT k, {v_out} AS v, {e_out} AS e FROM (SELECT d.k, {} FROM (SELECT * FROM {criteria_rows} WHERE h IS NULL) d JOIN ({source_rows}) s ON {} GROUP BY d.k)",
                aggregates("s"), compared.join(" AND ")
            );
            let by_key = format!("SELECT d.k, g.v, g.e FROM {criteria_rows} d JOIN ({grouped}) g ON g.k = d.h");
            let sql = format!("WITH {criteria_rows} AS MATERIALIZED ({distinct}) SELECT * FROM ({by_key}) UNION ALL SELECT * FROM ({by_comparison})");
            let steps = std::mem::take(&mut self.steps);
            self.stages.push(Stage::Steps(steps));
            self.stages.push(Stage::Correlated { alias: alias.clone(), sql, key: probe });
        }
        let (jk, jv, je) = (ident(&format!("{alias}k")), ident(&format!("{alias}v")), ident(&format!("{alias}e")));
        let value = format!("CASE WHEN {jk} IS NOT NULL THEN {jv} ELSE {} END", no_match.0);
        let error = format!("CASE WHEN {jk} IS NOT NULL THEN {je} ELSE {} END", no_match.1);
        Ok(self.make(value, Some(error), Ty::Number, span))
    }

    /// SUM, AVERAGE, MIN, MAX, COUNT or COUNTA of whole columns of one table (`SUM(T[c])`,
    /// `MAX(T[a], T[b])`): one aggregate of the table, the same value for every row. As in
    /// sheets, text and logical cells are skipped (COUNTA counts every non-blank cell, errors
    /// included), and the first error in argument then row order is the result, except for
    /// COUNT and COUNTA, which never fail.
    pub(super) fn whole_columns(&mut self, name: &str, args: &[Expr], span: Span) -> Result<V, FormulaError> {
        let mut columns = Vec::new();
        for a in args {
            match &a.kind {
                ExprKind::TableColumn { table, column } => columns.push((table.clone(), column.clone(), a.span)),
                _ => return fail(format!("{name} of whole columns takes only whole columns of one table, e.g. {name}(Orders[amount]); add other values outside it"), a.span),
            }
        }
        let table = columns[0].0.clone();
        if let Some((_, _, at)) = columns.iter().find(|(t, _, _)| !t.eq_ignore_ascii_case(&table)) {
            return fail("all ranges must be columns of the same table", *at);
        }
        let rel = self.lookup_relation(&table, columns[0].2)?;
        let mut parts = Vec::new();
        for (i, (_, c, at)) in columns.iter().enumerate() {
            let (col, ty) = Self::lookup_column(&rel, &table, c, *at)?;
            let x = format!("s.{}", ident(&col));
            let xe = rel.errors.iter().find(|(n, _)| *n == col).map(|(_, e)| format!("s.{}", ident(e)));
            let v = match ty { Ty::Number => format!("CAST({x} AS DOUBLE)"), Ty::Date => serial(&x), _ => "CAST(NULL AS DOUBLE)".into() };
            let filled = match &xe { Some(e) => format!("({x} IS NOT NULL OR {e} IS NOT NULL)"), None => format!("({x} IS NOT NULL)") };
            let first_error = match &xe { Some(e) => format!("arg_min({e}, s._rf_ord) FILTER (WHERE {e} IS NOT NULL)"), None => "CAST(NULL AS VARCHAR)".into() };
            parts.push(format!(
                "sum({v}) AS sm{i}, count({v}) AS cn{i}, min({v}) AS mn{i}, max({v}) AS mx{i}, count(*) FILTER (WHERE {filled}) AS ca{i}, {first_error} AS er{i}"
            ));
        }
        let n = columns.len();
        let each = |p: &str| (0..n).map(|i| format!("{p}{i}")).collect::<Vec<_>>();
        let total = |p: &str| each(p).iter().map(|c| format!("COALESCE({c}, 0)")).collect::<Vec<_>>().join(" + ");
        let error = if n == 1 { "er0".to_string() } else { format!("COALESCE({})", each("er").join(", ")) };
        let (v, e) = match name {
            "SUM" => (total("sm"), error),
            "AVERAGE" => (
                format!("CASE WHEN ({cn}) > 0 THEN ({sm}) / ({cn}) END", cn = total("cn"), sm = total("sm")),
                format!("COALESCE({error}, CASE WHEN ({}) = 0 THEN '#DIV/0!' END)", total("cn")),
            ),
            "MIN" => (format!("COALESCE(least({}), 0)", each("mn").join(", ")), error),
            "MAX" => (format!("COALESCE(greatest({}), 0)", each("mx").join(", ")), error),
            "COUNT" => (format!("CAST({} AS DOUBLE)", total("cn")), "CAST(NULL AS VARCHAR)".into()),
            _ => (format!("CAST({} AS DOUBLE)", total("ca")), "CAST(NULL AS VARCHAR)".into()),
        };
        let e = format!("COALESCE({e}, CASE WHEN NOT isfinite({v}) THEN '#NUM!' END)");
        let grouped = format!("SELECT '' AS k, {v} AS v, {e} AS e FROM (SELECT {} FROM ({}) s)", parts.join(", "), rel.sql);
        let alias = format!("{}j{}", self.prefix, self.stages.len());
        let probe = self.bind("CAST('' AS VARCHAR)".into());
        let steps = std::mem::take(&mut self.steps);
        self.stages.push(Stage::Steps(steps));
        self.stages.push(Stage::Join { alias: alias.clone(), sql: grouped, key: probe, reads: rel.reads.clone() });
        let (jv, je) = (ident(&format!("{alias}v")), ident(&format!("{alias}e")));
        Ok(self.make(jv, Some(je), Ty::Number, span))
    }
}
