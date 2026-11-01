//! Workbook-scoped immutable function versions. Engines evaluate expressions; this
//! module validates definitions and coordinates pins, persistence and recalculation.
use crate::{
    formula,
    sql::{ident, lit},
    storage, Workbook,
};
use anyhow::{anyhow, bail, Context, Result};
use duckdb::Connection;
use ironcalc::base::expressions::{
    parser::{new_parser_english, Node},
    types::CellReferenceRC,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use ts_rs::TS;

pub const ADAPTER: &str = "alephgrid-functions-v1/ironcalc-0.9.1";
const KEY: &str = "function_registry";
const MAX_REGISTRY_BYTES: usize = 4 << 20;
/// Longest Python source of one function.
const MAX_PYTHON_SOURCE: usize = 65_536;
/// `_rf_meta` key of the volatile functions' refresh epoch.
const EPOCH_KEY: &str = "function_epoch";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FunctionKind {
    Formula,
    Sql,
    /// Python, run by isolated workers (phase 8, docs/PYTHON_FUNCTIONS.md).
    Python,
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
    /// Python: what the function returns (required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub returns: Option<crate::external::ResultType>,
    /// Python: the result may change with the same arguments (clock, randomness); computed
    /// again only on `refresh_functions`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub volatile: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct FunctionVersion {
    pub version: u32,
    pub definition: FunctionDefinition,
    pub adapter: String,
    pub duckdb_version: String,
    pub dependencies: BTreeMap<String, u32>,
    /// Python: the runtime it was published with; calls need exactly this runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime: Option<crate::external::RuntimeIdentity>,
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
    let max_source = if d.kind == FunctionKind::Python { MAX_PYTHON_SOURCE } else { 4096 };
    if !name_ok(&d.name)
        || d.arguments.is_empty()
        || d.arguments.len() > 16
        || d.source.len() > max_source
        || d.documentation.len() > 8192
        || d.tests.len() > 32
    {
        bail!("invalid function name or size (1–16 arguments, source ≤{max_source} bytes, ≤32 tests)");
    }
    if d.kind != FunctionKind::Python && (d.returns.is_some() || d.volatile) {
        bail!("only Python functions declare a result type or volatility");
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
        FunctionKind::Python => {
            if d.returns.is_none() {
                bail!("a Python function declares what it returns: number, text or boolean");
            }
            if !d.dependencies.is_empty() {
                bail!("Python functions cannot call other workbook functions");
            }
            crate::external::python_name_ok(&d.name)?;
            for a in &d.arguments {
                crate::external::python_name_ok(a)?;
            }
            let entry = python_entry(&d.name);
            if !d.source.lines().any(|l| l.trim_start().starts_with(&format!("def {entry}(")) && !l.starts_with(char::is_whitespace)) {
                bail!("the source must define the function at the top level: def {entry}({}):", d.arguments.iter().map(|a| a.to_lowercase()).collect::<Vec<_>>().join(", "));
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

/// The Python function a definition must define: its name in lower case.
pub fn python_entry(name: &str) -> String {
    name.to_lowercase()
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
        if v.definition.kind == FunctionKind::Python {
            // Not expanded into other expressions: no expansion cost.
            done.insert(v.definition.name.clone(), v.version);
            costs.insert(v.definition.name.clone(), 0);
            out.push(v);
            continue;
        }
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
/// The checks a SQL function's definition passes when published, without the registry
/// context (names, arguments, references, the function policy).
fn check_sql_definition(conn: &Connection, d: &FunctionDefinition) -> Result<()> {
    if !name_ok(&d.name) || d.arguments.is_empty() || d.arguments.len() > 16 || d.source.len() > 4096 || !d.arguments.iter().all(|a| name_ok(a)) {
        bail!("stored function {:?} has an invalid definition", d.name);
    }
    let refs = formula::analyze(conn, &d.source).with_context(|| format!("stored function {} does not validate", d.name))?;
    if refs.iter().any(|r| r.qualified || !d.arguments.iter().any(|a| a.eq_ignore_ascii_case(&r.name))) {
        bail!("stored function {} references more than its arguments", d.name);
    }
    let tree = crate::sqlpolicy::parse(conn, &format!("SELECT ({})", crate::sql::formula_to_sql(&d.source)))?;
    sql_calls(&tree, &mut BTreeSet::new())?;
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
        if definition.kind == FunctionKind::Python {
            let runtime = self.function_runtime.clone().ok_or_else(|| anyhow!(NO_RUNTIME))?;
            validate(&self.conn, &upper(definition), &self.functions()?)?;
            return Ok(test_python(definition, runtime.as_ref(), &std::sync::atomic::AtomicBool::new(false))?.results);
        }
        test_definition(&self.conn, definition)
    }
    pub fn publish_function(&mut self, definition: FunctionDefinition) -> Result<WorkbookFunction> {
        self.publish_function_tested(definition, None)
    }
    /// Publish, using `tested` (Python tests run outside the workbook's lock, 8.07) when it
    /// is for exactly this definition and the runtime this workbook has now.
    pub fn publish_function_tested(&mut self, mut definition: FunctionDefinition, tested: Option<TestedPython>) -> Result<WorkbookFunction> {
        definition.name = definition.name.to_uppercase();
        self.mutating("publish_function", None, |wb| {
            let mut items = wb.functions()?;
            if items.len() >= 128 && !items.iter().any(|f| f.name == definition.name) {
                bail!("workbook function limit reached");
            }
            if !items.iter().any(|f| f.name == definition.name) && wb.sheets().has_function_name(&definition.name) {
                bail!("name conflicts with an existing named range or imported sheet name definition");
            }
            let dependencies = validate(&wb.conn, &definition, &items)?;
            let runtime = if definition.kind == FunctionKind::Python {
                let current = wb.function_runtime.clone().ok_or_else(|| anyhow!(NO_RUNTIME))?;
                let tested = match tested {
                    Some(t) if t.definition == serde_json::to_string(&definition)? && t.runtime == current.identity() => t,
                    _ => test_python(&definition, current.as_ref(), &std::sync::atomic::AtomicBool::new(false))?,
                };
                if tested.results.iter().any(|t| !t.passed) {
                    bail!("function tests failed");
                }
                Some(tested.runtime)
            } else {
                if wb.test_function(&definition)?.iter().any(|t| !t.passed) {
                    bail!("function tests failed");
                }
                None
            };
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
                runtime,
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
    pub(crate) fn apply_functions(&mut self, items: &[WorkbookFunction]) -> Result<()> {
        let versions = ordered(items)?;
        let epoch = epoch(&self.conn)?;
        for v in &versions {
            match v.definition.kind {
                FunctionKind::Sql => {
                    // The registry is read back from the file: check each definition again
                    // before it becomes a macro (SEC.01).
                    check_sql_definition(&self.conn, &v.definition)?;
                    self.conn.execute_batch(&macro_sql(&v.definition))?
                }
                FunctionKind::Python => {
                    let d = &v.definition;
                    let suffix = if d.volatile { format!("#{epoch}") } else { String::new() };
                    let returns = d.returns.unwrap_or(crate::external::ResultType::Text);
                    self.conn.execute_batch(&crate::external::lookup_macro(&d.name, v.version, &d.arguments, returns, &suffix))?;
                }
                FunctionKind::Formula => {}
            }
        }
        self.activate_workers(&versions, epoch)?;
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

const NO_RUNTIME: &str = "this server has no Python runtime; Python functions cannot run here";

fn upper(d: &FunctionDefinition) -> FunctionDefinition {
    FunctionDefinition { name: d.name.to_uppercase(), ..d.clone() }
}

fn epoch(conn: &Connection) -> Result<u64> {
    Ok(storage::meta_get(conn, EPOCH_KEY)?.and_then(|v| v.parse().ok()).unwrap_or(0))
}

/// Tests of a Python definition, run by a worker without the workbook (outside its lock,
/// 8.07). Publishing accepts them for exactly this definition and runtime.
#[derive(Debug, Clone)]
pub struct TestedPython {
    pub definition: String,
    pub runtime: crate::external::RuntimeIdentity,
    pub results: Vec<FunctionTestResult>,
}

/// Run a Python definition's tests in `runtime`: each test's arguments are read like cell
/// inputs, its result written as [`crate::external::CallResult::display`].
pub fn test_python(definition: &FunctionDefinition, runtime: &dyn crate::external::FunctionRuntime, cancel: &std::sync::atomic::AtomicBool) -> Result<TestedPython> {
    use crate::external::{check_result, test_value, CallContext, WorkerJob};
    let d = upper(definition);
    if d.kind != FunctionKind::Python {
        bail!("not a Python function");
    }
    let returns = d.returns.ok_or_else(|| anyhow!("a Python function declares what it returns: number, text or boolean"))?;
    if d.tests.len() > 32 || d.tests.iter().any(|t| t.arguments.len() != d.arguments.len() || t.arguments.iter().any(|a| a.len() > 4096) || t.expected.len() > 4096) {
        bail!("invalid test arguments or size");
    }
    let job = WorkerJob {
        function: d.name.clone(),
        version: 0,
        source: d.source.clone(),
        entry: python_entry(&d.name),
        arguments: d.arguments.clone(),
        returns,
        context: CallContext::Sheet,
        calls: d.tests.iter().map(|t| t.arguments.iter().map(|a| test_value(a)).collect()).collect(),
    };
    let results = if job.calls.is_empty() { vec![] } else { runtime.run(&job, cancel).map_err(|e| anyhow!("the tests could not run: {e}"))? };
    let results = results
        .into_iter()
        .zip(&d.tests)
        .map(|(r, t)| {
            let actual = check_result(r, returns, CallContext::Sheet).display();
            FunctionTestResult { passed: actual == t.expected, actual, expected: t.expected.clone() }
        })
        .collect();
    Ok(TestedPython { definition: serde_json::to_string(&d)?, runtime: runtime.identity(), results })
}

/// What storing a batch of results did.
#[derive(Debug, Clone, Default)]
pub struct StoredResults {
    /// Results written (an existing result is kept).
    pub stored: usize,
    /// False when the version is no longer pinned: nothing was written.
    pub applied: bool,
    /// Sheets were evaluated again.
    pub sheets: bool,
    /// Tables with columns calling the function.
    pub tables: Vec<String>,
}

impl Workbook {
    /// The runtime worker functions run in here, and why they may not run (Python off for
    /// this workbook), if so. Re-activates the pinned versions.
    pub fn set_function_runtime(&mut self, runtime: Option<std::sync::Arc<dyn crate::external::FunctionRuntime>>, disabled: Option<String>) -> Result<()> {
        *self.shared.functions.runtime.write().unwrap() = runtime.as_ref().map(|r| r.identity());
        *self.shared.functions.disabled.write().unwrap() = disabled;
        *self.shared.functions.runtime_impl.write().unwrap() = runtime.clone();
        self.function_runtime = runtime;
        let items = self.functions()?;
        let versions = ordered(&items)?;
        let epoch = epoch(&self.conn)?;
        self.activate_workers(&versions, epoch)
    }

    /// The result cache worker functions read (for the server's jobs).
    pub fn function_results(&self) -> std::sync::Arc<crate::external::ResultCache> {
        self.shared.functions.clone()
    }

    pub fn function_runtime(&self) -> Option<std::sync::Arc<dyn crate::external::FunctionRuntime>> {
        self.function_runtime.clone()
    }

    /// Make the pinned worker versions the ones the hook evaluates, load their results and
    /// evaluate the sheets again (calls missing become pending).
    fn activate_workers(&mut self, versions: &[FunctionVersion], epoch: u64) -> Result<()> {
        use crate::external::ActiveFunction;
        let cache = self.shared.functions.clone();
        cache.epoch.store(epoch, std::sync::atomic::Ordering::SeqCst);
        let runtime = cache.runtime.read().unwrap().clone();
        let disabled = cache.disabled.read().unwrap().clone();
        let mut active = std::collections::HashMap::new();
        let mut load = Vec::new();
        for v in versions.iter().filter(|v| v.definition.kind == FunctionKind::Python) {
            let d = &v.definition;
            let unavailable = if let Some(why) = &disabled {
                Some(why.clone())
            } else {
                match (&runtime, &v.runtime) {
                    (None, _) => Some(NO_RUNTIME.to_string()),
                    (Some(have), Some(want)) if have.id != want.id => Some(format!(
                        "{} needs Python runtime {} (Python {}); this server has {} (Python {})",
                        d.name, want.id, want.version, have.id, have.version
                    )),
                    _ => None,
                }
            };
            active.insert(
                d.name.clone(),
                ActiveFunction { version: v.version, returns: d.returns.unwrap_or(crate::external::ResultType::Text), volatile: d.volatile, arity: d.arguments.len(), unavailable },
            );
            load.push((d.name.clone(), v.version));
        }
        let changed = *cache.active.read().unwrap() != active;
        *cache.active.write().unwrap() = active;
        cache.clear();
        crate::external::load(&self.conn, &cache, &load)?;
        if changed || !load.is_empty() {
            self.shared.sheets.lock().unwrap_or_else(|e| e.into_inner()).reevaluate();
        }
        Ok(())
    }

    /// Store a batch of results of `function` `version`, if that version is still pinned
    /// and worker functions may still run here (8.07, 8.10); then evaluate the sheets again.
    /// Not a revision: no input changes.
    pub fn store_function_results(&mut self, function: &str, version: u32, results: &[(String, crate::external::CallResult)]) -> Result<StoredResults> {
        let items = self.functions()?;
        let pinned = items.iter().find(|f| f.name == function).map(|f| f.pinned);
        let cache = self.shared.functions.clone();
        let allowed = cache.disabled.read().unwrap().is_none() && cache.active.read().unwrap().get(function).is_some_and(|a| a.version == version && a.unavailable.is_none());
        if pinned != Some(version) || !allowed {
            return Ok(StoredResults::default());
        }
        self.conn.execute_batch("BEGIN TRANSACTION;")?;
        let stored = match crate::external::store(&self.conn, function, version, results) {
            Ok(n) => {
                self.conn.execute_batch("COMMIT;")?;
                n
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                return Err(e);
            }
        };
        for (key, r) in results {
            // The first stored result stands (a duplicate delivery changes nothing).
            match cache.get(function, version, key) {
                Some(existing) if !existing.is_transient() => {}
                _ => cache.put(function, version, key, r.clone()),
            }
        }
        let sheets = {
            let mut s = self.shared.sheets.lock().unwrap_or_else(|e| e.into_inner());
            s.reevaluate()
        };
        let tables = self.db().worker_columns(&cache)?.into_iter().filter(|c| c.function == function).map(|c| c.table).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        Ok(StoredResults { stored, applied: true, sheets, tables })
    }

    /// Compute volatile functions again and retry calls that failed for want of time,
    /// memory or a worker (8.08, 8.10).
    pub fn refresh_functions(&mut self) -> Result<u64> {
        self.mutating("refresh_functions", None, |wb| {
            let next = epoch(&wb.conn)? + 1;
            storage::meta_set(&wb.conn, EPOCH_KEY, &next.to_string())?;
            wb.conn.execute_batch("DELETE FROM _rf_fn_results WHERE transient;")?;
            let items = wb.functions()?;
            wb.apply_functions(&items)?;
            Ok(next)
        })
    }
}

impl crate::workbook::Reader {
    pub fn functions(&self) -> Result<Vec<WorkbookFunction>> {
        list(self.connection())
    }

    /// Run a definition's tests; Python ones in the workbook's runtime, without the writer.
    pub fn test_function(&self, definition: &FunctionDefinition) -> Result<Vec<FunctionTestResult>> {
        if definition.kind == FunctionKind::Python {
            let runtime = self.shared().functions.runtime_impl().ok_or_else(|| anyhow!(NO_RUNTIME))?;
            validate(self.connection(), &upper(definition), &list(self.connection())?)?;
            return Ok(test_python(definition, runtime.as_ref(), &std::sync::atomic::AtomicBool::new(false))?.results);
        }
        test_definition(self.connection(), definition)
    }

    /// Calls of worker functions that table columns need and the cache lacks: up to `limit`
    /// per column, from one snapshot.
    pub fn missing_table_calls(&self, limit: usize) -> Result<Vec<(crate::external::ColumnCall, Vec<crate::external::PendingCall>)>> {
        let db = self.db_for_reports();
        let cache = self.shared().functions.clone();
        db.snapshot(|| {
            let mut out = Vec::new();
            for call in db.worker_columns(&cache)? {
                let (version, suffix, ok) = {
                    let a = cache.active.read().unwrap();
                    match a.get(&call.function) {
                        Some(f) => (f.version, cache_suffix(&cache, f.volatile), f.unavailable.is_none()),
                        None => continue,
                    }
                };
                if !ok {
                    continue;
                }
                let base = db.base_select(&call.table)?;
                let missing = crate::external::missing_table_calls(db.conn, &base, &call, version, &suffix, limit)?;
                if !missing.is_empty() {
                    out.push((call, missing));
                }
            }
            Ok(out)
        })
    }
}

fn cache_suffix(cache: &crate::external::ResultCache, volatile: bool) -> String {
    if volatile {
        format!("#{}", cache.epoch.load(std::sync::atomic::Ordering::SeqCst))
    } else {
        String::new()
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
            runtime: None,
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
            FunctionKind::Python => bail!("Python functions run in a worker; see test_python"),
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
            FunctionKind::Python => bail!("Python functions run in a worker; see test_python"),
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
