//! SQL over a store whose values have attributes: a [`Schema`] names them
//! and gives each a [`Domain`], a codec turns them into the bytes of a value,
//! and `INSERT` writes them. The engine underneath is unchanged.
//!
//! ```text
//! statement := select | insert | delete | update
//! select    := SELECT [DISTINCT] fields [WHERE condition]
//! fields    := '*' | name { ',' name }
//! condition := name '=' literal
//!            | name BETWEEN literal AND literal
//! insert    := INSERT INTO store VALUES '(' literal { ',' literal } ')'
//! delete    := DELETE FROM store [WHERE condition]
//! update    := UPDATE store SET assign { ',' assign } [WHERE condition]
//! assign    := name '=' literal | name '=' name '+' integer
//! literal   := string | integer
//! ```
//!
//! The schema is given when the store is used, and isn't stored in it.

use std::collections::HashSet;
use std::fmt;
use std::ops::Bound;

use yabiir::{Bitcask, Cond, Engine, KeyOrder, Op};

use crate::ast::Literal;
use crate::parse::{ParseError, Parser, Token, tokenize};

/// The set of values an attribute can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Text,
    Integer,
}

impl fmt::Display for Domain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Domain::Text => "text",
            Domain::Integer => "an integer",
        })
    }
}

/// One attribute's value in one row.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Datum {
    Integer(i64),
    Text(String),
}

impl Datum {
    pub fn domain(&self) -> Domain {
        match self {
            Datum::Integer(_) => Domain::Integer,
            Datum::Text(_) => Domain::Text,
        }
    }
}

/// How a datum prints in a result.
impl fmt::Display for Datum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Datum::Integer(n) => write!(f, "{n}"),
            Datum::Text(s) => f.write_str(s),
        }
    }
}

/// A datum as SQL.
struct Sql<'a>(&'a Datum);

impl fmt::Display for Sql<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Datum::Integer(n) => write!(f, "{n}"),
            Datum::Text(s) => write!(f, "{}", Literal(s.as_bytes())),
        }
    }
}

const TAG_INTEGER: u8 = 1;
const TAG_TEXT: u8 = 2;

/// The attributes of the store's table. Column 0 is always `key`, in the
/// domain of text; the schema lists the ones stored in the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub(crate) columns: Vec<(String, Domain)>,
}

impl Schema {
    /// # Panics
    /// If two attributes share a name, ignoring case, or one is named `key`.
    pub fn new(attributes: &[(&str, Domain)]) -> Self {
        let mut columns = vec![("key".to_string(), Domain::Text)];
        for (name, domain) in attributes {
            assert!(
                !columns
                    .iter()
                    .any(|(c, _)| c.eq_ignore_ascii_case(name)),
                "two attributes named {name}"
            );
            columns.push((name.to_string(), *domain));
        }
        Schema { columns }
    }

    /// The names of the table's fields, `key` first.
    pub fn names(&self) -> Vec<&str> {
        self.columns
            .iter()
            .map(|(name, _)| name.as_str())
            .collect()
    }

    pub(crate) fn index_of(&self, name: &str) -> Result<usize, Error> {
        self.columns
            .iter()
            .position(|(c, _)| c.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::NoSuchField(name.to_ascii_lowercase()))
    }

    /// Fail unless `datum` is in the domain of column `index`.
    pub(crate) fn check(&self, index: usize, datum: &Datum) -> Result<(), Error> {
        let (name, domain) = &self.columns[index];
        if datum.domain() == *domain {
            Ok(())
        } else {
            Err(Error::Domain {
                field: name.clone(),
                domain: *domain,
                found: Sql(datum).to_string(),
            })
        }
    }

    /// The bytes of a value: each attribute in schema order, as a tag byte
    /// and its contents. An integer is 8 bytes, little-endian; text is a
    /// 4-byte little-endian length and its UTF-8.
    ///
    /// `attributes` are the row's fields after the key, already checked
    /// against their domains.
    pub fn encode(&self, attributes: &[Datum]) -> Vec<u8> {
        let mut out = Vec::new();
        for datum in attributes {
            match datum {
                Datum::Integer(n) => {
                    out.push(TAG_INTEGER);
                    out.extend_from_slice(&n.to_le_bytes());
                }
                Datum::Text(s) => {
                    out.push(TAG_TEXT);
                    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                    out.extend_from_slice(s.as_bytes());
                }
            }
        }
        out
    }

    /// The attributes in the bytes of a value. Fails unless the bytes are
    /// exactly one datum per attribute, each in its domain.
    pub fn decode(&self, mut bytes: &[u8]) -> Result<Vec<Datum>, String> {
        let mut out = Vec::with_capacity(
            self.columns
                .len()
                - 1,
        );
        for (name, domain) in &self.columns[1..] {
            let (&tag, rest) = bytes
                .split_first()
                .ok_or_else(|| format!("the value ends before {name}"))?;
            bytes = rest;
            let datum = match (tag, domain) {
                (TAG_INTEGER, Domain::Integer) => {
                    let (n, rest) = bytes
                        .split_first_chunk::<8>()
                        .ok_or_else(|| format!("{name} is cut short"))?;
                    bytes = rest;
                    Datum::Integer(i64::from_le_bytes(*n))
                }
                (TAG_TEXT, Domain::Text) => {
                    let (len, rest) = bytes
                        .split_first_chunk::<4>()
                        .ok_or_else(|| format!("{name} is cut short"))?;
                    let len = u32::from_le_bytes(*len) as usize;
                    if rest.len() < len {
                        return Err(format!("{name} is cut short"));
                    }
                    let (text, rest) = rest.split_at(len);
                    bytes = rest;
                    Datum::Text(
                        String::from_utf8(text.to_vec())
                            .map_err(|_| format!("{name} isn't UTF-8"))?,
                    )
                }
                _ => return Err(format!("{name} should be {domain}; its tag is {tag}")),
            };
            out.push(datum);
        }
        if bytes.is_empty() {
            Ok(out)
        } else {
            Err(format!("{} bytes left after the last field", bytes.len()))
        }
    }

