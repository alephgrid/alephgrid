//! `INSERT`, `UPDATE` and `DELETE` typed as SQL, applied as Rowfinity changes (4.08).
//!
//! A raw DuckDB `UPDATE` or `INSERT` would bypass record ids, order keys, the position index,
//! revisions and history. So the statement is split into its parts (target table, columns,
//! expressions, condition, source query) by the tokenizer, and the parts are reassembled into
//! one SELECT that DuckDB's parser checks against the SQL policy *and* against the expected
//! shape (exactly our select list, exactly the target table, the condition in WHERE, nothing
//! else). That SELECT runs once, into a staging table; values are checked to convert to the
//! target columns' types; then the change is applied with the same bookkeeping as edits,
//! inserts and deletes, inside [`Workbook::mutate`]'s transaction. Anything else that would
//! change data or schema is refused.

use anyhow::{anyhow, bail, Result};
use duckdb::OptionalExt;
use serde_json::Value;

use crate::catalog::PosIndex;
use crate::model::*;
use crate::reads::{is_internal, user_error};
use crate::sql::ident;
use crate::sqllex::{self, Tok, Token};
use crate::sqlpolicy::{self, NotAllowed, QueryParam};
use crate::storage::{self, ID, ORD, ORD_GAP};
use crate::workbook::AfterCommit;
use crate::Workbook;

const STAGE: &str = "_rf_change_stage";

#[derive(Debug)]
enum Change {
    Insert { table: String, columns: Option<Vec<String>>, query: String },
    Update { table: String, alias: Option<String>, sets: Vec<(String, String)>, cond: Option<String> },
    Delete { table: String, alias: Option<String>, cond: Option<String> },
}

fn refuse(msg: impl Into<String>) -> anyhow::Error {
    anyhow!(NotAllowed(msg.into()))
}

/// Unquote one identifier token.
fn name_of(t: &Token, sql: &str) -> Option<String> {
    match t.kind {
        Tok::Word => Some(t.text(sql).to_string()),
        Tok::Quoted => {
            let s = t.text(sql);
            (s.len() >= 2 && s.ends_with('"')).then(|| s[1..s.len() - 1].replace("\"\"", "\""))
        }
        _ => None,
    }
}

/// `name`, `main.name` or `"main"."name"` at `i`; returns the name and the next index.
fn table_name(toks: &[Token], sql: &str, i: usize) -> Result<(String, usize)> {
    let first = toks.get(i).and_then(|t| name_of(t, sql)).ok_or_else(|| refuse("expected a table name"))?;
    if toks.get(i + 1).is_some_and(|t| t.kind == Tok::Punct('.')) {
        let second = toks.get(i + 2).and_then(|t| name_of(t, sql)).ok_or_else(|| refuse("expected a table name after the dot"))?;
        if !first.eq_ignore_ascii_case("main") {
            bail!(refuse(format!("{first}.{second} is outside this workbook")));
        }
        return Ok((second, i + 3));
    }
    Ok((first, i + 1))
}

/// Text between two token indexes (`from..to`, exclusive), verbatim.
fn between(toks: &[Token], sql: &str, from: usize, to: usize) -> String {
    if from >= to {
        return String::new();
    }
    sql[toks[from].span.start..toks[to - 1].span.end].to_string()
}

/// Index of the first top-level word in `words` at or after `from`.
fn find_top(toks: &[Token], sql: &str, from: usize, words: &[&str]) -> Option<usize> {
    (from..toks.len()).find(|&k| toks[k].depth == 0 && words.iter().any(|w| toks[k].is_word(sql, w)))
}

