//! Tokenizer and recursive-descent parser for spreadsheet-syntax table formulas.
//!
//! Positions are character offsets into the formula as typed (including a leading `=`), so
//! error messages can point at the exact place. See docs/TABLE_FORMULAS.md for the grammar.

use std::fmt;

/// A character range `[start, end)` in the formula text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// A refusal or syntax error, with where it applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormulaError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for FormulaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at character {})", self.message, self.span.start + 1)
    }
}

impl std::error::Error for FormulaError {}

pub type ParseResult<T> = Result<T, FormulaError>;

fn err<T>(message: impl Into<String>, start: usize, end: usize) -> ParseResult<T> {
    Err(FormulaError { message: message.into(), span: Span { start, end: end.max(start + 1) } })
}

/// Spreadsheet error values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    Na,
    Div0,
    Value,
    Num,
    Ref,
    Name,
    Null,
}

impl ErrorCode {
    pub fn code(self) -> &'static str {
        match self {
            ErrorCode::Na => "#N/A",
            ErrorCode::Div0 => "#DIV/0!",
            ErrorCode::Value => "#VALUE!",
            ErrorCode::Num => "#NUM!",
            ErrorCode::Ref => "#REF!",
            ErrorCode::Name => "#NAME?",
            ErrorCode::Null => "#NULL!",
        }
    }

    pub fn parse(text: &str) -> Option<ErrorCode> {
        [ErrorCode::Na, ErrorCode::Div0, ErrorCode::Value, ErrorCode::Num, ErrorCode::Ref, ErrorCode::Name, ErrorCode::Null]
            .into_iter()
            .find(|e| e.code().eq_ignore_ascii_case(text))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Plus,
    Percent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinaryOp {
    pub fn is_comparison(self) -> bool {
        matches!(self, BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge)
    }
}

/// One sheet cell, as a table formula reads it: `Sheet1!B2`, `'Q1 plan'!$B$2`. Rows and
/// columns count from 1; sheet names compare case-insensitively.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellRef {
    pub sheet: String,
    pub row: u32,
    pub column: u32,
}

/// Excel's grid: what a sheet cell reference may address.
pub const MAX_SHEET_ROW: u32 = 1_048_576;
pub const MAX_SHEET_COLUMN: u32 = 16_384;

impl CellRef {
    /// The reference as typed in a formula, quoting the sheet name where needed and keeping
    /// `$` markers: `(column fixed, row fixed)`.
    pub fn text(&self, fixed: (bool, bool)) -> String {
        format!("{}!{}{}{}{}", sheet_name_text(&self.sheet), if fixed.0 { "$" } else { "" }, column_letters(self.column), if fixed.1 { "$" } else { "" }, self.row)
    }

    pub fn same(&self, other: &CellRef) -> bool {
        self.row == other.row && self.column == other.column && self.sheet.to_lowercase() == other.sheet.to_lowercase()
    }
}

/// `A`, `Z`, `AA`, … for a column number from 1.
pub fn column_letters(mut column: u32) -> String {
    let mut out = Vec::new();
    while column > 0 {
        let d = (column - 1) % 26;
        out.push((b'A' + d as u8) as char);
        column = (column - 1) / 26;
    }
    out.into_iter().rev().collect()
}

/// A sheet name as written before `!`: bare when it reads as a plain name, else quoted.
pub fn sheet_name_text(name: &str) -> String {
    let mut chars = name.chars();
    let plain = chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && !looks_like_cell(name)
        && !name.eq_ignore_ascii_case("TRUE")
        && !name.eq_ignore_ascii_case("FALSE");
    if plain { name.to_string() } else { format!("'{}'", name.replace('\'', "''")) }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Number(f64),
    Text(String),
    Bool(bool),
    Error(ErrorCode),
    /// A column of the formula's own table. `this_row` is false for `[col]` without `@`,
    /// which means this row except where a function expects a whole column.
    Column { name: String, this_row: bool },
    /// A whole column of a (possibly other) table: `Orders[amount]`.
    TableColumn { table: String, column: String },
    /// A whole table's data rows: `Customers`, `Customers[]` or `Customers[#Data]`.
    Table { table: String },
    /// A single sheet cell; `fixed` records the `$` markers as typed (column, row).
    Cell { cell: CellRef, fixed: (bool, bool) },
    Unary { op: UnaryOp, expr: Box<Expr> },
    Binary { op: BinaryOp, left: Box<Expr>, right: Box<Expr> },
    Call { name: String, args: Vec<Expr> },
    /// An omitted argument, as in `ROUND(x,)`.
    Empty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Number(f64),
    Text(String),
    Ident(String),
    Error(ErrorCode),
    Cell(CellRef, (bool, bool)),
    /// `[...]` after a table name or alone: the raw bracket contents, parsed later.
    Brackets(Vec<BracketItem>),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    End,
}