    /// A stored record as a row: the key, then its attributes.
    pub(crate) fn row(&self, key: &[u8], value: &[u8]) -> Result<Vec<Datum>, Error> {
        let show = || String::from_utf8_lossy(key).into_owned();
        let text = std::str::from_utf8(key).map_err(|_| Error::BadRecord {
            key: show(),
            why: "the key isn't UTF-8".into(),
        })?;
        let mut row = vec![Datum::Text(text.to_string())];
        row.extend(
            self.decode(value)
                .map_err(|why| Error::BadRecord { key: show(), why })?,
        );
        Ok(row)
    }
}

/// Why a statement failed.
#[derive(Debug)]
pub enum Error {
    Parse(ParseError),
    /// The statement names a table that isn't the store.
    NoSuchTable(String),
    NoSuchField(String),
    /// `INSERT` gave the wrong number of values.
    WrongArity {
        expected: Vec<String>,
        found: usize,
    },
    /// A value outside its attribute's domain.
    Domain {
        field: String,
        domain: Domain,
        found: String,
    },
    /// `INSERT` of a key the store already holds.
    DuplicateKey(String),
    /// A stored value that doesn't decode with this schema.
    BadRecord {
        key: String,
        why: String,
    },
    /// A write kept finding its rows changed, and gave up.
    TooManyConflicts(usize),
    /// `UPDATE` assigned one field twice.
    RepeatedField(String),
    /// `field + n` left the 64-bit integers.
    Overflow(String),
    /// An index was asked for on a field that doesn't have one.
    NotIndexed(String),
    Store(yabiir::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Parse(e) => write!(f, "syntax error {e}"),
            Error::NoSuchTable(t) => write!(f, "no table named {t}; the only table is store"),
            Error::NoSuchField(name) => write!(f, "no field named {name}"),
            Error::WrongArity { expected, found } => write!(
                f,
                "store has {} fields ({}); got {found} values",
                expected.len(),
                expected.join(", ")
            ),
            Error::Domain {
                field,
                domain,
                found,
            } => write!(f, "{field} must be {domain}; {found} isn't"),
            Error::DuplicateKey(key) => write!(f, "key {} already exists", Literal(key.as_bytes())),
            Error::BadRecord { key, why } => {
                write!(
                    f,
                    "record {} doesn't fit the schema: {why}",
                    Literal(key.as_bytes())
                )
            }
            Error::TooManyConflicts(attempts) => {
                write!(
                    f,
                    "the rows kept changing; gave up after {attempts} attempts"
                )
            }
            Error::RepeatedField(field) => write!(f, "{field} is assigned twice"),
            Error::Overflow(field) => write!(f, "{field} would overflow 64 bits"),
            Error::NotIndexed(field) => write!(f, "{field} has no index"),
            Error::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

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

/// A statement, with the spelling gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    Select(Select),
    Insert(Insert),
    Delete(Delete),
    Update(Update),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Select {
    pub distinct: bool,
    pub fields: Fields,
    pub condition: Option<Condition>,
}

/// The projection. Names are as written, upper-cased by the tokenizer, and
/// are matched against the schema by the planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fields {
    All,
    Named(Vec<String>),
}

/// The selection: a test on one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub field: String,
    pub test: Test,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Test {
    Equals(Datum),
    /// Both ends included, as in SQL.
    Between(Datum, Datum),
}

impl Test {
    fn holds_for(&self, datum: &Datum) -> bool {
        match self {
            Test::Equals(v) => datum == v,
            Test::Between(low, high) => low <= datum && datum <= high,
        }
    }
}

/// `INSERT INTO table VALUES (…)`: the key first, then the attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insert {
    pub table: String,
    pub values: Vec<Datum>,
}

/// `DELETE FROM table [WHERE condition]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delete {
    pub table: String,
    pub condition: Option<Condition>,
}

/// `UPDATE table SET assignments [WHERE condition]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub table: String,
    pub assignments: Vec<Assignment>,
    pub condition: Option<Condition>,
}

/// `field = value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub field: String,
    pub value: Value,
}

/// What an assignment stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Literal(Datum),
    /// A field of the row as it was, plus an integer.
    Plus(String, i64),
}

const RESERVED: [&str; 12] = [
    "SELECT", "DISTINCT", "WHERE", "BETWEEN", "AND", "INSERT", "INTO", "VALUES", "DELETE", "FROM",
    "UPDATE", "SET",
];

/// Parse one statement.
pub fn parse(text: &str) -> Result<Statement, ParseError> {
    let mut p = Parser {
        tokens: tokenize(text)?,
        next: 0,
    };
    let statement = if p.is_word("SELECT") {
        Statement::Select(select(&mut p)?)
    } else if p.is_word("INSERT") {
        Statement::Insert(insert(&mut p)?)
    } else if p.is_word("DELETE") {
        Statement::Delete(delete(&mut p)?)
    } else if p.is_word("UPDATE") {
        Statement::Update(update(&mut p)?)
    } else {
        return Err(p.error("SELECT, INSERT, DELETE or UPDATE"));
    };
    if *p.peek() != Token::End {
        return Err(p.error("the end"));
    }
    Ok(statement)
}

fn select(p: &mut Parser) -> Result<Select, ParseError> {
    p.expect_word("SELECT")?;
    let distinct = p.is_word("DISTINCT");
    if distinct {
        p.advance();
    }
    let fields = if *p.peek() == Token::Star {
        p.advance();
        Fields::All
    } else {
        let mut names = vec![name(p, "'*' or a field")?];
        while *p.peek() == Token::Comma {
            p.advance();
            names.push(name(p, "a field")?);
        }
        Fields::Named(names)
    };
    let condition = if p.is_word("WHERE") {
        p.advance();
        Some(condition(p)?)
    } else {
        None
    };
    Ok(Select {
        distinct,
        fields,
        condition,
    })
}

