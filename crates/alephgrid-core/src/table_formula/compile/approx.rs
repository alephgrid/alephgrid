//! Approximate, wildcard and sorted lookups over whole table columns (F.07).
//!
//! The sheet engine's rules, verified differentially against it:
//!
//! * **Nearest match** (XLOOKUP/XMATCH `match_mode` -1 or 1, searching from the first or the
//!   last row): an exact match if there is one, otherwise the largest smaller (or smallest
//!   larger) key in the engine's comparison order (`compare_values`): numbers at 15 digits,
//!   then text by its upper case, then logical values, then errors; a blank key compares as
//!   0, "" or FALSE against a number, text or logical value. Among equal keys the first in
//!   search order wins. The plan groups the keys once and finds each row's neighbour with an
//!   ASOF join, one per comparable kind; nothing runs per row.
//! * **Wildcards** (`match_mode` 2, MATCH 0 with text): `*`, `?` and `~` against text keys,
//!   case-insensitively; each distinct pattern is matched against the keys once.
//! * **Sorted** (VLOOKUP without FALSE, MATCH 1 and -1): the last row whose key is at most
//!   (MATCH -1: at least) the value, in a key column that must be sorted ascending
//!   (descending), with no blanks or errors. Sheets give arbitrary results on unsorted data;
//!   tables give `#N/A` on every row instead.
//!
//! Binary search (`search_mode` 2 and -2) is refused: on a table it would give the same
//! results as searching from the first or last row, which the plans above already do in one
//! pass, and on unsorted data it gives arbitrary ones.

use super::*;

/// A lookup's error column says "not found" with this; callers substitute `if_not_found`.
pub(super) const NOT_FOUND: &str = "'#ALEPHGRID_NOT_FOUND'";

