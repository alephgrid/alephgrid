//! Binding, static typing and SQL generation for table formulas.
//!
//! Every node compiles to a value and an optional error, each a SQL expression. Non-trivial
//! expressions are bound once as select-list aliases (`steps`, in dependency order), so a
//! value used several times is computed once and the SQL stays linear in the formula size.
//! All SQL that could fail at run time (casts, date arithmetic, string slicing) is guarded:
//! invalid inputs produce a spreadsheet error value, never a failed query.

use std::collections::BTreeSet;

use super::parse::{parse, BinaryOp, CellRef, ErrorCode, Expr, ExprKind, FormulaError, Span, UnaryOp};
use crate::sql::{ident, lit};

mod aggregate;

/// Static type of a table formula value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Number,
    Text,
    Bool,
    Date,
    /// An omitted argument: 0, "" or FALSE depending on where it is used.
    Blank,
}

impl Ty {
    pub fn name(self) -> &'static str {
        match self {
            Ty::Number => "Number",
            Ty::Text => "Text",
            Ty::Bool => "Logical",
            Ty::Date => "Date",
            Ty::Blank => "Blank",
        }
    }

    /// SQL type of a column holding this type.
    pub fn sql_type(self) -> &'static str {
        match self {
            Ty::Number | Ty::Blank => "DOUBLE",
            Ty::Text => "VARCHAR",
            Ty::Bool => "BOOLEAN",
            Ty::Date => "DATE",
        }
    }
}

/// The table-formula type of a DuckDB column type, if formulas can use it.
pub fn column_type(sql_type: &str) -> Option<Ty> {
    let t = sql_type.trim().to_ascii_uppercase();
    let base = t.split('(').next().unwrap_or("").trim();
    match base {
        "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "HUGEINT" | "UTINYINT" | "USMALLINT" | "UINTEGER" | "UBIGINT" | "UHUGEINT"
        | "FLOAT" | "DOUBLE" | "REAL" | "DECIMAL" | "NUMERIC" | "INT" | "INT1" | "INT2" | "INT4" | "INT8" => Some(Ty::Number),
        "VARCHAR" | "TEXT" | "STRING" => Some(Ty::Text),
        "BOOLEAN" | "BOOL" => Some(Ty::Bool),
        "DATE" => Some(Ty::Date),
        _ => None,
    }
}

/// Another table a formula can read whole columns of (lookups).
pub struct Relation {
    /// A `SELECT` producing the table's rows, including `_rf_ord`.
    pub sql: String,
    pub columns: Vec<(String, String)>,
    /// (column, error column) for its spreadsheet-formula columns.
    pub errors: Vec<(String, String)>,
    /// Tables whose data `sql` reads (lowercase), for caching results built from it.
    pub reads: Vec<String>,
}

/// The value of a sheet cell a formula reads, as the sheet engine computed it.
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    Blank,
    Number(f64),
    Text(String),
    Bool(bool),
    /// An error value as the sheet shows it: `#DIV/0!`, `#REF!`, …
    Error(String),
}

/// Resolves table names in `Table[column]` references, and sheet cells.
pub trait Relations {
    fn relation(&self, table: &str) -> Result<Option<Relation>, String>;
    /// The value of a sheet cell, or `None` when there is no such sheet.
    fn cell(&self, _cell: &CellRef) -> Result<Option<CellValue>, String> {
        Ok(None)
    }
}

struct NoRelations;
impl Relations for NoRelations {
    fn relation(&self, _: &str) -> Result<Option<Relation>, String> { Ok(None) }
}

/// Evaluation stages: select-list steps, or a left join of a grouped lookup relation whose
/// `k`, `v`, `e` outputs become columns `<alias>k`, `<alias>v`, `<alias>e`.
#[derive(Debug, Clone)]
pub enum Stage {
    Steps(Vec<(String, String)>),
    /// `reads` are the tables whose data `sql` reads (see [`Relation::reads`]).
    Join { alias: String, sql: String, key: String, reads: Vec<String> },
    /// Like `Join`, but `sql` also reads the input relation, under the name `<alias>o`
    /// (conditional aggregates whose criteria come from the rows themselves).
    Correlated { alias: String, sql: String, key: String },
}

/// A compiled table formula: `stages` add columns to the input relation in order; `value`
/// and `error` then read them.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub stages: Vec<Stage>,
    /// Value, `NULL` where the row has an error.
    pub value: String,
    /// Error code (`'#DIV/0!'`…) or `NULL`.
    pub error: String,
    pub ty: Ty,
    /// Columns of the table the formula reads, as named in the table.
    pub columns: Vec<String>,
    /// Other tables the formula reads.
    pub tables: Vec<String>,
}

impl Compiled {
    /// The input relation with every stage applied (temporary columns included).
    pub fn wrap(&self, input: &str) -> String {
        let mut sql = input.to_string();
        for stage in &self.stages {
            sql = match stage {
                Stage::Steps(steps) if steps.is_empty() => sql,
                Stage::Steps(steps) => {
                    let list: String = steps.iter().map(|(a, s)| format!(", {s} AS {}", ident(a))).collect();
                    format!("SELECT *{list} FROM ({sql})")
                }
                Stage::Join { alias, sql: sub, key, .. } => format!(
                    "SELECT o.*, j.k AS {k}, j.v AS {v}, j.e AS {e} FROM ({sql}) o LEFT JOIN ({sub}) j ON j.k = o.{key}",
                    k = ident(&format!("{alias}k")), v = ident(&format!("{alias}v")), e = ident(&format!("{alias}e"))
                ),
                Stage::Correlated { alias, sql: sub, key } => format!(
                    "WITH {input} AS MATERIALIZED ({sql}) SELECT o.*, j.k AS {k}, j.v AS {v}, j.e AS {e} FROM {input} o LEFT JOIN ({sub}) j ON j.k = o.{key}",
                    input = ident(&format!("{alias}o")),
                    k = ident(&format!("{alias}k")), v = ident(&format!("{alias}v")), e = ident(&format!("{alias}e"))
                ),
            };
        }
        sql
    }

    /// Names of the temporary columns `wrap` adds.
    pub fn temps(&self) -> Vec<String> {
        self.stages.iter().flat_map(|s| match s {
            Stage::Steps(steps) => steps.iter().map(|(a, _)| a.clone()).collect::<Vec<_>>(),
            Stage::Join { alias, .. } | Stage::Correlated { alias, .. } => vec![format!("{alias}k"), format!("{alias}v"), format!("{alias}e")],
        }).collect()
    }
}

#[derive(Debug, Clone)]
struct V {
    v: String,
    e: Option<String>,
    ty: Ty,
    /// A column value, which aggregate-style functions treat like a referenced cell.
    is_ref: bool,
    /// The literal `""`, which becomes blank where another type is expected.
    empty_text: bool,
    span: Span,
}

struct Compiler<'a> {
    columns: &'a [(String, String)],
    /// Columns that carry an error code in a companion column (other spreadsheet formulas).
    errors: &'a [(String, String)],
    relations: &'a dyn Relations,
    stages: Vec<Stage>,
    tables: BTreeSet<String>,
    steps: Vec<(String, String)>,
    prefix: String,
    refs: BTreeSet<String>,
}

fn fail<T>(message: impl Into<String>, span: Span) -> Result<T, FormulaError> {
    Err(FormulaError { message: message.into(), span })
}

fn num_lit(n: f64) -> String {
    format!("CAST({n:?} AS DOUBLE)")
}

fn err_lit(code: ErrorCode) -> String {
    lit(code.code())
}

/// First error of the operands, left to right, then the operation's own.
fn first_error(list: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let all: Vec<String> = list.into_iter().flatten().collect();
    match all.len() {
        0 => None,
        1 => all.into_iter().next(),
        _ => Some(format!("COALESCE({})", all.join(", "))),
    }
}

/// 15 significant digits, as sheet arithmetic compares and rounds.
fn p15(x: &str) -> String {
    format!("CAST(printf('%.14e', CAST({x} AS DOUBLE)) AS DOUBLE)")
}

/// Day serial of a date in the 1900 date system (same epoch as Alephgrid sheets).
fn serial(d: &str) -> String {
    format!("CAST(date_diff('day', DATE '1899-12-30', {d}) AS DOUBLE)")
}

/// The date of a serial, guarded to the supported range (1 .. 9999-12-31).
fn serial_date(n: &str) -> (String, String) {
    let ok = format!("({n} >= 1 AND {n} < 2958466)");
    (
        format!("CASE WHEN {ok} THEN CAST(DATE '1899-12-30' + CAST(floor({n}) AS INTEGER) AS DATE) END"),
        format!("CASE WHEN NOT {ok} THEN '#NUM!' END"),
    )
}

