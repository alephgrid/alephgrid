//! Functions computed outside the engine, by isolated workers (phase 8,
//! docs/PYTHON_FUNCTIONS.md).
//!
//! Neither the sheet engine nor DuckDB ever runs such a function. Both read a result cache
//! kept by *(function, version, exact arguments)*: sheet cells through IronCalc's external
//! function hook (Alephgrid patch 2), table columns through a DuckDB macro that looks the
//! result up in `_rf_fn_results`. A missing result is a *pending call*; the server computes
//! pending calls in batches, in a worker, outside any lock, and stores them with
//! [`crate::Workbook::store_function_results`], which re-evaluates sheets.
//!
//! This module holds the values exchanged with workers, the cache, the hook, and the
//! [`FunctionRuntime`] contract the server's workers implement.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{bail, Result};
use duckdb::types::Value as DbValue;
use duckdb::Connection;
use ironcalc::base::expressions::token::Error as CellError;
use ironcalc::base::external::{ExternalFunctions, ExternalResult, ExternalValue};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::sql::{ident, lit};

/// At most this many calls, and about this many bytes of arguments, go to a worker at once.
pub const BATCH_CALLS: usize = 1000;
pub const BATCH_BYTES: usize = 4 << 20;
/// Pending sheet calls kept between two jobs; beyond it, cells wait for the next round.
const MAX_PENDING: usize = 100_000;
/// Longest text a function may return.
pub const MAX_TEXT: usize = 32_767;

/// A value passed to a worker (docs/PYTHON_FUNCTIONS.md, "Values in and out").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum WorkerValue {
    /// A spreadsheet number or a `DOUBLE`.
    Number(f64),
    /// An integer column's value, exact, as decimal digits.
    Integer(String),
    /// A `DECIMAL`, exact, as text.
    Decimal(String),
    Text(String),
    Boolean(bool),
    /// A `DATE`, `YYYY-MM-DD`.
    Date(String),
    Null,
    /// A range or array, by rows (sheets only).
    Array(Vec<Vec<WorkerValue>>),
}

/// What one call produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CallResult {
    Number(f64),
    Text(String),
    Boolean(bool),
    Empty,
    /// `code` is a spreadsheet error (`#VALUE!`, `#NUM!`, `#CALC!`…). A transient error (the
    /// worker ran out of time or memory, or stopped) is not kept for good: recalculating
    /// tries again.
    Error {
        code: String,
        message: String,
        #[serde(default)]
        transient: bool,
    },
}

impl CallResult {
    pub fn error(code: &str, message: impl Into<String>, transient: bool) -> CallResult {
        CallResult::Error { code: code.to_string(), message: message.into(), transient }
    }

    pub fn is_transient(&self) -> bool {
        matches!(self, CallResult::Error { transient: true, .. })
    }

    /// The result as a test expects it written: numbers as the shortest text that reads
    /// back the same, logicals `TRUE`/`FALSE`, errors by code, empty as "".
    pub fn display(&self) -> String {
        match self {
            CallResult::Number(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
            CallResult::Text(s) => s.clone(),
            CallResult::Boolean(b) => if *b { "TRUE" } else { "FALSE" }.into(),
            CallResult::Empty => String::new(),
            CallResult::Error { code, .. } => code.clone(),
        }
    }
}

/// What a worker function returns, declared with its definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ResultType {
    Number,
    Text,
    Boolean,
}

impl ResultType {
    fn sql_column(self) -> &'static str {
        match self {
            ResultType::Number => "num",
            ResultType::Text => "txt",
            ResultType::Boolean => "bool",
        }
    }
}

/// Where the calls of a job come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CallContext {
    Sheet,
    Table,
}

/// One batch of calls of one function version, for a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerJob {
    pub function: String,
    pub version: u32,
    /// Source defining `entry`.
    pub source: String,
    pub entry: String,
    pub arguments: Vec<String>,
    pub returns: ResultType,
    pub context: CallContext,
    pub calls: Vec<Vec<WorkerValue>>,
}

