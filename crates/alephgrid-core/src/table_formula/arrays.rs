//! Table results that spill into sheets (F.10): `FILTER`, `SORT`, `UNIQUE` and `TAKE` over
//! whole columns of a table (or the whole table), written in a sheet cell. Each compiles to
//! one ordered query over the table; inside `FILTER`'s condition, `T[x]` means the row's `x`.
//!
//! Semantics follow the sheet engine: rows keep table order unless sorted; `SORT` orders
//! numbers, then text by its upper case, then logical values, blanks last, ties in table
//! order; `UNIQUE` compares text case-insensitively and keeps the first row of each value;
//! `FILTER` without a match gives its `if_empty` value, or `#CALC!`.

use super::compile::{compile_ast, Relation, Relations, Ty};
use super::parse::{parse, Expr, ExprKind, FormulaError, Span};
use crate::sql::ident;

/// The query of an array result: `SELECT c0, c1, … FROM … ORDER BY …`, its column types,
/// and the tables it reads.
#[derive(Debug, Clone)]
pub struct ArrayPlan {
    pub sql: String,
    pub types: Vec<Ty>,
    pub tables: Vec<String>,
    /// The value when there are no rows (FILTER's `if_empty`), as a formula literal.
    pub if_empty: Option<Expr>,
}

fn fail<T>(message: impl Into<String>, span: Span) -> Result<T, FormulaError> {
    Err(FormulaError { message: message.into(), span })
}

/// A plan in the making: `sql` yields `c0…`, `o` (order) and `rel` is the table's relation.
struct Source {
    table: String,
    rel: Relation,
    sql: String,
    types: Vec<Ty>,
    if_empty: Option<Expr>,
}

fn literal_number(e: Option<&Expr>, default: f64) -> Option<f64> {
    match e.map(|e| &e.kind) {
        None | Some(ExprKind::Empty) => Some(default),
        Some(ExprKind::Number(n)) => Some(*n),
        Some(ExprKind::Unary { op: super::parse::UnaryOp::Neg, expr }) => match expr.kind { ExprKind::Number(n) => Some(-n), _ => None },
        _ => None,
    }
}

/// `T[x]` of `table` as the row's `[@x]`, everywhere in `e`.
fn as_row(e: &Expr, table: &str) -> Expr {
    let mut out = e.clone();
    fn walk(e: &mut Expr, table: &str) {
        match &mut e.kind {
            ExprKind::TableColumn { table: t, column } if t.eq_ignore_ascii_case(table) => {
                e.kind = ExprKind::Column { name: column.clone(), this_row: true };
            }
            ExprKind::Unary { expr, .. } => walk(expr, table),
            ExprKind::Binary { left, right, .. } => { walk(left, table); walk(right, table); }
            ExprKind::Call { args, .. } => args.iter_mut().for_each(|a| walk(a, table)),
            _ => {}
        }
    }
    walk(&mut out, table);
    out
}