/// The parts of a structured reference: `[@col]`, `[[#This Row],[col]]`, `[col]`.
#[derive(Debug, Clone, PartialEq)]
enum BracketItem {
    Column(String),
    ThisRow,
    Special(String),
}

struct Lexer<'a> {
    chars: Vec<char>,
    pos: usize,
    _text: &'a str,
    /// The sheet a bare cell such as `A1` is on, when the formula belongs to a sheet cell.
    home: Option<String>,
}

impl<'a> Lexer<'a> {
    fn skip_space(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    /// Next token and its span.
    fn next(&mut self) -> ParseResult<(Tok, Span)> {
        self.skip_space();
        let start = self.pos;
        let Some(c) = self.peek(0) else { return Ok((Tok::End, Span { start, end: start })) };
        let tok = match c {
            '0'..='9' | '.' => self.number()?,
            '"' => self.text()?,
            '#' => self.error_literal()?,
            '[' => Tok::Brackets(self.brackets()?),
            '(' => { self.pos += 1; Tok::LParen }
            ')' => { self.pos += 1; Tok::RParen }
            ',' => { self.pos += 1; Tok::Comma }
            '+' | '-' | '*' | '/' | '^' | '&' | '%' | '=' => {
                self.pos += 1;
                Tok::Op(match c { '+' => "+", '-' => "-", '*' => "*", '/' => "/", '^' => "^", '&' => "&", '%' => "%", _ => "=" })
            }
            '<' => {
                self.pos += 1;
                match self.peek(0) {
                    Some('=') => { self.pos += 1; Tok::Op("<=") }
                    Some('>') => { self.pos += 1; Tok::Op("<>") }
                    _ => Tok::Op("<"),
                }
            }
            '>' => {
                self.pos += 1;
                if self.peek(0) == Some('=') { self.pos += 1; Tok::Op(">=") } else { Tok::Op(">") }
            }
            c if c.is_alphabetic() || c == '_' || c == '\\' => {
                while self.peek(0).is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '\\') {
                    self.pos += 1;
                }
                let name: String = self.chars[start..self.pos].iter().collect();
                if self.peek(0) == Some('!') {
                    self.pos += 1;
                    self.cell(name, start)?
                } else if let Some(home) = self.home.clone().filter(|_| looks_like_cell(&name) && !matches!(self.peek(0), Some('(' | '['))) {
                    self.pos = start;
                    self.cell(home, start)?
                } else {
                    Tok::Ident(name)
                }
            }
            '\'' => {
                self.pos += 1;
                let mut name = String::new();
                loop {
                    match self.peek(0) {
                        None => return err("this sheet name has no closing quote", start, self.pos),
                        Some('\'') if self.peek(1) == Some('\'') => { name.push('\''); self.pos += 2; }
                        Some('\'') => { self.pos += 1; break; }
                        Some(c) => { name.push(c); self.pos += 1; }
                    }
                }
                if self.peek(0) != Some('!') {
                    return err("a quoted sheet name must be followed by ! and a cell, e.g. 'Q1 plan'!B2", start, self.pos);
                }
                self.pos += 1;
                if name.is_empty() {
                    return err("empty sheet name", start, self.pos);
                }
                self.cell(name, start)?
            }
            ':' => return err("ranges such as A1:B4 are not available in table formulas; use Table[column]", start, start + 1),
            '{' => return err("array constants are not available in table formulas", start, start + 1),
            '$' if self.home.is_some() => {
                let home = self.home.clone().unwrap_or_default();
                self.cell(home, start)?
            }
            '$' => return err("a sheet cell needs its sheet name, e.g. Sheet1!$A$1; refer to columns with [@column]", start, start + 1),
            '!' => return err("a sheet name is missing before !, e.g. Sheet1!A1", start, start + 1),
            ';' => return err("separate arguments with commas", start, start + 1),
            other => return err(format!("unexpected character {other:?}"), start, start + 1),
        };
        Ok((tok, Span { start, end: self.pos }))
    }