/// Why a whole batch produced no results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, thiserror::Error)]
#[error("{message}")]
pub struct WorkerFailure {
    pub kind: WorkerFailureKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum WorkerFailureKind {
    /// No runtime, no sandbox, or Python turned off: nothing ran.
    Unavailable,
    Timeout,
    Memory,
    /// The worker wrote more than its output limit.
    Output,
    /// The worker stopped (a crash, a signal) or broke the protocol.
    Crashed,
    Cancelled,
}

/// What a runtime is: kept with each function version published with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RuntimeIdentity {
    /// `python`.
    pub kind: String,
    /// Content hash of its manifest (interpreter version, packages and their hashes).
    pub id: String,
    /// The interpreter's version.
    pub version: String,
    /// Packages and versions, `name==version`.
    pub packages: Vec<String>,
}

/// Runs batches of calls in isolated workers (the server's Python runtime, 8.13). Blocking;
/// `cancel` set stops the worker.
pub trait FunctionRuntime: Send + Sync {
    fn identity(&self) -> RuntimeIdentity;
    /// One result per call, in order, or why there are none.
    fn run(&self, job: &WorkerJob, cancel: &AtomicBool) -> std::result::Result<Vec<CallResult>, WorkerFailure>;
}

// ------------------------------------------------------------------ cache

/// A function the hook evaluates: its pinned version and how calls are keyed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActiveFunction {
    pub version: u32,
    pub returns: ResultType,
    pub volatile: bool,
    pub arity: usize,
    /// Why new calls cannot be computed here (another runtime, Python off), if so.
    pub unavailable: Option<String>,
}

/// A call to compute.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingCall {
    pub function: String,
    pub version: u32,
    pub key: String,
    pub args: Vec<WorkerValue>,
}

/// Results of worker functions by (function, version, key), shared by the sheet hook (on
/// the writer and readers) and the server's jobs.
#[derive(Default)]
pub struct ResultCache {
    pub(crate) active: RwLock<HashMap<String, ActiveFunction>>,
    results: RwLock<HashMap<(String, u32, String), CallResult>>,
    pending: Mutex<BTreeMap<(String, u32, String), Vec<WorkerValue>>>,
    /// Bumped by `refresh_functions`: part of the key of volatile functions' calls.
    pub(crate) epoch: std::sync::atomic::AtomicU64,
    /// The runtime the server runs worker functions in, if any.
    pub(crate) runtime: RwLock<Option<RuntimeIdentity>>,
    /// Why worker functions may not run for this workbook (turned off), if so.
    pub(crate) disabled: RwLock<Option<String>>,
    pub(crate) runtime_impl: RwLock<Option<Arc<dyn FunctionRuntime>>>,
}

impl ResultCache {
    pub fn get(&self, function: &str, version: u32, key: &str) -> Option<CallResult> {
        self.results.read().unwrap().get(&(function.to_string(), version, key.to_string())).cloned()
    }

    pub(crate) fn put(&self, function: &str, version: u32, key: &str, result: CallResult) {
        self.results.write().unwrap().insert((function.to_string(), version, key.to_string()), result);
    }

    pub(crate) fn clear(&self) {
        self.results.write().unwrap().clear();
        self.pending.lock().unwrap().clear();
    }

    /// The runtime calls run in, unless worker functions are turned off here.
    pub fn runtime_impl(&self) -> Option<Arc<dyn FunctionRuntime>> {
        if self.is_disabled() {
            return None;
        }
        self.runtime_impl.read().unwrap().clone()
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled.read().unwrap().is_some()
    }

    /// Take the sheet calls evaluation found missing.
    pub fn take_pending(&self) -> Vec<PendingCall> {
        let mut p = self.pending.lock().unwrap();
        std::mem::take(&mut *p).into_iter().map(|((function, version, key), args)| PendingCall { function, version, key, args }).collect()
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.lock().unwrap().is_empty()
    }

    /// Names of the worker functions pinned now.
    pub fn worker_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.active.read().unwrap().keys().cloned().collect();
        v.sort();
        v
    }

    /// Whether `function` is evaluated by workers (the hook claims it).
    pub fn is_worker_function(&self, function: &str) -> bool {
        self.active.read().unwrap().contains_key(&function.to_uppercase())
    }

    fn key_suffix(&self, f: &ActiveFunction) -> String {
        if f.volatile {
            format!("#{}", self.epoch.load(std::sync::atomic::Ordering::SeqCst))
        } else {
            String::new()
        }
    }
}