fn parse_change(sql: &str) -> Result<Change> {
    let toks = sqllex::tokens(sql);
    if toks.iter().any(|t| t.kind == Tok::Punct(';')) {
        bail!(refuse("run one statement at a time"));
    }
    let kw = |i: usize, w: &str| toks.get(i).is_some_and(|t| t.is_word(sql, w));
    let returning = find_top(&toks, sql, 0, &["RETURNING"]).is_some();
    if returning {
        bail!(refuse("RETURNING is not supported; run a query afterwards to see the rows"));
    }
    if kw(0, "INSERT") {
        if kw(1, "OR") {
            bail!(refuse("INSERT OR REPLACE/IGNORE is not supported; use UPDATE for existing rows"));
        }
        if !kw(1, "INTO") {
            bail!(refuse("expected INSERT INTO <table>"));
        }
        let (table, mut i) = table_name(&toks, sql, 2)?;
        if kw(i, "AS") {
            bail!(refuse("INSERT INTO … AS alias is not supported"));
        }
        let mut columns = None;
        if toks.get(i).is_some_and(|t| t.kind == Tok::Punct('(')) && !(kw(i + 1, "SELECT") || kw(i + 1, "WITH") || kw(i + 1, "VALUES") || kw(i + 1, "FROM")) {
            let mut cols = Vec::new();
            let mut k = i + 1;
            loop {
                let c = toks.get(k).and_then(|t| name_of(t, sql)).ok_or_else(|| refuse("expected a column name in the column list"))?;
                cols.push(c);
                match toks.get(k + 1).map(|t| t.kind) {
                    Some(Tok::Punct(',')) => k += 2,
                    Some(Tok::Punct(')')) => {
                        i = k + 2;
                        break;
                    }
                    _ => bail!(refuse("expected , or ) in the column list")),
                }
            }
            columns = Some(cols);
        }
        if kw(i, "BY") {
            bail!(refuse("INSERT … BY NAME/POSITION is not supported; list the columns instead"));
        }
        if kw(i, "DEFAULT") {
            bail!(refuse("DEFAULT VALUES is not supported; add rows in the grid or with VALUES"));
        }
        if find_top(&toks, sql, i, &["ON"]).is_some_and(|k| kw(k + 1, "CONFLICT")) {
            bail!(refuse("ON CONFLICT is not supported"));
        }
        let query = between(&toks, sql, i, toks.len());
        if query.is_empty() {
            bail!(refuse("INSERT needs VALUES or a query"));
        }
        return Ok(Change::Insert { table, columns, query });
    }
    if kw(0, "UPDATE") {
        let (table, mut i) = table_name(&toks, sql, 1)?;
        let mut alias = None;
        if kw(i, "AS") {
            alias = Some(toks.get(i + 1).and_then(|t| name_of(t, sql)).ok_or_else(|| refuse("expected an alias after AS"))?);
            i += 2;
        } else if !kw(i, "SET") {
            alias = toks.get(i).and_then(|t| name_of(t, sql));
            i += 1;
        }
        if !kw(i, "SET") {
            bail!(refuse("expected UPDATE <table> SET <column> = <value>"));
        }
        let wh = find_top(&toks, sql, i, &["WHERE"]);
        let end = wh.unwrap_or(toks.len());
        if (i + 1..end).any(|k| toks[k].depth == 0 && toks[k].is_word(sql, "FROM") && !(k > 0 && toks[k - 1].is_word(sql, "DISTINCT"))) {
            bail!(refuse("UPDATE … FROM is not supported; use a subquery in SET or WHERE"));
        }
        let mut sets = Vec::new();
        let mut k = i + 1;
        while k < end {
            let mut j = k;
            let column = toks.get(j).and_then(|t| name_of(t, sql)).ok_or_else(|| refuse("expected a column name after SET"))?;
            if toks.get(j + 1).is_some_and(|t| t.kind == Tok::Punct('.')) {
                bail!(refuse("write SET column = …, without a table name"));
            }
            if !toks.get(j + 1).is_some_and(|t| t.kind == Tok::Punct('=')) {
                bail!(refuse(format!("expected = after {column}")));
            }
            j += 2;
            let stop = (j..end).find(|&m| toks[m].depth == 0 && toks[m].kind == Tok::Punct(',')).unwrap_or(end);
            let expr = between(&toks, sql, j, stop);
            if expr.is_empty() {
                bail!(refuse(format!("{column} = needs a value")));
            }
            sets.push((column, expr));
            k = stop + 1;
        }
        if sets.is_empty() {
            bail!(refuse("UPDATE needs at least one SET column = value"));
        }
        let cond = wh.map(|w| between(&toks, sql, w + 1, toks.len()));
        return Ok(Change::Update { table, alias, sets, cond });
    }
    if kw(0, "DELETE") {
        if !kw(1, "FROM") {
            bail!(refuse("expected DELETE FROM <table>"));
        }
        let (table, mut i) = table_name(&toks, sql, 2)?;
        let mut alias = None;
        if kw(i, "AS") {
            alias = Some(toks.get(i + 1).and_then(|t| name_of(t, sql)).ok_or_else(|| refuse("expected an alias after AS"))?);
            i += 2;
        } else if i < toks.len() && !kw(i, "WHERE") && !kw(i, "USING") {
            alias = toks.get(i).and_then(|t| name_of(t, sql));
            i += 1;
        }
        if kw(i, "USING") {
            bail!(refuse("DELETE … USING is not supported; use a subquery in WHERE"));
        }
        let cond = if kw(i, "WHERE") {
            Some(between(&toks, sql, i + 1, toks.len()))
        } else if i < toks.len() {
            bail!(refuse("expected WHERE after the table name"));
        } else {
            None
        };
        return Ok(Change::Delete { table, alias, cond });
    }
    bail!(refuse("only INSERT INTO, UPDATE and DELETE FROM can change data from SQL"))
}