/// What a lookup returns: a column of the looked-up table, or the 1-based position of the
/// matching row in persisted order (MATCH, XMATCH).
#[derive(Clone, Copy)]
pub(super) enum Returns<'a> {
    Column(&'a str),
    Position,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Nearest {
    Smaller,
    Larger,
}

/// Rank, number and text of a value in the engine's comparison order, as SQL.
type Ordered = (String, String, String);

fn ordered(v: &str, ty: Ty) -> Ordered {
    match ty {
        Ty::Text => ("1".into(), "CAST(NULL AS DOUBLE)".into(), format!("upper({v})")),
        Ty::Bool => ("2".into(), format!("CASE WHEN {v} THEN 1.0 ELSE 0.0 END"), "CAST(NULL AS VARCHAR)".into()),
        Ty::Date => ("0".into(), p15(&serial(v)), "CAST(NULL AS VARCHAR)".into()),
        _ => ("0".into(), p15(v), "CAST(NULL AS VARCHAR)".into()),
    }
}

/// How a blank compares against values of type `ty`: as 0, "" or FALSE.
fn zero(ty: Ty) -> Ordered {
    match ty {
        Ty::Text => ("1".into(), "CAST(NULL AS DOUBLE)".into(), "''".into()),
        Ty::Bool => ("2".into(), "0.0".into(), "CAST(NULL AS VARCHAR)".into()),
        _ => ("0".into(), "0.0".into(), "CAST(NULL AS VARCHAR)".into()),
    }
}

/// Errors sort after everything, by Excel's canonical rank (`error_sort_rank`).
fn error_rank(e: &str) -> String {
    format!("CAST(CASE {e} WHEN '#NULL!' THEN 1 WHEN '#DIV/0!' THEN 2 WHEN '#VALUE!' THEN 3 WHEN '#REF!' THEN 4 WHEN '#NAME?' THEN 5 WHEN '#NUM!' THEN 6 WHEN '#N/A' THEN 7 ELSE 8 END AS DOUBLE)")
}

/// `a < b` for (r, n, t) columns prefixed `a`/`b`.
fn less(a: &str, b: &str) -> String {
    format!(
        "({a}r < {b}r OR ({a}r = {b}r AND (COALESCE({a}n, 0) < COALESCE({b}n, 0) OR (COALESCE({a}n, 0) = COALESCE({b}n, 0) AND COALESCE({a}t, '') < COALESCE({b}t, '')))))"
    )
}

impl Compiler<'_> {
    /// The source rows of a lookup, `(r, n, t, is_blank, v, e, o, bad, raw)`: the key in
    /// comparison order (a blank key as `blank`), the returned value and error, the persisted
    /// order, whether the key is blank or an error, and the key as text.
    fn ordered_source(rel: &Relation, key_col: &str, key_ty: Ty, ret: Returns, blank: &Ordered) -> String {
        let k = ident(key_col);
        let ke = rel.errors.iter().find(|(n, _)| n == key_col).map(|(_, e)| ident(e));
        let (kr, kn, kt) = ordered(&k, key_ty);
        let (zr, zn, zt) = blank;
        let (er, en, et) = match &ke {
            Some(e) => (format!("WHEN {e} IS NOT NULL THEN 3 "), format!("WHEN {e} IS NOT NULL THEN {} ", error_rank(e)), format!("WHEN {e} IS NOT NULL THEN CAST(NULL AS VARCHAR) ")),
            None => (String::new(), String::new(), String::new()),
        };
        let (v, e) = match ret {
            Returns::Column(c) => (ident(c), rel.errors.iter().find(|(n, _)| n == c).map(|(_, e)| ident(e)).unwrap_or_else(|| "CAST(NULL AS VARCHAR)".into())),
            Returns::Position => ("CAST(row_number() OVER (ORDER BY _rf_ord) AS DOUBLE)".into(), "CAST(NULL AS VARCHAR)".into()),
        };
        let bad = match &ke { Some(e) => format!("({k} IS NULL OR {e} IS NOT NULL)"), None => format!("({k} IS NULL)") };
        let is_blank = match &ke { Some(e) => format!("({k} IS NULL AND {e} IS NULL)"), None => format!("({k} IS NULL)") };
        format!(
            "SELECT CASE {er}WHEN {k} IS NULL THEN {zr} ELSE {kr} END AS r, CASE {en}WHEN {k} IS NULL THEN {zn} ELSE {kn} END AS n, \
             CASE {et}WHEN {k} IS NULL THEN {zt} ELSE {kt} END AS t, {is_blank} AS is_blank, {v} AS v, {e} AS e, _rf_ord AS o, {bad} AS bad, \
             CAST({k} AS VARCHAR) AS raw FROM ({})",
            rel.sql
        )
    }

    /// The row's probe in comparison order, bound as steps: `(key, r, n, t)` column names.
    /// A blank probe compares as `blank`.
    fn ordered_probe(&mut self, probe: &V, blank: &Ordered) -> (String, String, String, String) {
        let (pr, pn, pt) = if probe.ty == Ty::Blank { blank.clone() } else { ordered(&probe.v, probe.ty) };
        let (zr, zn, zt) = blank;
        let null = if probe.ty == Ty::Blank { "TRUE".to_string() } else { format!("{} IS NULL", probe.v) };
        let r = self.bind(format!("CAST(CASE WHEN {null} THEN {zr} ELSE {pr} END AS INTEGER)"));
        let n = self.bind(format!("CAST(CASE WHEN {null} THEN {zn} ELSE {pn} END AS DOUBLE)"));
        let t = self.bind(format!("CAST(CASE WHEN {null} THEN {zt} ELSE {pt} END AS VARCHAR)"));
        let k = self.bind(format!("CAST({r} AS VARCHAR) || ':' || COALESCE(CAST({n} AS VARCHAR), '') || ':' || COALESCE({t}, '')"));
        (k, r, n, t)
    }

    /// The nearest key to each row's value (see the module documentation), as (value,
    /// error) where the error is [`NOT_FOUND`] when there is none. `inclusive` counts an
    /// equal key as nearest (sorted lookups); otherwise equal keys are left to an exact
    /// lookup that runs first. `last` picks the last of equal keys instead of the first.
    /// With `sorted`, the keys must be in that order (true: ascending) without blanks or
    /// errors, or every row gets `#N/A`.
    ///
    /// The engine compares a blank key with the value as 0, "" or FALSE of the value's type,
    /// but with other keys as their type's zero. When the value has the keys' type that is
    /// one order, searched with an ASOF join. Otherwise every key of the column is on the
    /// same side of the value, so the answer is one of a few extremes chosen per row.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn nearest(&mut self, rel: &Relation, probe: &V, key_col: &str, key_ty: Ty, ret: Returns, toward: Nearest, inclusive: bool, last: bool, sorted: Option<bool>) -> (String, String) {
        let blank_key = zero(key_ty);
        let (_, pr, pn, pt) = self.ordered_probe(probe, &blank_key);
        let src = Self::ordered_source(rel, key_col, key_ty, ret, &blank_key);
        let dir = if last { "DESC" } else { "ASC" };
        let smaller = toward == Nearest::Smaller;
        let rank = |t: Ty| match t { Ty::Text => 1, Ty::Bool => 2, _ => 0 };
        let same = probe.ty == Ty::Blank || rank(probe.ty) == rank(key_ty);
        let key_rank = rank(key_ty);
        // One row per key (the first or last in row order), as ordered.
        let grouped = format!("SELECT r, n, t, v, e FROM (SELECT *, row_number() OVER (PARTITION BY r, n, t ORDER BY o {dir}) AS rn FROM ({src})) WHERE rn = 1");
        // Values of the keys' type (and blank values, which take it): one order, one ASOF
        // join, which reads only the rows asked for.
        let op = match (smaller, inclusive) { (true, false) => ">", (true, true) => ">=", (false, false) => "<", (false, true) => "<=" };
        let (col, pcol) = if key_ty == Ty::Text { ("t", &pt) } else { ("n", &pn) };
        let near = format!("{}j{}", self.prefix, self.stages.len());
        let steps = std::mem::take(&mut self.steps);
        self.stages.push(Stage::Steps(steps));
        self.stages.push(Stage::Asof {
            alias: near.clone(),
            sql: format!("SELECT * FROM ({grouped}) WHERE r = {key_rank}"),
            on: format!("o.{pr} = j.r AND o.{pcol} {op} j.{col}"),
            reads: rel.reads.clone(),
        });
        // One row of constants: the extremes values of another type fall back to, the
        // smallest error key (larger than everything), and whether sorted keys are sorted.
        let order = if key_ty == Ty::Text { "t" } else { "n" };
        let by = format!("{order} {}, o {dir}", if smaller { "DESC" } else { "ASC" });
        let ok = match sorted {
            None => "TRUE".to_string(),
            Some(ascending) => {
                let broken = if ascending { less("", "p_") } else { less("p_", "") };
                format!("(SELECT NOT COALESCE(bool_or(bad OR {broken}), FALSE) FROM (SELECT *, lag(r) OVER w AS p_r, lag(n) OVER w AS p_n, lag(t) OVER w AS p_t FROM src WINDOW w AS (ORDER BY o)))")
            }
        };
        let consts = format!(
            "WITH src AS MATERIALIZED ({src}), g AS ({grouped}) SELECT '' AS k, struct_pack(\
               every := (SELECT struct_pack(v := v, e := e) FROM src WHERE r = {key_rank} ORDER BY {by} LIMIT 1), \
               present := (SELECT struct_pack(v := v, e := e) FROM src WHERE r = {key_rank} AND NOT is_blank ORDER BY {by} LIMIT 1), \
               blank := (SELECT struct_pack(v := v, e := e) FROM src WHERE is_blank ORDER BY o {dir} LIMIT 1), \
               err := (SELECT struct_pack(v := v, e := e) FROM g WHERE r = 3 ORDER BY n LIMIT 1), \
               ok := {ok}) AS v, CAST(NULL AS VARCHAR) AS e"
        );
        let consts_alias = format!("{}j{}", self.prefix, self.stages.len() + 1);
        let key = self.bind("CAST('' AS VARCHAR)".into());
        let steps = std::mem::take(&mut self.steps);
        self.stages.push(Stage::Steps(steps));
        self.stages.push(Stage::Join { alias: consts_alias.clone(), sql: consts, key, reads: rel.reads.clone() });
        let (hit, nv, ne) = (ident(&format!("{near}k")), ident(&format!("{near}v")), ident(&format!("{near}e")));
        let c = ident(&format!("{consts_alias}v"));
        // Nothing larger of the same kind: the smallest error key, if any.
        let (near_v, near_e) = if smaller {
            (format!("CASE WHEN {hit} THEN {nv} END"), format!("CASE WHEN {hit} THEN {ne} ELSE {NOT_FOUND} END"))
        } else {
            (format!("CASE WHEN {hit} THEN {nv} ELSE ({c}).err.v END"), format!("CASE WHEN {hit} THEN {ne} WHEN ({c}).err IS NOT NULL THEN ({c}).err.e ELSE {NOT_FOUND} END"))
        };
        let (value, error) = if same {
            (near_v, near_e)
        } else {
            // Values of another type: every key of the column is smaller (or larger), by
            // type; a blank key is too when the value's zero is.
            let probe_rank = rank(probe.ty);
            let keys_qualify = if smaller { key_rank < probe_rank } else { key_rank > probe_rank };
            let (_, zn, zt) = zero(probe.ty);
            let blank_qualifies = match (probe.ty, smaller) {
                (Ty::Text, true) => format!("({zt} < {pt})"),
                (Ty::Text, false) => format!("({zt} > {pt})"),
                (_, true) => format!("({zn} < {pn})"),
                (_, false) => format!("({zn} > {pn})"),
            };
            let choice = if keys_qualify {
                format!("CASE WHEN {blank_qualifies} THEN ({c}).every ELSE ({c}).present END")
            } else {
                format!("CASE WHEN {blank_qualifies} THEN ({c}).blank END")
            };
            let choice = if smaller { choice } else { format!("COALESCE({choice}, ({c}).err)") };
            let x = self.bind(choice);
            (
                format!("CASE WHEN {pr} = {key_rank} THEN {near_v} ELSE ({x}).v END"),
                format!("CASE WHEN {pr} = {key_rank} THEN {near_e} WHEN {x} IS NULL THEN {NOT_FOUND} ELSE ({x}).e END"),
            )
        };
        (format!("CASE WHEN ({c}).ok THEN {value} END"), format!("CASE WHEN NOT ({c}).ok THEN '#N/A' ELSE {error} END"))
    }

    /// The first (or `last`) text key matching each row's text value as a wildcard pattern,
    /// case-insensitively, as (value, error) with [`NOT_FOUND`] for no match. Only text keys
    /// match; values without `*`, `?` or `~` are compared directly.
    pub(super) fn wildcard(&mut self, rel: &Relation, probe: &V, key_col: &str, key_ty: Ty, ret: Returns, last: bool) -> (String, String) {
        let pattern = self.bind(format!("lower({})", probe.v));
        let alias = format!("{}j{}", self.prefix, self.stages.len());
        let input = ident(&format!("{alias}o"));
        let blank = zero(key_ty);
        let src = Self::ordered_source(rel, key_col, key_ty, ret, &blank);
        // Text keys only, lower-cased as the engine compares them.
        let text_keys = if key_ty == Ty::Text {
            "SELECT lower(raw) AS kt, v, e, o FROM src WHERE r = 1 AND NOT bad".to_string()
        } else {
            "SELECT CAST(NULL AS VARCHAR) AS kt, v, e, o FROM src WHERE FALSE".into()
        };
        let pick = if last { "arg_max" } else { "arg_min" };
        let regex = wildcard_regex("p.pat");
        let sql = format!(
            "WITH src AS MATERIALIZED ({src}), kt AS MATERIALIZED ({text_keys}), \
             p AS (SELECT DISTINCT {pat} AS pat FROM {input} WHERE {pat} IS NOT NULL), \
             lit AS (SELECT p.pat AS k, {pick}(struct_pack(v := kt.v, e := kt.e), kt.o) AS x FROM p JOIN kt ON kt.kt = p.pat WHERE NOT regexp_matches(p.pat, '[*?~]') GROUP BY p.pat), \
             pat AS (SELECT p.pat AS k, {pick}(struct_pack(v := kt.v, e := kt.e), kt.o) AS x FROM p JOIN kt ON regexp_full_match(kt.kt, {regex}) WHERE regexp_matches(p.pat, '[*?~]') GROUP BY p.pat) \
             SELECT p.pat AS k, COALESCE(lit.x, pat.x).v AS v, CASE WHEN COALESCE(lit.x, pat.x) IS NULL THEN {NOT_FOUND} ELSE COALESCE(lit.x, pat.x).e END AS e \
             FROM p LEFT JOIN lit ON lit.k = p.pat LEFT JOIN pat ON pat.k = p.pat",
            pat = pattern,
        );
        let steps = std::mem::take(&mut self.steps);
        self.stages.push(Stage::Steps(steps));
        self.stages.push(Stage::Correlated { alias: alias.clone(), sql, key: pattern });
        (ident(&format!("{alias}v")), ident(&format!("{alias}e")))
    }

    /// Find each row's match by `mode` (XLOOKUP/XMATCH `match_mode`: 0 exact, -1/1 exact or
    /// nearest, 2 wildcard), searching from the first or `last` row, as (value, error) with
    /// [`NOT_FOUND`] when there is no match.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn find(&mut self, rel: &Relation, probe: V, key_col: &str, key_ty: Ty, ret: Returns, mode: i32, last: bool) -> (String, String) {
        let exact = |me: &mut Self, probe: V| -> (String, String) {
            let (jk, jv, je) = me.lookup_join(rel, &[(probe, key_col.to_string(), key_ty)], ret, last, true);
            (format!("CASE WHEN {jk} IS NOT NULL THEN {jv} END"), format!("CASE WHEN {jk} IS NOT NULL THEN {je} ELSE {NOT_FOUND} END"))
        };
        match mode {
            -1 | 1 => {
                let toward = if mode < 0 { Nearest::Smaller } else { Nearest::Larger };
                let (ev, ee) = exact(self, probe.clone());
                let (nv, ne) = self.nearest(rel, &probe, key_col, key_ty, ret, toward, false, last, None);
                let hit = format!("({ee}) IS DISTINCT FROM {NOT_FOUND}");
                (format!("CASE WHEN {hit} THEN {ev} ELSE {nv} END"), format!("CASE WHEN {hit} THEN {ee} ELSE {ne} END"))
            }
            // Text values are patterns; a blank and other types match as in exact mode.
            2 if probe.ty == Ty::Text => {
                let null = format!("({} IS NULL)", probe.v);
                let (ev, ee) = exact(self, probe.clone());
                let (wv, we) = self.wildcard(rel, &probe, key_col, key_ty, ret, last);
                (format!("CASE WHEN {null} THEN {ev} ELSE {wv} END"), format!("CASE WHEN {null} THEN {ee} ELSE {we} END"))
            }
            _ => exact(self, probe),
        }
    }

    /// A literal argument (`default` when omitted) among `allowed`.
    fn mode_arg(args: &[Expr], i: usize, default: i32, allowed: &[i32], what: &str) -> Result<i32, FormulaError> {
        let value = match args.get(i).map(|a| &a.kind) {
            None | Some(ExprKind::Empty) => return Ok(default),
            Some(ExprKind::Number(n)) => Some(*n),
            Some(ExprKind::Unary { op: UnaryOp::Neg, expr }) => match expr.kind { ExprKind::Number(n) => Some(-n), _ => None },
            _ => None,
        };
        match value {
            Some(n) if n.fract() == 0.0 && allowed.contains(&(n as i32)) => Ok(n as i32),
            _ => fail(what.to_string(), args[i].span),
        }
    }

    fn whole_column(e: &Expr, name: &str) -> Result<(String, String), FormulaError> {
        match &e.kind {
            ExprKind::TableColumn { table, column } => Ok((table.clone(), column.clone())),
            _ => fail(format!("{name} needs a whole column of a table here, e.g. Customers[id]"), e.span),
        }
    }

    /// `MATCH(value, T[k], [type])`: the position (from 1, in row order) of the first exact
    /// match (type 0; text values are wildcard patterns, and nothing converts between types),
    /// or of the last key at most (1, the default) or at least (-1) the value in a column
    /// sorted ascending (descending). A blank value matches nothing.
    pub(super) fn match_fn(&mut self, args: &[Expr], span: Span) -> Result<V, FormulaError> {
        Self::arity("MATCH", args, 2, 3, span)?;
        let kind = Self::mode_arg(args, 2, 1, &[1, 0, -1], "the match type must be written in the formula: 0 (exact), 1 (largest key at most the value, sorted ascending) or -1 (smallest key at least the value, sorted descending)")?;
        let (table, column) = Self::whole_column(&args[1], "MATCH")?;
        let rel = self.lookup_relation(&table, args[1].span)?;
        let (key_col, key_ty) = Self::lookup_column(&rel, &table, &column, args[1].span)?;
        let probe = self.expr(&args[0])?;
        let probe_error = probe.e.clone();
        // A blank value matches nothing, whatever the type.
        let null = if probe.ty == Ty::Blank { "TRUE".to_string() } else { format!("({} IS NULL)", probe.v) };
        let (fv, fe) = match kind {
            0 if probe.ty == Ty::Text => self.wildcard(&rel, &probe, &key_col, key_ty, Returns::Position, false),
            0 => {
                let (jk, jv, je) = self.lookup_join(&rel, &[(probe, key_col, key_ty)], Returns::Position, false, false);
                (format!("CASE WHEN {jk} IS NOT NULL THEN {jv} END"), format!("CASE WHEN {jk} IS NOT NULL THEN {je} ELSE {NOT_FOUND} END"))
            }
            _ => {
                let (toward, ascending) = if kind == 1 { (Nearest::Smaller, true) } else { (Nearest::Larger, false) };
                self.nearest(&rel, &probe, &key_col, key_ty, Returns::Position, toward, true, true, Some(ascending))
            }
        };
        let fe = format!("CASE WHEN {null} THEN {NOT_FOUND} ELSE {fe} END");
        Ok(self.not_found_is_na(fv, fe, probe_error, span))
    }

    /// `XMATCH(value, T[k], [match_mode], [search_mode])`: XLOOKUP's search, giving the
    /// position (from 1, in row order).
    pub(super) fn xmatch(&mut self, args: &[Expr], span: Span) -> Result<V, FormulaError> {
        Self::arity("XMATCH", args, 2, 4, span)?;
        let mode = Self::mode_arg(args, 2, 0, &[0, -1, 1, 2], "match_mode must be written in the formula: 0 (exact), -1 (exact or next smaller), 1 (exact or next larger) or 2 (wildcards)")?;
        let last = Self::mode_arg(args, 3, 1, &[1, -1], "search_mode must be 1 (from the first row) or -1 (from the last); binary search (2, -2) is not available for tables")? < 0;
        let (table, column) = Self::whole_column(&args[1], "XMATCH")?;
        let rel = self.lookup_relation(&table, args[1].span)?;
        let (key_col, key_ty) = Self::lookup_column(&rel, &table, &column, args[1].span)?;
        let probe = self.expr(&args[0])?;
        let probe_error = probe.e.clone();
        let (fv, fe) = self.find(&rel, probe, &key_col, key_ty, Returns::Position, mode, last);
        Ok(self.not_found_is_na(fv, fe, probe_error, span))
    }

    /// A position lookup's result: `#N/A` when nothing matched.
    fn not_found_is_na(&mut self, fv: String, fe: String, probe_error: Option<String>, span: Span) -> V {
        let fe = self.bind(fe);
        let value = format!("CASE WHEN {fe} IS NOT DISTINCT FROM {NOT_FOUND} THEN NULL ELSE {fv} END");
        let error = first_error([probe_error, Some(format!("CASE WHEN {fe} IS NOT DISTINCT FROM {NOT_FOUND} THEN '#N/A' ELSE {fe} END"))]);
        self.make(value, error, Ty::Number, span)
    }

    /// `INDEX(T[c], n)` and `INDEX(T, n, column)`: the value in row `n` (from 1, in row
    /// order). `n` below 1 gives #VALUE! (a whole column cannot be one value), past the end
    /// #REF!; the column number must be written in the formula.
    pub(super) fn index(&mut self, args: &[Expr], span: Span) -> Result<V, FormulaError> {
        Self::arity("INDEX", args, 2, 3, span)?;
        let (table, column) = match &args[0].kind {
            ExprKind::TableColumn { table, column } => {
                if args.len() == 3 {
                    Self::mode_arg(args, 2, 1, &[1], "INDEX of one column takes column number 1 or none")?;
                }
                (table.clone(), Some(column.clone()))
            }
            ExprKind::Table { table } => (table.clone(), None),
            _ => return fail("INDEX needs a whole column or table here, e.g. Customers[name] or Customers", args[0].span),
        };
        let rel = self.lookup_relation(&table, args[0].span)?;
        let column = match column {
            Some(c) => c,
            None => {
                let k = match args.get(2).map(|a| &a.kind) {
                    Some(ExprKind::Number(n)) if *n >= 1.0 => n.trunc() as usize,
                    _ => return fail("the column number must be written in the formula, e.g. 2", args.get(2).map_or(span, |a| a.span)),
                };
                match rel.columns.get(k - 1) {
                    Some((name, _)) => name.clone(),
                    None => return Ok(self.make("NULL".into(), Some("'#REF!'".into()), Ty::Number, span)),
                }
            }
        };
        let (col, ty) = Self::lookup_column(&rel, &table, &column, args[0].span)?;
        let n = self.expr(&args[1])?;
        let (nv, ne) = self.num(&n);
        let row = self.bind(format!("trunc({nv})"));
        let key = self.bind(format!("CAST(CAST({row} AS BIGINT) AS VARCHAR)"));
        let err = rel.errors.iter().find(|(c, _)| *c == col).map(|(_, e)| ident(e)).unwrap_or_else(|| "CAST(NULL AS VARCHAR)".into());
        let sub = format!("SELECT CAST(row_number() OVER (ORDER BY _rf_ord) AS VARCHAR) AS k, {} AS v, {err} AS e FROM ({})", ident(&col), rel.sql);
        let alias = format!("{}j{}", self.prefix, self.stages.len());
        let steps = std::mem::take(&mut self.steps);
        self.stages.push(Stage::Steps(steps));
        self.stages.push(Stage::Join { alias: alias.clone(), sql: sub, key, reads: rel.reads.clone() });
        let (jk, jv, je) = (ident(&format!("{alias}k")), ident(&format!("{alias}v")), ident(&format!("{alias}e")));
        let value = format!("CASE WHEN {jk} IS NOT NULL THEN {jv} END");
        let error = first_error([n.e.clone(), ne, Some(format!("CASE WHEN {row} IS NULL OR {row} < 1 THEN '#VALUE!' WHEN {jk} IS NULL THEN '#REF!' ELSE {je} END"))]);
        Ok(self.make(value, error, ty, span))
    }
}