fn source(e: &Expr, relations: &dyn Relations) -> Result<Source, FormulaError> {
    let relation = |table: &str, span: Span| -> Result<Relation, FormulaError> {
        relations.relation(table).map_err(|m| FormulaError { message: m, span })?
            .ok_or_else(|| FormulaError { message: format!("there is no table named {table:?}"), span })
    };
    let typed = |rel: &Relation, name: &str, span: Span| -> Result<(String, Ty), FormulaError> {
        let (actual, t) = rel.columns.iter().find(|(c, _)| c.eq_ignore_ascii_case(name))
            .ok_or_else(|| FormulaError { message: format!("no column named {name:?}"), span })?;
        let ty = super::compile::column_type(t).ok_or_else(|| FormulaError { message: format!("column {actual:?} has type {t}, which table formulas cannot use yet"), span })?;
        Ok((actual.clone(), ty))
    };
    match &e.kind {
        ExprKind::TableColumn { table, column } => {
            let rel = relation(table, e.span)?;
            let (c, ty) = typed(&rel, column, e.span)?;
            let sql = format!("SELECT {} AS c0, _rf_ord AS o FROM ({})", ident(&c), rel.sql);
            Ok(Source { table: table.clone(), rel, sql, types: vec![ty], if_empty: None })
        }
        ExprKind::Table { table } => {
            let rel = relation(table, e.span)?;
            let mut cols = Vec::new();
            let mut types = Vec::new();
            for (k, (name, _)) in rel.columns.iter().enumerate() {
                let (c, ty) = typed(&rel, name, e.span)?;
                cols.push(format!("{} AS c{k}", ident(&c)));
                types.push(ty);
            }
            let sql = format!("SELECT {}, _rf_ord AS o FROM ({})", cols.join(", "), rel.sql);
            Ok(Source { table: table.clone(), rel, sql, types, if_empty: None })
        }
        ExprKind::Call { name, args } => {
            let upper = name.to_uppercase();
            let cols = |n: usize| (0..n).map(|k| format!("c{k}")).collect::<Vec<_>>().join(", ");
            match upper.as_str() {
                "FILTER" => {
                    if !(2..=3).contains(&args.len()) {
                        return fail("FILTER takes an array, a condition and optionally a value for no match", e.span);
                    }
                    if !matches!(args[0].kind, ExprKind::TableColumn { .. } | ExprKind::Table { .. }) {
                        return fail("FILTER needs whole columns of a table here, e.g. Orders[amount] or Orders; sort or take afterwards", args[0].span);
                    }
                    let mut s = source(&args[0], relations)?;
                    // The condition, per row of the table.
                    let cond = as_row(&args[1], &s.table);
                    let columns: Vec<(String, String)> = s.rel.columns.clone();
                    let c = compile_ast(&cond, &columns, &s.rel.errors, relations, "_rf_a_")?;
                    let keep = format!("SELECT _rf_ord FROM ({}) WHERE COALESCE(CAST({} AS BOOLEAN), FALSE) AND {} IS NULL", c.wrap(&s.rel.sql), c.value, c.error);
                    s.sql = format!("SELECT * FROM ({}) WHERE o IN ({keep})", s.sql);
                    if let Some(x) = args.get(2) {
                        if !matches!(x.kind, ExprKind::Number(_) | ExprKind::Text(_) | ExprKind::Bool(_) | ExprKind::Error(_)) {
                            return fail("FILTER's value for no match must be written in the formula", x.span);
                        }
                        s.if_empty = Some(x.clone());
                    }
                    Ok(s)
                }
                "SORT" => {
                    let mut s = source(args.first().ok_or_else(|| FormulaError { message: "SORT needs an array".into(), span: e.span })?, relations)?;
                    let index = literal_number(args.get(1), 1.0).ok_or_else(|| FormulaError { message: "the sort column must be written in the formula".into(), span: e.span })?;
                    let order = literal_number(args.get(2), 1.0).filter(|o| *o == 1.0 || *o == -1.0)
                        .ok_or_else(|| FormulaError { message: "the sort order must be 1 or -1".into(), span: e.span })?;
                    if args.len() > 3 {
                        return fail("SORT by column (by_col) is not available for table results", args[3].span);
                    }
                    let k = index.trunc() as usize;
                    if k < 1 || k > s.types.len() {
                        return fail("the sort column is outside the array", args.get(1).map_or(e.span, |a| a.span));
                    }
                    let key = match s.types[k - 1] { Ty::Text => format!("upper(c{})", k - 1), Ty::Bool => format!("CAST(c{} AS INTEGER)", k - 1), Ty::Date => format!("c{}", k - 1), _ => format!("c{}", k - 1) };
                    let dir = if order < 0.0 { "DESC" } else { "ASC" };
                    s.sql = format!("SELECT {}, row_number() OVER (ORDER BY {key} {dir} NULLS LAST, o) AS o FROM ({})", cols(s.types.len()), s.sql);
                    Ok(s)
                }
                "UNIQUE" => {
                    if args.len() > 1 {
                        return fail("UNIQUE's by_col and exactly_once are not available for table results", args[1].span);
                    }
                    let mut s = source(args.first().ok_or_else(|| FormulaError { message: "UNIQUE needs an array".into(), span: e.span })?, relations)?;
                    let keys: Vec<String> = s.types.iter().enumerate().map(|(k, t)| if *t == Ty::Text { format!("upper(c{k})") } else { format!("c{k}") }).collect();
                    let firsts: Vec<String> = (0..s.types.len()).map(|k| format!("arg_min(c{k}, o) AS c{k}")).collect();
                    s.sql = format!("SELECT {}, min(o) AS o FROM ({}) GROUP BY {}", firsts.join(", "), s.sql, keys.join(", "));
                    Ok(s)
                }
                "TAKE" => {
                    let mut s = source(args.first().ok_or_else(|| FormulaError { message: "TAKE needs an array".into(), span: e.span })?, relations)?;
                    let n = literal_number(args.get(1), 0.0).filter(|n| *n != 0.0)
                        .ok_or_else(|| FormulaError { message: "TAKE's row count must be written in the formula".into(), span: e.span })?;
                    if args.len() > 2 {
                        return fail("TAKE by columns is not available for table results", args[2].span);
                    }
                    let (dir, n) = if n < 0.0 { ("DESC", -n) } else { ("ASC", n) };
                    s.sql = format!("SELECT * FROM ({}) ORDER BY o {dir} LIMIT {}", s.sql, n.trunc() as i64);
                    Ok(s)
                }
                _ => fail(format!("{upper} does not give a table result here; use FILTER, SORT, UNIQUE or TAKE over whole columns"), e.span),
            }
        }
        _ => fail("a table result reads whole columns of a table, e.g. Orders[region] or Orders", e.span),
    }
}

/// The plan of a sheet cell's formula when it is an array over a table (a top-level FILTER,
/// SORT, UNIQUE or TAKE); `Ok(None)` for other formulas.
pub fn array_plan(formula: &str, relations: &dyn Relations) -> Result<Option<ArrayPlan>, FormulaError> {
    let ast = parse(formula)?;
    let ExprKind::Call { name, .. } = &ast.kind else { return Ok(None) };
    if !matches!(name.to_uppercase().as_str(), "FILTER" | "SORT" | "UNIQUE" | "TAKE") {
        return Ok(None);
    }
    let s = source(&ast, relations)?;
    let n = s.types.len();
    let cols = (0..n).map(|k| format!("c{k}")).collect::<Vec<_>>().join(", ");
    Ok(Some(ArrayPlan { sql: format!("SELECT {cols} FROM ({}) ORDER BY o", s.sql), types: s.types, tables: vec![s.table.to_lowercase()], if_empty: s.if_empty }))
}