impl Workbook {
    /// Apply an `INSERT`, `UPDATE` or `DELETE` as one Rowfinity change. See the module docs.
    pub fn sql_change(&mut self, sql: &str, params: &[QueryParam]) -> Result<SqlChangeOutcome> {
        let sql = sqllex::trim_statement(sql).to_string();
        let change = parse_change(&sql)?;
        let tables = storage::user_tables(&self.conn)?;
        let target = |name: &str| -> Result<String> {
            if name.to_lowercase().starts_with("_rf_") {
                bail!(refuse(format!("{name} is internal to Rowfinity and cannot be changed")));
            }
            tables
                .iter()
                .find(|t| t.eq_ignore_ascii_case(name))
                .cloned()
                .ok_or_else(|| refuse(format!("there is no table named {name:?} in this workbook")))
        };
        match change {
            Change::Insert { table, columns, query } => {
                let table = target(&table)?;
                let cols = self.writable_columns(&table, columns.as_deref())?;
                let checked = self.db().check(&query)?;
                sqlpolicy::validate_params(&checked.params, params)?;
                self.mutating("sql_change", None, |wb| {
                    let q = wb.db().expand_query(&checked)?;
                    let staged = wb.stage(&q.sql, q.lines_before, params)?;
                    if staged.len() != cols.len() {
                        bail!(
                            "the query gives {} column{} but {} {} listed for {table:?}; nothing was changed",
                            staged.len(),
                            if staged.len() == 1 { "" } else { "s" },
                            cols.len(),
                            if cols.len() == 1 { "is" } else { "are" }
                        );
                    }
                    let pairs: Vec<(&Column, String)> = cols.iter().zip(staged.iter().map(|c| ident(&c.name))).collect();
                    wb.check_conversions(&pairs, &staged)?;
                    let n = wb.append_staged(&table, &pairs)?;
                    Ok(SqlChangeOutcome { table: table.clone(), kind: "insert".into(), rows: n })
                })
            }
            Change::Update { table, alias, sets, cond } => {
                let table = target(&table)?;
                let names: Vec<String> = sets.iter().map(|(c, _)| c.clone()).collect();
                let cols = self.writable_columns(&table, Some(&names))?;
                let alias = alias.unwrap_or_else(|| table.clone());
                let mut select = format!("SELECT {}.{ID} AS _rf_target", ident(&alias));
                for (i, (_, expr)) in sets.iter().enumerate() {
                    select.push_str(&format!(", (\n{expr}\n) AS _rf_v{i}"));
                }
                select.push_str(&format!("\nFROM {} AS {}", ident(&table), ident(&alias)));
                if let Some(c) = &cond {
                    select.push_str(&format!("\nWHERE (\n{c}\n)"));
                }
                let checked = self.checked_shape(&select, &table, &alias, sets.len(), cond.is_some())?;
                sqlpolicy::validate_params(&checked.params, params)?;
                self.mutating("sql_change", None, |wb| {
                    let q = wb.db().expand_query(&checked)?;
                    let staged = wb.stage(&q.sql, q.lines_before, params)?;
                    let pairs: Vec<(&Column, String)> = cols.iter().enumerate().map(|(i, c)| (c, format!("_rf_v{i}"))).collect();
                    wb.check_conversions(&pairs, &staged)?;
                    let assignments: Vec<String> =
                        pairs.iter().map(|(c, v)| format!("{} = CAST(s.{v} AS {})", ident(&c.name), c.sql_type)).collect();
                    let n = wb.conn.execute(
                        &format!(
                            "UPDATE main.{t} SET {} FROM temp.{STAGE} s WHERE main.{t}.{ID} = s._rf_target",
                            assignments.join(", "),
                            t = ident(&table)
                        ),
                        [],
                    )?;
                    crate::faults::check("sql_change")?;
                    storage::update_table(&wb.conn, &table, None, None)?;
                    wb.conn.execute_batch(&format!("DROP TABLE temp.{STAGE};"))?;
                    Ok(SqlChangeOutcome { table: table.clone(), kind: "update".into(), rows: n as u64 })
                })
            }
            Change::Delete { table, alias, cond } => {
                let table = target(&table)?;
                let alias = alias.unwrap_or_else(|| table.clone());
                let mut select = format!("SELECT {}.{ID} AS _rf_target\nFROM {} AS {}", ident(&alias), ident(&table), ident(&alias));
                if let Some(c) = &cond {
                    select.push_str(&format!("\nWHERE (\n{c}\n)"));
                }
                let checked = self.checked_shape(&select, &table, &alias, 0, cond.is_some())?;
                sqlpolicy::validate_params(&checked.params, params)?;
                self.mutating("sql_change", None, |wb| {
                    let q = wb.db().expand_query(&checked)?;
                    wb.stage(&q.sql, q.lines_before, params)?;
                    let n = wb.conn.execute(
                        &format!("DELETE FROM main.{} WHERE {ID} IN (SELECT _rf_target FROM temp.{STAGE})", ident(&table)),
                        [],
                    )?;
                    crate::faults::check("sql_change")?;
                    storage::update_table(&wb.conn, &table, None, Some(false))?;
                    wb.conn.execute_batch(&format!("DROP TABLE temp.{STAGE};"))?;
                    Ok(SqlChangeOutcome { table: table.clone(), kind: "delete".into(), rows: n as u64 })
                })
            }
        }
    }