impl Compiler<'_> {
    fn bind(&mut self, sql: String) -> String {
        let n: usize = self.steps.len() + self.stages.iter().map(|s| match s { Stage::Steps(x) => x.len(), Stage::Join { .. } | Stage::Correlated { .. } => 1 }).sum::<usize>();
        let name = format!("{}{}", self.prefix, n);
        self.steps.push((name.clone(), sql));
        ident(&name)
    }

    /// End the current select-list layer: later steps read the earlier ones as columns of a
    /// subquery. DuckDB inlines a lateral alias at each use, so a long chain of steps that
    /// reuse each other must be split, or its expression grows exponentially.
    fn layer(&mut self) {
        if !self.steps.is_empty() {
            let steps = std::mem::take(&mut self.steps);
            self.stages.push(Stage::Steps(steps));
        }
    }

    /// Literals and plain identifiers are used in place; anything else is bound once.
    fn simple(sql: &str) -> bool {
        let quoted = |q: char| sql.len() >= 2 && sql.starts_with(q) && sql.ends_with(q) && !sql[1..sql.len() - 1].replace(&format!("{q}{q}"), "").contains(q);
        let number = sql.strip_prefix("CAST(").and_then(|r| r.strip_suffix(" AS DOUBLE)")).is_some_and(|n| n.parse::<f64>().is_ok());
        quoted('"') || quoted('\'') || number || matches!(sql, "TRUE" | "FALSE" | "NULL")
    }

    fn make(&mut self, v: String, e: Option<String>, ty: Ty, span: Span) -> V {
        let v = if Self::simple(&v) { v } else { self.bind(v) };
        let e = e.map(|e| if Self::simple(&e) { e } else { self.bind(e) });
        V { v, e, ty, is_ref: false, empty_text: false, span }
    }

    // ---------------------------------------------------------------- coercions

    /// As a number: blank 0, logical 1/0, dates their serial, text parsed (else #VALUE!).
    fn num(&mut self, x: &V) -> (String, Option<String>) {
        let v = &x.v;
        match x.ty {
            Ty::Number => (format!("COALESCE(CAST({v} AS DOUBLE), 0)"), None),
            Ty::Blank => ("CAST(0 AS DOUBLE)".into(), None),
            Ty::Bool => (format!("CASE WHEN {v} THEN CAST(1 AS DOUBLE) ELSE CAST(0 AS DOUBLE) END"), None),
            Ty::Date => (format!("COALESCE({}, 0)", serial(v)), None),
            Ty::Text => {
                let parsed = self.bind(format!(
                    "CASE WHEN {v} IS NULL THEN CAST(0 AS DOUBLE) \
                     WHEN regexp_full_match(trim({v}, ' \t\n\r'), '[+-]?([0-9]+[.]?[0-9]*|[.][0-9]+)([eE][+-]?[0-9]+)?') \
                     THEN TRY_CAST(trim({v}, ' \t\n\r') AS DOUBLE) END"
                ));
                (format!("COALESCE({parsed}, 0)"), Some(format!("CASE WHEN {parsed} IS NULL OR NOT isfinite({parsed}) THEN '#VALUE!' END")))
            }
        }
    }

    /// As text: numbers in their shortest decimal form, logical TRUE/FALSE, blank "".
    fn text(&mut self, x: &V) -> Result<(String, Option<String>), FormulaError> {
        let v = &x.v;
        Ok(match x.ty {
            Ty::Text => (format!("COALESCE({v}, '')"), None),
            Ty::Blank => ("''".into(), None),
            Ty::Bool => (format!("CASE WHEN {v} IS NULL THEN '' WHEN {v} THEN 'TRUE' ELSE 'FALSE' END"), None),
            Ty::Number => (format!("CASE WHEN {v} IS NULL THEN '' ELSE {} END", number_text(&format!("CAST({v} AS DOUBLE)"))), None),
            Ty::Date => return fail("a date cannot be used as text here; use YEAR, MONTH and DAY", x.span),
        })
    }

    /// As a condition: numbers ≠ 0, text "TRUE"/"FALSE" (else #VALUE!), blank FALSE.
    fn cond(&mut self, x: &V) -> (String, Option<String>) {
        let v = &x.v;
        match x.ty {
            Ty::Bool => (format!("COALESCE({v}, FALSE)"), None),
            Ty::Blank => ("FALSE".into(), None),
            Ty::Number => (format!("COALESCE(CAST({v} AS DOUBLE) <> 0, FALSE)"), None),
            Ty::Date => (format!("COALESCE({} <> 0, FALSE)", serial(v)), None),
            Ty::Text => (
                format!("COALESCE(upper({v}) = 'TRUE', FALSE)"),
                Some(format!("CASE WHEN {v} IS NOT NULL AND upper({v}) NOT IN ('TRUE', 'FALSE') THEN '#VALUE!' END")),
            ),
        }
    }

    // ---------------------------------------------------------------- nodes

    fn expr(&mut self, e: &Expr) -> Result<V, FormulaError> {
        let span = e.span;
        match &e.kind {
            ExprKind::Number(n) => Ok(V { v: num_lit(*n), e: None, ty: Ty::Number, is_ref: false, empty_text: false, span }),
            ExprKind::Text(s) => Ok(V { v: lit(s), e: None, ty: Ty::Text, is_ref: false, empty_text: s.is_empty(), span }),
            ExprKind::Bool(b) => Ok(V { v: if *b { "TRUE" } else { "FALSE" }.into(), e: None, ty: Ty::Bool, is_ref: false, empty_text: false, span }),
            ExprKind::Error(code) => Ok(V { v: "NULL".into(), e: Some(err_lit(*code)), ty: Ty::Number, is_ref: false, empty_text: false, span }),
            ExprKind::Empty => Ok(V { v: "NULL".into(), e: None, ty: Ty::Blank, is_ref: false, empty_text: false, span }),
            ExprKind::Column { name, .. } => self.column(name, span),
            ExprKind::Cell { cell, .. } => self.cell(cell, span),
            ExprKind::TableColumn { table, column } => fail(
                format!("{table}[{column}] is a whole column; use it inside a lookup or conditional aggregate, or [@{column}] for this row"),
                span,
            ),
            ExprKind::Table { table } => fail(
                format!("{table} is not a function call or a column; write [@{table}] for a column of this row, or use a whole table inside VLOOKUP"),
                span,
            ),
            ExprKind::Unary { op, expr } => {
                let x = self.expr(expr)?;
                let (n, ne) = self.num(&x);
                let v = match op {
                    UnaryOp::Neg => format!("-({n})"),
                    UnaryOp::Plus => return Ok(V { span, ..x }),
                    UnaryOp::Percent => format!("({n}) / 100"),
                };
                Ok(self.make(v, first_error([x.e.clone(), ne]), Ty::Number, span))
            }
            ExprKind::Binary { op, left, right } => self.binary(*op, left, right, span),
            ExprKind::Call { name, args } => self.call(name, args, span),
        }
    }

    fn column(&mut self, name: &str, span: Span) -> Result<V, FormulaError> {
        let Some((actual, sql_type)) = self.columns.iter().find(|(c, _)| c.eq_ignore_ascii_case(name) || c == name) else {
            return fail(format!("this table has no column named {name:?}"), span);
        };
        let Some(ty) = column_type(sql_type) else {
            return fail(format!("column {actual:?} has type {sql_type}, which table formulas cannot use yet"), span);
        };
        self.refs.insert(actual.clone());
        let e = self.errors.iter().find(|(c, _)| c == actual).map(|(_, err)| ident(err));
        Ok(V { v: ident(actual), e, ty, is_ref: true, empty_text: false, span })
    }

    /// A sheet cell's value as of this evaluation, used like a referenced cell: blank is a
    /// NULL value, as an empty column value is.
    fn cell(&mut self, cell: &CellRef, span: Span) -> Result<V, FormulaError> {
        let Some(value) = self.relations.cell(cell).map_err(|message| FormulaError { message, span })? else {
            return fail(format!("there is no sheet named {:?}", cell.sheet), span);
        };
        let (v, e, ty) = match value {
            CellValue::Blank => ("CAST(NULL AS DOUBLE)".to_string(), None, Ty::Number),
            CellValue::Number(n) => (num_lit(n), None, Ty::Number),
            CellValue::Text(t) => (lit(&t), None, Ty::Text),
            CellValue::Bool(b) => (if b { "TRUE" } else { "FALSE" }.to_string(), None, Ty::Bool),
            CellValue::Error(code) => ("CAST(NULL AS DOUBLE)".to_string(), Some(lit(&code)), Ty::Number),
        };
        Ok(V { v, e, ty, is_ref: true, empty_text: false, span })
    }

    fn binary(&mut self, op: BinaryOp, left: &Expr, right: &Expr, span: Span) -> Result<V, FormulaError> {
        let a = self.expr(left)?;
        let b = self.expr(right)?;
        if op == BinaryOp::Concat {
            let (x, xe) = self.text(&a)?;
            let (y, ye) = self.text(&b)?;
            return Ok(self.make(format!("({x}) || ({y})"), first_error([a.e.clone(), xe, b.e.clone(), ye]), Ty::Text, span));
        }
        if op.is_comparison() {
            let v = self.compare(op, &a, &b)?;
            let (v, ve) = v;
            return Ok(self.make(v, first_error([a.e.clone(), b.e.clone(), ve]), Ty::Bool, span));
        }
        // Date arithmetic keeps dates where a spreadsheet's date format would.
        let result_ty = match (op, a.ty, b.ty) {
            (BinaryOp::Add, Ty::Date, Ty::Number | Ty::Blank | Ty::Bool) | (BinaryOp::Add, Ty::Number | Ty::Blank | Ty::Bool, Ty::Date) => Ty::Date,
            (BinaryOp::Sub, Ty::Date, Ty::Number | Ty::Blank | Ty::Bool) => Ty::Date,
            _ => Ty::Number,
        };
        let (x, xe) = self.num(&a);
        let (y, ye) = self.num(&b);
        let (x, y) = (self.bind(x), self.bind(y));
        let (raw, own) = match op {
            BinaryOp::Add => (format!("{x} + {y}"), None),
            BinaryOp::Sub => (format!("{x} - {y}"), None),
            BinaryOp::Mul => (format!("{x} * {y}"), None),
            BinaryOp::Div => (format!("CASE WHEN {y} <> 0 THEN {x} / {y} END"), Some(format!("CASE WHEN {y} = 0 THEN '#DIV/0!' END"))),
            BinaryOp::Pow => (
                format!("CASE WHEN NOT ({x} = 0 AND {y} <= 0) AND NOT ({x} < 0 AND {y} <> floor({y})) THEN pow({x}, {y}) END"),
                Some(format!("CASE WHEN {x} = 0 AND {y} < 0 THEN '#DIV/0!' WHEN {x} = 0 AND {y} = 0 THEN '#NUM!' WHEN {x} < 0 AND {y} <> floor({y}) THEN '#NUM!' END")),
            ),
            _ => unreachable!(),
        };
        let raw = self.bind(raw);
        let finite = format!("CASE WHEN {raw} IS NOT NULL AND NOT isfinite({raw}) THEN '#NUM!' END");
        let e = first_error([a.e.clone(), xe, b.e.clone(), ye, own, Some(finite)]);
        if result_ty == Ty::Date {
            let (d, de) = serial_date(&raw);
            return Ok(self.make(d, first_error([e, Some(de)]), Ty::Date, span));
        }
        Ok(self.make(format!("CASE WHEN isfinite({raw}) THEN {raw} END"), e, Ty::Number, span))
    }

    /// Spreadsheet comparison: numbers at 15 significant digits, text case-insensitively,
    /// Number < Text < Logical across types; blank takes the other side's zero.
    fn compare(&mut self, op: BinaryOp, a: &V, b: &V) -> Result<(String, Option<String>), FormulaError> {
        let sym = match op { BinaryOp::Eq => "=", BinaryOp::Ne => "<>", BinaryOp::Lt => "<", BinaryOp::Le => "<=", BinaryOp::Gt => ">", _ => ">=" };
        // Ordering result of comparing a (rank) with b (rank) as -1/0/1.
        let cmp_to_bool = |c: String| -> String { format!("(({c}) {sym} 0)") };
        let rank = |t: Ty| match t { Ty::Number | Ty::Date => 0, Ty::Text => 1, Ty::Bool => 2, Ty::Blank => -1 };
        let numeric = |me: &mut Self, x: &V| -> String {
            match x.ty {
                Ty::Date => format!("COALESCE({}, 0)", serial(&x.v)),
                Ty::Blank => "CAST(0 AS DOUBLE)".into(),
                Ty::Bool => format!("CASE WHEN {} THEN 1 ELSE 0 END", x.v),
                _ => { let _ = me; format!("COALESCE(CAST({} AS DOUBLE), 0)", x.v) }
            }
        };
        let num_cmp = |x: &str, y: &str| -> String {
            let (px, py) = (p15(x), p15(y));
            format!("CASE WHEN abs({py} - {px}) < 2.220446049250313e-16 THEN 0 WHEN {px} < {py} THEN -1 ELSE 1 END")
        };
        let text_cmp = |x: &str, y: &str| -> String {
            format!("CASE WHEN upper({x}) = upper({y}) THEN 0 WHEN upper({x}) < upper({y}) THEN -1 ELSE 1 END")
        };
        let bool_cmp = |x: &str, y: &str| -> String {
            format!("CASE WHEN {x} = {y} THEN 0 WHEN {x} THEN 1 ELSE -1 END")
        };
        let same = |me: &mut Self, t: Ty, x: &V, y: &V| -> String {
            match t {
                Ty::Text => text_cmp(&format!("COALESCE({}, '')", x.v), &format!("COALESCE({}, '')", y.v)),
                Ty::Bool => bool_cmp(&format!("COALESCE({}, FALSE)", x.v), &format!("COALESCE({}, FALSE)", y.v)),
                _ => { let (nx, ny) = (numeric(me, x), numeric(me, y)); num_cmp(&nx, &ny) }
            }
        };
        let (ra, rb) = (rank(a.ty), rank(b.ty));
        let c = if ra == rb || ra < 0 || rb < 0 {
            let t = if ra < 0 { b.ty } else { a.ty };
            same(self, t, a, b)
        } else {
            // Different types: a blank value takes the other side's type; otherwise the
            // type order decides.
            let blank_a = same(self, b.ty, &V { v: "NULL".into(), ..a.clone() }, b);
            let blank_b = same(self, a.ty, a, &V { v: "NULL".into(), ..b.clone() });
            let order = if ra < rb { "-1" } else { "1" };
            format!(
                "CASE WHEN {av} IS NULL AND {bv} IS NULL THEN 0 WHEN {av} IS NULL THEN {blank_a} WHEN {bv} IS NULL THEN {blank_b} ELSE {order} END",
                av = a.v, bv = b.v
            )
        };
        Ok((cmp_to_bool(c), None))
    }

    // ---------------------------------------------------------------- functions

    fn arity(name: &str, args: &[Expr], min: usize, max: usize, span: Span) -> Result<(), FormulaError> {
        if args.len() < min || args.len() > max {
            let want = if min == max { format!("{min}") } else if max == usize::MAX { format!("at least {min}") } else { format!("{min} to {max}") };
            return fail(format!("{name} takes {want} argument{}, not {}", if want == "1" { "" } else { "s" }, args.len()), span);
        }
        Ok(())
    }

    /// One result type for alternatives (IF branches, IFERROR value and fallback).
    fn unify(&self, name: &str, vals: &[&V], span: Span) -> Result<Ty, FormulaError> {
        let mut ty: Option<Ty> = None;
        for v in vals {
            if v.ty == Ty::Blank || v.empty_text { continue; }
            match ty {
                None => ty = Some(v.ty),
                Some(t) if t == v.ty => {}
                Some(t) => {
                    return fail(format!(
                        "{name} returns {} here and {} there; a column needs one type (use \"\" for an empty result)",
                        t.name(), v.ty.name()
                    ), v.span)
                }
            }
        }
        // All alternatives empty text: the result is text.
        Ok(ty.unwrap_or(if vals.iter().any(|v| v.empty_text) { Ty::Text } else { Ty::Blank }))
        .and_then(|t| if t == Ty::Blank { fail(format!("{name} always returns an empty value"), span) } else { Ok(t) })
    }

    /// A branch value as the unified type (`""` becomes blank for non-text types).
    fn as_ty(&mut self, v: &V, ty: Ty) -> String {
        if (v.empty_text || v.ty == Ty::Blank) && ty != Ty::Text {
            return "NULL".into();
        }
        if v.ty == Ty::Blank {
            return "''".into();
        }
        v.v.clone()
    }

    fn call(&mut self, name: &str, args: &[Expr], span: Span) -> Result<V, FormulaError> {
        let name_upper = name.to_uppercase();
        match name_upper.as_str() {
            "\u{0}THISROW" => {
                // Table[[#This Row],[col]]: only the formula's own table has a current row.
                fail("refer to this row of the same table as [@column]; other tables are reached through lookups", span)
            }
            "IF" => {
                Self::arity("IF", args, 2, 3, span)?;
                let c = self.expr(&args[0])?;
                let a = self.expr(&args[1])?;
                let b = if args.len() == 3 { Some(self.expr(&args[2])?) } else { None };
                if b.is_none() && a.ty != Ty::Bool {
                    return fail("add a value for when the condition is false: a spreadsheet returns FALSE, which a non-logical column cannot hold", span);
                }
                let b = b.unwrap_or(V { v: "FALSE".into(), e: None, ty: Ty::Bool, is_ref: false, empty_text: false, span });
                let ty = self.unify("IF", &[&a, &b], span)?;
                let (cv, ce) = self.cond(&c);
                let cv = self.bind(cv);
                let (av, bv) = (self.as_ty(&a, ty), self.as_ty(&b, ty));
                let ae = a.e.clone().unwrap_or_else(|| "NULL".into());
                let be = b.e.clone().unwrap_or_else(|| "NULL".into());
                let cerr = first_error([c.e.clone(), ce]);
                let e = match &cerr {
                    Some(ce) => format!("CASE WHEN {ce} IS NOT NULL THEN {ce} WHEN {cv} THEN {ae} ELSE {be} END"),
                    None => format!("CASE WHEN {cv} THEN {ae} ELSE {be} END"),
                };
                let e = if a.e.is_none() && b.e.is_none() && cerr.is_none() { None } else { Some(e) };
                Ok(self.make(format!("CASE WHEN {cv} THEN {av} ELSE {bv} END"), e, ty, span))
            }
            "IFS" => {
                if args.len() < 2 || !args.len().is_multiple_of(2) {
                    return fail("IFS takes pairs of condition and value", span);
                }
                let mut pairs = Vec::new();
                for p in args.chunks(2) {
                    pairs.push((self.expr(&p[0])?, self.expr(&p[1])?));
                }
                let values: Vec<&V> = pairs.iter().map(|(_, v)| v).collect();
                let ty = self.unify("IFS", &values, span)?;
                let mut v = String::from("CASE");
                let mut e = String::from("CASE");
                for (c, x) in &pairs {
                    let (cv, ce) = self.cond(c);
                    let cv = self.bind(cv);
                    if let Some(ce) = first_error([c.e.clone(), ce]) {
                        e.push_str(&format!(" WHEN {ce} IS NOT NULL THEN {ce}"));
                        v.push_str(&format!(" WHEN {ce} IS NOT NULL THEN NULL"));
                    }
                    let xv = self.as_ty(x, ty);
                    e.push_str(&format!(" WHEN {cv} THEN {}", x.e.clone().unwrap_or_else(|| "NULL".into())));
                    v.push_str(&format!(" WHEN {cv} THEN {xv}"));
                }
                e.push_str(" ELSE '#N/A' END");
                v.push_str(" END");
                Ok(self.make(v, Some(e), ty, span))
            }
            "IFERROR" | "IFNA" => {
                Self::arity(&name_upper, args, 2, 2, span)?;
                let a = self.expr(&args[0])?;
                let b = self.expr(&args[1])?;
                let ty = self.unify(&name_upper, &[&a, &b], span)?;
                let (av, bv) = (self.as_ty(&a, ty), self.as_ty(&b, ty));
                let Some(ae) = a.e.clone() else {
                    return Ok(V { v: av, ty, empty_text: false, is_ref: a.is_ref, ..a });
                };
                let caught = if name_upper == "IFNA" { format!("{ae} = '#N/A'") } else { format!("{ae} IS NOT NULL") };
                let be = b.e.clone().unwrap_or_else(|| "NULL".into());
                Ok(self.make(
                    format!("CASE WHEN {caught} THEN {bv} ELSE {av} END"),
                    Some(format!("CASE WHEN {caught} THEN {be} ELSE {ae} END")),
                    ty,
                    span,
                ))
            }
            "AND" | "OR" => {
                Self::arity(&name_upper, args, 1, 255, span)?;
                let mut terms = Vec::new();
                let mut errs = Vec::new();
                for a in args {
                    let x = self.expr(a)?;
                    errs.push(x.e.clone());
                    match (x.ty, x.is_ref) {
                        // Referenced text and blanks are skipped, as for cells in a range.
                        (Ty::Text, true) => {}
                        (Ty::Text, false) => return fail(format!("{name_upper} takes conditions; text is not a condition"), x.span),
                        (Ty::Blank, _) => {}
                        (Ty::Bool, _) => terms.push((x.v.clone(), format!("{} IS NOT NULL", x.v))),
                        (Ty::Number, _) => terms.push((format!("CAST({} AS DOUBLE) <> 0", x.v), format!("{} IS NOT NULL", x.v))),
                        (Ty::Date, _) => terms.push((format!("{} <> 0", serial(&x.v)), format!("{} IS NOT NULL", x.v))),
                    }
                }
                if terms.is_empty() {
                    return Ok(self.make("NULL".into(), first_error(errs.into_iter().chain([Some("'#VALUE!'".to_string())])), Ty::Bool, span));
                }
                let any_value = terms.iter().map(|(_, p)| p.clone()).collect::<Vec<_>>().join(" OR ");
                let (combine, neutral) = if name_upper == "AND" { (" AND ", "TRUE") } else { (" OR ", "FALSE") };
                let v = terms.iter().map(|(t, _)| format!("COALESCE({t}, {neutral})")).collect::<Vec<_>>().join(combine);
                let none = format!("CASE WHEN NOT ({any_value}) THEN '#VALUE!' END");
                Ok(self.make(v, first_error(errs.into_iter().chain([Some(none)])), Ty::Bool, span))
            }
            "NOT" => {
                Self::arity("NOT", args, 1, 1, span)?;
                let x = self.expr(&args[0])?;
                let (c, ce) = self.cond(&x);
                Ok(self.make(format!("NOT ({c})"), first_error([x.e.clone(), ce]), Ty::Bool, span))
            }
            "ISBLANK" | "ISNUMBER" | "ISTEXT" | "ISLOGICAL" | "ISERROR" | "ISERR" | "ISNA" => {
                Self::arity(&name_upper, args, 1, 1, span)?;
                let x = self.expr(&args[0])?;
                let no_error = x.e.as_ref().map(|e| format!("{e} IS NULL")).unwrap_or_else(|| "TRUE".into());
                let present = format!("{} IS NOT NULL", x.v);
                let v = match name_upper.as_str() {
                    "ISBLANK" => format!("({no_error}) AND {} IS NULL AND {}", x.v, if x.is_ref { "TRUE" } else { "FALSE" }),
                    "ISNUMBER" => if matches!(x.ty, Ty::Number | Ty::Date) { format!("({no_error}) AND {present}") } else { "FALSE".into() },
                    "ISTEXT" => if x.ty == Ty::Text { format!("({no_error}) AND {present}") } else { "FALSE".into() },
                    "ISLOGICAL" => if x.ty == Ty::Bool { format!("({no_error}) AND {present}") } else { "FALSE".into() },
                    "ISERROR" => format!("NOT ({no_error})"),
                    "ISNA" => x.e.as_ref().map(|e| format!("COALESCE({e} = '#N/A', FALSE)")).unwrap_or_else(|| "FALSE".into()),
                    _ => x.e.as_ref().map(|e| format!("COALESCE({e} <> '#N/A', FALSE)")).unwrap_or_else(|| "FALSE".into()),
                };
                Ok(self.make(v, None, Ty::Bool, span))
            }
            "ABS" | "INT" => {
                Self::arity(&name_upper, args, 1, 1, span)?;
                let x = self.expr(&args[0])?;
                let (n, ne) = self.num(&x);
                let f = if name_upper == "ABS" { "abs" } else { "floor" };
                Ok(self.make(format!("{f}({n})"), first_error([x.e.clone(), ne]), Ty::Number, span))
            }
            "ROUND" | "ROUNDUP" | "ROUNDDOWN" => {
                Self::arity(&name_upper, args, 2, 2, span)?;
                let x = self.expr(&args[0])?;
                let d = self.expr(&args[1])?;
                let (n, ne) = self.num(&x);
                let (dn, de) = self.num(&d);
                let value = self.bind(p15(&n));
                let digits = self.bind(format!("CASE WHEN {dn} > 0 THEN floor({dn}) ELSE ceil({dn}) END"));
                let scale = self.bind(format!("CASE WHEN {digits} > 308 THEN CAST('inf' AS DOUBLE) WHEN {digits} < -308 THEN CAST(0 AS DOUBLE) ELSE pow(10, {digits}) END"));
                let scaled = self.bind(format!("CASE WHEN isfinite({value} * {scale}) THEN {} END", p15(&format!("{value} * {scale}"))));
                let rounded = match name_upper.as_str() {
                    "ROUND" => format!("round({scaled})"),
                    "ROUNDUP" => format!("CASE WHEN {value} > 0 THEN ceil({scaled}) ELSE floor({scaled}) END"),
                    _ => format!("trunc({scaled})"),
                };
                let up = name_upper == "ROUNDUP";
                let v = format!(
                    "CASE WHEN {scale} = 0 THEN {zero} WHEN {scaled} IS NULL THEN {value} ELSE ({rounded}) / {scale} END",
                    zero = if up { format!("CASE WHEN {value} = 0 THEN 0 END") } else { "CAST(0 AS DOUBLE)".into() }
                );
                let v = self.bind(v);
                let big = format!("CASE WHEN {v} IS NULL OR NOT isfinite({v}) THEN '#NUM!' END");
                Ok(self.make(v, first_error([x.e.clone(), ne, d.e.clone(), de, Some(big)]), Ty::Number, span))
            }
            "MOD" => {
                Self::arity("MOD", args, 2, 2, span)?;
                let a = self.expr(&args[0])?;
                let b = self.expr(&args[1])?;
                let (x, xe) = self.num(&a);
                let (y, ye) = self.num(&b);
                let (x, y) = (self.bind(x), self.bind(y));
                let r = self.bind(format!("CASE WHEN {y} <> 0 THEN {x} - {y} * floor({x} / {y}) END"));
                let e = first_error([a.e.clone(), xe, b.e.clone(), ye, Some(format!("CASE WHEN {y} = 0 THEN '#DIV/0!' WHEN NOT isfinite({r}) THEN '#NUM!' END"))]);
                Ok(self.make(r, e, Ty::Number, span))
            }
            "SUM" | "MIN" | "MAX" | "AVERAGE" => {
                Self::arity(&name_upper, args, 1, 255, span)?;
                let mut nums = Vec::new(); // (value or NULL when skipped)
                let mut errs = Vec::new();
                for a in args {
                    let x = self.expr(a)?;
                    errs.push(x.e.clone());
                    if x.is_ref {
                        // Referenced text, logical values and blanks are skipped.
                        match x.ty {
                            Ty::Number => nums.push(format!("CAST({} AS DOUBLE)", x.v)),
                            Ty::Date => nums.push(serial(&x.v)),
                            _ => {}
                        }
                    } else {
                        let (n, ne) = self.num(&x);
                        errs.push(ne);
                        nums.push(n);
                    }
                }
                let present: Vec<String> = nums.iter().map(|n| format!("CASE WHEN {n} IS NULL THEN 0 ELSE 1 END")).collect();
                let count = if present.is_empty() { "0".to_string() } else { present.join(" + ") };
                let v = if nums.is_empty() {
                    "CAST(0 AS DOUBLE)".to_string()
                } else {
                    match name_upper.as_str() {
                        "SUM" => nums.iter().map(|n| format!("COALESCE({n}, 0)")).collect::<Vec<_>>().join(" + "),
                        "MIN" => format!("COALESCE(least({}), 0)", nums.join(", ")),
                        "MAX" => format!("COALESCE(greatest({}), 0)", nums.join(", ")),
                        _ => format!("CASE WHEN ({count}) > 0 THEN ({}) / ({count}) END", nums.iter().map(|n| format!("COALESCE({n}, 0)")).collect::<Vec<_>>().join(" + ")),
                    }
                };
                let v = self.bind(v);
                if name_upper == "AVERAGE" {
                    errs.push(Some(format!("CASE WHEN ({count}) = 0 THEN '#DIV/0!' END")));
                }
                errs.push(Some(format!("CASE WHEN {v} IS NOT NULL AND NOT isfinite({v}) THEN '#NUM!' END")));
                Ok(self.make(v, first_error(errs), Ty::Number, span))
            }
            "LEFT" | "RIGHT" => {
                Self::arity(&name_upper, args, 1, 2, span)?;
                let t = self.expr(&args[0])?;
                let (s, se) = self.text(&t)?;
                let (n, ne, nerr) = if args.len() == 2 {
                    let c = self.expr(&args[1])?;
                    let (n, ne) = self.num(&c);
                    (n, ne, c.e.clone())
                } else {
                    ("CAST(1 AS DOUBLE)".to_string(), None, None)
                };
                let n = self.bind(n);
                let count = format!("CAST(least(floor({n}), 2147483647) AS BIGINT)");
                let f = if name_upper == "LEFT" { "left" } else { "right" };
                let e = first_error([t.e.clone(), se, nerr, ne, Some(format!("CASE WHEN {n} < 0 THEN '#VALUE!' END"))]);
                Ok(self.make(format!("CASE WHEN {n} >= 0 THEN {f}({s}, {count}) END"), e, Ty::Text, span))
            }
            "MID" => {
                Self::arity("MID", args, 3, 3, span)?;
                let t = self.expr(&args[0])?;
                let st = self.expr(&args[1])?;
                let ln = self.expr(&args[2])?;
                let (s, se) = self.text(&t)?;
                let (a, ae) = self.num(&st);
                let (b, be) = self.num(&ln);
                let (a, b) = (self.bind(a), self.bind(b));
                let e = first_error([t.e.clone(), se, st.e.clone(), ae, ln.e.clone(), be, Some(format!("CASE WHEN {a} < 1 OR {b} < 0 THEN '#VALUE!' END"))]);
                Ok(self.make(
                    format!("CASE WHEN {a} >= 1 AND {b} >= 0 THEN substring({s}, CAST(least(floor({a}), 2147483647) AS BIGINT), CAST(least(floor({b}), 2147483647) AS BIGINT)) END"),
                    e,
                    Ty::Text,
                    span,
                ))
            }
            "LEN" | "TRIM" | "UPPER" | "LOWER" => {
                Self::arity(&name_upper, args, 1, 1, span)?;
                let t = self.expr(&args[0])?;
                let (s, se) = self.text(&t)?;
                let (v, ty) = match name_upper.as_str() {
                    "LEN" => (format!("CAST(length({s}) AS DOUBLE)"), Ty::Number),
                    "TRIM" => (format!("regexp_replace(trim({s}, ' '), ' {{2,}}', ' ', 'g')"), Ty::Text),
                    "UPPER" => (format!("upper({s})"), Ty::Text),
                    _ => (format!("lower({s})"), Ty::Text),
                };
                Ok(self.make(v, first_error([t.e.clone(), se]), ty, span))
            }
            "SUBSTITUTE" => {
                Self::arity("SUBSTITUTE", args, 3, 4, span)?;
                let t = self.expr(&args[0])?;
                let o = self.expr(&args[1])?;
                let n = self.expr(&args[2])?;
                let (s, se) = self.text(&t)?;
                let (old, oe) = self.text(&o)?;
                let (new, ne) = self.text(&n)?;
                let (s, old, new) = (self.bind(s), self.bind(old), self.bind(new));
                let mut errs = vec![t.e.clone(), se, o.e.clone(), oe, n.e.clone(), ne];
                let v = if args.len() == 4 {
                    let i = self.expr(&args[3])?;
                    let (k, ke) = self.num(&i);
                    let k = self.bind(format!("floor({k})"));
                    errs.extend([i.e.clone(), ke, Some(format!("CASE WHEN {k} < 1 THEN '#VALUE!' END"))]);
                    let parts = self.bind(format!("CASE WHEN {old} <> '' THEN string_split({s}, {old}) END"));
                    format!(
                        "CASE WHEN {k} < 1 THEN NULL WHEN {old} = '' OR len({parts}) <= {k} THEN {s} \
                         ELSE array_to_string(list_slice({parts}, 1, CAST({k} AS BIGINT)), {old}) || {new} || array_to_string(list_slice({parts}, CAST({k} AS BIGINT) + 1, len({parts})), {old}) END"
                    )
                } else {
                    format!("CASE WHEN {old} = '' THEN {s} ELSE replace({s}, {old}, {new}) END")
                };
                Ok(self.make(v, first_error(errs), Ty::Text, span))
            }
            "CONCAT" => {
                Self::arity("CONCAT", args, 1, 255, span)?;
                let mut parts = Vec::new();
                let mut errs = Vec::new();
                for a in args {
                    let x = self.expr(a)?;
                    let (s, se) = self.text(&x)?;
                    errs.push(x.e.clone());
                    errs.push(se);
                    parts.push(format!("({s})"));
                }
                Ok(self.make(parts.join(" || "), first_error(errs), Ty::Text, span))
            }
            "DATE" => {
                Self::arity("DATE", args, 3, 3, span)?;
                let mut ns = Vec::new();
                let mut errs = Vec::new();
                for a in args {
                    let x = self.expr(a)?;
                    if x.ty == Ty::Text {
                        return fail("DATE takes numbers; convert text first", x.span);
                    }
                    let (n, ne) = self.num(&x);
                    errs.push(x.e.clone());
                    errs.push(ne);
                    ns.push(self.bind(format!("trunc({n})")));
                }
                let (y, m, d) = (&ns[0], &ns[1], &ns[2]);
                let year = self.bind(format!("CASE WHEN {y} >= 0 AND {y} < 1900 THEN {y} + 1900 ELSE {y} END"));
                let ok = format!("({year} >= 1900 AND {year} < 10000 AND abs({m}) < 120000 AND abs({d}) < 4000000)");
                let date = self.bind(format!(
                    "CASE WHEN {ok} THEN CAST(make_date(CAST({year} AS BIGINT), 1, 1) + to_months(CAST({m} - 1 AS INTEGER)) + to_days(CAST({d} - 1 AS INTEGER)) AS DATE) END"
                ));
                let in_range = format!("({date} >= DATE '1899-12-31' AND {date} <= DATE '9999-12-31')");
                errs.push(Some(format!("CASE WHEN NOT {ok} OR NOT {in_range} THEN '#NUM!' END")));
                Ok(self.make(format!("CASE WHEN {in_range} THEN {date} END"), first_error(errs), Ty::Date, span))
            }
            "YEAR" | "MONTH" | "DAY" => {
                Self::arity(&name_upper, args, 1, 1, span)?;
                let x = self.expr(&args[0])?;
                let (d, de) = self.date_arg(&x)?;
                let f = name_upper.to_lowercase();
                Ok(self.make(format!("CAST({f}({d}) AS DOUBLE)"), first_error([x.e.clone(), de]), Ty::Number, span))
            }
            "EDATE" | "EOMONTH" => {
                Self::arity(&name_upper, args, 2, 2, span)?;
                let x = self.expr(&args[0])?;
                let m = self.expr(&args[1])?;
                let (d, de) = self.date_arg(&x)?;
                let (n, ne) = self.num(&m);
                let n = self.bind(format!("trunc({n})"));
                let ok = format!("(abs({n}) < 120000)");
                let moved = self.bind(format!("CASE WHEN {ok} AND {d} IS NOT NULL THEN CAST({d} + to_months(CAST({n} AS INTEGER)) AS DATE) END"));
                let v = if name_upper == "EDATE" { moved.clone() } else { format!("last_day({moved})") };
                let v = self.bind(v);
                let in_range = format!("({v} >= DATE '1899-12-31' AND {v} <= DATE '9999-12-31')");
                let e = first_error([x.e.clone(), de, m.e.clone(), ne, Some(format!("CASE WHEN NOT {ok} OR NOT {in_range} THEN '#NUM!' END"))]);
                Ok(self.make(format!("CASE WHEN {in_range} THEN {v} END"), e, Ty::Date, span))
            }
            "TODAY" | "NOW" | "RAND" | "RANDBETWEEN" | "RANDARRAY" => fail(
                format!("{name_upper} changes on every calculation; table formulas only use deterministic functions for now"),
                span,
            ),
            "XLOOKUP" => self.xlookup(args, span),
            "VLOOKUP" => self.vlookup(args, span),
            "HLOOKUP" | "XMATCH" | "MATCH" | "INDEX" | "LOOKUP" => fail(
                format!("{name_upper} over table columns is not available yet (planned: exact lookups through batched joins)"),
                span,
            ),
            "SUMIF" | "SUMIFS" | "COUNTIF" | "COUNTIFS" | "AVERAGEIF" | "AVERAGEIFS" => self.conditional(&name_upper, args, span),
            "COUNT" | "COUNTA" => fail(
                format!("{name_upper} over table columns is not available yet; COUNTIFS counts rows that match criteria"),
                span,
            ),
            _ => fail(format!("{name_upper} is not available in table formulas; see the list in TABLE_FORMULAS.md"), span),
        }
    }

    /// The relation of another table named in a lookup.
    fn lookup_relation(&mut self, table: &str, span: Span) -> Result<Relation, FormulaError> {
        let rel = self.relations.relation(table).map_err(|m| FormulaError { message: m, span })?
            .ok_or_else(|| FormulaError { message: format!("there is no table named {table:?}"), span })?;
        self.tables.insert(table.to_string());
        Ok(rel)
    }

    /// A column of a lookup table, by name, with its formula type.
    fn lookup_column(rel: &Relation, table: &str, name: &str, span: Span) -> Result<(String, Ty), FormulaError> {
        let (actual, t) = rel.columns.iter().find(|(c, _)| c.eq_ignore_ascii_case(name))
            .ok_or_else(|| FormulaError { message: format!("table {table:?} has no column named {name:?}"), span })?;
        let ty = column_type(t).ok_or_else(|| FormulaError { message: format!("column {actual:?} has type {t}, which table formulas cannot use yet"), span })?;
        Ok((actual.clone(), ty))
    }

    /// Exact lookup: one grouped relation of `rel`, keyed by the normalized values of the
    /// `keys` columns, left-joined on the row's normalized probes, so every row gets at
    /// most one match (the first, or with `last` the last, in persisted row order). The
    /// source is scanned once. Returns the joined key, value and error columns.
    fn lookup_join(&mut self, rel: &Relation, keys: &[(V, String, Ty)], ret_col: &str, last: bool) -> (String, String, String) {
        // Spreadsheet exact matching: numbers at 15 digits, text case-insensitively, types
        // never equal each other; a blank matches 0, "", FALSE and blank cells.
        let key_of = |v: &str, t: Ty| -> String {
            match t {
                Ty::Text => format!("'t' || upper({v})"),
                Ty::Bool => format!("'b' || CAST({v} AS VARCHAR)"),
                Ty::Date => format!("'n' || CAST({} AS VARCHAR)", p15(&serial(v))),
                _ => format!("'n' || CAST({} AS VARCHAR)", p15(v)),
            }
        };
        // Several keys: length-prefixed parts, so no two key tuples share an encoding.
        let combine = |parts: &[String]| -> String {
            if parts.len() == 1 {
                return parts[0].clone();
            }
            parts.iter().map(|p| format!("CAST(length({p}) AS VARCHAR) || ':' || {p}")).collect::<Vec<_>>().join(" || ")
        };
        let err_of = |c: &str| rel.errors.iter().find(|(n, _)| n == c).map(|(_, e)| ident(e));
        // Each source row contributes every probe key it matches: its own value, 'blank'
        // when that is 0, "" or FALSE, and 0, "", FALSE and blank when it is blank.
        let mut variants = Vec::new();
        let mut ok = Vec::new();
        for (_, col, ty) in keys {
            let k = ident(col);
            let blank = format!("list_value('blank', {}, 't', 'bfalse')", key_of("CAST(0 AS DOUBLE)", Ty::Number));
            let zero = match ty { Ty::Text => Some(format!("{k} = ''")), Ty::Bool => Some(format!("{k} = FALSE")), Ty::Date => None, _ => Some(format!("{k} = 0")) };
            let own = key_of(&k, *ty);
            variants.push(match zero {
                Some(z) => format!("CASE WHEN {k} IS NULL THEN {blank} WHEN {z} THEN list_value({own}, 'blank') ELSE list_value({own}) END"),
                None => format!("CASE WHEN {k} IS NULL THEN {blank} ELSE list_value({own}) END"),
            });
            if let Some(e) = err_of(col) { ok.push(format!("{e} IS NULL")); }
        }
        let ret_err = err_of(ret_col).unwrap_or_else(|| "CAST(NULL AS VARCHAR)".into());
        let lists: String = variants.iter().enumerate().map(|(i, v)| format!(", {v} AS l{i}")).collect();
        let filter = if ok.is_empty() { String::new() } else { format!(" WHERE {}", ok.join(" AND ")) };
        let mut sql = format!("SELECT {} AS v, {ret_err} AS e, _rf_ord AS o{lists} FROM ({}){filter}", ident(ret_col), rel.sql);
        for i in 0..keys.len() {
            sql = format!("SELECT * EXCLUDE (l{i}), unnest(l{i}) AS u{i} FROM ({sql})");
        }
        let parts: Vec<String> = (0..keys.len()).map(|i| format!("u{i}")).collect();
        let order = if last { "DESC" } else { "ASC" };
        let sub = format!(
            "SELECT k, v, e FROM (SELECT k, v, e, row_number() OVER (PARTITION BY k ORDER BY o {order}) AS rn FROM (SELECT {} AS k, v, e, o FROM ({sql}))) WHERE rn = 1",
            combine(&parts)
        );
        let probes: Vec<String> = keys.iter().map(|(p, _, _)| {
            if p.ty == Ty::Blank { "'blank'".to_string() } else { format!("CASE WHEN {} IS NULL THEN 'blank' ELSE {} END", p.v, key_of(&p.v, p.ty)) }
        }).collect();
        let probe = self.bind(combine(&probes));
        // Close the current steps and join.
        let alias = format!("{}j{}", self.prefix, self.stages.len());
        let steps = std::mem::take(&mut self.steps);
        self.stages.push(Stage::Steps(steps));
        self.stages.push(Stage::Join { alias: alias.clone(), sql: sub, key: probe, reads: rel.reads.clone() });
        (ident(&format!("{alias}k")), ident(&format!("{alias}v")), ident(&format!("{alias}e")))
    }

    /// The equality factors of a multi-key lookup array: `(T[a]=x)*(T[b]=y)…` with value 1,
    /// or a single `T[a]=x` with value TRUE. Each is (table, column, row expression).
    fn key_factors<'e>(lookup: &Expr, array: &'e Expr) -> Option<Vec<(String, String, &'e Expr)>> {
        fn whole_free(e: &Expr) -> bool {
            match &e.kind {
                ExprKind::TableColumn { .. } | ExprKind::Table { .. } => false,
                ExprKind::Unary { expr, .. } => whole_free(expr),
                ExprKind::Binary { left, right, .. } => whole_free(left) && whole_free(right),
                ExprKind::Call { args, .. } => args.iter().all(whole_free),
                _ => true,
            }
        }
        fn factor(e: &Expr) -> Option<(String, String, &Expr)> {
            let ExprKind::Binary { op: BinaryOp::Eq, left, right } = &e.kind else { return None };
            match (&left.kind, &right.kind) {
                (ExprKind::TableColumn { table, column }, _) if whole_free(right) => Some((table.clone(), column.clone(), right)),
                (_, ExprKind::TableColumn { table, column }) if whole_free(left) => Some((table.clone(), column.clone(), left)),
                _ => None,
            }
        }
        fn product<'a>(e: &'a Expr, out: &mut Vec<(String, String, &'a Expr)>) -> bool {
            match &e.kind {
                ExprKind::Binary { op: BinaryOp::Mul, left, right } => product(left, out) && product(right, out),
                _ => match factor(e) { Some(f) => { out.push(f); true } None => false },
            }
        }
        match lookup.kind {
            // 1 against products of comparisons (numbers 1/0), as in spreadsheets.
            ExprKind::Number(1.0) => {
                let mut out = Vec::new();
                (matches!(array.kind, ExprKind::Binary { op: BinaryOp::Mul, .. }) && product(array, &mut out)).then_some(out)
            }
            // TRUE against one comparison (logical values).
            ExprKind::Bool(true) => factor(array).map(|f| vec![f]),
            _ => None,
        }
    }

    /// Exact XLOOKUP over another table, by one column or by several (multi-key form).
    fn xlookup(&mut self, args: &[Expr], span: Span) -> Result<V, FormulaError> {
        Self::arity("XLOOKUP", args, 3, 6, span)?;
        let literal = |i: usize, allowed: &[f64], what: &str| -> Result<f64, FormulaError> {
            match args.get(i).map(|a| &a.kind) {
                None | Some(ExprKind::Empty) => Ok(allowed[0]),
                Some(ExprKind::Number(n)) if allowed.contains(n) => Ok(*n),
                Some(ExprKind::Unary { op: UnaryOp::Neg, expr }) if matches!(expr.kind, ExprKind::Number(n) if allowed.contains(&-n)) => {
                    let ExprKind::Number(n) = expr.kind else { unreachable!() };
                    Ok(-n)
                }
                _ => fail(what.to_string(), args[i].span),
            }
        };
        literal(4, &[0.0], "only exact match (match_mode 0) is available for table lookups; approximate and wildcard modes are not yet")?;
        let last = literal(5, &[1.0, -1.0], "search_mode must be 1 (first match) or -1 (last match); binary search is not available for tables")? < 0.0;
        let ExprKind::TableColumn { table: rt, column: rc } = &args[2].kind else {
            return fail("XLOOKUP needs whole columns of a table here, e.g. Customers[name]", args[2].span);
        };
        // (table, column, probe expression) per key.
        let factors: Vec<(String, String, &Expr)> = match &args[1].kind {
            ExprKind::TableColumn { table, column } => vec![(table.clone(), column.clone(), &args[0])],
            _ => Self::key_factors(&args[0], &args[1]).ok_or_else(|| FormulaError {
                message: "XLOOKUP needs whole columns of a table here, e.g. Customers[id], or several keys as 1, (T[a]=[@a])*(T[b]=[@b])".into(),
                span: args[1].span,
            })?,
        };
        if factors.iter().any(|(t, _, _)| !t.eq_ignore_ascii_case(rt)) {
            return fail("the lookup and return columns must come from the same table", args[2].span);
        }
        let rel = self.lookup_relation(rt, args[1].span)?;
        let (ret_col, ret_ty) = Self::lookup_column(&rel, rt, rc, args[2].span)?;
        let mut keys = Vec::new();
        let mut probe_errors = Vec::new();
        for (_, column, probe) in &factors {
            let (col, ty) = Self::lookup_column(&rel, rt, column, args[1].span)?;
            let p = self.expr(probe)?;
            probe_errors.push(p.e.clone());
            keys.push((p, col, ty));
        }
        let nf = match args.get(3) {
            None | Some(Expr { kind: ExprKind::Empty, .. }) => None,
            Some(a) => Some(self.expr(a)?),
        };
        let ret = V { v: "NULL".into(), e: None, ty: ret_ty, is_ref: false, empty_text: false, span: args[2].span };
        let ty = match &nf { Some(n) => self.unify("XLOOKUP", &[&ret, n], span)?, None => ret_ty };
        let (jk, jv, je) = self.lookup_join(&rel, &keys, &ret_col, last);
        let (nv, ne) = match &nf {
            Some(n) => (self.as_ty(n, ty), n.e.clone().unwrap_or_else(|| "NULL".into())),
            None => ("NULL".into(), "'#N/A'".into()),
        };
        let value = format!("CASE WHEN {jk} IS NOT NULL THEN {jv} ELSE {nv} END");
        let mut errors = probe_errors;
        errors.push(Some(format!("CASE WHEN {jk} IS NOT NULL THEN {je} ELSE {ne} END")));
        Ok(self.make(value, first_error(errors), ty, span))
    }

    /// Exact VLOOKUP over another table: `VLOOKUP(value, Table, column_number, FALSE)`
    /// matches the table's first column and returns its `column_number`th column, in the
    /// table's column order. Spreadsheet VLOOKUP treats `*`, `?` and `~` in a text value as
    /// a wildcard pattern even in exact mode; those values give #VALUE! here (no pattern
    /// matching yet), never a literal match.
    fn vlookup(&mut self, args: &[Expr], span: Span) -> Result<V, FormulaError> {
        Self::arity("VLOOKUP", args, 3, 4, span)?;
        let ExprKind::Table { table } = &args[1].kind else {
            return fail("VLOOKUP needs a whole table here, e.g. Customers; for one key column and one result column XLOOKUP is clearer", args[1].span);
        };
        match args.get(3).map(|a| &a.kind) {
            Some(ExprKind::Bool(false)) => {}
            Some(ExprKind::Number(n)) if *n == 0.0 => {}
            _ => return fail(
                "VLOOKUP without FALSE as its 4th argument finds an approximate match, which is not available for tables yet; add FALSE for an exact match",
                args.get(3).map_or(span, |a| a.span),
            ),
        }
        let index = match &args[2].kind {
            ExprKind::Number(n) => n.trunc(),
            ExprKind::Unary { op: UnaryOp::Neg, expr } if matches!(expr.kind, ExprKind::Number(_)) => -1.0,
            _ => return fail("the column number must be written in the formula, e.g. 2", args[2].span),
        };
        let rel = self.lookup_relation(table, args[1].span)?;
        let Some((key_name, _)) = rel.columns.first().cloned() else {
            return fail(format!("table {table:?} has no columns"), args[1].span);
        };
        let probe = self.expr(&args[0])?;
        // Spreadsheet errors for a column number outside the table, like the sheet engine.
        if index < 1.0 || index > rel.columns.len() as f64 {
            let code = if index < 1.0 { "'#VALUE!'" } else { "'#REF!'" };
            return Ok(self.make("NULL".into(), first_error([probe.e.clone(), Some(code.into())]), Ty::Number, span));
        }
        let (key_col, key_ty) = Self::lookup_column(&rel, table, &key_name, args[1].span)?;
        let ret_name = rel.columns[index as usize - 1].0.clone();
        let (ret_col, ret_ty) = Self::lookup_column(&rel, table, &ret_name, args[2].span)?;
        let wildcard = match probe.ty {
            Ty::Text => Some(format!("CASE WHEN regexp_matches(COALESCE({}, ''), '[*?~]') THEN '#VALUE!' END", probe.v)),
            _ => None,
        };
        let probe_error = probe.e.clone();
        let (jk, jv, je) = self.lookup_join(&rel, &[(probe, key_col, key_ty)], &ret_col, false);
        let value = format!("CASE WHEN {jk} IS NOT NULL THEN {jv} END");
        let error = first_error([probe_error, wildcard, Some(format!("CASE WHEN {jk} IS NOT NULL THEN {je} ELSE '#N/A' END"))]);
        Ok(self.make(value, error, ret_ty, span))
    }

    /// A date argument: a Date, or a Number serial (1 = 1899-12-31). Text is refused.
    fn date_arg(&mut self, x: &V) -> Result<(String, Option<String>), FormulaError> {
        match x.ty {
            Ty::Date => Ok((x.v.clone(), Some(format!("CASE WHEN {} IS NULL THEN '#NUM!' END", x.v)))),
            Ty::Text => fail("convert text to a date first, e.g. with DATE", x.span),
            _ => {
                let (n, ne) = self.num(x);
                let n = self.bind(n);
                let (d, de) = serial_date(&n);
                Ok((self.bind(d), first_error([ne, Some(de)])))
            }
        }
    }
}