fn name(p: &mut Parser, expected: &str) -> Result<String, ParseError> {
    match p.peek() {
        Token::Word(w) if !RESERVED.contains(&w.as_str()) => {
            let w = w.clone();
            p.advance();
            Ok(w)
        }
        _ => Err(p.error(expected)),
    }
}

fn condition(p: &mut Parser) -> Result<Condition, ParseError> {
    let field = name(p, "a field")?;
    let test = match p.peek() {
        Token::Equals => {
            p.advance();
            Test::Equals(literal(p)?)
        }
        Token::Word(w) if w == "BETWEEN" => {
            p.advance();
            let low = literal(p)?;
            p.expect_word("AND")?;
            Test::Between(low, literal(p)?)
        }
        _ => return Err(p.error("'=' or BETWEEN")),
    };
    Ok(Condition { field, test })
}

fn literal(p: &mut Parser) -> Result<Datum, ParseError> {
    let datum = match p.peek() {
        Token::Integer(n) => Datum::Integer(*n),
        // The tokenizer cuts strings at ASCII quotes, so they stay UTF-8.
        Token::String(s) => Datum::Text(String::from_utf8_lossy(s).into_owned()),
        _ => return Err(p.error("a string or an integer")),
    };
    p.advance();
    Ok(datum)
}

fn insert(p: &mut Parser) -> Result<Insert, ParseError> {
    p.expect_word("INSERT")?;
    p.expect_word("INTO")?;
    let table = name(p, "a table")?;
    p.expect_word("VALUES")?;
    if *p.peek() != Token::LeftParen {
        return Err(p.error("'('"));
    }
    p.advance();
    let mut values = vec![literal(p)?];
    while *p.peek() == Token::Comma {
        p.advance();
        values.push(literal(p)?);
    }
    if *p.peek() != Token::RightParen {
        return Err(p.error("',' or ')'"));
    }
    p.advance();
    Ok(Insert { table, values })
}

fn delete(p: &mut Parser) -> Result<Delete, ParseError> {
    p.expect_word("DELETE")?;
    p.expect_word("FROM")?;
    let table = name(p, "a table")?;
    let condition = if p.is_word("WHERE") {
        p.advance();
        Some(condition(p)?)
    } else {
        None
    };
    Ok(Delete { table, condition })
}

fn update(p: &mut Parser) -> Result<Update, ParseError> {
    p.expect_word("UPDATE")?;
    let table = name(p, "a table")?;
    p.expect_word("SET")?;
    let mut assignments = vec![assignment(p)?];
    while *p.peek() == Token::Comma {
        p.advance();
        assignments.push(assignment(p)?);
    }
    let condition = if p.is_word("WHERE") {
        p.advance();
        Some(condition(p)?)
    } else {
        None
    };
    Ok(Update {
        table,
        assignments,
        condition,
    })
}

fn assignment(p: &mut Parser) -> Result<Assignment, ParseError> {
    let field = name(p, "a field")?;
    if *p.peek() != Token::Equals {
        return Err(p.error("'='"));
    }
    p.advance();
    let value = if matches!(p.peek(), Token::Word(_)) {
        let source = name(p, "a field or a literal")?;
        if *p.peek() != Token::Plus {
            return Err(p.error("'+'"));
        }
        p.advance();
        let Token::Integer(n) = *p.peek() else {
            return Err(p.error("an integer"));
        };
        p.advance();
        Value::Plus(source, n)
    } else {
        Value::Literal(literal(p)?)
    };
    Ok(Assignment { field, value })
}

/// A tree of operators over rows of [`Datum`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Every row: one `fold`.
    FullScan,
    /// At most one row: one keydir lookup.
    Get(String),
    /// The rows from `low` to `high`, both included, in key order.
    RangeScan(String, String),
    /// The input's rows whose field `index` passes `test`.
    Filter {
        index: usize,
        name: String,
        test: Test,
        input: Box<Plan>,
    },
    /// The fields at `indices` of each of the input's rows.
    Project {
        indices: Vec<usize>,
        names: Vec<String>,
        input: Box<Plan>,
    },
    /// The input's rows, each once.
    Distinct(Box<Plan>),
}

/// Decide how to answer `select`, checking its names and literals against
/// the schema.
pub fn plan(select: &Select, schema: &Schema, order: KeyOrder) -> Result<Plan, Error> {
    let projection = match &select.fields {
        Fields::All => None,
        Fields::Named(names) => Some(
            names
                .iter()
                .map(|name| schema.index_of(name))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    };
    let condition = match &select.condition {
        None => None,
        Some(c) => {
            let index = schema.index_of(&c.field)?;
            match &c.test {
                Test::Equals(v) => schema.check(index, v)?,
                Test::Between(low, high) => {
                    schema.check(index, low)?;
                    schema.check(index, high)?;
                }
            }
            Some((
                index,
                c.test
                    .clone(),
            ))
        }
    };

    // The selection picks the leaf. Only the key has an index.
    let at_most_one_row = matches!(&condition, Some((0, Test::Equals(_))));
    let mut plan = match condition {
        None => Plan::FullScan,
        Some((0, Test::Equals(Datum::Text(key)))) => Plan::Get(key),
        Some((0, Test::Between(Datum::Text(low), Datum::Text(high))))
            if order == KeyOrder::Sorted =>
        {
            Plan::RangeScan(low, high)
        }
        Some((index, test)) => Plan::Filter {
            index,
            name: schema.columns[index]
                .0
                .clone(),
            test,
            input: Box::new(Plan::FullScan),
        },
    };
    let keeps_key = projection
        .as_ref()
        .is_none_or(|indices| indices.contains(&0));
    if let Some(indices) = projection {
        let names = indices
            .iter()
            .map(|&i| {
                schema.columns[i]
                    .0
                    .clone()
            })
            .collect();
        plan = Plan::Project {
            indices,
            names,
            input: Box::new(plan),
        };
    }
    if select.distinct && !keeps_key && !at_most_one_row {
        plan = Plan::Distinct(Box::new(plan));
    }
    Ok(plan)
}

/// A query's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Datum>>,
    /// How many records the plan's leaf read from the store.
    pub visited: usize,
}