    /// Target columns: the named ones, or every data column in order. Formula and internal
    /// columns cannot be written.
    fn writable_columns(&self, table: &str, names: Option<&[String]>) -> Result<Vec<Column>> {
        let all = self.columns(table)?;
        let Some(names) = names else {
            return Ok(all.into_iter().filter(|c| !c.is_computed()).collect());
        };
        let mut out: Vec<Column> = Vec::new();
        for n in names {
            if is_internal(&n.to_lowercase()) {
                bail!(refuse(format!("{n} is managed by Rowfinity and cannot be written")));
            }
            let c = all
                .iter()
                .find(|c| crate::formula::same_name(&c.name, n))
                .ok_or_else(|| refuse(format!("{table:?} has no column {n:?}")))?;
            if c.is_computed() {
                bail!(refuse(format!("{:?} is a formula column; change its formula instead", c.name)));
            }
            if out.iter().any(|o| o.name == c.name) {
                bail!(refuse(format!("{:?} is set twice", c.name)));
            }
            out.push(c.clone());
        }
        Ok(out)
    }

    /// Check `select` with the policy and confirm DuckDB parsed it as exactly the statement
    /// we assembled: our select list, the target table, the condition, nothing more. A part
    /// that escaped its parentheses would change that shape.
    fn checked_shape(&self, select: &str, table: &str, alias: &str, values: usize, has_cond: bool) -> Result<sqlpolicy::CheckedSelect> {
        let checked = self.db().check(select)?;
        let tree = sqlpolicy::parse(&self.conn, select)?;
        let node = &tree["statements"][0]["node"];
        let list = node["select_list"].as_array().cloned().unwrap_or_default();
        let target_ok = list.first().is_some_and(|c| {
            c["class"] == "COLUMN_REF"
                && c["alias"] == "_rf_target"
                && c["column_names"].as_array().is_some_and(|n| n.len() == 2 && n[1] == ID && n[0].as_str().is_some_and(|a| a.eq_ignore_ascii_case(alias)))
        });
        let values_ok = list.len() == values + 1 && (0..values).all(|i| list[i + 1]["alias"] == format!("_rf_v{i}"));
        let from = &node["from_table"];
        let from_ok = from["type"] == "BASE_TABLE"
            && from["table_name"].as_str().is_some_and(|t| t.eq_ignore_ascii_case(table))
            && from["schema_name"] == ""
            && from["alias"].as_str().is_some_and(|a| a.eq_ignore_ascii_case(alias));
        let empty = |v: &Value| v.as_array().is_some_and(|a| a.is_empty());
        let rest_ok = node["type"] == "SELECT_NODE"
            && node["where_clause"].is_null() != has_cond
            && node["having"].is_null()
            && node["qualify"].is_null()
            && node["sample"].is_null()
            && empty(&node["group_expressions"])
            && empty(&node["modifiers"])
            && empty(&node["cte_map"]["map"]);
        if !(target_ok && values_ok && from_ok && rest_ok) {
            bail!(refuse("could not read this statement as one change of one table; check its parentheses and clauses"));
        }
        Ok(checked)
    }