/// Shortest round-trip decimal text of a double, without exponent for integers below 1e15.
fn number_text(n: &str) -> String {
    format!(
        "CASE WHEN {n} = trunc({n}) AND abs({n}) < 1e15 THEN CAST(CAST({n} AS BIGINT) AS VARCHAR) ELSE CAST({n} AS VARCHAR) END"
    )
}

/// Compile `formula` for a table with `columns` (name, DuckDB type). Step aliases start
/// with `prefix`, which must not collide with column names.
pub fn compile(formula: &str, columns: &[(String, String)], prefix: &str) -> Result<Compiled, FormulaError> {
    compile_with_errors(formula, columns, &[], prefix)
}

/// Like [`compile`]; `errors` pairs a column with the column holding its error codes, so
/// errors of one formula column propagate into formulas that read it.
pub fn compile_with_errors(formula: &str, columns: &[(String, String)], errors: &[(String, String)], prefix: &str) -> Result<Compiled, FormulaError> {
    compile_full(formula, columns, errors, &NoRelations, prefix)
}

/// Like [`compile_with_errors`], resolving other tables through `relations`.
pub fn compile_full(formula: &str, columns: &[(String, String)], errors: &[(String, String)], relations: &dyn Relations, prefix: &str) -> Result<Compiled, FormulaError> {
    let ast = parse(formula)?;
    let mut c = Compiler { columns, errors, relations, stages: Vec::new(), tables: BTreeSet::new(), steps: Vec::new(), prefix: prefix.to_string(), refs: BTreeSet::new() };
    let top = c.expr(&ast)?;
    let ty = match top.ty {
        Ty::Blank => return fail("the formula always returns an empty value", top.span),
        t => t,
    };
    // `""` alone is an empty text result.
    let error = top.e.clone().unwrap_or_else(|| "NULL".into());
    let value = if top.e.is_some() { format!("CASE WHEN {error} IS NULL THEN {} END", top.v) } else { top.v.clone() };
    let value = format!("CAST({value} AS {})", ty.sql_type());
    let error = format!("CAST({error} AS VARCHAR)");
    let mut stages = c.stages;
    stages.push(Stage::Steps(c.steps));
    Ok(Compiled { stages, value, error, ty, columns: c.refs.into_iter().collect(), tables: c.tables.into_iter().collect() })
}