    /// The cell after `Sheet!`: `B2`, `$B$2`. Ranges are refused.
    fn cell(&mut self, sheet: String, start: usize) -> ParseResult<Tok> {
        let at = self.pos;
        let fixed_column = self.peek(0) == Some('$');
        if fixed_column { self.pos += 1; }
        let letters_at = self.pos;
        while self.peek(0).is_some_and(|c| c.is_ascii_alphabetic()) { self.pos += 1; }
        let letters: String = self.chars[letters_at..self.pos].iter().collect::<String>().to_ascii_uppercase();
        let fixed_row = self.peek(0) == Some('$');
        if fixed_row { self.pos += 1; }
        let digits_at = self.pos;
        while self.peek(0).is_some_and(|c| c.is_ascii_digit()) { self.pos += 1; }
        let digits: String = self.chars[digits_at..self.pos].iter().collect();
        let bad = || err("expected a cell such as A1 or $B$2 after the sheet name", at, at + 1);
        if letters.is_empty() || letters.len() > 3 || digits.is_empty() || self.peek(0).is_some_and(|c| c.is_alphanumeric() || c == '_') {
            return bad();
        }
        let column = letters.bytes().fold(0u32, |n, b| n * 26 + (b - b'A' + 1) as u32);
        let row: u32 = digits.parse().unwrap_or(0);
        if column > MAX_SHEET_COLUMN || row == 0 || row > MAX_SHEET_ROW {
            return err(format!("{letters}{digits} is outside the sheet"), at, self.pos);
        }
        let end = self.pos;
        self.skip_space();
        if self.peek(0) == Some(':') {
            return err("table formulas read single sheet cells; ranges such as Sheet1!A1:B4 are not available", start, self.pos + 1);
        }
        self.pos = end;
        Ok(Tok::Cell(CellRef { sheet, row, column }, (fixed_column, fixed_row)))
    }

