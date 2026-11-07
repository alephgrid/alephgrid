//! Formulas copied from other spreadsheets in R1C1 notation (F.04b), rewritten in A1
//! notation for the cell they are pasted into.
//!
//! The clipboard HTML of some spreadsheets carries each formula relative to its own cell:
//! `=R[0]C[-1]*2` is "the cell to the left, times two". This is formula text to formula
//! text, done by a small tokenizer that rewrites only reference tokens: string literals,
//! quoted sheet names, structured-reference brackets, numbers, function and other names
//! are copied as they are. The result is then parsed and checked by the normal sheet
//! formula path (`Sheets::paste_cells`), never trusted as is.

use anyhow::{bail, Result};

use super::{column_name, MAX_COLUMNS, MAX_ROWS};

/// One coordinate of an R1C1 reference: `R5` (absolute), `R[-1]` (relative) or `R`
/// (relative, the same row).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Part {
    Absolute(i64),
    Relative(i64),
}

/// A reference token: a cell (`R1C1`), a whole row (`R1`) or a whole column (`C1`).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Token {
    Cell(Part, Part),
    Row(Part),
    Column(Part),
}

/// `formula` (starting with `=`) written in R1C1 notation, as typed into the 0-based cell
/// (`row`, `column`), in A1 notation. Fails when a reference falls outside the sheet.
pub fn r1c1_to_a1(formula: &str, row: u32, column: u32) -> Result<String> {
    let chars: Vec<char> = formula.chars().collect();
    let mut out = String::with_capacity(formula.len());
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match ch {
            // A string literal or a quoted sheet name: copied up to its closing quote, a
            // doubled quote being one quote inside it.
            '"' | '\'' => {
                let end = quoted_end(&chars, i, ch);
                out.extend(&chars[i..end]);
                i = end;
            }
            // A structured reference's brackets (`Table1[Amount]`): copied whole.
            '[' => {
                let end = bracket_end(&chars, i);
                out.extend(&chars[i..end]);
                i = end;
            }
            c if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) => {
                let end = number_end(&chars, i);
                out.extend(&chars[i..end]);
                i = end;
            }
            c if is_name_start(c) => {
                if let Some((first, end)) = reference(&chars, i) {
                    // `R1:R3` and `C1:C2` are row and column ranges; a lone `R1` or `C2`
                    // is a name, not a reference.
                    if let Some((second, end2)) = range_end(&chars, end, first) {
                        out.push_str(&a1(first, row, column)?);
                        out.push(':');
                        out.push_str(&a1(second, row, column)?);
                        i = end2;
                        continue;
                    }
                    if let Token::Cell(..) = first {
                        out.push_str(&a1(first, row, column)?);
                        i = end;
                        continue;
                    }
                }
                let end = name_end(&chars, i);
                out.extend(&chars[i..end]);
                i = end;
            }
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }
    Ok(out)
}

fn is_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '\\'
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.' || c == '\\' || c == '?'
}

fn name_end(chars: &[char], from: usize) -> usize {
    let mut i = from;
    while i < chars.len() && is_name_char(chars[i]) {
        i += 1;
    }
    i
}

