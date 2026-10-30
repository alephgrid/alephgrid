//! What SQL written by a user may do (checklist 4.07), and how its parameters are bound (4.02).
//!
//! Every path that runs user SQL calls [`check_select`] first. It parses the statement with
//! DuckDB's own parser (`json_serialize_sql`, which never binds or executes anything) and
//! walks the parse tree:
//!
//! * exactly one statement, and it is a query (`SELECT`, `WITH`, `VALUES`, `FROM`-first,
//!   set operations, `DESCRIBE`/`SUMMARIZE` of a query);
//! * relations are user tables of this workbook or CTEs in scope: no `_rf_*` internals, no
//!   other catalogs or schemas, no system views, no files or URLs (replacement scans);
//! * table functions come from a short allow-list; scalar functions that read settings,
//!   secrets or variables, run SQL text, plan SQL or touch sequences are refused;
//! * a CTE may not reuse the name of a system relation, so a reference that the walker
//!   takes for a CTE can never bind to one;
//! * parameters are named (`$region`); positional `?`/`$1` are refused.
//!
//! This is the first layer. The second is the database itself: workbooks served by the
//! server run with external access disabled (see [`crate::Workbook::restrict_access`]).

use std::collections::{BTreeSet, HashSet};

use anyhow::{anyhow, bail, Result};
use duckdb::types::{TimeUnit, Value as DbValue};
use duckdb::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::sqllex;

/// A statement refused by the SQL policy. Nothing was executed.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct NotAllowed(pub String);

/// The statement is a change (`INSERT`, `UPDATE`, `DELETE`): run it with `sql_change`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct IsChange(pub String);

/// A parameter is missing, unknown or its value does not parse as its type.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct BadParameter(pub String);

/// Table functions user SQL may call. Everything that reads files, URLs, the catalog or
/// settings (`read_csv`, `glob`, `duckdb_settings`, `query`, …) is absent on purpose.
pub const TABLE_FUNCTIONS: &[&str] = &["range", "generate_series", "unnest"];

/// Scalar functions that are refused even though the database would run them.
const DENIED_FUNCTIONS: &[&str] = &[
    "current_setting",
    "getenv",
    "getvariable",
    "json_execute_serialized_sql",
    "json_serialize_plan",
    "nextval",
    "currval",
    "setseed",
    "which_secret",
];

/// Function names refused by pattern: settings, secrets, variables, extensions, dynamic SQL.
fn denied_function(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    DENIED_FUNCTIONS.contains(&n.as_str())
        || ["setting", "secret", "variable", "extension", "execute", "pragma"].iter().any(|p| n.contains(p))
        || n.starts_with("duckdb_")
}

/// A user query that passed the policy.
#[derive(Debug, Clone)]
pub struct CheckedSelect {
    /// The statement text, trimmed, without a trailing `;` or comments.
    pub sql: String,
    /// User tables it reads, as stored, sorted.
    pub sources: Vec<String>,
    /// Named parameters, in order of first use.
    pub params: Vec<String>,
}

/// Type of a bound parameter value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    Text,
    Integer,
    Decimal,
    Double,
    Boolean,
    Date,
    Timestamp,
}

/// A value for a `$name` placeholder. `value: null` binds SQL NULL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct QueryParam {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ParamType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub value: Option<String>,
}

/// Check `sql` against the policy for this workbook (`user_tables` as stored).
pub fn check_select(conn: &Connection, user_tables: &[String], sql: &str) -> Result<CheckedSelect> {
    let sql = sqllex::trim_statement(sql);
    if sql.is_empty() {
        bail!(NotAllowed("the query is empty".into()));
    }
    let tree = parse(conn, sql)?;
    let statements = tree["statements"].as_array().ok_or_else(|| anyhow!("SQL error: could not parse the query"))?;
    let node = match statements.as_slice() {
        [one] => &one["node"],
        _ => bail!(NotAllowed("run one statement at a time".into())),
    };
    let mut walk = Walk::new(conn, user_tables)?;
    walk.query_node(node, &[])?;
    walk.check_cte_names()?;
    Ok(CheckedSelect { sql: sql.to_string(), sources: walk.sources.into_iter().collect(), params: walk.params })
}