/// The canonical key of a sheet call's arguments.
pub fn sheet_key(args: &[WorkerValue]) -> String {
    serde_json::to_string(args).expect("worker values serialize")
}

fn cell_error(code: &str) -> CellError {
    match code {
        "#DIV/0!" => CellError::DIV,
        "#N/A" => CellError::NA,
        "#NAME?" => CellError::NAME,
        "#NUM!" => CellError::NUM,
        "#REF!" => CellError::REF,
        "#NULL!" => CellError::NULL,
        "#CALC!" => CellError::CALC,
        "#SPILL!" => CellError::SPILL,
        _ => CellError::VALUE,
    }
}

fn to_worker(v: &ExternalValue) -> std::result::Result<WorkerValue, CellError> {
    Ok(match v {
        ExternalValue::Number(n) => WorkerValue::Number(*n),
        ExternalValue::Text(s) => WorkerValue::Text(s.clone()),
        ExternalValue::Boolean(b) => WorkerValue::Boolean(*b),
        ExternalValue::Empty => WorkerValue::Null,
        ExternalValue::Error(e) => return Err(e.clone()),
        ExternalValue::Array(rows) => WorkerValue::Array(
            rows.iter()
                .map(|r| r.iter().map(|v| match v {
                    // An error inside a range is passed on as its code, not raised: the
                    // function decides (it is the only way to see errors of a range).
                    ExternalValue::Error(e) => Ok(WorkerValue::Text(e.to_string())),
                    other => to_worker(other),
                }).collect::<std::result::Result<Vec<_>, _>>())
                .collect::<std::result::Result<Vec<_>, _>>()?,
        ),
    })
}

/// The hook IronCalc calls for worker functions in sheet cells.
pub(crate) struct SheetHook(pub Arc<ResultCache>);

impl ExternalFunctions for SheetHook {
    fn has(&self, name: &str) -> bool {
        self.0.active.read().unwrap().contains_key(name)
    }

    fn call(&self, name: &str, args: &[ExternalValue]) -> ExternalResult {
        let Some(f) = self.0.active.read().unwrap().get(name).cloned() else {
            return ExternalResult::Error { error: CellError::NAME, message: format!("Invalid function: {name}") };
        };
        if args.len() != f.arity {
            return ExternalResult::Error { error: CellError::ERROR, message: format!("{name} takes {} argument(s)", f.arity) };
        }
        let mut values = Vec::with_capacity(args.len());
        for a in args {
            match to_worker(a) {
                Ok(v) => values.push(v),
                // An error argument is the result, without calling.
                Err(e) => return ExternalResult::Value(ExternalValue::Error(e)),
            }
        }
        let key = sheet_key(&values) + &self.0.key_suffix(&f);
        if let Some(r) = self.0.get(name, f.version, &key) {
            return match r {
                CallResult::Number(n) => ExternalResult::Value(ExternalValue::Number(n)),
                CallResult::Text(s) => ExternalResult::Value(ExternalValue::Text(s)),
                CallResult::Boolean(b) => ExternalResult::Value(ExternalValue::Boolean(b)),
                CallResult::Empty => ExternalResult::Value(ExternalValue::Empty),
                CallResult::Error { code, message, .. } => ExternalResult::Error { error: cell_error(&code), message },
            };
        }
        if let Some(why) = f.unavailable {
            return ExternalResult::Error { error: CellError::NAME, message: why };
        }
        let mut pending = self.0.pending.lock().unwrap();
        if pending.len() < MAX_PENDING {
            pending.entry((name.to_string(), f.version, key)).or_insert(values);
        }
        ExternalResult::Error { error: CellError::CALC, message: format!("Calculating {name}…") }
    }
}

// --------------------------------------------------------------- storage