    fn number(&mut self) -> ParseResult<Tok> {
        let start = self.pos;
        while self.peek(0).is_some_and(|c| c.is_ascii_digit()) { self.pos += 1; }
        if self.peek(0) == Some('.') {
            self.pos += 1;
            while self.peek(0).is_some_and(|c| c.is_ascii_digit()) { self.pos += 1; }
        }
        if matches!(self.peek(0), Some('e' | 'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(0), Some('+' | '-')) { self.pos += 1; }
            if self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                while self.peek(0).is_some_and(|c| c.is_ascii_digit()) { self.pos += 1; }
            } else {
                self.pos = save;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        match text.parse::<f64>() {
            Ok(n) if n.is_finite() && text != "." => Ok(Tok::Number(n)),
            _ => err(format!("{text:?} is not a number"), start, self.pos),
        }
    }

    fn text(&mut self) -> ParseResult<Tok> {
        let start = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek(0) {
                None => return err("this text has no closing quote", start, self.pos),
                Some('"') if self.peek(1) == Some('"') => { out.push('"'); self.pos += 2; }
                Some('"') => { self.pos += 1; return Ok(Tok::Text(out)); }
                Some(c) => { out.push(c); self.pos += 1; }
            }
        }
    }

    fn error_literal(&mut self) -> ParseResult<Tok> {
        let start = self.pos;
        for len in [7, 6, 5, 4] {
            let candidate: String = self.chars[start..(start + len).min(self.chars.len())].iter().collect();
            if let Some(code) = ErrorCode::parse(&candidate) {
                self.pos = start + len;
                return Ok(Tok::Error(code));
            }
        }
        err("unknown error value", start, start + 1)
    }

    /// Reads `[...]` including nested `[[a],[b]]`; `'` escapes the next character.
    fn brackets(&mut self) -> ParseResult<Vec<BracketItem>> {
        let start = self.pos;
        self.pos += 1;
        let read_name = |lx: &mut Lexer, open: usize| -> ParseResult<String> {
            let mut s = String::new();
            loop {
                match lx.peek(0) {
                    None => return err("this reference has no closing ]", open, lx.pos),
                    Some('\'') => {
                        let Some(c) = lx.peek(1) else { return err("nothing to escape after '", lx.pos, lx.pos + 1) };
                        s.push(c);
                        lx.pos += 2;
                    }
                    Some(']') => { lx.pos += 1; return Ok(s); }
                    Some('[') => return err("unexpected [ inside a column name; escape it as '[", lx.pos, lx.pos + 1),
                    Some(c) => { s.push(c); lx.pos += 1; }
                }
            }
        };
        let classify = |raw: String, at: usize| -> ParseResult<BracketItem> {
            let t = raw.trim();
            if let Some(special) = t.strip_prefix('#') {
                return if special.eq_ignore_ascii_case("this row") { Ok(BracketItem::ThisRow) } else { Ok(BracketItem::Special(t.to_string())) };
            }
            if t.is_empty() { return err("empty column name", at, at + 2); }
            Ok(BracketItem::Column(t.to_string()))
        };
        self.skip_space();
        let mut items = Vec::new();
        if self.peek(0) == Some('[') {
            // [[a],[b]] or [@[a]]
            loop {
                self.skip_space();
                let at = self.pos;
                if self.peek(0) != Some('[') { return err("expected [ in a structured reference", at, at + 1); }
                self.pos += 1;
                let name = read_name(self, at)?;
                items.push(classify(name, at)?);
                self.skip_space();
                match self.peek(0) {
                    Some(',') => { self.pos += 1; }
                    Some(']') => { self.pos += 1; break; }
                    _ => return err("expected , or ] in a structured reference", self.pos, self.pos + 1),
                }
            }
            return Ok(items);
        }
        if self.peek(0) == Some('@') {
            self.pos += 1;
            items.push(BracketItem::ThisRow);
            self.skip_space();
            if self.peek(0) == Some('[') {
                let at = self.pos;
                self.pos += 1;
                let name = read_name(self, at)?;
                items.push(classify(name, at)?);
                self.skip_space();
                if self.peek(0) != Some(']') { return err("expected ] after the column", self.pos, self.pos + 1); }
                self.pos += 1;
                return Ok(items);
            }
        }
        let name = read_name(self, start)?;
        items.push(classify(name, start)?);
        Ok(items)
    }
}

pub struct Parser<'a> {
    lexer: Lexer<'a>,
    tok: Tok,
    span: Span,
}

/// Parse a formula as typed (a leading `=` is optional).
pub fn parse(formula: &str) -> ParseResult<Expr> {
    parse_in(formula, None)
}

/// Like [`parse`], for a formula of a sheet cell on sheet `home`: bare cells (`A1`, `$B$2`)
/// are on that sheet.
pub fn parse_in(formula: &str, home: Option<&str>) -> ParseResult<Expr> {
    let chars: Vec<char> = formula.chars().collect();
    let mut lexer = Lexer { chars, pos: 0, _text: formula, home: home.map(str::to_string) };
    lexer.skip_space();
    if lexer.peek(0) == Some('=') { lexer.pos += 1; }
    let mut p = Parser { lexer, tok: Tok::End, span: Span { start: 0, end: 0 } };
    p.advance()?;
    if p.tok == Tok::End {
        return err("the formula is empty", 0, formula.chars().count());
    }
    let e = p.comparison()?;
    if p.tok != Tok::End {
        return err("unexpected input after the end of the formula", p.span.start, p.span.end);
    }
    Ok(e)
}

