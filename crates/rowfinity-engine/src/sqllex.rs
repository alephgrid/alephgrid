//! A small SQL tokenizer for *routing*, never for deciding what is allowed.
//!
//! It finds the end of a statement (to drop a trailing `;` and comments), the first keyword
//! (to tell a SELECT from an UPDATE in an error message), and the top-level structure of
//! `INSERT`/`UPDATE`/`DELETE` statements so their parts can be checked one by one by
//! DuckDB's own parser (see `sqlpolicy.rs` and `sqlchange.rs`). Whatever this module gets
//! wrong makes a statement fail to parse or be refused; it never makes a statement run that
//! the parser-level checks did not see.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tok {
    /// An unquoted word: a keyword or an identifier.
    Word,
    /// `"identifier"`.
    Quoted,
    /// `'text'`, `E'text'`, `$tag$text$tag$`.
    Str,
    Number,
    /// `$name` or `?`.
    Param,
    /// Any other character (or `::`, `<=`, … as single characters).
    Punct(char),
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub kind: Tok,
    pub span: Range<usize>,
    /// Nesting depth of `()`, `[]` and `{}` at the token (an opening one has the outer depth).
    pub depth: u32,
}

impl Token {
    pub fn text<'a>(&self, sql: &'a str) -> &'a str {
        &sql[self.span.clone()]
    }

    pub fn is_word(&self, sql: &str, word: &str) -> bool {
        self.kind == Tok::Word && self.text(sql).eq_ignore_ascii_case(word)
    }
}

