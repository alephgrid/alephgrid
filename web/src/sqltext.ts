// Plain-text helpers for the SQL workspace: quoting identifiers, finding `$name`
// parameters, and the completion context at the cursor. These only shape suggestions
// and the parameter form; the server checks every statement itself.

/**
 * Words that must be quoted to be used as a name. DuckDB's reserved keywords plus the
 * ones that are only allowed as function or type names; quoting one more than needed
 * is harmless, quoting one less is not.
 */
const RESERVED = new Set(`
  all analyse analyze and any array as asc asymmetric both case cast check collate column constraint create
  default deferrable desc describe distinct do else end except false fetch for foreign from grant group having
  in initially intersect into lateral leading limit not null offset on only or order pivot pivot_longer
  pivot_wider placing primary qualify references returning select show some summarize symmetric table then to
  trailing true union unique unpivot using variadic when where window with
  anti asof authorization binary collation concurrently cross freeze full glob ilike inner is isnull join left
  like map natural notnull outer overlaps positional right semi similar struct tablesample try_cast verbose
`.trim().split(/\s+/));

const PLAIN = /^[a-z_][a-z0-9_]*$/;

/** `name` as SQL: bare when it is a plain lower-case identifier, otherwise double-quoted. */
export function quoteIdent(name: string): string {
  if (PLAIN.test(name) && !RESERVED.has(name)) return name;
  return `"${name.replace(/"/g, '""')}"`;
}

/** Internal names are never offered (the server never lists them either). */
export function isInternal(name: string): boolean {
  return name.toLowerCase().startsWith("_rf_");
}

type Scan = {
  /** `$name` placeholders outside strings and comments, in order of first use. */
  params: string[];
  /** Identifiers outside strings and comments (quoted ones unquoted), lower-cased. */
  words: Set<string>;
  /** What the scan was inside of when it reached `end`. */
  state: "code" | "string" | "quoted" | "line-comment" | "block-comment";
  /** Where the quoted identifier open at `end` started (its `"`). */
  quoteStart: number;
};

const IDENT_START = /[\p{L}_]/u;
const IDENT_PART = /[\p{L}\p{N}_]/u;

/** One pass over `sql[0..end)`, skipping strings and comments. */
function scan(sql: string, end = sql.length): Scan {
  const params: string[] = [];
  const words = new Set<string>();
  let state: Scan["state"] = "code";
  let quoteStart = -1;
  let i = 0;
  while (i < end) {
    const ch = sql[i];
    if (state === "string") {
      if (ch === "'") {
        if (sql[i + 1] === "'" && i + 1 < end) i++;
        else state = "code";
      }
      i++;
      continue;
    }
    if (state === "quoted") {
      if (ch === '"') {
        if (sql[i + 1] === '"' && i + 1 < end) i++;
        else {
          words.add(sql.slice(quoteStart + 1, i).replace(/""/g, '"').toLowerCase());
          state = "code";
        }
      }
      i++;
      continue;
    }
    if (state === "line-comment") {
      if (ch === "\n") state = "code";
      i++;
      continue;
    }
    if (state === "block-comment") {
      if (ch === "*" && sql[i + 1] === "/") { state = "code"; i += 2; continue; }
      i++;
      continue;
    }
    if (ch === "'") { state = "string"; i++; continue; }
    if (ch === '"') { state = "quoted"; quoteStart = i; i++; continue; }
    if (ch === "-" && sql[i + 1] === "-") { state = "line-comment"; i += 2; continue; }
    if (ch === "/" && sql[i + 1] === "*") { state = "block-comment"; i += 2; continue; }
    if (ch === "$" && i + 1 < end && IDENT_START.test(sql[i + 1]) && !(i > 0 && IDENT_PART.test(sql[i - 1]))) {
      let j = i + 1;
      while (j < end && IDENT_PART.test(sql[j])) j++;
      const name = sql.slice(i + 1, j);
      if (!params.includes(name)) params.push(name);
      i = j;
      continue;
    }
    if (IDENT_START.test(ch)) {
      let j = i + 1;
      while (j < end && IDENT_PART.test(sql[j])) j++;
      words.add(sql.slice(i, j).toLowerCase());
      i = j;
      continue;
    }
    i++;
  }
  return { params, words, state, quoteStart };
}

/** The `$name` parameters a statement uses, in order of first use. */
export function findParams(sql: string): string[] {
  return scan(sql).params;
}

/** Identifiers mentioned in a statement (lower-cased), for "columns of tables in use". */
export function mentionedNames(sql: string): Set<string> {
  return scan(sql).words;
}

export type Completion = {
  /** Text being completed, and where it starts (a quoted one starts at its `"`). */
  prefix: string;
  start: number;
  /** The keyword before it: suggest tables after FROM/JOIN/INTO/UPDATE. */
  afterTableKeyword: boolean;
  /** `qualifier.` before the prefix, if any (a table name or an alias). */
  qualifier: string | null;
};

const TABLE_KEYWORDS = new Set(["from", "join", "into", "update", "table"]);

/**
 * What to complete at `caret`, or null when the cursor is not after at least one
 * identifier character (or is inside a string, a comment or a `$parameter`).
 */
export function completionAt(sql: string, caret: number): Completion | null {
  const s = scan(sql, caret);
  let start: number;
  let prefix: string;
  if (s.state === "quoted") {
    start = s.quoteStart;
    prefix = sql.slice(start + 1, caret).replace(/""/g, '"');
    if (!prefix) return null;
  } else if (s.state === "code") {
    start = caret;
    while (start > 0 && IDENT_PART.test(sql[start - 1])) start--;
    prefix = sql.slice(start, caret);
    if (!prefix || !IDENT_START.test(prefix[0]) || sql[start - 1] === "$") return null;
  } else {
    return null;
  }
  let qualifier: string | null = null;
  if (sql[start - 1] === ".") {
    const q = /(?:"((?:[^"]|"")+)"|([\p{L}_][\p{L}\p{N}_]*))$/u.exec(sql.slice(0, start - 1));
    if (q) qualifier = q[1] !== undefined ? q[1].replace(/""/g, '"') : q[2];
  }
  const before = /([\p{L}_]+)\s*$/u.exec(sql.slice(0, start));
  const afterTableKeyword = !qualifier && !!before && TABLE_KEYWORDS.has(before[1].toLowerCase());
  return { prefix, start, afterTableKeyword, qualifier };
}
