//! SQL over one yabiir store, for the queries the book "Append Only" has
//! reached so far: text → [`parse`] → [`Select`] (the AST) → [`plan()`] →
//! [`Plan`] → [`execute`] → [`Rows`].
//!
//! ```text
//! SELECT [DISTINCT] { * | key | value }
//!        [WHERE key = 'k' | WHERE key BETWEEN 'low' AND 'high']
//! ```
//!
//! There is one table, the store, so there is no `FROM`.

mod ast;
mod parse;
mod plan;

use std::fmt;

pub use ast::{Condition, Fields, Select};
pub use parse::{ParseError, parse};
pub use plan::{Plan, Rows, execute, plan};

use yabiir::Engine;

/// Why a query failed: it didn't parse, or the store failed answering it.
#[derive(Debug)]
pub enum Error {
    Parse(ParseError),
    Store(yabiir::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Parse(e) => write!(f, "syntax error {e}"),
            Error::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Parse(e) => Some(e),
            Error::Store(e) => Some(e),
        }
    }
}

impl From<ParseError> for Error {
    fn from(e: ParseError) -> Self {
        Error::Parse(e)
    }
}

impl From<yabiir::Error> for Error {
    fn from(e: yabiir::Error) -> Self {
        Error::Store(e)
    }
}

/// Parse, plan and run one query against `store`.
pub fn query(store: &Engine, text: &str) -> Result<Rows, Error> {
    let select = parse(text)?;
    let plan = plan(&select, store.key_order());
    Ok(execute(&plan, store)?)
}