/// Check the functions used in an already-parsed expression tree (formula columns).
pub(crate) fn check_expression_functions(tree: &Value) -> Result<()> {
    let mut err = None;
    visit(tree, &mut |obj| {
        if err.is_none() {
            if let Err(e) = check_function(obj) {
                err = Some(e);
            }
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Parse with DuckDB's parser; refuse non-queries with a message that says why.
pub(crate) fn parse(conn: &Connection, sql: &str) -> Result<Value> {
    let json: String = conn.query_row("SELECT json_serialize_sql(?::VARCHAR)", [sql], |r| r.get(0))?;
    let tree: Value = serde_json::from_str(&json)?;
    if tree["error"].as_bool() == Some(true) {
        let msg = tree["error_message"].as_str().unwrap_or("could not parse the query");
        if msg.contains("Only SELECT statements") {
            if sqllex::statement_count(sql) > 1 {
                bail!(NotAllowed("run one statement at a time".into()));
            }
            let keyword = sqllex::first_keyword(sql).unwrap_or_default();
            if matches!(keyword.as_str(), "INSERT" | "UPDATE" | "DELETE") {
                bail!(IsChange(format!(
                    "{keyword} changes data; it runs as a change of the workbook (with its revision and history), not as a query"
                )));
            }
            if keyword == "PIVOT" || keyword == "UNPIVOT" || sql.to_ascii_uppercase().contains("PIVOT") {
                bail!(NotAllowed("PIVOT needs explicit values here, e.g. PIVOT t ON col IN ('a', 'b') USING sum(x)".into()));
            }
            let what = if keyword.is_empty() { "this statement".to_string() } else { format!("{keyword} statements") };
            bail!(NotAllowed(format!(
                "only queries run in the SQL editor; {what} are not allowed (use the workbook's menus to import, export, create or change tables)"
            )));
        }
        if msg.contains("Mixing named and positional") {
            bail!(BadParameter("use named parameters such as $region; positional ? and $1 are not supported".into()));
        }
        bail!("SQL error: {msg}");
    }
    Ok(tree)
}

struct Walk {
    /// User tables, lower-case → as stored.
    tables: Vec<(String, String)>,
    db_name: String,
    /// Lower-case names of system relations a CTE may not shadow.
    system: HashSet<String>,
    sources: BTreeSet<String>,
    params: Vec<String>,
    cte_names: Vec<String>,
}

impl Walk {
    fn new(conn: &Connection, user_tables: &[String]) -> Result<Walk> {
        let db_name: String = conn.query_row("SELECT current_database()", [], |r| r.get(0))?;
        let system: HashSet<String> = conn
            .prepare(
                "SELECT lower(table_name) FROM duckdb_tables()
                 UNION SELECT lower(view_name) FROM duckdb_views()
                 UNION SELECT lower(function_name) FROM duckdb_functions() WHERE function_type IN ('table', 'table_macro')",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(Walk {
            tables: user_tables.iter().map(|t| (t.to_lowercase(), t.clone())).collect(),
            db_name,
            system,
            sources: BTreeSet::new(),
            params: Vec::new(),
            cte_names: Vec::new(),
        })
    }

    /// A query node (`SELECT_NODE`, `SET_OPERATION_NODE`, `RECURSIVE_CTE_NODE`, …) with the
    /// CTE names visible from outside it.
    fn query_node(&mut self, node: &Value, scope: &[String]) -> Result<()> {
        let mut scope = scope.to_vec();
        if let Some(entries) = node["cte_map"]["map"].as_array() {
            for entry in entries {
                let name = entry["key"].as_str().unwrap_or_default().to_lowercase();
                // The body's place in the tree differs between DuckDB versions; refuse a CTE
                // whose body cannot be found rather than leave it unchecked.
                let body = [&entry["value"]["query_node"], &entry["value"]["query"]["node"]]
                    .into_iter()
                    .find(|b| b.is_object())
                    .ok_or_else(|| anyhow!(NotAllowed(format!("could not check the CTE {name:?}"))))?;
                // A CTE sees the ones before it, and itself only when it is recursive.
                let mut inner = scope.clone();
                if body["type"] == "RECURSIVE_CTE_NODE" {
                    inner.push(name.clone());
                }
                self.query_node(body, &inner)?;
                self.cte_names.push(name.clone());
                scope.push(name);
            }
        }
        if let Value::Object(obj) = node {
            for (key, child) in obj {
                if key != "cte_map" {
                    self.any(child, &scope)?;
                }
            }
        }
        Ok(())
    }

    fn any(&mut self, v: &Value, scope: &[String]) -> Result<()> {
        match v {
            Value::Array(items) => {
                for item in items {
                    self.any(item, scope)?;
                }
                Ok(())
            }
            Value::Object(obj) => {
                if obj.contains_key("cte_map") {
                    return self.query_node(v, scope);
                }
                match obj.get("type").and_then(Value::as_str) {
                    Some("BASE_TABLE") => self.table(obj, scope)?,
                    Some("TABLE_FUNCTION") => {
                        let f = &obj["function"];
                        let name = f["function_name"].as_str().unwrap_or_default().to_ascii_lowercase();
                        if !TABLE_FUNCTIONS.contains(&name.as_str()) || !plain_schema(f) {
                            bail!(NotAllowed(format!(
                                "the table function {name}() is not available in Alephgrid SQL; import files with Import, and query this workbook's tables by name"
                            )));
                        }
                    }
                    Some("SHOW_REF") if obj.get("query").is_none_or(Value::is_null) => {
                        bail!(NotAllowed("SHOW lists the database's internal objects; use the schema browser instead".into()));
                    }
                    _ => {}
                }
                match obj.get("class").and_then(Value::as_str) {
                    Some("FUNCTION" | "WINDOW") => check_function(obj)?,
                    Some("PARAMETER") => {
                        let name = obj["identifier"].as_str().unwrap_or_default();
                        if name.is_empty() || name.chars().all(|c| c.is_ascii_digit()) {
                            bail!(BadParameter("use named parameters such as $region; positional ? and $1 are not supported".into()));
                        }
                        if !self.params.iter().any(|p| p == name) {
                            self.params.push(name.to_string());
                        }
                    }
                    _ => {}
                }
                for child in obj.values() {
                    self.any(child, scope)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn table(&mut self, obj: &serde_json::Map<String, Value>, scope: &[String]) -> Result<()> {
        let name = obj.get("table_name").and_then(Value::as_str).unwrap_or_default();
        let schema = obj.get("schema_name").and_then(Value::as_str).unwrap_or_default();
        let catalog = obj.get("catalog_name").and_then(Value::as_str).unwrap_or_default();
        if obj.get("at_clause").is_some_and(|a| !a.is_null()) {
            bail!(NotAllowed("AT (VERSION …) is not supported".into()));
        }
        let lower = name.to_lowercase();
        if !catalog.is_empty() && !catalog.eq_ignore_ascii_case(&self.db_name) || !(schema.is_empty() || schema.eq_ignore_ascii_case("main")) {
            bail!(NotAllowed(format!("{} is outside this workbook; only its own tables can be queried", qualified(catalog, schema, name))));
        }
        if catalog.is_empty() && schema.is_empty() && scope.contains(&lower) {
            return Ok(());
        }
        if lower.starts_with("_rf_") {
            bail!(NotAllowed(format!("{name} is internal to Alephgrid and cannot be queried")));
        }
        match self.tables.iter().find(|(l, _)| *l == lower) {
            Some((_, stored)) => {
                self.sources.insert(stored.clone());
                Ok(())
            }
            None => bail!(NotAllowed(format!(
                "there is no table named {name:?} in this workbook{}",
                if name.contains(['.', '/', '\\', ':']) { " (files and URLs cannot be read from SQL; use Import)" } else { "" }
            ))),
        }
    }

    fn check_cte_names(&self) -> Result<()> {
        for name in &self.cte_names {
            if self.system.contains(name) && !self.tables.iter().any(|(l, _)| l == name) {
                bail!(NotAllowed(format!("a CTE cannot be called {name:?}: that name belongs to a system relation")));
            }
            if name.starts_with("_rf_") {
                bail!(NotAllowed(format!("a CTE cannot be called {name:?}: names starting with _rf_ are reserved")));
            }
        }
        Ok(())
    }
}

fn qualified(catalog: &str, schema: &str, name: &str) -> String {
    [catalog, schema, name].iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(".")
}

/// A function call may name no catalog, and only the `main` (or no) schema.
fn plain_schema(f: &Value) -> bool {
    let catalog = f["catalog"].as_str().unwrap_or_default();
    let schema = f["schema"].as_str().unwrap_or_default();
    catalog.is_empty() && (schema.is_empty() || schema.eq_ignore_ascii_case("main"))
}

fn check_function(obj: &serde_json::Map<String, Value>) -> Result<()> {
    if !matches!(obj.get("class").and_then(Value::as_str), Some("FUNCTION" | "WINDOW")) {
        return Ok(());
    }
    let name = obj.get("function_name").and_then(Value::as_str).unwrap_or_default();
    if denied_function(name) {
        bail!(NotAllowed(format!("the function {name}() is not available in Alephgrid SQL")));
    }
    if !plain_schema(&Value::Object(obj.clone())) {
        bail!(NotAllowed(format!("functions must be called by plain name ({name})")));
    }
    Ok(())
}

fn visit(v: &Value, f: &mut impl FnMut(&serde_json::Map<String, Value>)) {
    match v {
        Value::Object(obj) => {
            f(obj);
            for child in obj.values() {
                visit(child, f);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| visit(i, f)),
        _ => {}
    }
}

// ------------------------------------------------------------------ parameters

/// Bind `params` to a prepared statement whose placeholders are `wanted` (names in order of
/// the statement's parameter indexes, from [`duckdb::Statement::parameter_name`]). Every
/// placeholder needs a value and every value a placeholder.
pub(crate) fn bind(stmt: &mut duckdb::Statement<'_>, params: &[QueryParam]) -> Result<()> {
    let mut used = vec![false; params.len()];
    for i in 1..=stmt.parameter_count() {
        let name = stmt.parameter_name(i)?;
        let Some(k) = params.iter().position(|p| p.name == name) else {
            bail!(BadParameter(format!("parameter ${name} has no value")));
        };
        used[k] = true;
        stmt.raw_bind_parameter(i, to_db_value(&params[k])?)?;
    }
    if let Some(k) = used.iter().position(|u| !u) {
        bail!(BadParameter(format!("the query does not use the parameter ${}", params[k].name)));
    }
    Ok(())
}

/// Check that `params` name exactly the placeholders `wanted` and that every value parses.
pub fn validate_params(wanted: &[String], params: &[QueryParam]) -> Result<()> {
    let mut seen = HashSet::new();
    for p in params {
        if !seen.insert(p.name.as_str()) {
            bail!(BadParameter(format!("the parameter ${} is given twice", p.name)));
        }
        if !wanted.contains(&p.name) {
            bail!(BadParameter(format!("the query does not use the parameter ${}", p.name)));
        }
        to_db_value(p)?;
    }
    if let Some(missing) = wanted.iter().find(|w| !params.iter().any(|p| &p.name == *w)) {
        bail!(BadParameter(format!("parameter ${missing} has no value")));
    }
    Ok(())
}

/// The DuckDB value for a parameter, parsed exactly (no rounding, no locale).
pub fn to_db_value(p: &QueryParam) -> Result<DbValue> {
    let Some(text) = p.value.as_deref() else { return Ok(DbValue::Null) };
    let bad = |what: &str| BadParameter(format!("${}: {text:?} is not {what}", p.name));
    let t = text.trim();
    Ok(match p.ty {
        ParamType::Text => DbValue::Text(text.to_string()),
        ParamType::Integer => {
            let v: i128 = t.parse().map_err(|_| bad("an integer"))?;
            match i64::try_from(v) {
                Ok(v) => DbValue::BigInt(v),
                Err(_) => DbValue::HugeInt(v),
            }
        }
        ParamType::Decimal => {
            let (value, width, scale) = parse_decimal(t).ok_or_else(|| bad("a decimal number (at most 38 digits)"))?;
            DbValue::Decimal(duckdb::types::Decimal::new(width, scale, value).map_err(|_| bad("a decimal number (at most 38 digits)"))?)
        }
        ParamType::Double => {
            let v: f64 = match t.to_ascii_lowercase().as_str() {
                "nan" => f64::NAN,
                "inf" | "infinity" => f64::INFINITY,
                "-inf" | "-infinity" => f64::NEG_INFINITY,
                _ => t.parse().map_err(|_| bad("a number"))?,
            };
            DbValue::Double(v)
        }
        ParamType::Boolean => DbValue::Boolean(match t.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" => true,
            "false" | "f" | "0" | "no" => false,
            _ => bail!(bad("true or false")),
        }),
        ParamType::Date => DbValue::Date32(parse_date(t).ok_or_else(|| bad("a date (YYYY-MM-DD)"))?),
        ParamType::Timestamp => {
            DbValue::Timestamp(TimeUnit::Microsecond, parse_timestamp(t).ok_or_else(|| bad("a timestamp (YYYY-MM-DD HH:MM:SS[.ffffff])"))?)
        }
    })
}

/// `-123.4500` → (−1234500, width 7, scale 4).
fn parse_decimal(t: &str) -> Option<(i128, u8, u8)> {
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    if int.is_empty() && frac.is_empty() || !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let digits = format!("{int}{frac}");
    let significant = digits.trim_start_matches('0');
    // At least one integer digit, so DuckDB prints 0.5 rather than .5.
    let width = significant.len().max((frac.len() + 1).min(38)).max(1);
    if width > 38 {
        return None;
    }
    let mut v: i128 = if significant.is_empty() { 0 } else { significant.parse().ok()? };
    if neg {
        v = -v;
    }
    Some((v, width as u8, frac.len() as u8))
}

/// Days since 1970-01-01 for a proleptic Gregorian `YYYY-MM-DD` (years 1–9999).
fn parse_date(t: &str) -> Option<i32> {
    let mut parts = t.splitn(3, '-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days_in = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || d < 1 || d > days_in[(m - 1) as usize] {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    i32::try_from(era * 146_097 + doe - 719_468).ok()
}

/// Microseconds since the epoch for `YYYY-MM-DD[ T]HH:MM[:SS[.ffffff]]` (no time zone).
fn parse_timestamp(t: &str) -> Option<i64> {
    let (date, time) = t.split_once([' ', 'T']).unwrap_or((t, "00:00:00"));
    let days = parse_date(date)? as i64;
    let mut hms = time.splitn(3, ':');
    let h: i64 = hms.next()?.parse().ok()?;
    let mi: i64 = hms.next()?.parse().ok()?;
    let (s, frac) = match hms.next() {
        Some(sec) => match sec.split_once('.') {
            Some((s, f)) if !f.is_empty() && f.len() <= 6 && f.chars().all(|c| c.is_ascii_digit()) => {
                (s.parse::<i64>().ok()?, format!("{f:0<6}").parse::<i64>().ok()?)
            }
            Some(_) => return None,
            None => (sec.parse::<i64>().ok()?, 0),
        },
        None => (0, 0),
    };
    if !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..60).contains(&s) {
        return None;
    }
    Some(((days * 86_400 + h * 3_600 + mi * 60 + s) * 1_000_000) + frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE orders (id INT, region VARCHAR); CREATE TABLE \"Prix €\" (x INT); CREATE TABLE _rf_history (x INT);").unwrap();
        c
    }

    fn tables() -> Vec<String> {
        vec!["orders".into(), "Prix €".into()]
    }

    fn ok(sql: &str) -> CheckedSelect {
        check_select(&conn(), &tables(), sql).unwrap_or_else(|e| panic!("{sql}: {e:#}"))
    }

    fn refused(sql: &str) -> String {
        match check_select(&conn(), &tables(), sql) {
            Ok(_) => panic!("{sql} was allowed"),
            Err(e) => format!("{e:#}"),
        }
    }

    #[test]
    fn queries_that_read_this_workbook_are_allowed() {
        assert_eq!(ok("SELECT * FROM orders;  -- done").sql, "SELECT * FROM orders");
        assert_eq!(ok("select * from ORDERS o join \"Prix €\" p on o.id = p.x").sources, ["Prix €", "orders"]);
        assert_eq!(ok("WITH a AS (SELECT * FROM orders), b AS (SELECT * FROM a) SELECT * FROM b").sources, ["orders"]);
        assert_eq!(ok("WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 5) SELECT * FROM r").sources, Vec::<String>::new());
        ok("FROM orders SELECT region, count(*) GROUP BY ALL");
        ok("VALUES (1, 'a'), (2, 'b')");
        ok("SELECT * FROM range(10) r(i), unnest([1, 2]) u(x), generate_series(1, 3) g(y)");
        ok("SELECT 1 UNION ALL SELECT id FROM main.orders");
        ok("SELECT * FROM orders WHERE id IN (SELECT x FROM \"Prix €\") AND EXISTS (SELECT 1 FROM orders)");
        ok("DESCRIBE SELECT * FROM orders");
        ok("SUMMARIZE orders");
        ok("SELECT list_transform([1, 2], x -> x + 1), sum(id) OVER (PARTITION BY region) FROM orders");
        ok("SELECT * FROM (PIVOT orders ON region IN ('a', 'b') USING sum(id))");
        assert_eq!(ok("SELECT * FROM orders WHERE region = $r AND id > $min OR region = $r").params, ["r", "min"]);
    }

    #[test]
    fn statements_other_than_one_query_are_refused() {
        for sql in [
            "SELECT 1; DROP TABLE orders",
            "SELECT 1; SELECT 2",
            "CREATE TABLE x AS SELECT 1",
            "DROP TABLE orders",
            "ATTACH 'x.db' AS x",
            "COPY orders TO 'out.csv'",
            "INSTALL httpfs",
            "LOAD httpfs",
            "SET memory_limit = '100GB'",
            "RESET threads",
            "PRAGMA database_list",
            "CALL duckdb_settings()",
            "CHECKPOINT",
            "EXPORT DATABASE 'x'",
            "PIVOT orders ON region USING sum(id)",
            "SELECT * FROM (PIVOT orders ON region USING sum(id))",
            "",
            "  -- just a comment",
        ] {
            let e = refused(sql);
            assert!(!e.contains("panicked"), "{sql}: {e}");
        }
        assert!(check_select(&conn(), &tables(), "UPDATE orders SET id = 1").unwrap_err().downcast_ref::<IsChange>().is_some());
    }

    #[test]
    fn files_urls_internals_and_other_catalogs_are_refused() {
        for sql in [
            "SELECT * FROM 'data.csv'",
            "SELECT * FROM '/etc/passwd'",
            "SELECT * FROM 'https://example.com/x.parquet'",
            "SELECT * FROM read_csv('x.csv')",
            "SELECT * FROM read_text('/etc/hostname')",
            "SELECT * FROM glob('/*')",
            "SELECT * FROM query('SELECT 1')",
            "SELECT * FROM query_table('orders')",
            "SELECT * FROM duckdb_settings()",
            "SELECT * FROM duckdb_settings",
            "SELECT * FROM information_schema.tables",
            "SELECT * FROM pg_catalog.pg_class",
            "SELECT * FROM system.main.duckdb_tables",
            "SELECT * FROM _rf_history",
            "SELECT * FROM main._RF_history",
            "SELECT * FROM rf_scratch.v1",
            "SELECT * FROM temp.anything",
            "SELECT * FROM other_db.main.orders",
            "SHOW TABLES",
            "SHOW ALL TABLES",
            "SELECT current_setting('temp_directory')",
            "SELECT json_execute_serialized_sql('{}')",
            "SELECT nextval('s')",
            "SELECT getvariable('x')",
            "SELECT system.main.lower('x')",
            "SELECT * FROM orders AT (VERSION => 1)",
            "WITH duckdb_settings AS (SELECT 1) SELECT * FROM (WITH x AS (SELECT 1) SELECT * FROM duckdb_settings)",
            "WITH a AS (SELECT * FROM later), later AS (SELECT 1) SELECT * FROM a",
            "SELECT * FROM (WITH c AS (SELECT 1) SELECT * FROM c) s, c",
            "WITH _rf_x AS (SELECT 1) SELECT * FROM _rf_x",
            "SELECT * FROM missing_table",
        ] {
            let e = refused(sql);
            assert!(check_select(&conn(), &tables(), sql).unwrap_err().downcast_ref::<NotAllowed>().is_some(), "{sql}: {e}");
        }
    }

    #[test]
    fn positional_parameters_are_refused() {
        for sql in ["SELECT * FROM orders WHERE id = ?", "SELECT * FROM orders WHERE id = $1", "SELECT $a, ?"] {
            let e = check_select(&conn(), &tables(), sql).unwrap_err();
            assert!(e.downcast_ref::<BadParameter>().is_some(), "{sql}: {e:#}");
        }
    }

    #[test]
    fn parameter_values_parse_exactly() {
        let p = |ty, v: &str| to_db_value(&QueryParam { name: "p".into(), ty, value: Some(v.into()) });
        assert_eq!(p(ParamType::Integer, "9007199254740993").unwrap(), DbValue::BigInt(9_007_199_254_740_993));
        assert_eq!(p(ParamType::Integer, "-170141183460469231731687303715884105728").unwrap(), DbValue::HugeInt(i128::MIN));
        assert!(p(ParamType::Integer, "1.5").is_err());
        assert!(p(ParamType::Integer, "1 OR 1=1").is_err());
        assert_eq!(p(ParamType::Decimal, "-123.4500").unwrap(), DbValue::Decimal(duckdb::types::Decimal::new(7, 4, -1_234_500).unwrap()));
        assert_eq!(p(ParamType::Decimal, "0.001").unwrap(), DbValue::Decimal(duckdb::types::Decimal::new(4, 3, 1).unwrap()));
        assert!(p(ParamType::Decimal, "1e5").is_err());
        assert!(p(ParamType::Decimal, &"9".repeat(39)).is_err());
        assert_eq!(p(ParamType::Date, "1970-01-01").unwrap(), DbValue::Date32(0));
        assert_eq!(p(ParamType::Date, "2024-02-29").unwrap(), DbValue::Date32(19_782));
        assert_eq!(p(ParamType::Date, "0001-01-01").unwrap(), DbValue::Date32(-719_162));
        assert!(p(ParamType::Date, "2023-02-29").is_err());
        assert!(p(ParamType::Date, "2024-1-01").is_err());
        assert_eq!(p(ParamType::Timestamp, "1970-01-02 00:00:01.5").unwrap(), DbValue::Timestamp(TimeUnit::Microsecond, 86_401_500_000));
        assert!(p(ParamType::Timestamp, "1970-01-01 24:00:00").is_err());
        assert_eq!(p(ParamType::Boolean, "TRUE").unwrap(), DbValue::Boolean(true));
        assert!(p(ParamType::Boolean, "maybe").is_err());
        assert_eq!(p(ParamType::Text, " '; DROP TABLE orders; -- ").unwrap(), DbValue::Text(" '; DROP TABLE orders; -- ".into()));
        assert_eq!(to_db_value(&QueryParam { name: "p".into(), ty: ParamType::Integer, value: None }).unwrap(), DbValue::Null);
    }

    #[test]
    fn dates_match_duckdb() {
        let c = Connection::open_in_memory().unwrap();
        for d in ["0001-01-01", "1600-02-29", "1899-12-31", "1969-12-31", "2000-02-29", "2038-01-19", "9999-12-31"] {
            let expected: i32 = c.query_row(&format!("SELECT (DATE '{d}' - DATE '1970-01-01')::INTEGER"), [], |r| r.get(0)).unwrap();
            assert_eq!(parse_date(d), Some(expected), "{d}");
        }
    }

    #[test]
    fn params_must_match_placeholders() {
        let wanted = vec!["a".to_string()];
        let v = |n: &str| QueryParam { name: n.into(), ty: ParamType::Text, value: Some("x".into()) };
        validate_params(&wanted, &[v("a")]).unwrap();
        assert!(validate_params(&wanted, &[]).unwrap_err().to_string().contains("$a has no value"));
        assert!(validate_params(&wanted, &[v("a"), v("b")]).unwrap_err().to_string().contains("$b"));
        assert!(validate_params(&wanted, &[v("a"), v("a")]).unwrap_err().to_string().contains("twice"));
    }
}
