//! Size limits applied before work is done or memory is allocated. Every number a client
//! sends (row counts, column counts, cell lists) is clamped or checked against these.

use serde_json::Value;

/// A request exceeded a size limit; the message names the limit.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct TooLarge(pub String);

/// Most rows in one fetch.
pub const MAX_FETCH_ROWS: u64 = 5_000;
/// Most cells (rows × columns) in one fetch; wide views get fewer rows.
pub const MAX_FETCH_CELLS: u64 = 250_000;
/// Replies stop adding rows beyond this size (estimated JSON bytes); at least one row is sent.
pub const MAX_REPLY_BYTES: usize = 8 << 20;
/// Most rows a query preview returns.
pub const MAX_QUERY_ROWS: usize = 10_000;
/// Most rows inserted by one request.
pub const MAX_INSERT_ROWS: u64 = 1_000_000;
/// Most cells in one table edit or paste (sheets use the same bound).
pub const MAX_EDIT_CELLS: usize = crate::sheets::MAX_BULK_CELLS;
/// Rows a demo table may have.
pub const MAX_DEMO_ROWS: u64 = 1_000_000_000;
/// Most rows of grouped lookup results (one per distinct key of a looked-up table) kept in
/// the in-memory scratch database, across all of a workbook's lookups. Least recently used
/// lookups are dropped first; a lookup larger than this is evaluated uncached.
pub const LOOKUP_CACHE_ROWS: u64 = 5_000_000;

/// Rows a fetch of `columns` columns may return: at most `requested`, [`MAX_FETCH_ROWS`]
/// and the cell budget.
pub fn fetch_rows(requested: u64, columns: usize) -> u64 {
    requested.min(MAX_FETCH_ROWS).min(MAX_FETCH_CELLS / columns.max(1) as u64).max(requested.min(1))
}

/// Bytes `v` takes in JSON, without allocating the text (strings counted as UTF-8 plus quotes;
/// escapes are not counted, so this is a lower bound for strings with control characters).
pub fn json_size(v: &Value) -> usize {
    match v {
        Value::Null => 4,
        Value::Bool(b) => if *b { 4 } else { 5 },
        Value::Number(n) => n.to_string().len(),
        Value::String(s) => s.len() + 2,
        Value::Array(a) => a.iter().map(json_size).sum::<usize>() + a.len() + 1,
        Value::Object(o) => o.iter().map(|(k, v)| k.len() + 3 + json_size(v)).sum::<usize>() + o.len() + 1,
    }
}

/// Cells one sheet evaluation may read before it stops (SEC.07). Formulas over whole
/// columns read only the used area; this bounds a formula over a huge declared range,
/// or a sheet whose used area was stretched by a value in a far corner.
pub const SHEET_EVALUATION_CELLS: u64 = 20_000_000;

/// Bytes the parts of an imported .xlsx may expand to (SEC.08)…
pub const XLSX_MAX_EXPANDED_BYTES: u64 = 256 << 20;
/// …and at most this many times the file's size (zip bombs), but never less than
/// [`XLSX_MIN_ALLOWANCE`].
pub const XLSX_MAX_RATIO: u64 = 100;
pub const XLSX_MIN_ALLOWANCE: u64 = 16 << 20;
/// Cells an .xlsx import may bring into sheets.
pub const XLSX_MAX_CELLS: usize = 2_000_000;

/// Sheet undo history: at most this many steps, and this many cell changes in all; the
/// oldest are forgotten first (SEC.12).
pub const SHEET_UNDO_STEPS: usize = 200;
pub const SHEET_UNDO_ENTRIES: usize = 500_000;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fetch_rows_is_clamped_by_rows_and_cells() {
        assert_eq!(fetch_rows(u64::MAX, 1), MAX_FETCH_ROWS);
        assert_eq!(fetch_rows(100, 10), 100);
        assert_eq!(fetch_rows(5_000, 500), 500);
        assert_eq!(fetch_rows(5_000, 1_000_000), 1, "one row even for absurdly wide views");
        assert_eq!(fetch_rows(0, 10), 0);
        assert_eq!(fetch_rows(10, 0), 10);
    }

    #[test]
    fn json_size_matches_serialization_for_plain_values() {
        for v in [json!(null), json!(true), json!(false), json!(12345), json!(-1.5), json!("héllo"), json!([1, "a", null]), json!({"k": [1, 2]})] {
            assert_eq!(json_size(&v), serde_json::to_string(&v).unwrap().len(), "{v}");
        }
    }
}