impl Parser<'_> {
    fn advance(&mut self) -> ParseResult<()> {
        let (t, s) = self.lexer.next()?;
        self.tok = t;
        self.span = s;
        Ok(())
    }

    fn op(&self) -> Option<&'static str> {
        if let Tok::Op(o) = self.tok { Some(o) } else { None }
    }

    fn binary(&mut self, op: BinaryOp, left: Expr, next: fn(&mut Self) -> ParseResult<Expr>) -> ParseResult<Expr> {
        self.advance()?;
        let right = next(self)?;
        let span = Span { start: left.span.start, end: right.span.end };
        Ok(Expr { kind: ExprKind::Binary { op, left: Box::new(left), right: Box::new(right) }, span })
    }

    fn comparison(&mut self) -> ParseResult<Expr> {
        let mut left = self.concat()?;
        loop {
            let op = match self.op() {
                Some("=") => BinaryOp::Eq, Some("<>") => BinaryOp::Ne, Some("<") => BinaryOp::Lt,
                Some("<=") => BinaryOp::Le, Some(">") => BinaryOp::Gt, Some(">=") => BinaryOp::Ge,
                _ => return Ok(left),
            };
            left = self.binary(op, left, Self::concat)?;
        }
    }

    fn concat(&mut self) -> ParseResult<Expr> {
        let mut left = self.additive()?;
        while self.op() == Some("&") {
            left = self.binary(BinaryOp::Concat, left, Self::additive)?;
        }
        Ok(left)
    }

    fn additive(&mut self) -> ParseResult<Expr> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.op() { Some("+") => BinaryOp::Add, Some("-") => BinaryOp::Sub, _ => return Ok(left) };
            left = self.binary(op, left, Self::multiplicative)?;
        }
    }

    fn multiplicative(&mut self) -> ParseResult<Expr> {
        let mut left = self.power()?;
        loop {
            let op = match self.op() { Some("*") => BinaryOp::Mul, Some("/") => BinaryOp::Div, _ => return Ok(left) };
            left = self.binary(op, left, Self::power)?;
        }
    }

    /// `^` is left-associative in spreadsheets: `2^3^2` is 64.
    fn power(&mut self) -> ParseResult<Expr> {
        let mut left = self.percent()?;
        while self.op() == Some("^") {
            left = self.binary(BinaryOp::Pow, left, Self::percent)?;
        }
        Ok(left)
    }

    fn percent(&mut self) -> ParseResult<Expr> {
        let mut e = self.unary()?;
        while self.op() == Some("%") {
            let end = self.span.end;
            self.advance()?;
            let span = Span { start: e.span.start, end };
            e = Expr { kind: ExprKind::Unary { op: UnaryOp::Percent, expr: Box::new(e) }, span };
        }
        Ok(e)
    }

    /// Negation binds tighter than `^`: `-2^2` is 4.
    fn unary(&mut self) -> ParseResult<Expr> {
        let op = match self.op() { Some("-") => Some(UnaryOp::Neg), Some("+") => Some(UnaryOp::Plus), _ => None };
        if let Some(op) = op {
            let start = self.span.start;
            self.advance()?;
            let e = self.unary()?;
            let span = Span { start, end: e.span.end };
            return Ok(Expr { kind: ExprKind::Unary { op, expr: Box::new(e) }, span });
        }
        self.primary()
    }

    fn primary(&mut self) -> ParseResult<Expr> {
        let span = self.span;
        let tok = std::mem::replace(&mut self.tok, Tok::End);
        let done = |p: &mut Self, kind: ExprKind| -> ParseResult<Expr> { p.advance()?; Ok(Expr { kind, span }) };
        match tok {
            Tok::Number(n) => done(self, ExprKind::Number(n)),
            Tok::Text(s) => done(self, ExprKind::Text(s)),
            Tok::Error(e) => done(self, ExprKind::Error(e)),
            Tok::Cell(cell, fixed) => done(self, ExprKind::Cell { cell, fixed }),
            Tok::Brackets(items) => {
                self.advance()?;
                let kind = this_table_reference(items, span)?;
                Ok(Expr { kind, span: Span { start: span.start, end: span.end } })
            }
            Tok::LParen => {
                self.advance()?;
                let e = self.comparison()?;
                if self.tok != Tok::RParen {
                    return err("expected )", self.span.start, self.span.end);
                }
                let end = self.span.end;
                self.advance()?;
                Ok(Expr { kind: e.kind, span: Span { start: span.start, end } })
            }
            Tok::Ident(name) => {
                self.advance()?;
                match &self.tok {
                    Tok::LParen => self.call(name, span),
                    Tok::Brackets(items) => {
                        let items = items.clone();
                        let end = self.span.end;
                        self.advance()?;
                        let kind = table_reference(&name, items, Span { start: span.start, end })?;
                        Ok(Expr { kind, span: Span { start: span.start, end } })
                    }
                    _ if name.eq_ignore_ascii_case("TRUE") => Ok(Expr { kind: ExprKind::Bool(true), span }),
                    _ if name.eq_ignore_ascii_case("FALSE") => Ok(Expr { kind: ExprKind::Bool(false), span }),
                    _ if looks_like_cell(&name) => err(format!("{name} needs its sheet name, e.g. Sheet1!{name}; refer to columns with [@column]"), span.start, span.end),
                    // A bare name is a table, as in spreadsheets; binding explains other uses.
                    _ => Ok(Expr { kind: ExprKind::Table { table: name }, span }),
                }
            }
            Tok::End => err("the formula ends too early", span.start, span.end),
            Tok::RParen => err("unexpected )", span.start, span.end),
            Tok::Comma => err("unexpected ,", span.start, span.end),
            Tok::Op(o) => err(format!("unexpected {o}"), span.start, span.end),
        }
    }

    fn call(&mut self, name: String, name_span: Span) -> ParseResult<Expr> {
        self.advance()?; // (
        let mut args = Vec::new();
        if self.tok == Tok::RParen {
            let end = self.span.end;
            self.advance()?;
            return Ok(Expr { kind: ExprKind::Call { name: name.to_uppercase(), args }, span: Span { start: name_span.start, end } });
        }
        loop {
            if matches!(self.tok, Tok::Comma | Tok::RParen) {
                args.push(Expr { kind: ExprKind::Empty, span: Span { start: self.span.start, end: self.span.start } });
            } else {
                args.push(self.comparison()?);
            }
            match self.tok {
                Tok::Comma => self.advance()?,
                Tok::RParen => {
                    let end = self.span.end;
                    self.advance()?;
                    return Ok(Expr { kind: ExprKind::Call { name: name.to_uppercase(), args }, span: Span { start: name_span.start, end } });
                }
                Tok::End => return err(format!("{name}( has no closing )"), name_span.start, self.span.start),
                _ => return err("expected , or )", self.span.start, self.span.end),
            }
        }
    }
}