    /// Run a checked, expanded SELECT once into the staging table; returns its columns.
    fn stage(&mut self, select: &str, lines_before: usize, params: &[QueryParam]) -> Result<Vec<Column>> {
        let sql = format!("CREATE OR REPLACE TEMP TABLE {STAGE} AS SELECT * FROM (\n{select}\n)");
        let mut stmt = self.conn.prepare(&sql).map_err(|e| user_error(e.into(), lines_before + 1))?;
        sqlpolicy::bind(&mut stmt, params)?;
        stmt.raw_execute().map_err(|e| user_error(e.into(), lines_before + 1))?;
        self.db().describe(&format!("SELECT * FROM temp.{STAGE}"))
    }

    /// Refuse the change if any staged value does not convert to its column's type, or a
    /// number would be rounded on the way in.
    fn check_conversions(&self, pairs: &[(&Column, String)], staged: &[Column]) -> Result<()> {
        for (col, source) in pairs {
            let from_numeric = staged.iter().find(|s| ident(&s.name) == *source || s.name == *source).is_some_and(|s| s.kind.is_numeric());
            let cast = format!("TRY_CAST({source} AS {})", col.sql_type);
            let bad = if from_numeric && col.kind.is_numeric() {
                format!("{source} IS NOT NULL AND ({cast} IS NULL OR {cast} <> {source})")
            } else {
                format!("{source} IS NOT NULL AND {cast} IS NULL")
            };
            let found: Option<String> = self
                .conn
                .query_row(&format!("SELECT {source}::VARCHAR FROM temp.{STAGE} WHERE {bad} LIMIT 1"), [], |r| r.get(0))
                .optional()?;
            if let Some(v) = found {
                bail!("{v:?} cannot be stored in column {:?} ({}) without changing it; nothing was changed", col.name, col.sql_type);
            }
        }
        Ok(())
    }