/// Run `plan` against `store`.
pub fn execute(plan: &Plan, schema: &Schema, store: &Engine) -> Result<Rows, Error> {
    let mut rows = Vec::new();
    let mut visited = 0;
    run(plan, schema, store, &mut visited, &mut |row| rows.push(row))?;
    Ok(Rows {
        columns: plan.columns(schema),
        rows,
        visited,
    })
}

fn run(
    plan: &Plan,
    schema: &Schema,
    store: &Engine,
    visited: &mut usize,
    emit: &mut dyn FnMut(Vec<Datum>),
) -> Result<(), Error> {
    match plan {
        Plan::FullScan => {
            let mut failed = None;
            store.fold(
                |key, value, ()| {
                    *visited += 1;
                    if failed.is_none() {
                        match schema.row(key, value) {
                            Ok(row) => emit(row),
                            Err(e) => failed = Some(e),
                        }
                    }
                },
                (),
            )?;
            failed.map_or(Ok(()), Err)
        }
        Plan::Get(key) => {
            if let Some(value) = store.get(key.as_bytes())? {
                *visited += 1;
                emit(schema.row(key.as_bytes(), &value)?);
            }
            Ok(())
        }
        Plan::RangeScan(low, high) => {
            let range = store.scan(
                Bound::Included(low.as_bytes()),
                Bound::Included(high.as_bytes()),
            )?;
            for (key, value) in range {
                *visited += 1;
                emit(schema.row(&key, &value)?);
            }
            Ok(())
        }
        Plan::Filter {
            index, test, input, ..
        } => run(input, schema, store, visited, &mut |row| {
            if test.holds_for(&row[*index]) {
                emit(row);
            }
        }),
        Plan::Project { indices, input, .. } => run(input, schema, store, visited, &mut |row| {
            emit(
                indices
                    .iter()
                    .map(|&i| row[i].clone())
                    .collect(),
            );
        }),
        Plan::Distinct(input) => {
            let mut seen = HashSet::new();
            run(input, schema, store, visited, &mut |row| {
                if seen.insert(row.clone()) {
                    emit(row);
                }
            })
        }
    }
}

impl Plan {
    fn columns(&self, schema: &Schema) -> Vec<String> {
        match self {
            Plan::FullScan | Plan::Get(_) | Plan::RangeScan(..) => schema
                .names()
                .iter()
                .map(|n| n.to_string())
                .collect(),
            Plan::Filter { input, .. } | Plan::Distinct(input) => input.columns(schema),
            Plan::Project { names, .. } => names.clone(),
        }
    }

    /// Whether `row`, read again by its key, still passes this plan's
    /// conditions. A leaf's own condition is on the key, which a row keeps.
    fn still_selects(&self, row: &[Datum]) -> bool {
        match self {
            Plan::FullScan | Plan::Get(_) | Plan::RangeScan(..) => true,
            Plan::Filter {
                index, test, input, ..
            } => test.holds_for(&row[*index]) && input.still_selects(row),
            Plan::Project { input, .. } | Plan::Distinct(input) => input.still_selects(row),
        }
    }

    fn explain(&self, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
        let indent = "  ".repeat(depth);
        match self {
            Plan::FullScan => writeln!(f, "{indent}FullScan (fold: every row)"),
            Plan::Get(key) => {
                writeln!(
                    f,
                    "{indent}Get {} (get: one lookup)",
                    Literal(key.as_bytes())
                )
            }
            Plan::RangeScan(low, high) => writeln!(
                f,
                "{indent}RangeScan {} .. {} (scan: sorted keydir)",
                Literal(low.as_bytes()),
                Literal(high.as_bytes())
            ),
            Plan::Filter {
                name, test, input, ..
            } => {
                match test {
                    Test::Equals(v) => writeln!(f, "{indent}Filter {name} = {}", Sql(v))?,
                    Test::Between(low, high) => writeln!(
                        f,
                        "{indent}Filter {name} BETWEEN {} AND {}",
                        Sql(low),
                        Sql(high)
                    )?,
                }
                input.explain(f, depth + 1)
            }
            Plan::Project { names, input, .. } => {
                writeln!(f, "{indent}Project {}", names.join(", "))?;
                input.explain(f, depth + 1)
            }
            Plan::Distinct(input) => {
                writeln!(f, "{indent}Distinct")?;
                input.explain(f, depth + 1)
            }
        }
    }
}

/// The plan as an indented tree, root first.
impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.explain(f, 0)
    }
}

/// Check `insert` against the schema, and split it into the key and the
/// bytes of the value.
pub fn encode_insert(insert: &Insert, schema: &Schema) -> Result<(Vec<u8>, Vec<u8>), Error> {
    if !insert
        .table
        .eq_ignore_ascii_case("store")
    {
        return Err(Error::NoSuchTable(
            insert
                .table
                .to_ascii_lowercase(),
        ));
    }
    if insert
        .values
        .len()
        != schema
            .columns
            .len()
    {
        return Err(Error::WrongArity {
            expected: schema
                .names()
                .iter()
                .map(|n| n.to_string())
                .collect(),
            found: insert
                .values
                .len(),
        });
    }
    for (index, datum) in insert
        .values
        .iter()
        .enumerate()
    {
        schema.check(index, datum)?;
    }
    let Datum::Text(key) = &insert.values[0] else {
        unreachable!("the key's domain is text");
    };
    Ok((
        key.clone()
            .into_bytes(),
        schema.encode(&insert.values[1..]),
    ))
}

/// Run an `INSERT`: the row goes in only if its key isn't in the store. The
/// check and the put are one batch, so no other writer can get in between.
pub fn insert_row(insert: &Insert, schema: &Schema, store: &Engine) -> Result<(), Error> {
    let (key, value) = encode_insert(insert, schema)?;
    let batch = vec![
        Op::Check {
            key: key.clone(),
            cond: Cond::Absent,
        },
        Op::Put {
            key: key.clone(),
            value,
        },
    ];
    match store
        .write_batch(batch)
        .wait()
    {
        Ok(_) => Ok(()),
        Err(yabiir::Error::ConditionFailed { .. }) => Err(Error::DuplicateKey(
            String::from_utf8_lossy(&key).into_owned(),
        )),
        Err(e) => Err(e.into()),
    }
}