/// Something a formula reads, for dependency tracking and rename rewriting.
#[derive(Debug, Clone, PartialEq)]
pub enum Reference {
    /// A column of the formula's own table: `[@c]`, `[c]`.
    Own { column: String, span: Span },
    /// A whole column of a table, possibly the formula's own: `T[c]`.
    Column { table: String, column: String, span: Span },
    /// A whole table (`T`, `T[#Data]`) and the column numbers (from 1) a VLOOKUP reads:
    /// its first column and the returned one.
    Table { table: String, positions: Vec<usize>, span: Span },
    /// A sheet cell, with its `$` markers as typed.
    Cell { cell: CellRef, fixed: (bool, bool), span: Span },
}

/// Everything a formula reads, without compiling it.
pub fn references(formula: &str) -> Result<Vec<Reference>, FormulaError> {
    fn walk(e: &Expr, out: &mut Vec<Reference>) {
        match &e.kind {
            ExprKind::Column { name, .. } => out.push(Reference::Own { column: name.clone(), span: e.span }),
            ExprKind::TableColumn { table, column } => out.push(Reference::Column { table: table.clone(), column: column.clone(), span: e.span }),
            ExprKind::Table { table } => out.push(Reference::Table { table: table.clone(), positions: Vec::new(), span: e.span }),
            ExprKind::Cell { cell, fixed } => out.push(Reference::Cell { cell: cell.clone(), fixed: *fixed, span: e.span }),
            ExprKind::Unary { expr, .. } => walk(expr, out),
            ExprKind::Binary { left, right, .. } => { walk(left, out); walk(right, out); }
            ExprKind::Call { name, args } => {
                let at = out.len();
                args.iter().for_each(|a| walk(a, out));
                // VLOOKUP(value, Table, n, …) reads the table's first and n-th columns.
                if name == "VLOOKUP" {
                    if let (Some(Expr { kind: ExprKind::Table { .. }, span }), Some(Expr { kind: ExprKind::Number(n), .. })) = (args.get(1), args.get(2)) {
                        let n = n.trunc();
                        for r in &mut out[at..] {
                            if let Reference::Table { positions, span: s, .. } = r {
                                if s == span && (1.0..1e6).contains(&n) {
                                    *positions = if n as usize == 1 { vec![1] } else { vec![1, n as usize] };
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(&parse(formula)?, &mut out);
    Ok(out)
}

/// Columns of its own table a formula reads (for rename/delete dependency checks).
pub fn referenced_columns(formula: &str) -> Result<Vec<(String, Span)>, FormulaError> {
    Ok(references(formula)?.into_iter().filter_map(|r| match r { Reference::Own { column, span } => Some((column, span)), _ => None }).collect())
}

/// Whether `name` can be written as a table name in a formula (`Name[col]`, `Name`).
pub fn valid_table_name(name: &str) -> bool {
    let mut chars = name.chars();
    let first = chars.next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '\\');
    first && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '\\')
        && !name.eq_ignore_ascii_case("TRUE") && !name.eq_ignore_ascii_case("FALSE")
        && parse(&format!("={name}")).is_ok_and(|e| matches!(e.kind, ExprKind::Table { .. }))
}

fn escape_column(name: &str) -> String {
    name.chars().flat_map(|c| if "[]#'".contains(c) { vec!['\'', c] } else { vec![c] }).collect()
}

/// Replace `spans` of `formula` (any order) with the given texts.
fn splice(formula: &str, mut edits: Vec<(Span, String)>) -> String {
    let mut out: Vec<char> = formula.chars().collect();
    edits.sort_by_key(|(s, _)| std::cmp::Reverse(s.start));
    for (s, text) in edits {
        out.splice(s.start..s.end, text.chars());
    }
    out.into_iter().collect()
}

/// Rewrite whole-column references `table[old]` as `table[new]`. `None` when there are none.
pub fn rename_table_column(formula: &str, table: &str, old: &str, new: &str) -> Result<Option<String>, FormulaError> {
    let chars: Vec<char> = formula.chars().collect();
    let edits: Vec<(Span, String)> = references(formula)?.into_iter().filter_map(|r| match r {
        Reference::Column { table: t, column, span } if t.eq_ignore_ascii_case(table) && column.eq_ignore_ascii_case(old) => {
            let typed: String = chars[span.start..span.start + t.chars().count()].iter().collect();
            Some((span, format!("{typed}[{}]", escape_column(new))))
        }
        _ => None,
    }).collect();
    Ok((!edits.is_empty()).then(|| splice(formula, edits)))
}

/// Rewrite references to table `old` (`old[col]`, `old`, `old[#Data]`) as `new`. `None`
/// when there are none. `new` must be a [`valid_table_name`].
pub fn rename_table(formula: &str, old: &str, new: &str) -> Result<Option<String>, FormulaError> {
    let edits: Vec<(Span, String)> = references(formula)?.into_iter().filter_map(|r| match r {
        Reference::Column { table, span, .. } | Reference::Table { table, span, .. } if table.eq_ignore_ascii_case(old) => {
            let n = table.chars().count();
            Some((Span { start: span.start, end: span.start + n }, new.to_string()))
        }
        _ => None,
    }).collect();
    Ok((!edits.is_empty()).then(|| splice(formula, edits)))
}

/// Rewrite sheet cell references: `map` gives a cell's new place (`Some(None)` when it is
/// gone, written `#REF!`) or `None` to keep it. `None` when nothing changes.
pub fn rewrite_cells(formula: &str, map: impl Fn(&CellRef) -> Option<Option<CellRef>>) -> Result<Option<String>, FormulaError> {
    let edits: Vec<(Span, String)> = references(formula)?.into_iter().filter_map(|r| match r {
        Reference::Cell { cell, fixed, span } => match map(&cell)? {
            Some(to) => (to != cell).then(|| (span, to.text(fixed))),
            None => Some((span, "#REF!".to_string())),
        },
        _ => None,
    }).collect();
    Ok((!edits.is_empty()).then(|| splice(formula, edits)))
}

/// Rewrite references to column `old` as `new`, keeping the rest of the formula as typed.
/// `None` when the formula does not refer to `old`.
pub fn rename_column(formula: &str, old: &str, new: &str) -> Result<Option<String>, FormulaError> {
    let chars: Vec<char> = formula.chars().collect();
    let mut hits: Vec<Span> = referenced_columns(formula)?.into_iter().filter(|(n, _)| n.eq_ignore_ascii_case(old)).map(|(_, s)| s).collect();
    if hits.is_empty() {
        return Ok(None);
    }
    let escaped: String = new.chars().flat_map(|c| if "[]#'".contains(c) { vec!['\'', c] } else { vec![c] }).collect();
    let plain = new.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    hits.sort_by_key(|s| std::cmp::Reverse(s.start));
    let mut out = chars.clone();
    for s in hits {
        let this_row = chars.get(s.start + 1) == Some(&'@');
        let inner = if plain { escaped.clone() } else { format!("[{escaped}]") };
        let text = format!("[{}{inner}]", if this_row { "@" } else { "" });
        out.splice(s.start..s.end, text.chars());
    }
    Ok(Some(out.into_iter().collect()))
}