    /// Append the staged rows after the last row, with new record ids and order keys.
    fn append_staged(&mut self, table: &str, pairs: &[(&Column, String)]) -> Result<u64> {
        let meta = storage::table_meta(&self.conn, table)?;
        let index: std::sync::Arc<PosIndex> = self.db().index(table)?;
        let rows = index.rows;
        let base = if index.is_dense() {
            rows as i64 * ORD_GAP
        } else if rows == 0 {
            0
        } else {
            index.ord_at(&self.conn, table, rows - 1)? + ORD_GAP
        };
        let targets: Vec<String> = pairs.iter().map(|(c, _)| ident(&c.name)).collect();
        let values: Vec<String> = pairs.iter().map(|(c, s)| format!("CAST({s} AS {})", c.sql_type)).collect();
        let n = self.conn.execute(
            &format!(
                "INSERT INTO main.{t} ({ID}, {ORD}{sep}{targets})
                 SELECT {next} + _rf_n, {base} + _rf_n * {ORD_GAP}{sep}{values}
                 FROM (SELECT (row_number() OVER () - 1)::BIGINT AS _rf_n, * FROM temp.{STAGE})",
                t = ident(table),
                sep = if targets.is_empty() { "" } else { ", " },
                targets = targets.join(", "),
                values = values.join(", "),
                next = meta.next_id,
            ),
            [],
        )? as u64;
        crate::faults::check("sql_change")?;
        self.conn.execute_batch(&format!("DROP TABLE temp.{STAGE};"))?;
        storage::update_table(&self.conn, table, Some(meta.next_id + n as i64), None)?;
        self.after_commit.push(AfterCommit::Index(table.to_string(), index.appended(n)));
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_split_into_their_parts() {
        match parse_change("INSERT INTO \"My \"\"T\"\"\" (a, \"B c\") SELECT 1, 'x, y'").unwrap() {
            Change::Insert { table, columns, query } => {
                assert_eq!(table, "My \"T\"");
                assert_eq!(columns.unwrap(), ["a", "B c"]);
                assert_eq!(query, "SELECT 1, 'x, y'");
            }
            other => panic!("{other:?}"),
        }
        match parse_change("insert into main.t (select 1)").unwrap() {
            Change::Insert { columns, query, .. } => {
                assert!(columns.is_none());
                assert_eq!(query, "(select 1)");
            }
            other => panic!("{other:?}"),
        }
        match parse_change("UPDATE t SET a = f(x, y), b = [1, 2], c = x IS DISTINCT FROM y WHERE id IN (SELECT id FROM u WHERE z = ',')").unwrap() {
            Change::Update { sets, cond, alias, .. } => {
                assert_eq!(sets, [("a".into(), "f(x, y)".into()), ("b".into(), "[1, 2]".into()), ("c".into(), "x IS DISTINCT FROM y".into())]);
                assert_eq!(cond.unwrap(), "id IN (SELECT id FROM u WHERE z = ',')");
                assert!(alias.is_none());
            }
            other => panic!("{other:?}"),
        }
        match parse_change("DELETE FROM t AS x WHERE x.a > 1").unwrap() {
            Change::Delete { alias, cond, .. } => {
                assert_eq!(alias.as_deref(), Some("x"));
                assert_eq!(cond.as_deref(), Some("x.a > 1"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(parse_change("DELETE FROM t").unwrap(), Change::Delete { cond: None, .. }));
    }

    #[test]
    fn unsupported_forms_are_refused() {
        for sql in [
            "UPDATE t SET a = 1 FROM u WHERE t.id = u.id",
            "UPDATE t SET a = 1 RETURNING *",
            "DELETE FROM t USING u WHERE t.id = u.id",
            "DELETE FROM t WHERE a = 1 RETURNING a",
            "INSERT OR REPLACE INTO t VALUES (1)",
            "INSERT INTO t BY NAME SELECT 1 AS a",
            "INSERT INTO t DEFAULT VALUES",
            "INSERT INTO t VALUES (1) ON CONFLICT DO NOTHING",
            "INSERT INTO other.t VALUES (1)",
            "UPDATE t SET a = 1; DROP TABLE t",
            "UPDATE t SET t.a = 1",
            "UPDATE t SET",
            "TRUNCATE t",
            "MERGE INTO t USING u ON true WHEN MATCHED THEN DELETE",
        ] {
            assert!(parse_change(sql).is_err(), "{sql}");
        }
    }
}