/// Decide which rows a `DELETE` removes: the plan of the `SELECT *` with the
/// same condition.
pub fn plan_delete(delete: &Delete, schema: &Schema, order: KeyOrder) -> Result<Plan, Error> {
    if !delete
        .table
        .eq_ignore_ascii_case("store")
    {
        return Err(Error::NoSuchTable(
            delete
                .table
                .to_ascii_lowercase(),
        ));
    }
    let select = Select {
        distinct: false,
        fields: Fields::All,
        condition: delete
            .condition
            .clone(),
    };
    plan(&select, schema, order)
}

/// What a `DELETE` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deleted {
    pub rows: usize,
    /// How many records the plan's leaf read, over every attempt.
    pub visited: usize,
}

/// How many times a write starts over before it reports a conflict.
pub(crate) const WRITE_ATTEMPTS: usize = 1000;

/// Wait before attempt `attempt + 1` of a write whose check failed. Writers
/// that retry at once keep colliding, and one can lose every time, so each
/// waits a random time from a range that doubles up to about a millisecond.
pub(crate) fn back_off(attempt: usize) {
    use std::hash::{BuildHasher, RandomState};
    let ceiling = 1u64 << attempt.min(10);
    let micros = RandomState::new().hash_one(attempt) % ceiling;
    if micros == 0 {
        std::thread::yield_now();
    } else {
        std::thread::sleep(std::time::Duration::from_micros(micros));
    }
}

/// Run a `DELETE`: find the rows, then remove them in one batch, each guarded
/// by the version it had when it was checked against the condition. Either
/// every row goes or none does, and a row that changed in between stops the
/// batch, which starts over. A row that starts to match after the scan stays.
pub fn delete_rows(plan: &Plan, schema: &Schema, store: &Engine) -> Result<Deleted, Error> {
    let mut visited = 0;
    for attempt in 0..WRITE_ATTEMPTS {
        let found = execute(plan, schema, store)?;
        visited += found.visited;
        let mut batch = Vec::with_capacity(
            2 * found
                .rows
                .len(),
        );
        for row in &found.rows {
            let Datum::Text(key) = &row[0] else {
                unreachable!("the key's domain is text");
            };
            let key = key.as_bytes();
            let Some(current) = store.get_versioned(key)? else {
                continue;
            };
            if plan.still_selects(&schema.row(key, &current.value)?) {
                batch.push(Op::Check {
                    key: key.to_vec(),
                    cond: Cond::Version(current.version),
                });
                batch.push(Op::Delete { key: key.to_vec() });
            }
        }
        let rows = batch.len() / 2;
        if rows == 0 {
            return Ok(Deleted { rows, visited });
        }
        match store
            .write_batch(batch)
            .wait()
        {
            Ok(_) => return Ok(Deleted { rows, visited }),
            Err(yabiir::Error::ConditionFailed { .. }) => back_off(attempt),
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::TooManyConflicts(WRITE_ATTEMPTS))
}

/// How an `UPDATE` computes one field.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Compute {
    Set(Datum),
    Add(usize, i64),
}

/// Which rows an `UPDATE` changes, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePlan {
    selection: Plan,
    sets: Vec<(usize, Compute)>,
    label: String,
}

impl UpdatePlan {
    /// The row with the assignments applied, all computed from the old row.
    fn apply(&self, old: &[Datum], schema: &Schema) -> Result<Vec<Datum>, Error> {
        let mut new = old.to_vec();
        for (index, compute) in &self.sets {
            new[*index] = match compute {
                Compute::Set(datum) => datum.clone(),
                Compute::Add(source, n) => {
                    let Datum::Integer(v) = old[*source] else {
                        unreachable!("the planner checked the domain");
                    };
                    Datum::Integer(
                        v.checked_add(*n)
                            .ok_or_else(|| {
                                Error::Overflow(
                                    schema.columns[*index]
                                        .0
                                        .clone(),
                                )
                            })?,
                    )
                }
            };
        }
        Ok(new)
    }
}

/// The plan as an indented tree, root first.
impl fmt::Display for UpdatePlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Update {} (one batch)", self.label)?;
        self.selection
            .explain(f, 1)
    }
}

/// Check an `UPDATE` against the schema, and plan it.
pub fn plan_update(update: &Update, schema: &Schema, order: KeyOrder) -> Result<UpdatePlan, Error> {
    if !update
        .table
        .eq_ignore_ascii_case("store")
    {
        return Err(Error::NoSuchTable(
            update
                .table
                .to_ascii_lowercase(),
        ));
    }
    let mut sets: Vec<(usize, Compute)> = Vec::new();
    let mut labels = Vec::new();
    for assignment in &update.assignments {
        let index = schema.index_of(&assignment.field)?;
        let name = &schema.columns[index].0;
        if sets
            .iter()
            .any(|(i, _)| *i == index)
        {
            return Err(Error::RepeatedField(name.clone()));
        }
        let compute = match &assignment.value {
            Value::Literal(datum) => {
                schema.check(index, datum)?;
                labels.push(format!("{name} = {}", Sql(datum)));
                Compute::Set(datum.clone())
            }
            Value::Plus(source, n) => {
                let from = schema.index_of(source)?;
                let source = &schema.columns[from].0;
                for (field, other) in [(name, source), (source, name)] {
                    if schema.columns[schema.index_of(field)?].1 != Domain::Integer {
                        return Err(Error::Domain {
                            field: field.clone(),
                            domain: Domain::Integer,
                            found: format!("{other} + {n}"),
                        });
                    }
                }
                labels.push(format!("{name} = {source} + {n}"));
                Compute::Add(from, *n)
            }
        };
        sets.push((index, compute));
    }
    let select = Select {
        distinct: false,
        fields: Fields::All,
        condition: update
            .condition
            .clone(),
    };
    Ok(UpdatePlan {
        selection: plan(&select, schema, order)?,
        sets,
        label: labels.join(", "),
    })
}

