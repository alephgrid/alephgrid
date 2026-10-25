//! Small helpers for building DuckDB SQL safely.

/// Quote an identifier (table or column name).
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Quote a string literal.
pub fn lit(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Turn a user-facing name into something safe and readable for a table name.
pub fn sanitize_table_name(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    // Leading underscores are trimmed, so user tables can never collide with `_rf_*` internals.
    if out.is_empty() {
        "table".to_string()
    } else {
        out
    }
}

/// Spreadsheet-flavoured formula -> DuckDB expression.
///
/// Accepts plain DuckDB SQL expressions plus two conveniences:
/// a leading `=` is ignored and `[Column Name]` becomes `"Column Name"`.
pub fn formula_to_sql(formula: &str) -> String {
    let body = formula.trim().strip_prefix('=').unwrap_or(formula.trim());
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_string = !in_string;
                out.push(c);
            }
            '[' if !in_string => {
                let mut name = String::new();
                for n in chars.by_ref() {
                    if n == ']' {
                        break;
                    }
                    name.push(n);
                }
                out.push_str(&ident(&name));
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(lit("it's"), "'it''s'");
    }

    #[test]
    fn table_names() {
        assert_eq!(sanitize_table_name("US flights (2024).csv"), "US_flights_2024_csv");
        assert_eq!(sanitize_table_name("___"), "table");
        assert_eq!(sanitize_table_name("_rf_columns"), "rf_columns");
    }

    #[test]
    fn formulas() {
        assert_eq!(formula_to_sql("=[Fare USD] * 2"), "\"Fare USD\" * 2");
        assert_eq!(formula_to_sql("'[not a ref]' || x"), "'[not a ref]' || x");
        assert_eq!(formula_to_sql("price * qty"), "price * qty");
    }
}
