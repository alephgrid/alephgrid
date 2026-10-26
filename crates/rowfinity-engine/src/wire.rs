//! Exact value transport and input validation.
//!
//! JSON numbers are IEEE doubles in every browser, so only values that survive that
//! conversion exactly are sent as numbers:
//!
//! * integers within ±(2^53 − 1) → JSON number; larger integers → JSON string of digits;
//! * DECIMAL → JSON string with the stored scale (`"1.10"`), never a double;
//! * FLOAT/DOUBLE → JSON number (non-finite values as `"NaN"`, `"inf"`, `"-inf"`);
//! * BOOLEAN → JSON boolean; everything else → its DuckDB text form.

use duckdb::types::ValueRef;
use serde_json::Value;

use crate::model::{Column, Kind};

pub const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    /// Read the DuckDB value as is (small integers, doubles, booleans, text).
    Native,
    /// Read as text; emit a number when it is a safe integer.
    IntText,
    /// Read as text and always emit the text (decimals).
    ExactText,
}

fn upper(sql_type: &str) -> String {
    sql_type.trim().to_ascii_uppercase()
}

pub(crate) fn is_wide_integer(sql_type: &str) -> bool {
    matches!(upper(sql_type).as_str(), "BIGINT" | "UBIGINT" | "HUGEINT" | "UHUGEINT" | "VARINT" | "BIGNUM")
}

pub(crate) fn decimal_scale(sql_type: &str) -> Option<u32> {
    let t = upper(sql_type);
    let args = t.strip_prefix("DECIMAL").or_else(|| t.strip_prefix("NUMERIC"))?;
    let args = args.trim().trim_start_matches('(').trim_end_matches(')');
    match args.split(',').nth(1) {
        Some(s) => s.trim().parse().ok(),
        None => Some(if args.trim().is_empty() { 3 } else { 0 }),
    }
}

pub(crate) fn codec(sql_type: &str) -> Codec {
    if is_wide_integer(sql_type) {
        Codec::IntText
    } else if decimal_scale(sql_type).is_some() {
        Codec::ExactText
    } else {
        Codec::Native
    }
}

/// SQL expression that reads `quoted_column` in the form [`decode`] expects.
pub(crate) fn projection(column: &Column, quoted_column: &str) -> String {
    match (codec(&column.sql_type), column.kind) {
        (Codec::IntText | Codec::ExactText, _) => format!("CAST({quoted_column} AS VARCHAR)"),
        (Codec::Native, Kind::Int) => format!("CAST({quoted_column} AS BIGINT)"),
        (Codec::Native, Kind::Number) => format!("CAST({quoted_column} AS DOUBLE)"),
        (Codec::Native, Kind::Bool) => quoted_column.to_string(),
        _ => format!("CAST({quoted_column} AS VARCHAR)"),
    }
}

pub(crate) fn decode(codec: Codec, v: ValueRef<'_>) -> Value {
    match (codec, v) {
        (_, ValueRef::Null) => Value::Null,
        (Codec::IntText, ValueRef::Text(bytes)) => {
            let text = String::from_utf8_lossy(bytes);
            match text.parse::<i64>() {
                Ok(i) if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&i) => i.into(),
                _ => Value::String(text.into_owned()),
            }
        }
        (_, other) => native(other),
    }
}

fn native(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Boolean(b) => Value::Bool(b),
        ValueRef::TinyInt(i) => i.into(),
        ValueRef::SmallInt(i) => i.into(),
        ValueRef::Int(i) => i.into(),
        ValueRef::BigInt(i) => int(i as i128),
        ValueRef::HugeInt(i) => int(i),
        ValueRef::UTinyInt(i) => i.into(),
        ValueRef::USmallInt(i) => i.into(),
        ValueRef::UInt(i) => i.into(),
        ValueRef::UBigInt(i) => int(i as i128),
        ValueRef::Float(f) => float(f as f64),
        ValueRef::Double(f) => float(f),
        ValueRef::Text(bytes) => Value::String(String::from_utf8_lossy(bytes).into_owned()),
        other => Value::String(format!("{other:?}")),
    }
}

fn int(i: i128) -> Value {
    if (-(MAX_SAFE_INTEGER as i128)..=MAX_SAFE_INTEGER as i128).contains(&i) {
        (i as i64).into()
    } else {
        Value::String(i.to_string())
    }
}