/// What an `UPDATE` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Updated {
    pub rows: usize,
    /// How many records the plan's leaf read, over every attempt.
    pub visited: usize,
    /// How many times the batch failed its checks and started over.
    pub retries: usize,
}

/// Run an `UPDATE`: read each row with its version, compute the new row from
/// it, and write them all in one batch guarded by those versions. If any row
/// changed in between, nothing is written and the statement starts over. A
/// new key is a delete and an insert, and it must not be taken.
pub fn update_rows(plan: &UpdatePlan, schema: &Schema, store: &Engine) -> Result<Updated, Error> {
    let mut visited = 0;
    for retries in 0..WRITE_ATTEMPTS {
        let found = execute(&plan.selection, schema, store)?;
        visited += found.visited;
        let mut batch = Vec::new();
        let mut rows = 0;
        let mut moved_to = None;
        for row in &found.rows {
            let Datum::Text(key) = &row[0] else {
                unreachable!("the key's domain is text");
            };
            let key = key.as_bytes();
            let Some(current) = store.get_versioned(key)? else {
                continue;
            };
            let old = schema.row(key, &current.value)?;
            if !plan
                .selection
                .still_selects(&old)
            {
                continue;
            }
            let new = plan.apply(&old, schema)?;
            let Datum::Text(new_key) = &new[0] else {
                unreachable!("the key's domain is text");
            };
            rows += 1;
            batch.push(Op::Check {
                key: key.to_vec(),
                cond: Cond::Version(current.version),
            });
            if new_key.as_bytes() != key {
                moved_to = Some(new_key.clone());
                batch.push(Op::Check {
                    key: new_key
                        .clone()
                        .into_bytes(),
                    cond: Cond::Absent,
                });
                batch.push(Op::Delete { key: key.to_vec() });
            }
            batch.push(Op::Put {
                key: new_key
                    .clone()
                    .into_bytes(),
                value: schema.encode(&new[1..]),
            });
        }
        if rows == 0 {
            return Ok(Updated {
                rows,
                visited,
                retries,
            });
        }
        // Keys are assigned literals, so two rows would get the same one.
        if let (Some(key), true) = (&moved_to, rows > 1) {
            return Err(Error::DuplicateKey(key.clone()));
        }
        match store
            .write_batch(batch)
            .wait()
        {
            Ok(_) => {
                return Ok(Updated {
                    rows,
                    visited,
                    retries,
                });
            }
            Err(yabiir::Error::ConditionFailed { index: 1 }) if moved_to.is_some() => {
                return Err(Error::DuplicateKey(moved_to.unwrap_or_default()));
            }
            Err(yabiir::Error::ConditionFailed { .. }) => back_off(retries),
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::TooManyConflicts(WRITE_ATTEMPTS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yabiir::Options;
    use yabiir_testutil::TempDir;

    fn schema() -> Schema {
        Schema::new(&[("maker", Domain::Text), ("year", Domain::Integer)])
    }

    fn open(dir: &TempDir, key_order: KeyOrder) -> Engine {
        Engine::open(
            &**dir,
            Options {
                key_order,
                ..Options::default()
            },
        )
        .unwrap()
    }

    fn exec(store: &Engine, text: &str) -> Result<Option<Rows>, Error> {
        match parse(text)? {
            Statement::Insert(i) => insert_row(&i, &schema(), store).map(|()| None),
            Statement::Delete(d) => {
                let p = plan_delete(&d, &schema(), store.key_order())?;
                delete_rows(&p, &schema(), store).map(|_| None)
            }
            Statement::Update(u) => {
                let p = plan_update(&u, &schema(), store.key_order())?;
                update_rows(&p, &schema(), store).map(|_| None)
            }
            Statement::Select(s) => {
                let p = plan(&s, &schema(), store.key_order())?;
                execute(&p, &schema(), store).map(Some)
            }
        }
    }

    fn fill(store: &Engine) {
        for row in [
            "('apple-ii', 'apple', 1977)",
            "('ibm-pc', 'ibm', 1981)",
            "('bbc-micro', 'acorn', 1981)",
            "('zx-spectrum', 'sinclair', 1982)",
            "('c64', 'commodore', 1982)",
        ] {
            exec(store, &format!("INSERT INTO store VALUES {row}")).unwrap();
        }
    }

    fn sorted(rows: &Rows) -> Vec<String> {
        let mut out: Vec<String> = rows
            .rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(Datum::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn a_row_round_trips_through_the_codec() {
        let s = schema();
        let row = [Datum::Text("o'neil ñ".into()), Datum::Integer(-1982)];
        assert_eq!(
            s.decode(&s.encode(&row))
                .unwrap(),
            row
        );
    }

    #[test]
    fn decode_rejects_bytes_that_do_not_fit_the_schema() {
        let s = schema();
        let good = s.encode(&[Datum::Text("acorn".into()), Datum::Integer(1981)]);
        for cut in 0..good.len() {
            assert!(
                s.decode(&good[..cut])
                    .is_err(),
                "cut at {cut}"
            );
        }
        let mut long = good.clone();
        long.push(0);
        assert!(
            s.decode(&long)
                .is_err()
        );
        let swapped = Schema::new(&[("year", Domain::Integer), ("maker", Domain::Text)]);
        assert!(
            swapped
                .decode(&good)
                .is_err()
        );
        assert!(
            s.decode(b"1982")
                .is_err()
        );
    }

    #[test]
    fn projection_and_selection_on_attributes() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        let rows = exec(&store, "SELECT key, maker WHERE year = 1982")
            .unwrap()
            .unwrap();
        assert_eq!(rows.columns, ["key", "maker"]);
        assert_eq!(rows.visited, 5);
        assert_eq!(sorted(&rows), ["c64 commodore", "zx-spectrum sinclair"]);
        let rows = exec(&store, "SELECT key WHERE year BETWEEN 1978 AND 1981")
            .unwrap()
            .unwrap();
        assert_eq!(sorted(&rows), ["bbc-micro", "ibm-pc"]);
    }

    #[test]
    fn distinct_applies_only_where_it_can_remove_a_row() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        let rows = exec(&store, "SELECT DISTINCT year")
            .unwrap()
            .unwrap();
        assert_eq!(sorted(&rows), ["1977", "1981", "1982"]);
        for (text, distinct) in [
            ("SELECT DISTINCT year", true),
            ("SELECT DISTINCT key, year", false),
            ("SELECT DISTINCT year WHERE key = 'c64'", false),
        ] {
            let Statement::Select(s) = parse(text).unwrap() else {
                unreachable!()
            };
            let p = plan(&s, &schema(), KeyOrder::Hashed).unwrap();
            assert_eq!(matches!(p, Plan::Distinct(_)), distinct, "{text}");
        }
    }

    #[test]
    fn the_key_picks_the_leaf_and_an_attribute_scans() {
        let leaf = |text: &str, order| {
            let Statement::Select(s) = parse(text).unwrap() else {
                unreachable!()
            };
            plan(&s, &schema(), order).unwrap()
        };
        assert_eq!(
            leaf("SELECT * WHERE key = 'c64'", KeyOrder::Hashed),
            Plan::Get("c64".into())
        );
        assert_eq!(
            leaf("SELECT * WHERE key BETWEEN 'a' AND 'c'", KeyOrder::Sorted),
            Plan::RangeScan("a".into(), "c".into())
        );
        assert!(matches!(
            leaf("SELECT * WHERE key BETWEEN 'a' AND 'c'", KeyOrder::Hashed),
            Plan::Filter { .. }
        ));
        assert!(matches!(
            leaf("SELECT * WHERE year = 1982", KeyOrder::Sorted),
            Plan::Filter { .. }
        ));
    }

    #[test]
    fn a_range_on_a_sorted_store_comes_back_in_key_order() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Sorted);
        fill(&store);
        let rows = exec(&store, "SELECT key WHERE key BETWEEN 'a' AND 'c'")
            .unwrap()
            .unwrap();
        assert_eq!(rows.visited, 2);
        assert_eq!(
            rows.rows,
            [
                [Datum::Text("apple-ii".into())],
                [Datum::Text("bbc-micro".into())]
            ]
        );
    }

    #[test]
    fn insert_refuses_a_key_the_store_holds_and_leaves_its_row() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        let err = exec(&store, "INSERT INTO store VALUES ('c64', 'atari', 1999)").unwrap_err();
        assert!(matches!(err, Error::DuplicateKey(k) if k == "c64"));
        let rows = exec(&store, "SELECT * WHERE key = 'c64'")
            .unwrap()
            .unwrap();
        assert_eq!(sorted(&rows), ["c64 commodore 1982"]);
    }

    #[test]
    fn values_outside_their_domains_are_refused_and_nothing_is_written() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        for text in [
            "INSERT INTO store VALUES ('amiga', 'commodore', 'nineteen')",
            "INSERT INTO store VALUES ('amiga', 1985, 1985)",
            "INSERT INTO store VALUES (1, 'commodore', 1985)",
            "INSERT INTO store VALUES ('amiga', 'commodore')",
            "INSERT INTO store VALUES ('amiga', 'commodore', 1985, 1)",
            "INSERT INTO computers VALUES ('amiga', 'commodore', 1985)",
        ] {
            assert!(exec(&store, text).is_err(), "{text}");
        }
        assert_eq!(
            exec(&store, "SELECT *")
                .unwrap()
                .unwrap()
                .rows
                .len(),
            0
        );
    }

    #[test]
    fn conditions_are_checked_against_the_schema() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        assert!(matches!(
            exec(&store, "SELECT * WHERE year = '1982'"),
            Err(Error::Domain { .. })
        ));
        assert!(matches!(
            exec(&store, "SELECT price"),
            Err(Error::NoSuchField(f)) if f == "price"
        ));
    }

    #[test]
    fn a_record_that_does_not_fit_the_schema_is_an_error() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        store
            .put(b"apple-ii", b"1977", yabiir::now_unix())
            .unwrap();
        assert!(matches!(
            exec(&store, "SELECT *"),
            Err(Error::BadRecord { key, .. }) if key == "apple-ii"
        ));
    }

    fn delete(store: &Engine, text: &str) -> Result<Deleted, Error> {
        let Statement::Delete(d) = parse(text)? else {
            unreachable!()
        };
        let p = plan_delete(&d, &schema(), store.key_order())?;
        delete_rows(&p, &schema(), store)
    }

    fn keys(store: &Engine) -> Vec<String> {
        sorted(
            &exec(store, "SELECT key")
                .unwrap()
                .unwrap(),
        )
    }

    #[test]
    fn delete_by_key_removes_one_row_and_a_missing_key_removes_none() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        let done = delete(&store, "DELETE FROM store WHERE key = 'c64'").unwrap();
        assert_eq!(
            done,
            Deleted {
                rows: 1,
                visited: 1
            }
        );
        let done = delete(&store, "DELETE FROM store WHERE key = 'c64'").unwrap();
        assert_eq!(
            done,
            Deleted {
                rows: 0,
                visited: 0
            }
        );
        assert_eq!(
            keys(&store),
            ["apple-ii", "bbc-micro", "ibm-pc", "zx-spectrum"]
        );
    }

    #[test]
    fn delete_by_condition_removes_every_match_and_nothing_else() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        let done = delete(&store, "DELETE FROM store WHERE year = 1982").unwrap();
        assert_eq!(
            done,
            Deleted {
                rows: 2,
                visited: 5
            }
        );
        assert_eq!(keys(&store), ["apple-ii", "bbc-micro", "ibm-pc"]);
        let done = delete(&store, "DELETE FROM store").unwrap();
        assert_eq!(done.rows, 3);
        assert!(keys(&store).is_empty());
    }

    #[test]
    fn delete_checks_its_table_and_its_condition() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        assert!(matches!(
            delete(&store, "DELETE FROM computers"),
            Err(Error::NoSuchTable(t)) if t == "computers"
        ));
        assert!(matches!(
            delete(&store, "DELETE FROM store WHERE year = '1982'"),
            Err(Error::Domain { .. })
        ));
        assert_eq!(keys(&store).len(), 5);
    }

    #[test]
    fn delete_leaves_a_row_that_stopped_matching_after_the_scan() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        let Statement::Delete(d) = parse("DELETE FROM store WHERE year = 1982").unwrap() else {
            unreachable!()
        };
        let p = plan_delete(&d, &schema(), store.key_order()).unwrap();
        // The row changes after a scan would have found it: re-reading it by
        // key is what keeps it.
        let moved = schema().encode(&[Datum::Text("commodore".into()), Datum::Integer(1990)]);
        store
            .put(b"c64", &moved, yabiir::now_unix())
            .unwrap();
        let row = schema()
            .row(b"c64", &moved)
            .unwrap();
        assert!(!p.still_selects(&row));
        assert_eq!(
            delete_rows(&p, &schema(), &store)
                .unwrap()
                .rows,
            1
        );
        assert_eq!(keys(&store), ["apple-ii", "bbc-micro", "c64", "ibm-pc"]);
    }

    fn update(store: &Engine, text: &str) -> Result<Updated, Error> {
        let Statement::Update(u) = parse(text)? else {
            unreachable!()
        };
        let p = plan_update(&u, &schema(), store.key_order())?;
        update_rows(&p, &schema(), store)
    }

    fn all(store: &Engine) -> Vec<String> {
        sorted(
            &exec(store, "SELECT *")
                .unwrap()
                .unwrap(),
        )
    }

    #[test]
    fn update_changes_the_matching_rows_and_only_those() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        let done = update(&store, "UPDATE store SET maker = 'cbm' WHERE key = 'c64'").unwrap();
        assert_eq!((done.rows, done.visited), (1, 1));
        let done = update(
            &store,
            "UPDATE store SET year = year + 10 WHERE year = 1981",
        )
        .unwrap();
        assert_eq!(done.rows, 2);
        assert_eq!(
            all(&store),
            [
                "apple-ii apple 1977",
                "bbc-micro acorn 1991",
                "c64 cbm 1982",
                "ibm-pc ibm 1991",
                "zx-spectrum sinclair 1982"
            ]
        );
        assert_eq!(
            update(&store, "UPDATE store SET year = 1 WHERE key = 'nope'")
                .unwrap()
                .rows,
            0
        );
    }

    #[test]
    fn every_assignment_reads_the_old_row() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        update(
            &store,
            "UPDATE store SET year = year + 1, maker = 'x' WHERE key = 'c64'",
        )
        .unwrap();
        assert!(all(&store).contains(&"c64 x 1983".to_string()));
    }

    #[test]
    fn updating_the_key_moves_the_row_unless_the_new_key_is_taken() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        update(&store, "UPDATE store SET key = 'c128' WHERE key = 'c64'").unwrap();
        let rows = all(&store);
        assert!(rows.contains(&"c128 commodore 1982".to_string()));
        assert!(
            !rows
                .iter()
                .any(|r| r.starts_with("c64 "))
        );
        assert!(matches!(
            update(&store, "UPDATE store SET key = 'ibm-pc' WHERE key = 'c128'"),
            Err(Error::DuplicateKey(k)) if k == "ibm-pc"
        ));
        assert!(matches!(
            update(&store, "UPDATE store SET key = 'same' WHERE year = 1981"),
            Err(Error::DuplicateKey(k)) if k == "same"
        ));
        assert_eq!(all(&store), rows);
        assert_eq!(
            update(&store, "UPDATE store SET key = 'c128' WHERE key = 'c128'")
                .unwrap()
                .rows,
            1
        );
        assert_eq!(all(&store), rows);
    }

    #[test]
    fn update_checks_domains_and_names() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        let before = all(&store);
        for text in [
            "UPDATE store SET year = 'x'",
            "UPDATE store SET maker = 1",
            "UPDATE store SET maker = maker + 1",
            "UPDATE store SET year = maker + 1",
            "UPDATE store SET key = year + 1",
            "UPDATE store SET price = 1",
            "UPDATE computers SET year = 1",
            "UPDATE store SET year = 1, year = 2",
            "UPDATE store SET year = year + 9223372036854775807",
        ] {
            assert!(update(&store, text).is_err(), "{text}");
        }
        assert_eq!(all(&store), before);
    }

    #[test]
    fn concurrent_increments_all_count() {
        let dir = TempDir::new();
        let store = open(&dir, KeyOrder::Hashed);
        fill(&store);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..50 {
                        update(&store, "UPDATE store SET year = year + 1 WHERE key = 'c64'")
                            .unwrap();
                    }
                });
            }
        });
        assert!(all(&store).contains(&"c64 commodore 2182".to_string()));
    }

    #[test]
    fn syntax_errors_say_where() {
        for (text, at) in [
            ("DROP TABLE store", 0),
            ("SELECT maker,", 13),
            ("SELECT * WHERE", 14),
            ("INSERT store", 7),
            ("DELETE store", 7),
            ("UPDATE store year = 1", 13),
            ("UPDATE store SET year", 21),
            ("UPDATE store SET year = year", 28),
            ("UPDATE store SET year = year + 'a'", 31),
            ("DELETE FROM", 11),
            ("DELETE FROM store WHERE", 23),
            ("INSERT INTO store VALUES 'a'", 25),
            ("INSERT INTO store VALUES ('a' 'b')", 30),
            ("SELECT * WHERE year = 99999999999999999999", 22),
        ] {
            assert_eq!(
                parse(text)
                    .unwrap_err()
                    .at,
                at,
                "{text}"
            );
        }
    }
}