/// Added in format 9: results of worker functions (see this module).
pub(crate) const V9_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS _rf_fn_results (
        name VARCHAR NOT NULL, version INTEGER NOT NULL, key VARCHAR NOT NULL, kind VARCHAR NOT NULL,
        num DOUBLE, txt VARCHAR, bool BOOLEAN, transient BOOLEAN NOT NULL DEFAULT false, computed_at BIGINT NOT NULL,
        PRIMARY KEY (name, version, key)
    );";

fn result_row(r: &CallResult) -> (&'static str, Option<f64>, Option<String>, Option<bool>, bool) {
    match r {
        CallResult::Number(n) => ("number", Some(*n), None, None, false),
        CallResult::Text(s) => ("text", None, Some(s.clone()), None, false),
        CallResult::Boolean(b) => ("boolean", None, None, Some(*b), false),
        CallResult::Empty => ("empty", None, Some(String::new()), None, false),
        CallResult::Error { code, message, transient } => ("error", None, Some(format!("{code}\u{1f}{message}")), None, *transient),
    }
}

fn row_result(kind: &str, num: Option<f64>, txt: Option<String>, b: Option<bool>, transient: bool) -> CallResult {
    match kind {
        "number" => CallResult::Number(num.unwrap_or(0.0)),
        "text" => CallResult::Text(txt.unwrap_or_default()),
        "boolean" => CallResult::Boolean(b.unwrap_or(false)),
        "empty" => CallResult::Empty,
        _ => {
            let t = txt.unwrap_or_default();
            let (code, message) = t.split_once('\u{1f}').unwrap_or(("#VALUE!", ""));
            CallResult::Error { code: code.to_string(), message: message.to_string(), transient }
        }
    }
}

/// Store results (an existing key keeps its first result: the same call delivered twice
/// writes once). A transient error never replaces anything and is replaced by a real result.
pub(crate) fn store(conn: &Connection, function: &str, version: u32, results: &[(String, CallResult)]) -> Result<usize> {
    let now = crate::storage::now_ms() as i64;
    let mut written = 0;
    let mut replace = conn.prepare(
        "INSERT INTO _rf_fn_results VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (name, version, key) DO UPDATE SET kind = excluded.kind, num = excluded.num, txt = excluded.txt, bool = excluded.bool,
             transient = excluded.transient, computed_at = excluded.computed_at
         WHERE _rf_fn_results.transient AND NOT excluded.transient",
    )?;
    let mut keep = conn.prepare("INSERT INTO _rf_fn_results VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING")?;
    for (key, r) in results {
        let (kind, num, txt, b, transient) = result_row(r);
        let params = duckdb::params![function, version, key, kind, num, txt, b, transient, now];
        written += if transient { keep.execute(params)? } else { replace.execute(params)? };
    }
    Ok(written)
}

/// Load the stored results of `versions` into `cache`.
pub(crate) fn load(conn: &Connection, cache: &ResultCache, versions: &[(String, u32)]) -> Result<()> {
    let mut stmt = conn.prepare("SELECT key, kind, num, txt, bool, transient FROM _rf_fn_results WHERE name = ? AND version = ?")?;
    let mut all = cache.results.write().unwrap();
    for (name, version) in versions {
        let rows = stmt.query_map(duckdb::params![name, version], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<f64>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<bool>>(4)?, r.get::<_, bool>(5)?))
        })?;
        for row in rows {
            let (key, kind, num, txt, b, transient) = row?;
            all.insert((name.clone(), *version, key), row_result(&kind, num, txt, b, transient));
        }
    }
    Ok(())
}

// ----------------------------------------------------------------- tables

/// The SQL key of a table call: type and text of each argument, so `1` (integer) and `1.0`
/// (double) are different calls. Prefixed `t`, never equal to a sheet key (JSON).
fn sql_key(args: &[String], suffix: &str) -> String {
    let parts: Vec<String> = args
        .iter()
        .map(|a| format!("CASE WHEN ({a}) IS NULL THEN 'N' ELSE typeof({a}) || ':' || CAST(({a}) AS VARCHAR) END"))
        .collect();
    let joined = if parts.is_empty() { "''".to_string() } else { format!("concat_ws(chr(31), {})", parts.join(", ")) };
    format!("('t' || {joined} || {})", lit(suffix))
}

