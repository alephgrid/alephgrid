//! Small helpers for building DuckDB SQL safely.

/// Quote an identifier (table or column name).
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Quote a string literal.
pub fn lit(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Row ids joined by commas, for `IN (...)`.
pub fn id_list<'a>(ids: impl IntoIterator<Item = &'a i64>) -> String {
    ids.into_iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
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

/// If `sql` starts with `WITH` (after whitespace and comments), the byte offset just
/// past `WITH` or `WITH RECURSIVE`.
pub(crate) fn leading_with(sql: &str) -> Option<usize> {
    let after_with = keyword_at(sql, skip_trivia(sql, 0), "WITH")?;
    let next = skip_trivia(sql, after_with);
    Some(keyword_at(sql, next, "RECURSIVE").unwrap_or(after_with))
}

/// Skip whitespace, `-- line` and `/* block */` comments from byte `i`.
fn skip_trivia(sql: &str, mut i: usize) -> usize {
    let b = sql.as_bytes();
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b[i..].starts_with(b"--") {
            i = sql[i..].find('\n').map(|n| i + n + 1).unwrap_or(b.len());
        } else if b[i..].starts_with(b"/*") {
            i = sql[i + 2..].find("*/").map(|n| i + 2 + n + 2).unwrap_or(b.len());
        } else {
            return i;
        }
    }
}

fn keyword_at(sql: &str, i: usize, word: &str) -> Option<usize> {
    let end = i + word.len();
    let candidate = sql.get(i..end)?;
    let boundary = sql[end..].chars().next().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
    (candidate.eq_ignore_ascii_case(word) && boundary).then_some(end)
}

/// Spreadsheet-flavoured formula -> DuckDB expression.
///
/// Accepts plain DuckDB SQL expressions plus two conveniences:
/// a leading `=` is ignored and `[Column Name]` becomes `"Column Name"`.
pub fn formula_to_sql(formula: &str) -> String {
    translate(formula).sql
}

/// A translated formula plus enough position information to map a span of the SQL
/// back onto the formula text the user wrote.
pub(crate) struct Translated {
    pub sql: String,
    /// For every byte offset of `sql` (plus one past the end), the formula offset it came from.
    origin: Vec<usize>,
    /// `[Name]` references: (sql range, formula range).
    brackets: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)>,
}

impl Translated {
    /// Formula range for a SQL range, and whether it is exactly one `[Name]` reference.
    pub fn formula_range(&self, sql: std::ops::Range<usize>) -> Option<(std::ops::Range<usize>, bool)> {
        if let Some((_, f)) = self.brackets.iter().find(|(s, _)| *s == sql) {
            return Some((f.clone(), true));
        }
        if sql.end > self.sql.len() || self.brackets.iter().any(|(s, _)| s.start < sql.end && sql.start < s.end) {
            return None;
        }
        Some((self.origin[sql.start]..self.origin[sql.end], false))
    }
}

pub(crate) fn translate(formula: &str) -> Translated {
    let trimmed_start = formula.len() - formula.trim_start().len();
    let trimmed = formula.trim();
    let (mut pos, body) = match trimmed.strip_prefix('=') {
        Some(rest) => (trimmed_start + 1, rest),
        None => (trimmed_start, trimmed),
    };
    let mut out = String::with_capacity(body.len());
    let mut origin = Vec::with_capacity(body.len() + 1);
    let mut brackets = Vec::new();
    let mut chars = body.char_indices().peekable();
    let mut in_string = false;
    let base = pos;
    while let Some((i, c)) = chars.next() {
        pos = base + i;
        match c {
            '[' if !in_string => {
                let mut name = String::new();
                let mut end = body.len();
                for (j, n) in chars.by_ref() {
                    if n == ']' {
                        end = j + 1;
                        break;
                    }
                    name.push(n);
                }
                let quoted = ident(&name);
                let sql_start = out.len();
                out.push_str(&quoted);
                origin.extend(std::iter::repeat_n(pos, quoted.len()));
                brackets.push((sql_start..out.len(), pos..base + end));
            }
            _ => {
                if c == '\'' {
                    in_string = !in_string;
                }
                out.push(c);
                origin.extend((0..c.len_utf8()).map(|k| pos + k));
            }
        }
    }
    origin.push(base + body.len());
    Translated { sql: out, origin, brackets }
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
    fn leading_with_detection() {
        assert_eq!(leading_with("WITH x AS (SELECT 1) SELECT * FROM x"), Some(4));
        assert_eq!(leading_with("  with recursive x AS (SELECT 1) SELECT 1"), Some(16));
        assert_eq!(leading_with("-- c\n/* d */ WITH x AS (SELECT 1) SELECT 1"), Some(17));
        assert_eq!(leading_with("WITHOUT"), None);
        assert_eq!(leading_with("SELECT 1"), None);
        assert_eq!(leading_with("WITH RECURSIVE_X AS (SELECT 1) SELECT 1"), Some(4));
        assert_eq!(leading_with(""), None);
        assert_eq!(leading_with("-- only a comment"), None);
    }

    #[test]
    fn translation_maps_positions_back() {
        let f = "  =[Prix €] + qty * '[x]'";
        let t = translate(f);
        assert_eq!(t.sql, "\"Prix €\" + qty * '[x]'");
        let (range, bracketed) = t.formula_range(0.."\"Prix €\"".len()).unwrap();
        assert_eq!((&f[range], bracketed), ("[Prix €]", true));
        let qty = t.sql.find("qty").unwrap();
        let (range, bracketed) = t.formula_range(qty..qty + 3).unwrap();
        assert_eq!((&f[range], bracketed), ("qty", false));
        assert!(t.formula_range(1..4).is_none(), "partial bracket spans do not map");
    }

    #[test]
    fn formulas() {
        assert_eq!(formula_to_sql("=[Fare USD] * 2"), "\"Fare USD\" * 2");
        assert_eq!(formula_to_sql("'[not a ref]' || x"), "'[not a ref]' || x");
        assert_eq!(formula_to_sql("price * qty"), "price * qty");
    }
}