/// Tokens of `sql`, skipping whitespace and comments. An unterminated string, quoted
/// identifier or block comment runs to the end of the input (the parser will reject it).
pub(crate) fn tokens(sql: &str) -> Vec<Token> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut depth: u32 = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"--") {
            i = sql[i..].find('\n').map(|n| i + n + 1).unwrap_or(b.len());
            continue;
        }
        if b[i..].starts_with(b"/*") {
            // DuckDB block comments nest.
            let mut level = 0usize;
            while i < b.len() {
                if b[i..].starts_with(b"/*") {
                    level += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    level -= 1;
                    i += 2;
                    if level == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }
        let start = i;
        let kind = if c == b'\'' || ((c == b'e' || c == b'E') && b.get(i + 1) == Some(&b'\'')) {
            if c != b'\'' {
                i += 1;
            }
            i = quoted_end(b, i + 1, b'\'', c != b'\'');
            Tok::Str
        } else if c == b'"' {
            i = quoted_end(b, i + 1, b'"', false);
            Tok::Quoted
        } else if c == b'$' {
            match dollar_tag(sql, i) {
                Some(tag) => {
                    let body = i + tag.len();
                    i = sql[body..].find(tag).map(|n| body + n + tag.len()).unwrap_or(b.len());
                    Tok::Str
                }
                None => {
                    i += 1;
                    while i < b.len() && is_word_byte(b[i]) {
                        i += 1;
                    }
                    Tok::Param
                }
            }
        } else if c == b'?' {
            i += 1;
            Tok::Param
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
            Tok::Number
        } else if is_word_byte(c) {
            while i < b.len() && is_word_byte(b[i]) {
                i += 1;
            }
            Tok::Word
        } else {
            let ch = sql[i..].chars().next().unwrap_or(' ');
            i += ch.len_utf8();
            Tok::Punct(ch)
        };
        let token_depth = match kind {
            Tok::Punct(')' | ']' | '}') => {
                depth = depth.saturating_sub(1);
                depth
            }
            _ => depth,
        };
        if matches!(kind, Tok::Punct('(' | '[' | '{')) {
            depth += 1;
        }
        out.push(Token { kind, span: start..i, depth: token_depth });
    }
    out
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// End of a quoted run starting after its opening quote; a doubled quote is an escaped one.
/// With `backslash`, `\x` is an escape too (E'' strings).
fn quoted_end(b: &[u8], mut i: usize, q: u8, backslash: bool) -> usize {
    while i < b.len() {
        if backslash && b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == q {
            if b.get(i + 1) == Some(&q) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    b.len()
}

/// `$$` or `$tag$` at `i`, if a dollar-quoted string starts there.
fn dollar_tag(sql: &str, i: usize) -> Option<&str> {
    let b = sql.as_bytes();
    let mut j = i + 1;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
        j += 1;
    }
    let tag_ok = j == i + 1 || !b[i + 1].is_ascii_digit();
    (j < b.len() && b[j] == b'$' && tag_ok).then(|| &sql[i..=j])
}

/// The statement without leading/trailing whitespace, trailing comments and one trailing
/// top-level `;`. Anything else (a second statement) is left for the parser to reject.
pub(crate) fn trim_statement(sql: &str) -> &str {
    let toks = tokens(sql);
    let mut end = toks.len();
    if end > 0 && toks[end - 1].kind == Tok::Punct(';') && toks[end - 1].depth == 0 {
        end -= 1;
    }
    match (toks.first(), end.checked_sub(1).and_then(|e| toks.get(e))) {
        (Some(first), Some(last)) if end > 0 => &sql[first.span.start..last.span.end],
        _ => "",
    }
}

/// How many statements `sql` holds (top-level `;` separators with something after them).
pub(crate) fn statement_count(sql: &str) -> usize {
    let toks = tokens(sql);
    let mut n = 0;
    let mut open = false;
    for t in &toks {
        if t.kind == Tok::Punct(';') && t.depth == 0 {
            if open {
                n += 1;
            }
            open = false;
        } else {
            open = true;
        }
    }
    n + usize::from(open)
}

/// The first word of the statement, upper-cased (`"SELECT"`, `"UPDATE"`, …).
pub(crate) fn first_keyword(sql: &str) -> Option<String> {
    tokens(sql).into_iter().find(|t| t.kind != Tok::Punct('(')).filter(|t| t.kind == Tok::Word).map(|t| t.text(sql).to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(sql: &str) -> Vec<String> {
        tokens(sql).iter().map(|t| t.text(sql).to_string()).collect()
    }

    #[test]
    fn strings_comments_and_quotes_are_single_tokens() {
        assert_eq!(words("SELECT 'a;b''c' -- x;\n, \"q\"\"x\" /* y; /* z */ ; */ ;"), ["SELECT", "'a;b''c'", ",", "\"q\"\"x\"", ";"]);
        assert_eq!(words("SELECT E'it\\'s;' , $$a;b$$, $t$x$t$, $p, ?"), ["SELECT", "E'it\\'s;'", ",", "$$a;b$$", ",", "$t$x$t$", ",", "$p", ",", "?"]);
        assert_eq!(words("1.5e3 .5 x::INT"), ["1.5e3", ".5", "x", ":", ":", "INT"]);
        assert_eq!(words("'unterminated ; DROP"), ["'unterminated ; DROP"]);
    }

    #[test]
    fn depth_tracks_parentheses() {
        let sql = "a (b (c) d) e";
        let d: Vec<u32> = tokens(sql).iter().map(|t| t.depth).collect();
        assert_eq!(d, [0, 0, 1, 1, 2, 1, 1, 0, 0]);
        let sql = "[1, {'a': 2}], x";
        let d: Vec<u32> = tokens(sql).iter().map(|t| t.depth).collect();
        assert_eq!(d, [0, 1, 1, 1, 2, 2, 2, 1, 0, 0, 0]);
    }

    #[test]
    fn trimming_and_first_keyword() {
        assert_eq!(trim_statement("  SELECT 1 ; -- done\n"), "SELECT 1");
        assert_eq!(trim_statement("SELECT ';' /* c */"), "SELECT ';'");
        assert_eq!(trim_statement("SELECT 1; SELECT 2;"), "SELECT 1; SELECT 2");
        assert_eq!(trim_statement(" -- nothing"), "");
        assert_eq!(first_keyword("/* c */ (update t set a = 1)"), Some("UPDATE".into()));
        assert_eq!(first_keyword("-- x\nwith a as (select 1) select * from a"), Some("WITH".into()));
        assert_eq!(first_keyword("'x'"), None);
        assert_eq!(statement_count("SELECT 1; DROP TABLE t"), 2);
        assert_eq!(statement_count("SELECT ';'; -- x"), 1);
        assert_eq!(statement_count(";;SELECT 1;;"), 1);
        assert_eq!(statement_count(""), 0);
    }
}