fn float(f: f64) -> Value {
    serde_json::Number::from_f64(f).map(Value::Number).unwrap_or_else(|| Value::String(f.to_string()))
}

/// Reject text that DuckDB would silently round when cast to `column`'s type:
/// fractional values for integer columns and extra digits for decimal columns.
/// Range and format errors are left to DuckDB's own `TRY_CAST`.
pub(crate) fn check_exact_input(column: &Column, text: &str) -> Result<(), String> {
    let t = text.trim();
    let (sign_free, _) = match t.strip_prefix(['+', '-']) {
        Some(rest) => (rest, true),
        None => (t, false),
    };
    let (whole, frac) = match sign_free.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (sign_free, None),
    };
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    let plain = digits(whole) && frac.is_none_or(digits) && !(whole.is_empty() && frac.is_none_or(str::is_empty));
    if column.kind == Kind::Int {
        let frac_is_zero = frac.is_none_or(|f| f.chars().all(|c| c == '0'));
        if !plain || whole.is_empty() || !frac_is_zero {
            return Err(format!("{text:?} is not a valid {} for column {:?} (whole numbers only)", column.sql_type, column.name));
        }
    } else if let Some(scale) = decimal_scale(&column.sql_type) {
        let significant = frac.map(|f| f.trim_end_matches('0').len()).unwrap_or(0) as u32;
        if !plain {
            return Err(format!("{text:?} is not a valid {} for column {:?}", column.sql_type, column.name));
        }
        if significant > scale {
            return Err(format!(
                "{text:?} is not a valid {} for column {:?} (at most {scale} decimal places)",
                column.sql_type, column.name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(sql_type: &str) -> Column {
        Column { name: "c".into(), sql_type: sql_type.into(), kind: Kind::from_sql_type(sql_type), formula: None }
    }

    #[test]
    fn decimal_scales() {
        assert_eq!(decimal_scale("DECIMAL(10,2)"), Some(2));
        assert_eq!(decimal_scale("decimal(38, 10)"), Some(10));
        assert_eq!(decimal_scale("DECIMAL(5)"), Some(0));
        assert_eq!(decimal_scale("DECIMAL"), Some(3));
        assert_eq!(decimal_scale("DOUBLE"), None);
    }

    #[test]
    fn exact_inputs() {
        let int = col("INTEGER");
        for ok in ["0", "-5", "+7", " 42 ", "42.", "42.000"] {
            assert!(check_exact_input(&int, ok).is_ok(), "{ok}");
        }
        for bad in ["1.5", "1e3", "", "-", ".", "1,000", "0x10", "١٢"] {
            assert!(check_exact_input(&int, bad).is_err(), "{bad}");
        }
        let money = col("DECIMAL(10,2)");
        for ok in ["1", "1.2", "1.23", "1.2300", ".5", "-0.01", "5."] {
            assert!(check_exact_input(&money, ok).is_ok(), "{ok}");
        }
        for bad in ["1.234", "1e2", "abc", ".", "", "1.2.3"] {
            assert!(check_exact_input(&money, bad).is_err(), "{bad}");
        }
        assert!(check_exact_input(&col("DOUBLE"), "1e300").is_ok());
        assert!(check_exact_input(&col("VARCHAR"), "anything").is_ok());
    }

    #[test]
    fn safe_integer_boundary() {
        assert_eq!(int(MAX_SAFE_INTEGER as i128), Value::from(MAX_SAFE_INTEGER));
        assert_eq!(int(MAX_SAFE_INTEGER as i128 + 1), Value::String("9007199254740992".into()));
        assert_eq!(int(-(MAX_SAFE_INTEGER as i128) - 1), Value::String("-9007199254740992".into()));
        assert_eq!(decode(Codec::IntText, ValueRef::Text(b"-9007199254740991")), Value::from(-MAX_SAFE_INTEGER));
        assert_eq!(decode(Codec::IntText, ValueRef::Text(b"18446744073709551615")), Value::String("18446744073709551615".into()));
        assert_eq!(decode(Codec::ExactText, ValueRef::Text(b"1.10")), Value::String("1.10".into()));
        assert_eq!(float(f64::NAN), Value::String("NaN".into()));
    }
}
