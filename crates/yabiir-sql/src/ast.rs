//! The abstract syntax tree: what a query says, with the spelling gone.
//!
//! `select distinct VALUE  where KEY between 'a' and 'c'` and
//! `SELECT DISTINCT value WHERE key BETWEEN 'a' AND 'c'` are different text
//! and the same tree. The tree says nothing about how to answer the query;
//! that's the planner's job ([`crate::plan()`]).

use std::fmt;

/// A `SELECT`, the only statement there is so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Select {
    /// `SELECT DISTINCT`: drop rows equal to one already returned.
    pub distinct: bool,
    /// What comes after `SELECT`: the projection.
    pub fields: Fields,
    /// The `WHERE` clause, if there is one: the selection.
    pub condition: Option<Condition>,
}

/// The fields a query keeps. The store is one table with two fields, so
/// these are all the projections there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fields {
    /// `*`: both fields, π_{key,value}.
    All,
    /// `key`: π_key.
    Key,
    /// `value`: π_value.
    Value,
}

impl Fields {
    /// The names of the result's columns, in order.
    pub fn columns(self) -> &'static [&'static str] {
        match self {
            Fields::All => &["key", "value"],
            Fields::Key => &["key"],
            Fields::Value => &["value"],
        }
    }

    /// Whether the result carries the key, which no two rows share — so
    /// no two result rows can be equal, and `DISTINCT` has nothing to do.
    pub fn includes_key(self) -> bool {
        matches!(self, Fields::All | Fields::Key)
    }
}

/// A condition on the key: what a `WHERE` clause can say so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// `WHERE key = 'k'`.
    KeyEquals(Vec<u8>),
    /// `WHERE key BETWEEN 'low' AND 'high'`, both ends included, as in SQL.
    KeyBetween(Vec<u8>, Vec<u8>),
}

impl Condition {
    /// Whether a row with this key satisfies the condition.
    pub fn holds_for(&self, key: &[u8]) -> bool {
        match self {
            Condition::KeyEquals(k) => key == k.as_slice(),
            Condition::KeyBetween(low, high) => low.as_slice() <= key && key <= high.as_slice(),
        }
    }
}

/// Back to SQL, in one canonical spelling.
impl fmt::Display for Select {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SELECT ")?;
        if self.distinct {
            f.write_str("DISTINCT ")?;
        }
        f.write_str(match self.fields {
            Fields::All => "*",
            Fields::Key => "key",
            Fields::Value => "value",
        })?;
        if let Some(condition) = &self.condition {
            write!(f, " WHERE {condition}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Condition::KeyEquals(k) => write!(f, "key = {}", Literal(k)),
            Condition::KeyBetween(low, high) => {
                write!(f, "key BETWEEN {} AND {}", Literal(low), Literal(high))
            }
        }
    }
}

/// A string as an SQL literal: in single quotes, a quote inside doubled.
pub(crate) struct Literal<'a>(pub &'a [u8]);

impl fmt::Display for Literal<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = String::from_utf8_lossy(self.0);
        write!(f, "'{}'", text.replace('\'', "''"))
    }
}