fn looks_like_cell(name: &str) -> bool {
    let letters = name.chars().take_while(|c| c.is_ascii_alphabetic()).count();
    (1..=3).contains(&letters) && name.len() > letters && name[letters..].chars().all(|c| c.is_ascii_digit())
}

/// `[@col]`, `[col]`, `[[#This Row],[col]]` without a table name.
fn this_table_reference(items: Vec<BracketItem>, span: Span) -> ParseResult<ExprKind> {
    match items.as_slice() {
        [BracketItem::Column(c)] => Ok(ExprKind::Column { name: c.clone(), this_row: false }),
        [BracketItem::ThisRow, BracketItem::Column(c)] => Ok(ExprKind::Column { name: c.clone(), this_row: true }),
        [BracketItem::ThisRow] => err("[@] needs a column name, e.g. [@amount]", span.start, span.end),
        _ => special(&items, span),
    }
}

/// `Table[col]`, `Table[[col]]`, `Table[[#This Row],[col]]`, `Table[@col]`.
fn table_reference(table: &str, items: Vec<BracketItem>, span: Span) -> ParseResult<ExprKind> {
    match items.as_slice() {
        [] => Ok(ExprKind::Table { table: table.to_string() }),
        [BracketItem::Special(s)] if s.eq_ignore_ascii_case("#data") => Ok(ExprKind::Table { table: table.to_string() }),
        [BracketItem::Column(c)] => Ok(ExprKind::TableColumn { table: table.to_string(), column: c.clone() }),
        // This row of a named table: only meaningful for the formula's own table; checked when binding.
        [BracketItem::ThisRow, BracketItem::Column(c)] => Ok(ExprKind::Call {
            name: "\u{0}THISROW".into(),
            args: vec![Expr { kind: ExprKind::Text(table.to_string()), span }, Expr { kind: ExprKind::Column { name: c.clone(), this_row: true }, span }],
        }),
        _ => special(&items, span),
    }
}