/// The macro a table column calls: a lookup of the result (NULL while pending or for an
/// error).
pub(crate) fn lookup_macro(name: &str, version: u32, arguments: &[String], returns: ResultType, suffix: &str) -> String {
    let params: Vec<String> = arguments.iter().map(|a| ident(a)).collect();
    format!(
        "CREATE OR REPLACE MACRO {}({}) AS ((SELECT r.{} FROM _rf_fn_results r WHERE r.name = {} AND r.version = {version} AND r.key = {} AND r.kind = {}));",
        ident(name),
        params.join(","),
        returns.sql_column(),
        lit(name),
        sql_key(&params, suffix),
        lit(match returns {
            ResultType::Number => "number",
            ResultType::Text => "text",
            ResultType::Boolean => "boolean",
        }),
    )
}

/// A table column whose whole formula is one call of a worker function.
#[derive(Debug, Clone)]
pub struct ColumnCall {
    pub table: String,
    pub column: String,
    pub function: String,
    /// The arguments as SQL expressions over the table's columns.
    pub args: Vec<String>,
}

/// `NAME(arg, …)` of a calculated column's formula (SQL language), if it is one call of a
/// worker function. Splits at top-level commas, minding brackets, parentheses and quotes.
pub(crate) fn parse_column_call(formula: &str, is_worker: impl Fn(&str) -> bool) -> Option<(String, Vec<String>)> {
    let f = formula.trim().trim_start_matches('=').trim();
    let open = f.find('(')?;
    let name = f[..open].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || !is_worker(&name.to_uppercase()) {
        return None;
    }
    if !f.ends_with(')') {
        return None;
    }
    let inner = &f[open + 1..f.len() - 1];
    let (mut depth, mut quote, mut bracket) = (0i32, None::<char>, false);
    let mut args = Vec::new();
    let mut cur = String::new();
    for c in inner.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '[') => bracket = true,
            (None, ']') => bracket = false,
            (None, '(') if !bracket => depth += 1,
            (None, ')') if !bracket => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            (None, ',') if depth == 0 && !bracket => {
                args.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if depth != 0 || quote.is_some() {
        return None;
    }
    if !cur.trim().is_empty() || !args.is_empty() {
        args.push(cur);
    }
    Some((name.to_uppercase(), args.into_iter().map(|a| crate::sql::formula_to_sql(a.trim())).collect()))
}

