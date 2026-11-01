//! Formula column analysis with DuckDB's own parser (`json_serialize_sql`).
//!
//! A formula must be a single scalar expression: no extra statements, clauses or
//! subqueries. The parse tree gives the exact location of every column reference, which
//! is how renames rewrite dependent formulas and deletes find dependents. Nothing here
//! evaluates formulas; DuckDB does that.

use anyhow::{anyhow, bail, Result};
use duckdb::Connection;
use serde_json::Value;

use crate::sql::{ident, translate};

const PREFIX: &str = "SELECT (";
/// Starts on a new line, so a `--` comment at the end of a formula cannot swallow it (SEC.14).
const SUFFIX: &str = "\n) AS v";

/// A column reference inside a formula, located in the translated SQL expression.
#[derive(Debug, Clone)]
pub(crate) struct ColumnRef {
    pub name: String,
    pub qualified: bool,
    pub span: Option<std::ops::Range<usize>>,
}

/// Validate the shape of a formula and return its column references.
pub(crate) fn analyze(conn: &Connection, formula: &str) -> Result<Vec<ColumnRef>> {
    let expr = translate(formula).sql;
    if expr.trim().is_empty() {
        bail!("formula error: the formula is empty");
    }
    let full = format!("{PREFIX}{expr}{SUFFIX}");
    let json: String = conn.query_row("SELECT json_serialize_sql(?::VARCHAR)", [&full], |r| r.get(0))?;
    let tree: Value = serde_json::from_str(&json)?;
    if tree["error"].as_bool() == Some(true) {
        let msg = tree["error_message"].as_str().unwrap_or("could not parse");
        bail!("formula error: {msg}");
    }
    let statements = tree["statements"].as_array().ok_or_else(|| anyhow!("formula error: could not parse"))?;
    let node = match statements.as_slice() {
        [one] => &one["node"],
        _ => bail!("formula error: a formula must be a single expression"),
    };
    let only_expression = node["type"] == "SELECT_NODE"
        && node["from_table"]["type"] == "EMPTY"
        && node["where_clause"].is_null()
        && node["having"].is_null()
        && node["qualify"].is_null()
        && node["sample"].is_null()
        && node["group_expressions"].as_array().is_some_and(|g| g.is_empty())
        && node["modifiers"].as_array().is_some_and(|m| m.is_empty())
        && node["cte_map"]["map"].as_array().is_some_and(|m| m.is_empty())
        && node["select_list"].as_array().is_some_and(|l| l.len() == 1)
        && node["select_list"][0]["alias"] == "v";
    if !only_expression {
        bail!("formula error: a formula must be a single expression");
    }
    // Same function policy as user SQL (settings, secrets, dynamic SQL, sequences).
    crate::sqlpolicy::check_expression_functions(&node["select_list"][0]).map_err(|e| anyhow!("formula error: {e}"))?;
    let mut refs = Vec::new();
    walk(&node["select_list"][0], &mut |obj| match obj.get("class").and_then(Value::as_str) {
        Some("SUBQUERY") => Err(anyhow!("formula error: subqueries are not allowed in formula columns")),
        Some("STAR") => Err(anyhow!("formula error: * is not allowed in formula columns")),
        Some("COLUMN_REF") => {
            let names: Vec<String> = obj
                .get("column_names")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str().map(String::from))
                .collect();
            let start = obj.get("query_location").and_then(Value::as_u64).map(|s| s as usize);
            let len = obj.get("query_location_length").and_then(Value::as_u64).map(|l| l as usize);
            let span = match (start, len) {
                (Some(s), Some(l)) if s >= PREFIX.len() && s + l <= PREFIX.len() + expr.len() && l > 0 => {
                    Some(s - PREFIX.len()..s - PREFIX.len() + l)
                }
                _ => None,
            };
            if let Some(name) = names.last() {
                refs.push(ColumnRef { name: name.clone(), qualified: names.len() > 1, span });
            }
            Ok(())
        }
        _ => Ok(()),
    })?;
    Ok(refs)
}

fn walk(v: &Value, f: &mut impl FnMut(&serde_json::Map<String, Value>) -> Result<()>) -> Result<()> {
    match v {
        Value::Object(obj) => {
            f(obj)?;
            for child in obj.values() {
                walk(child, f)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, f)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// DuckDB identifiers compare case-insensitively.
pub(crate) fn same_name(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// Does `formula` reference `column`?
pub(crate) fn references(conn: &Connection, formula: &str, column: &str) -> Result<bool> {
    Ok(analyze(conn, formula)?.iter().any(|r| same_name(&r.name, column)))
}

/// Rewrite references to `old` as `new`, keeping the rest of the formula text as typed.
/// Returns `None` when the formula does not reference `old`.
pub(crate) fn rename_reference(conn: &Connection, formula: &str, old: &str, new: &str) -> Result<Option<String>> {
    let refs = analyze(conn, formula)?;
    let hits: Vec<&ColumnRef> = refs.iter().filter(|r| same_name(&r.name, old)).collect();
    if hits.is_empty() {
        return Ok(None);
    }
    let translated = translate(formula);
    let mut edits = Vec::new();
    for r in hits {
        let span = r.span.clone().ok_or_else(|| anyhow!("cannot locate {old:?} in formula {formula:?}"))?;
        if r.qualified {
            bail!("cannot rename {old:?}: formula {formula:?} uses a qualified reference");
        }
        let (range, bracketed) = translated
            .formula_range(span)
            .ok_or_else(|| anyhow!("cannot locate {old:?} in formula {formula:?}"))?;
        let replacement = if bracketed && !new.contains(']') { format!("[{new}]") } else { ident(new) };
        edits.push((range, replacement));
    }
    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
    edits.dedup_by_key(|(r, _)| r.start);
    let mut out = formula.to_string();
    for (range, replacement) in edits {
        out.replace_range(range, &replacement);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_references_and_rejects_non_expressions() {
        let conn = Connection::open_in_memory().unwrap();
        let refs = analyze(&conn, "=[Fare USD] / distance + 'distance' + \"Mixed Case\"").unwrap();
        let names: Vec<_> = refs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Fare USD", "distance", "Mixed Case"]);
        assert!(refs.iter().all(|r| r.span.is_some() && !r.qualified));
        for bad in [
            "(SELECT 1)",
            "1) AS v FROM t WHERE (1",
            "1; SELECT 2",
            "",
            "  =  ",
            "1) AS v, (2",
            "current_setting('temp_directory')",
            "upper(json_execute_serialized_sql('{}'))",
            "nextval('s')",
            "system.main.lower('x')",
        ] {
            assert!(analyze(&conn, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rename_keeps_formatting_and_literals() {
        let conn = Connection::open_in_memory().unwrap();
        let f = "=[a] + A * 2 + 'a' + \"a\" + [b]";
        assert_eq!(rename_reference(&conn, f, "a", "new col").unwrap().unwrap(), "=[new col] + \"new col\" * 2 + 'a' + \"new col\" + [b]");
        assert_eq!(rename_reference(&conn, f, "zzz", "x").unwrap(), None);
        assert_eq!(rename_reference(&conn, "[a]", "a", "x]y").unwrap().unwrap(), "\"x]y\"");
    }
}