fn special(items: &[BracketItem], span: Span) -> ParseResult<ExprKind> {
    if let Some(BracketItem::Special(s)) = items.iter().find(|i| matches!(i, BracketItem::Special(_))) {
        return err(format!("{s} is not available in table formulas; use [@column] or Table[column]"), span.start, span.end);
    }
    err("multi-column references are not available in table formulas", span.start, span.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(e: &Expr) -> String {
        match &e.kind {
            ExprKind::Number(n) => format!("{n}"),
            ExprKind::Text(s) => format!("{s:?}"),
            ExprKind::Bool(b) => format!("{b}"),
            ExprKind::Error(c) => c.code().to_string(),
            ExprKind::Column { name, this_row } => format!("[{}{name}]", if *this_row { "@" } else { "" }),
            ExprKind::TableColumn { table, column } => format!("{table}[{column}]"),
            ExprKind::Table { table } => format!("{table}[#Data]"),
            ExprKind::Cell { cell, fixed } => format!("<{}>", cell.text(*fixed)),
            ExprKind::Unary { op, expr } => format!("({op:?} {})", shape(expr)),
            ExprKind::Binary { op, left, right } => format!("({op:?} {} {})", shape(left), shape(right)),
            ExprKind::Call { name, args } => format!("{}({})", name.replace('\u{0}', ""), args.iter().map(shape).collect::<Vec<_>>().join(",")),
            ExprKind::Empty => "_".into(),
        }
    }

    fn p(s: &str) -> String {
        shape(&parse(s).unwrap_or_else(|e| panic!("{s}: {e}")))
    }

    #[test]
    fn precedence_follows_spreadsheets() {
        assert_eq!(p("=-2^2"), "(Pow (Neg 2) 2)");
        assert_eq!(p("=2^3^2"), "(Pow (Pow 2 3) 2)");
        assert_eq!(p("=1+2*3"), "(Add 1 (Mul 2 3))");
        assert_eq!(p("=1&2+3"), "(Concat 1 (Add 2 3))");
        assert_eq!(p("=1+2=3"), "(Eq (Add 1 2) 3)");
        assert_eq!(p("=50%*2"), "(Mul (Percent 50) 2)");
        assert_eq!(p("=-[@a]%"), "(Percent (Neg [@a]))");
        assert_eq!(p("=(1+2)*3"), "(Mul (Add 1 2) 3)");
        assert_eq!(p("1<>2"), "(Ne 1 2)");
    }

    #[test]
    fn literals_and_references() {
        assert_eq!(p(r#"="say ""hi""""#), r#""say \"hi\"""#);
        assert_eq!(p("=.5+2E3+1e-2"), "(Add (Add 0.5 2000) 0.01)");
        assert_eq!(p("=TRUE"), "true");
        assert_eq!(p("=#N/A"), "#N/A");
        assert_eq!(p("=#div/0!"), "#DIV/0!");
        assert_eq!(p("=[@amount]+[amount]"), "(Add [@amount] [amount])");
        assert_eq!(p("=[@[unit price]]"), "[@unit price]");
        assert_eq!(p("=[@[a'[1']]]"), "[@a[1]]");
        assert_eq!(p("=[@[it''s]]"), "[@it's]");
        assert_eq!(p("=Orders[amount]"), "Orders[amount]");
        assert_eq!(p("=Orders[[amount]]"), "Orders[amount]");
        assert_eq!(p("=Orders[[#This Row],[amount]]"), "THISROW(\"Orders\",[@amount])");
        assert_eq!(p("=round( [@x] , 2 )"), "ROUND([@x],2)");
        assert_eq!(p("=ROUND([@x],)"), "ROUND([@x],_)");
        assert_eq!(p("=NOW()"), "NOW()");
        // A bare name is a whole table, as in spreadsheets; so is Table[#Data].
        assert_eq!(p("=VLOOKUP(1,Customers,2,FALSE)"), "VLOOKUP(1,Customers[#Data],2,false)");
        assert_eq!(p("=Customers[#Data]"), "Customers[#Data]");
        // Single sheet cells, with their sheet; `$` markers are kept for rewriting.
        assert_eq!(p("=[@a]*Settings!B2"), "(Mul [@a] <Settings!B2>)");
        assert_eq!(p("=settings!$b$2"), "<settings!$B$2>");
        assert_eq!(p("=Sheet1!A$7+Sheet1!$XFD1048576"), "(Add <Sheet1!A$7> <Sheet1!$XFD1048576>)");
        assert_eq!(p("='Q1 plan'!C3"), "<'Q1 plan'!C3>");
        assert_eq!(p("='It''s'!C3"), "<'It''s'!C3>");
        assert_eq!(p("='A1'!C3"), "<'A1'!C3>");
        assert_eq!(p("='Sheet1'!C3"), "<Sheet1!C3>");
    }

    #[test]
    fn cell_text_round_trips() {
        for (sheet, plain) in [("Sheet1", true), ("Q1 plan", false), ("It's", false), ("A1", false), ("true", false), ("1st", false), ("Prix_é", true), ("Prix €", false), ("a.b", true)] {
            let cell = CellRef { sheet: sheet.into(), row: 12, column: 28 };
            let text = cell.text((true, false));
            assert_eq!(!text.starts_with('\''), plain, "{text}");
            let ExprKind::Cell { cell: back, fixed } = parse(&format!("={text}")).unwrap().kind else { panic!("{text}") };
            assert_eq!((back, fixed), (cell, (true, false)), "{text}");
        }
        assert_eq!(column_letters(1), "A");
        assert_eq!(column_letters(26), "Z");
        assert_eq!(column_letters(27), "AA");
        assert_eq!(column_letters(16_384), "XFD");
    }

    #[test]
    fn errors_point_at_the_problem() {
        let e = |s: &str| parse(s).unwrap_err();
        assert_eq!(e("=1+").span.start, 3);
        assert_eq!(e("=1 2").span.start, 3);
        assert!(e("=A1+1").message.contains("Sheet1!A1"));
        assert!(e("=SUM(A1:A3)").message.contains("ranges"));
        assert!(e("=[@a]:[@b]").message.contains("ranges"));
        assert!(e("=Sheet1!A1:B2").message.contains("single sheet cells"));
        assert!(e("=Sheet1!A1 : B2").message.contains("single sheet cells"));
        assert!(e("=$A$1").message.contains("sheet name"));
        assert!(e("=!A1").message.contains("sheet name is missing"));
        assert!(e("=Sheet1!").message.contains("expected a cell"));
        assert!(e("=Sheet1!A").message.contains("expected a cell"));
        assert!(e("=Sheet1!1").message.contains("expected a cell"));
        assert!(e("=Sheet1!A1B").message.contains("expected a cell"));
        assert!(e("=Sheet1!ABCD1").message.contains("expected a cell"));
        assert!(e("=Sheet1!XFE1").message.contains("outside the sheet"));
        assert!(e("=Sheet1!A0").message.contains("outside the sheet"));
        assert!(e("=Sheet1!A1048577").message.contains("outside the sheet"));
        assert!(e("='Q1").message.contains("no closing quote"));
        assert!(e("='Q1'+1").message.contains("followed by !"));
        assert!(e("=''!A1").message.contains("empty sheet name"));
        assert!(e("=\"open").message.contains("closing quote"));
        assert!(e("=[@a").message.contains("closing ]"));
        assert!(e("=T[[#Headers],[a]]").message.contains("#Headers"));
        assert!(e("=T[[a],[b]]").message.contains("multi-column"));
        assert!(e("={1,2}").message.contains("array"));
        assert!(e("=SUM(1;2)").message.contains("commas"));
        assert!(e("=").message.contains("empty"));
        assert!(e("=SUM(1,2").message.contains("no closing )"));
        assert_eq!(e("=(1+2").span.start, 5);
        assert_eq!(e("=#BOGUS").span.start, 1);
    }
}