/// Up to `limit` distinct argument tuples of `call` over `base` (the table's select) not yet
/// in the cache, with their keys.
pub(crate) fn missing_table_calls(conn: &Connection, base: &str, call: &ColumnCall, version: u32, suffix: &str, limit: usize) -> Result<Vec<PendingCall>> {
    let args: Vec<String> = call.args.iter().enumerate().map(|(i, a)| format!("({a}) AS a{i}")).collect();
    let key = sql_key(&call.args, suffix);
    let sql = format!(
        "SELECT s.* FROM (SELECT DISTINCT {key} AS k{}{} FROM ({base}) b) s
         WHERE NOT EXISTS (SELECT 1 FROM _rf_fn_results r WHERE r.name = {} AND r.version = {version} AND r.key = s.k)
         LIMIT {limit}",
        if args.is_empty() { "" } else { ", " },
        args.join(", "),
        lit(&call.function),
    );
    let mut stmt = conn.prepare(&sql)?;
    let n = call.args.len();
    let rows = stmt.query_map([], |r| {
        let key: String = r.get(0)?;
        let mut values = Vec::with_capacity(n);
        for i in 0..n {
            values.push(db_value(r.get::<_, DbValue>(i + 1)?));
        }
        Ok((key, values))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (key, args) = row?;
        out.push(PendingCall { function: call.function.clone(), version, key, args });
    }
    Ok(out)
}

fn db_value(v: DbValue) -> WorkerValue {
    match v {
        DbValue::Null => WorkerValue::Null,
        DbValue::Boolean(b) => WorkerValue::Boolean(b),
        DbValue::TinyInt(i) => WorkerValue::Integer(i.to_string()),
        DbValue::SmallInt(i) => WorkerValue::Integer(i.to_string()),
        DbValue::Int(i) => WorkerValue::Integer(i.to_string()),
        DbValue::BigInt(i) => WorkerValue::Integer(i.to_string()),
        DbValue::HugeInt(i) => WorkerValue::Integer(i.to_string()),
        DbValue::UTinyInt(i) => WorkerValue::Integer(i.to_string()),
        DbValue::USmallInt(i) => WorkerValue::Integer(i.to_string()),
        DbValue::UInt(i) => WorkerValue::Integer(i.to_string()),
        DbValue::UBigInt(i) => WorkerValue::Integer(i.to_string()),
        DbValue::Float(f) => WorkerValue::Number(f as f64),
        DbValue::Double(f) => WorkerValue::Number(f),
        DbValue::Decimal(d) => WorkerValue::Decimal(d.to_string()),
        DbValue::Text(s) => WorkerValue::Text(s),
        DbValue::Date32(days) => WorkerValue::Date(civil_date(days as i64)),
        other => WorkerValue::Text(format!("{other:?}")),
    }
}

/// `YYYY-MM-DD` of a day number since 1970-01-01 (proleptic Gregorian).
fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Check a worker's results against the declared result type and the context, as the
/// contract says (`#NUM!` for numbers a sheet cannot hold, `#VALUE!` for the wrong type).
pub fn check_result(r: CallResult, returns: ResultType, context: CallContext) -> CallResult {
    match (&r, returns) {
        (CallResult::Error { .. } | CallResult::Empty, _) => r,
        (CallResult::Number(n), ResultType::Number) => {
            if !n.is_finite() {
                CallResult::error("#NUM!", "the function returned a number that is not finite", false)
            } else {
                r
            }
        }
        (CallResult::Text(s), ResultType::Text) => {
            if s.chars().count() > MAX_TEXT {
                CallResult::error("#VALUE!", format!("the function returned more than {MAX_TEXT} characters"), false)
            } else {
                r
            }
        }
        (CallResult::Boolean(_), ResultType::Boolean) => r,
        (other, expected) => {
            let got = match other {
                CallResult::Number(_) => "a number",
                CallResult::Text(_) => "text",
                CallResult::Boolean(_) => "a logical",
                _ => "a value",
            };
            let _ = context;
            CallResult::error("#VALUE!", format!("the function returned {got}, expected {}", match expected {
                ResultType::Number => "a number",
                ResultType::Text => "text",
                ResultType::Boolean => "a logical",
            }), false)
        }
    }
}

/// Parse a test argument written like a cell input: a number, `TRUE`/`FALSE`, empty, or text.
pub fn test_value(text: &str) -> WorkerValue {
    let t = text.trim();
    if t.is_empty() {
        WorkerValue::Null
    } else if t.eq_ignore_ascii_case("TRUE") {
        WorkerValue::Boolean(true)
    } else if t.eq_ignore_ascii_case("FALSE") {
        WorkerValue::Boolean(false)
    } else if let Ok(n) = t.parse::<f64>() {
        WorkerValue::Number(n)
    } else {
        WorkerValue::Text(text.to_string())
    }
}

/// Whether a SQL formula calls a worker function anywhere (a name followed by `(`).
pub(crate) fn mentions_worker(formula: &str, cache: &ResultCache) -> Option<String> {
    let active = cache.active.read().unwrap();
    let upper = formula.to_uppercase();
    active.keys().find(|name| {
        upper.match_indices(name.as_str()).any(|(i, _)| {
            let before = upper[..i].chars().next_back();
            let after = upper[i + name.len()..].trim_start().chars().next();
            !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') && after == Some('(')
        })
    }).cloned()
}

/// A Python identifier that is not a keyword.
pub(crate) fn python_name_ok(name: &str) -> Result<()> {
    const KEYWORDS: &[&str] = &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except",
        "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try",
        "while", "with", "yield", "match", "case",
    ];
    if KEYWORDS.contains(&name) || KEYWORDS.contains(&name.to_lowercase().as_str()) {
        bail!("{name} is a Python keyword; choose another name");
    }
    Ok(())
}
