//! Functions evaluated outside the engine (Alephgrid patch set 2).
//!
//! A host can register an [`ExternalFunctions`] hook on a [`crate::Model`]. A call of a name
//! the hook claims (one that is not a built-in, nor bound by LET) is evaluated by giving
//! the hook the evaluated arguments: scalars as they are, ranges and arrays as rows of
//! values. The hook answers synchronously, with a value or an error; it is expected to
//! answer from results computed elsewhere (Alephgrid keeps a cache filled by background
//! workers) and to say "not yet" with an error, never to block on slow work.

use std::sync::Arc;

use crate::expressions::token::Error;

/// Most cells one range argument may pass to an external function. Whole-row and
/// whole-column references are first trimmed to the sheet's used area.
pub const MAX_EXTERNAL_CELLS: usize = 100_000;

/// An argument or result of an external function.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalValue {
    Number(f64),
    Text(String),
    Boolean(bool),
    /// An empty cell or argument (as a result: empty text).
    Empty,
    Error(Error),
    /// A range or array argument, by rows. Not accepted as a result.
    Array(Vec<Vec<ExternalValue>>),
}

/// What the hook answers for one call.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalResult {
    Value(ExternalValue),
    Error { error: Error, message: String },
}

pub trait ExternalFunctions: Send + Sync {
    /// Whether `name` (upper case) is evaluated by this hook.
    fn has(&self, name: &str) -> bool;
    /// The result of `name(args)`.
    fn call(&self, name: &str, args: &[ExternalValue]) -> ExternalResult;
}

pub type ExternalHook = Arc<dyn ExternalFunctions>;