fn quoted_end(chars: &[char], from: usize, quote: char) -> usize {
    let mut i = from + 1;
    while i < chars.len() {
        if chars[i] == quote {
            if chars.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    chars.len()
}

fn bracket_end(chars: &[char], from: usize) -> usize {
    let mut depth = 0;
    for (i, &c) in chars.iter().enumerate().skip(from) {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    chars.len()
}

fn number_end(chars: &[char], from: usize) -> usize {
    let mut i = from;
    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
        i += 1;
    }
    // An exponent: 1.5E+3.
    if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
        let mut j = i + 1;
        if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
            j += 1;
        }
        if j < chars.len() && chars[j].is_ascii_digit() {
            i = j;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    i
}

/// The coordinate after an `R` or `C` at `from`: `[n]`, `n` or nothing.
fn part(chars: &[char], from: usize) -> Option<(Part, usize)> {
    match chars.get(from) {
        Some('[') => {
            let mut i = from + 1;
            let negative = chars.get(i) == Some(&'-');
            if negative || chars.get(i) == Some(&'+') {
                i += 1;
            }
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i == start || i - start > 9 || chars.get(i) != Some(&']') {
                return None;
            }
            let n: i64 = chars[start..i].iter().collect::<String>().parse().ok()?;
            Some((Part::Relative(if negative { -n } else { n }), i + 1))
        }
        Some(c) if c.is_ascii_digit() => {
            let start = from;
            let mut i = from;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i - start > 9 {
                return None;
            }
            let n: i64 = chars[start..i].iter().collect::<String>().parse().ok()?;
            Some((Part::Absolute(n), i))
        }
        _ => Some((Part::Relative(0), from)),
    }
}

/// A reference token starting at `from` (an `R` or a `C`), and where it ends. It must not
/// run on into a longer name (`RATE`, `R1C1X`) or be a function call (`C2(`).
fn reference(chars: &[char], from: usize) -> Option<(Token, usize)> {
    let upper = |i: usize| chars.get(i).map(|c| c.to_ascii_uppercase());
    let token = match upper(from)? {
        'R' => {
            let (r, next) = part(chars, from + 1)?;
            if upper(next) == Some('C') {
                let (c, end) = part(chars, next + 1)?;
                (Token::Cell(r, c), end)
            } else {
                // A bare `R` is not a row reference here (it is a name).
                if next == from + 1 {
                    return None;
                }
                (Token::Row(r), next)
            }
        }
        'C' => {
            let (c, next) = part(chars, from + 1)?;
            if next == from + 1 {
                return None;
            }
            (Token::Column(c), next)
        }
        _ => return None,
    };
    let end = token.1;
    match chars.get(end) {
        Some(&c) if is_name_char(c) || c == '(' || c == '!' => None,
        _ => Some(token),
    }
}

/// `:` and a second row (column) token after a first row (column) token.
fn range_end(chars: &[char], at: usize, first: Token) -> Option<(Token, usize)> {
    if chars.get(at) != Some(&':') {
        return None;
    }
    let (second, end) = reference(chars, at + 1)?;
    match (first, second) {
        (Token::Row(_), Token::Row(_)) | (Token::Column(_), Token::Column(_)) => Some((second, end)),
        _ => None,
    }
}

/// The 1-based row or column a coordinate names from the 0-based `at`, and whether it is absolute.
fn resolve(p: Part, at: u32, max: u32, what: &str) -> Result<(u32, bool)> {
    let (n, absolute) = match p {
        Part::Absolute(n) => (n, true),
        Part::Relative(d) => (at as i64 + 1 + d, false),
    };
    if n < 1 || n > max as i64 {
        bail!("the formula refers to a {what} outside the sheet");
    }
    Ok((n as u32, absolute))
}

fn a1(t: Token, row: u32, column: u32) -> Result<String> {
    let dollar = |absolute: bool| if absolute { "$" } else { "" };
    Ok(match t {
        Token::Cell(r, c) => {
            let (r, ra) = resolve(r, row, MAX_ROWS, "row")?;
            let (c, ca) = resolve(c, column, MAX_COLUMNS, "column")?;
            format!("{}{}{}{}", dollar(ca), column_name(c - 1), dollar(ra), r)
        }
        Token::Row(r) => {
            let (r, ra) = resolve(r, row, MAX_ROWS, "row")?;
            format!("{}{}", dollar(ra), r)
        }
        Token::Column(c) => {
            let (c, ca) = resolve(c, column, MAX_COLUMNS, "column")?;
            format!("{}{}", dollar(ca), column_name(c - 1))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::r1c1_to_a1;

    /// Expected texts written out by hand: the paste target is C5 (row 4, column 2, 0-based)
    /// unless said otherwise.
    #[test]
    fn relative_absolute_and_mixed_references() {
        let at = |f: &str| r1c1_to_a1(f, 4, 2).unwrap();
        assert_eq!(at("=R[0]C[-1]*2"), "=B5*2");
        assert_eq!(at("=RC[-2]+R[-4]C"), "=A5+C1");
        assert_eq!(at("=R1C1"), "=$A$1");
        assert_eq!(at("=R[1]C2+R3C[1]"), "=$B6+D$3");
        assert_eq!(at("=SUM(R[-3]C[0]:R[-1]C[0])"), "=SUM(C2:C4)");
        assert_eq!(at("=r[0]c[1]"), "=D5", "lower case as well");
        assert_eq!(at("=R[+2]C[+0]"), "=C7");
        // Whole rows and columns.
        assert_eq!(at("=SUM(R1:R3)"), "=SUM($1:$3)");
        assert_eq!(at("=SUM(C[-2]:C[-1])+COUNT(C4:C4)"), "=SUM(A:B)+COUNT($D:$D)");
    }

    #[test]
    fn other_sheets_strings_names_and_numbers_stay() {
        let at = |f: &str| r1c1_to_a1(f, 4, 2).unwrap();
        assert_eq!(at("='Other sheet'!R[0]C[0]+Data!R2C3"), "='Other sheet'!C5+Data!$C$2");
        assert_eq!(at("=\"R1C1 and RC[-1]\"&R[0]C[-1]"), "=\"R1C1 and RC[-1]\"&B5", "string literals are not rewritten");
        assert_eq!(at("=\"say \"\"RC\"\"\"&RC[1]"), "=\"say \"\"RC\"\"\"&D5", "doubled quotes stay inside the literal");
        assert_eq!(at("='It''s R1C1'!R1C1"), "='It''s R1C1'!$A$1");
        assert_eq!(at("=RATE(R1C1,RC2,R)"), "=RATE($A$1,$B5,R)", "names that start like references stay");
        assert_eq!(at("=R1C1X+C2(1)+R2C"), "=R1C1X+C2(1)+C$2", "longer names and function calls stay");
        assert_eq!(at("=Table1[R1C1]+1.5E+3+R2"), "=Table1[R1C1]+1.5E+3+R2", "brackets, numbers and lone R2 stay");
        assert_eq!(at("={1,2;3,4}"), "={1,2;3,4}");
    }

    #[test]
    fn references_outside_the_sheet_are_refused() {
        assert!(r1c1_to_a1("=R[-1]C[0]", 0, 0).is_err(), "above row 1");
        assert!(r1c1_to_a1("=R[0]C[-3]", 0, 2).is_err(), "left of column A");
        assert!(r1c1_to_a1("=R0C1", 0, 0).is_err());
        assert!(r1c1_to_a1("=R1C99999999", 0, 0).is_err());
        assert_eq!(r1c1_to_a1("=R[-1]C[0]", 1, 0).unwrap(), "=A1");
    }
}
