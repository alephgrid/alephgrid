//! Workbook-scoped immutable function versions. Engines evaluate expressions; this
//! module validates definitions and coordinates pins, persistence and recalculation.
use crate::{
    formula,
    sql::{ident, lit},
    storage, Workbook,
};
use anyhow::{anyhow, bail, Result};
use duckdb::Connection;
use ironcalc::base::expressions::{
    parser::{new_parser_english, Node},
    types::CellReferenceRC,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use ts_rs::TS;

pub const ADAPTER: &str = "rowfinity-functions-v1/ironcalc-0.9.1";
const KEY: &str = "function_registry";
const MAX_REGISTRY_BYTES: usize = 4 << 20;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FunctionKind {
    Formula,
    Sql,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct FunctionTest {
    pub arguments: Vec<String>,
    pub expected: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct FunctionDefinition {
    pub name: String,
    pub kind: FunctionKind,
    pub arguments: Vec<String>,
    pub source: String,
    pub documentation: String,
    pub tests: Vec<FunctionTest>,
    // Explicit dependency versions; omitted entries capture the current workbook pin.
    #[serde(default)]
    pub dependencies: BTreeMap<String, u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct FunctionVersion {
    pub version: u32,
    pub definition: FunctionDefinition,
    pub adapter: String,
    pub duckdb_version: String,
    pub dependencies: BTreeMap<String, u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct WorkbookFunction {
    pub name: String,
    pub pinned: u32,
    pub versions: Vec<FunctionVersion>,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct FunctionTestResult {
    pub actual: String,
    pub expected: String,
    pub passed: bool,
}

pub(crate) fn list(conn: &Connection) -> Result<Vec<WorkbookFunction>> {
    Ok(serde_json::from_str(&storage::meta_get(conn, KEY)?.unwrap_or_else(|| "[]".into()))?)
}
fn save_registry(conn: &Connection, items: &[WorkbookFunction]) -> Result<()> {
    let json = serde_json::to_string(items)?;
    if json.len() > MAX_REGISTRY_BYTES {
        bail!("workbook function registry exceeds 4 MiB; reduce documentation or test data");
    }
    storage::meta_set(conn, KEY, &json)
}
fn engine_version(conn: &Connection) -> Result<String> {
    Ok(conn.query_row("SELECT version()", [], |r| r.get(0))?)
}
fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !name.to_ascii_lowercase().starts_with("rf_")
}
fn active(items: &[WorkbookFunction]) -> Result<Vec<FunctionVersion>> {
    items
        .iter()
        .map(|f| f.versions.iter().find(|v| v.version == f.pinned).cloned().ok_or_else(|| anyhow!("missing pinned version of {}", f.name)))
        .collect()
}
fn validate(conn: &Connection, d: &FunctionDefinition, items: &[WorkbookFunction]) -> Result<BTreeMap<String, u32>> {
    if !name_ok(&d.name)
        || d.arguments.is_empty()
        || d.arguments.len() > 16
        || d.source.len() > 4096
        || d.documentation.len() > 8192
        || d.tests.len() > 32
    {
        bail!("invalid function name or size (1–16 arguments, source ≤4096 bytes, ≤32 tests)");
    }
    let mut args = BTreeSet::new();
    for a in &d.arguments {
        if !name_ok(a) || !args.insert(a.to_uppercase()) {
            bail!("invalid or duplicate argument {a}");
        }
    }
    let n = d.name.to_uppercase();
    let mut name_parser = new_parser_english(vec!["Sheet1".into()], vec![], Default::default());
    if !matches!(
        name_parser.parse(&format!("{n}(0)"), &CellReferenceRC { sheet: "Sheet1".into(), row: 1, column: 1 }),
        Node::NamedFunctionKind { .. }
    ) {
        bail!("function name conflicts with a built-in spreadsheet function");
    }
    let builtin: i64 =
        conn.query_row("SELECT count(*) FROM duckdb_functions() WHERE lower(function_name)=lower(?) AND internal", [&n], |r| r.get(0))?;
    if builtin > 0 {
        bail!("function name conflicts with a built-in SQL function");
    }
    let mut calls = BTreeSet::new();
    match d.kind {
        FunctionKind::Formula => {
            let mut parser = new_parser_english(vec!["Sheet1".into()], vec![], Default::default());
            let node = parser.parse(&lambda(d), &CellReferenceRC { sheet: "Sheet1".into(), row: 1, column: 1 });
            if let Node::LambdaDefKind { body, .. } = node {
                check_node(&body, &mut calls, &args)?;
            } else {
                bail!("invalid formula function signature");
            }
        }
        FunctionKind::Sql => {
            let refs = formula::analyze(conn, &d.source)?;
            if refs.iter().any(|r| r.qualified || !d.arguments.iter().any(|a| a.eq_ignore_ascii_case(&r.name))) {
                bail!("SQL function may reference only its arguments");
            }
            let tree = crate::sqlpolicy::parse(conn, &format!("SELECT ({})", crate::sql::formula_to_sql(&d.source)))?;
            sql_calls(&tree, &mut calls)?;
        }
    }
    let mut deps = BTreeMap::new();
    for call in calls {
        if call == n {
            bail!("recursive functions are not supported");
        }
        if let Some(f) = items.iter().find(|f| f.name == call) {
            let version = d.dependencies.get(&call).copied().unwrap_or(f.pinned);
            let v = f
                .versions
                .iter()
                .find(|v| v.version == version)
                .ok_or_else(|| anyhow!("missing function dependency version: {call} v{version}"))?;
            if v.definition.kind != d.kind {
                bail!("function dependency uses a different engine: {call}");
            }
            deps.insert(call, version);
        } else if d.kind == FunctionKind::Formula {
            bail!("unknown named function {call}");
        } else {
            let scalar: i64 = conn.query_row(
                "SELECT count(*) FROM duckdb_functions() WHERE upper(function_name)=? AND function_type='scalar' AND NOT has_side_effects",
                [&call],
                |r| r.get(0),
            )?;
            if scalar == 0
                || [
                    "NOW",
                    "TODAY",
                    "CURRENT_DATE",
                    "CURRENT_TIME",
                    "CURRENT_TIMESTAMP",
                    "TRANSACTION_TIMESTAMP",
                    "GET_CURRENT_TIMESTAMP",
                    "CURRENT_LOCALTIMESTAMP",
                    "CURRENT_LOCALTIME",
                ]
                .contains(&call.as_str())
            {
                bail!("unsupported SQL scalar function {call}");
            }
        }
    }
    if d.dependencies.keys().any(|n| !deps.contains_key(n)) {
        bail!("dependency overrides must name a called workbook function in uppercase");
    }
    Ok(deps)
}
// Inspect the actual IronCalc AST. Reject external cell/name captures and arbitrary
// lambda invocation; ranges remain valid call arguments in ordinary sheet cells.
fn check_node(n: &Node, calls: &mut BTreeSet<String>, declared: &BTreeSet<String>) -> Result<()> {
    use Node::*;
    match n {
        FunctionKind { kind, args } => {
            let name = kind.to_xlsx_string();
            if ![
                "IF",
                "IFERROR",
                "IFNA",
                "SUM",
                "AVERAGE",
                "MIN",
                "MAX",
                "COUNT",
                "COUNTA",
                "ABS",
                "ROUND",
                "ROUNDUP",
                "ROUNDDOWN",
                "AND",
                "OR",
                "NOT",
                "ISBLANK",
                "ISNUMBER",
                "ISTEXT",
                "LEN",
                "LOWER",
                "UPPER",
                "TRIM",
                "LEFT",
                "RIGHT",
                "MID",
                "DATE",
                "YEAR",
                "MONTH",
                "DAY",
                "SQRT",
            ]
            .contains(&name.as_str())
            {
                bail!("{name} is not supported in bounded formula functions yet");
            }
            for a in args {
                check_node(a, calls, declared)?;
            }
        }
        NamedFunctionKind { name, args, .. } => {
            calls.insert(name.to_uppercase());
            for a in args {
                check_node(a, calls, declared)?;
            }
        }
        OpSumKind { left, right, .. }
        | OpProductKind { left, right, .. }
        | OpPowerKind { left, right }
        | OpConcatenateKind { left, right }
        | CompareKind { left, right, .. } => {
            check_node(left, calls, declared)?;
            check_node(right, calls, declared)?;
        }
        UnaryKind { right, .. } => check_node(right, calls, declared)?,
        NamedVariableKind { name, .. } if declared.contains(&name.to_uppercase()) => {}
        BooleanKind(_) | NumberKind(_) | StringKind(_) | ErrorKind(_) | EmptyArgKind | ArrayKind(_) => {}
        ParseErrorKind { message, .. } => bail!("formula error: {message}"),
        _ => bail!("function bodies must use declared arguments, not cell references, external names or dynamic lambda calls"),
    }
    Ok(())
}
fn sql_calls(v: &serde_json::Value, calls: &mut BTreeSet<String>) -> Result<()> {
    match v {
        serde_json::Value::Object(o) => {
            if o.get("class").and_then(|v| v.as_str()).is_some_and(|c| ["PARAMETER", "WINDOW", "LAMBDA"].contains(&c)) {
                bail!("SQL functions require scalar expressions with declared arguments");
            }
            if let Some(n) = o.get("function_name").and_then(|x| x.as_str()) {
                calls.insert(n.to_uppercase());
            }
            for c in o.values() {
                sql_calls(c, calls)?;
            }
        }
        serde_json::Value::Array(a) => {
            for c in a {
                sql_calls(c, calls)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn lambda(d: &FunctionDefinition) -> String {
    format!("LAMBDA({},{})", d.arguments.join(","), d.source.trim().trim_start_matches('='))
}
pub(crate) fn canonical_lambda(d: &FunctionDefinition) -> String {
    let mut parser = new_parser_english(vec!["Sheet1".into()], vec![], Default::default());
    let context = CellReferenceRC { sheet: "Sheet1".into(), row: 1, column: 1 };
    ironcalc::base::expressions::parser::stringify::to_english_string(&parser.parse(&lambda(d), &context), &context)
}
fn ordered(items: &[WorkbookFunction]) -> Result<Vec<FunctionVersion>> {
    let mut pending = active(items)?;
    let mut done = BTreeMap::new();
    let mut costs = BTreeMap::<String, usize>::new();
    let mut out = vec![];
    while !pending.is_empty() {
        let Some(i) = pending.iter().position(|v| v.dependencies.iter().all(|(n, version)| done.get(n) == Some(version))) else {
            bail!("cyclic or incompatible pinned dependencies; select compatible versions together");
        };
        let v = pending.remove(i);
        let source = v.definition.source.to_uppercase();
        let mut cost = source.len();
        for dependency in v.dependencies.keys() {
            cost = cost.saturating_add(source.matches(dependency).count().saturating_mul(costs[dependency]));
        }
        if cost > 65_536 {
            bail!("expanded function expression is too large");
        }
        costs.insert(v.definition.name.clone(), cost);
        done.insert(v.definition.name.clone(), v.version);
        out.push(v);
    }
    Ok(out)
}
pub(crate) fn check_runtime(conn: &Connection, items: &[WorkbookFunction]) -> Result<()> {
    let engine = engine_version(conn)?;
    for v in ordered(items)? {
        if v.adapter != ADAPTER || v.duckdb_version != engine {
            bail!("unsupported function runtime for {}; restore its pinned engine versions", v.definition.name);
        }
    }
    Ok(())
}
fn macro_sql(d: &FunctionDefinition) -> String {
    format!(
        "CREATE OR REPLACE MACRO {}({}) AS (\n{}\n);",
        ident(&d.name),
        d.arguments.iter().map(|a| ident(a)).collect::<Vec<_>>().join(","),
        crate::sql::formula_to_sql(&d.source)
    )
}
impl Workbook {
    pub fn functions(&self) -> Result<Vec<WorkbookFunction>> {
        list(&self.conn)
    }
    pub fn test_function(&self, definition: &FunctionDefinition) -> Result<Vec<FunctionTestResult>> {
        test_definition(&self.conn, definition)
    }
    pub fn publish_function(&mut self, mut definition: FunctionDefinition) -> Result<WorkbookFunction> {
        definition.name = definition.name.to_uppercase();
        self.mutating("publish_function", None, |wb| {
            let mut items = wb.functions()?;
            if items.len() >= 128 && !items.iter().any(|f| f.name == definition.name) {
                bail!("workbook function limit reached");
            }
            if !items.iter().any(|f| f.name == definition.name) && wb.sheets().has_function_name(&definition.name) {
                bail!("name conflicts with an existing imported sheet name definition");
            }
            let dependencies = validate(&wb.conn, &definition, &items)?;
            if wb.test_function(&definition)?.iter().any(|t| !t.passed) {
                bail!("function tests failed");
            }
            let existing = items.iter().position(|f| f.name == definition.name);
            if let Some(i) = existing {
                if items[i].versions.len() >= 100 {
                    bail!("function version limit reached");
                }
                if items[i].versions[0].definition.kind != definition.kind {
                    bail!("cannot change a function's execution engine");
                }
            }
            let version = existing.map(|i| items[i].versions.len() as u32 + 1).unwrap_or(1);
            let v = FunctionVersion {
                version,
                definition: definition.clone(),
                adapter: ADAPTER.into(),
                duckdb_version: engine_version(&wb.conn)?,
                dependencies,
            };
            let i = if let Some(i) = existing {
                items[i].versions.push(v);
                i
            } else {
                items.push(WorkbookFunction { name: definition.name.clone(), pinned: 1, versions: vec![v] });
                items.len() - 1
            };
            ordered(&items)?;
            save_registry(&wb.conn, &items)?;
            if existing.is_none() {
                wb.apply_functions(&items)?;
            }
            Ok(items[i].clone())
        })
    }
    pub fn pin_functions(&mut self, pins: &BTreeMap<String, u32>) -> Result<Vec<WorkbookFunction>> {
        self.mutating("pin_functions", None, |wb| {
            let mut items = wb.functions()?;
            for (name, version) in pins {
                let f = items.iter_mut().find(|f| f.name == *name).ok_or_else(|| anyhow!("unknown function {name}"))?;
                if !f.versions.iter().any(|v| v.version == *version) {
                    bail!("unknown version {version}");
                }
                f.pinned = *version;
            }
            check_runtime(&wb.conn, &items)?;
            save_registry(&wb.conn, &items)?;
            wb.apply_functions(&items)?;
            Ok(items)
        })
    }
    fn apply_functions(&mut self, items: &[WorkbookFunction]) -> Result<()> {
        let versions = ordered(items)?;
        for v in &versions {
            if v.definition.kind == FunctionKind::Sql {
                self.conn.execute_batch(&macro_sql(&v.definition))?;
            }
        }
        self.with_sheets(|s| {
            for v in &versions {
                if v.definition.kind == FunctionKind::Formula {
                    s.install_function(&v.definition)?;
                }
            }
            Ok(())
        })?;
        // Every calculated projection may depend on a changed SQL function.
        self.conn.execute_batch("UPDATE _rf_tables SET gen=gen+1, data_rev=-1;")?;
        Ok(())
    }
}

pub(crate) fn test_definition(source_conn: &Connection, definition: &FunctionDefinition) -> Result<Vec<FunctionTestResult>> {
    let mut definition = definition.clone();
    definition.name = definition.name.to_uppercase();
    let definition = &definition;
    let items = list(source_conn)?;
    let dependencies = validate(source_conn, definition, &items)?;
    // Load only the candidate's ancestors. Never replace a dependency underneath
    // an existing dependent: that can create an indirect recursive draft.
    let mut required = BTreeMap::new();
    let mut scratch = Vec::new();
    let mut pending: Vec<_> = dependencies.iter().map(|(n, v)| (n.clone(), *v)).collect();
    while let Some((name, version)) = pending.pop() {
        if name == definition.name {
            bail!("recursive function dependency");
        }
        if let Some(previous) = required.insert(name.clone(), version) {
            if previous != version {
                bail!("incompatible dependency versions for {name}");
            }
            continue;
        }
        let f = items.iter().find(|f| f.name == name).ok_or_else(|| anyhow!("missing dependency"))?;
        let v = f.versions.iter().find(|v| v.version == version).ok_or_else(|| anyhow!("missing dependency version"))?;
        pending.extend(v.dependencies.iter().map(|(n, v)| (n.clone(), *v)));
        let mut f = f.clone();
        f.pinned = version;
        scratch.push(f);
    }
    scratch.push(WorkbookFunction {
        name: definition.name.clone(),
        pinned: 1,
        versions: vec![FunctionVersion {
            version: 1,
            definition: definition.clone(),
            adapter: ADAPTER.into(),
            duckdb_version: engine_version(source_conn)?,
            dependencies,
        }],
    });
    check_runtime(source_conn, &scratch)?;
    let versions = ordered(&scratch)?;
    let started = std::time::Instant::now();
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("SET enable_external_access=false; SET memory_limit='64MB'; SET threads=1; SET max_temp_directory_size='0B';")?;
    let interrupt = conn.interrupt_handle();
    let (finished, wait) = std::sync::mpsc::channel::<()>();
    let timer = std::thread::spawn(move || {
        if wait.recv_timeout(std::time::Duration::from_secs(2)) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
            interrupt.interrupt();
        }
    });
    let _deadline = TestDeadline { finished: Some(finished), timer: Some(timer) };
    let mut sheets = crate::sheets::Sheets::from_bytes(None)?;
    sheets.create("Tests")?;
    for v in versions {
        match v.definition.kind {
            FunctionKind::Formula => sheets.install_function(&v.definition)?,
            FunctionKind::Sql => conn.execute_batch(&macro_sql(&v.definition))?,
        }
    }
    let mut results = vec![];
    for t in &definition.tests {
        if started.elapsed() > std::time::Duration::from_secs(2) {
            bail!("function tests exceeded the time limit");
        }
        if t.arguments.len() != definition.arguments.len() || t.arguments.iter().any(|a| a.len() > 4096) || t.expected.len() > 4096 {
            bail!("invalid test arguments or size");
        }
        let actual = match definition.kind {
            FunctionKind::Formula => sheets.test_function_call(definition, &t.arguments)?,
            FunctionKind::Sql => {
                // Test values are strings (or the explicit NULL marker), never SQL fragments.
                let args = t.arguments.iter().map(|a| if a == "NULL" { "NULL".into() } else { lit(a) }).collect::<Vec<_>>().join(",");
                conn.query_row(&format!("SELECT CAST({}({args}) AS VARCHAR)", ident(&definition.name)), [], |r| {
                    r.get::<_, Option<String>>(0)
                })?
                .unwrap_or("NULL".into())
            }
        };
        results.push(FunctionTestResult { passed: actual == t.expected, actual, expected: t.expected.clone() });
    }
    Ok(results)
}

struct TestDeadline {
    finished: Option<std::sync::mpsc::Sender<()>>,
    timer: Option<std::thread::JoinHandle<()>>,
}
impl Drop for TestDeadline {
    fn drop(&mut self) {
        self.finished.take();
        if let Some(t) = self.timer.take() {
            let _ = t.join();
        }
    }
}
