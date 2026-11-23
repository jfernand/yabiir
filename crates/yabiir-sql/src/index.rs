//! Secondary indices over a table of rows, in one sorted store.
//!
//! ```text
//! (1, key)                → the row's attributes
//! (2 + i, value, key)     → nothing: index i says this row has this value
//! ```
//!
//! Keys are composite ([`crate::key`]), so an index is one more range of the
//! store. Every write is one batch: the row and all its index entries change
//! together, guarded by the version the row was read at.

use std::collections::BTreeSet;
use std::ops::Bound;

use yabiir::{Bitcask, Cond, Engine, KeyOrder, Op};

use crate::key::{decode_key, encode_key, prefix_range};
use crate::table::{Datum, Error, Schema, WRITE_ATTEMPTS, back_off};

const ROWS: i64 = 1;
const FIRST_INDEX: i64 = 2;

/// A table with an index on some of its attributes.
pub struct IndexedTable<'a> {
    store: &'a Engine,
    schema: Schema,
    /// The column of each index, in index order.
    indexed: Vec<usize>,
}

/// Rows and index entries that don't agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Disagreements {
    /// A row's value has no entry in its index.
    pub missing: usize,
    /// An entry names a row that doesn't exist or doesn't have that value.
    pub dangling: usize,
}

fn text(key: &str) -> Datum {
    Datum::Text(key.to_string())
}

type KeyRange = (Bound<Vec<u8>>, Bound<Vec<u8>>);

type Records = Vec<(Vec<u8>, Vec<u8>)>;

fn scan(store: &Engine, (low, high): KeyRange) -> Result<Records, Error> {
    Ok(store.scan(
        low.as_ref()
            .map(Vec::as_slice),
        high.as_ref()
            .map(Vec::as_slice),
    )?)
}

impl<'a> IndexedTable<'a> {
    /// A table over `store`, which must be sorted, with an index on each of
    /// `fields`.
    pub fn new(store: &'a Engine, schema: Schema, fields: &[&str]) -> Result<Self, Error> {
        if store.key_order() != KeyOrder::Sorted {
            return Err(Error::Store(yabiir::Error::Unordered));
        }
        let indexed = fields
            .iter()
            .map(|f| schema.index_of(f))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            store,
            schema,
            indexed,
        })
    }

    fn row_key(key: &str) -> Vec<u8> {
        encode_key(&[Datum::Integer(ROWS), text(key)])
    }

    fn entry_key(&self, index: usize, value: &Datum, key: &str) -> Vec<u8> {
        encode_key(&[
            Datum::Integer(FIRST_INDEX + index as i64),
            value.clone(),
            text(key),
        ])
    }

    /// The index entries of `row`.
    pub fn entries(&self, row: &[Datum]) -> Vec<Vec<u8>> {
        let Datum::Text(key) = &row[0] else {
            unreachable!("the key's domain is text");
        };
        self.indexed
            .iter()
            .enumerate()
            .map(|(i, &column)| self.entry_key(i, &row[column], key))
            .collect()
    }

    fn check_row(&self, row: &[Datum]) -> Result<(), Error> {
        if row.len()
            != self
                .schema
                .columns
                .len()
        {
            return Err(Error::WrongArity {
                expected: self
                    .schema
                    .names()
                    .iter()
                    .map(|n| n.to_string())
                    .collect(),
                found: row.len(),
            });
        }
        for (i, datum) in row
            .iter()
            .enumerate()
        {
            self.schema
                .check(i, datum)?;
        }
        Ok(())
    }

    /// The store key and value of `row`.
    pub fn encode_row(&self, row: &[Datum]) -> (Vec<u8>, Vec<u8>) {
        let Datum::Text(key) = &row[0] else {
            unreachable!("the key's domain is text");
        };
        (
            Self::row_key(key),
            self.schema
                .encode(&row[1..]),
        )
    }

    /// Add a row, and its index entries, in one batch.
    pub fn insert(&self, row: &[Datum]) -> Result<(), Error> {
        self.check_row(row)?;
        let (key, value) = self.encode_row(row);
        let mut batch = vec![
            Op::Check {
                key: key.clone(),
                cond: Cond::Absent,
            },
            Op::Put { key, value },
        ];
        for entry in self.entries(row) {
            batch.push(Op::Put {
                key: entry,
                value: Vec::new(),
            });
        }
        match self
            .store
            .write_batch(batch)
            .wait()
        {
            Ok(_) => Ok(()),
            Err(yabiir::Error::ConditionFailed { .. }) => {
                let Datum::Text(key) = &row[0] else {
                    unreachable!("the key's domain is text");
                };
                Err(Error::DuplicateKey(key.clone()))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// The row with this key.
    pub fn get(&self, key: &str) -> Result<Option<Vec<Datum>>, Error> {
        match self
            .store
            .get(&Self::row_key(key))?
        {
            Some(value) => Ok(Some(
                self.schema
                    .row(key.as_bytes(), &value)?,
            )),
            None => Ok(None),
        }
    }

    /// Set one attribute of a row. Replaces its index entry if the attribute
    /// is indexed, and starts over if the row changes meanwhile. Returns
    /// whether the row exists.
    pub fn update(&self, key: &str, field: &str, value: Datum) -> Result<bool, Error> {
        let column = self
            .schema
            .index_of(field)?;
        if column == 0 {
            return Err(Error::NotIndexed(field.to_ascii_lowercase()));
        }
        self.schema
            .check(column, &value)?;
        let row_key = Self::row_key(key);
        for attempt in 0..WRITE_ATTEMPTS {
            let Some(current) = self
                .store
                .get_versioned(&row_key)?
            else {
                return Ok(false);
            };
            let old = self
                .schema
                .row(key.as_bytes(), &current.value)?;
            let mut new = old.clone();
            new[column] = value.clone();
            let mut batch = vec![
                Op::Check {
                    key: row_key.clone(),
                    cond: Cond::Version(current.version),
                },
                Op::Put {
                    key: row_key.clone(),
                    value: self
                        .schema
                        .encode(&new[1..]),
                },
            ];
            for (before, after) in self
                .entries(&old)
                .into_iter()
                .zip(self.entries(&new))
            {
                if before != after {
                    batch.push(Op::Delete { key: before });
                    batch.push(Op::Put {
                        key: after,
                        value: Vec::new(),
                    });
                }
            }
            match self
                .store
                .write_batch(batch)
                .wait()
            {
                Ok(_) => return Ok(true),
                Err(yabiir::Error::ConditionFailed { .. }) => back_off(attempt),
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::TooManyConflicts(WRITE_ATTEMPTS))
    }

    /// Remove a row and its index entries. Returns whether it existed.
    pub fn delete(&self, key: &str) -> Result<bool, Error> {
        let row_key = Self::row_key(key);
        for attempt in 0..WRITE_ATTEMPTS {
            let Some(current) = self
                .store
                .get_versioned(&row_key)?
            else {
                return Ok(false);
            };
            let row = self
                .schema
                .row(key.as_bytes(), &current.value)?;
            let mut batch = vec![
                Op::Check {
                    key: row_key.clone(),
                    cond: Cond::Version(current.version),
                },
                Op::Delete {
                    key: row_key.clone(),
                },
            ];
            for entry in self.entries(&row) {
                batch.push(Op::Delete { key: entry });
            }
            match self
                .store
                .write_batch(batch)
                .wait()
            {
                Ok(_) => return Ok(true),
                Err(yabiir::Error::ConditionFailed { .. }) => back_off(attempt),
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::TooManyConflicts(WRITE_ATTEMPTS))
    }

    fn index_number(&self, field: &str) -> Result<(usize, usize), Error> {
        let column = self
            .schema
            .index_of(field)?;
        let index = self
            .indexed
            .iter()
            .position(|&c| c == column)
            .ok_or_else(|| Error::NotIndexed(field.to_ascii_lowercase()))?;
        Ok((index, column))
    }

    /// The rows whose `field` equals `value`, through its index: a scan of
    /// the entries, then a `get` of each row.
    pub fn lookup(&self, field: &str, value: &Datum) -> Result<Vec<Vec<Datum>>, Error> {
        self.between(field, value, value)
    }

    /// The rows whose `field` is from `low` to `high`, both included, in
    /// the index's order.
    pub fn between(
        &self,
        field: &str,
        low: &Datum,
        high: &Datum,
    ) -> Result<Vec<Vec<Datum>>, Error> {
        let (index, column) = self.index_number(field)?;
        self.schema
            .check(column, low)?;
        self.schema
            .check(column, high)?;
        let id = Datum::Integer(FIRST_INDEX + index as i64);
        let start = encode_key(&[id.clone(), low.clone()]);
        let (_, end) = prefix_range(&encode_key(&[id, high.clone()]));
        let mut rows = Vec::new();
        for (entry, _) in scan(self.store, (Bound::Included(start), end))? {
            let parts = decode_key(&entry).map_err(|why| Error::BadRecord {
                key: String::from_utf8_lossy(&entry).into_owned(),
                why,
            })?;
            let Some(Datum::Text(key)) = parts.last() else {
                unreachable!("an index entry ends in the row's key");
            };
            // The row can have changed since the scan: trust it, not the entry.
            if let Some(row) = self.get(key)?
                && *low <= row[column]
                && row[column] <= *high
            {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    /// Compare every row with every index entry.
    pub fn verify(&self) -> Result<Disagreements, Error> {
        let mut expected = BTreeSet::new();
        let prefix = encode_key(&[Datum::Integer(ROWS)]);
        for (key, value) in scan(self.store, prefix_range(&prefix))? {
            let parts = decode_key(&key).map_err(|why| Error::BadRecord {
                key: String::from_utf8_lossy(&key).into_owned(),
                why,
            })?;
            let Some(Datum::Text(name)) = parts.get(1) else {
                unreachable!("a row's key is (1, key)");
            };
            let row = self
                .schema
                .row(name.as_bytes(), &value)?;
            expected.extend(self.entries(&row));
        }
        let mut found = BTreeSet::new();
        for index in 0..self
            .indexed
            .len()
        {
            let prefix = encode_key(&[Datum::Integer(FIRST_INDEX + index as i64)]);
            found.extend(
                scan(self.store, prefix_range(&prefix))?
                    .into_iter()
                    .map(|(k, _)| k),
            );
        }
        Ok(Disagreements {
            missing: expected
                .difference(&found)
                .count(),
            dangling: found
                .difference(&expected)
                .count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::Domain;
    use yabiir::Options;
    use yabiir_testutil::TempDir;

    fn schema() -> Schema {
        Schema::new(&[("maker", Domain::Text), ("year", Domain::Integer)])
    }

    fn open(dir: &TempDir) -> Engine {
        let options = Options {
            key_order: KeyOrder::Sorted,
            ..Options::default()
        };
        Engine::open(&**dir, options).unwrap()
    }

    fn row(key: &str, maker: &str, year: i64) -> Vec<Datum> {
        vec![text(key), text(maker), Datum::Integer(year)]
    }

    fn fill(table: &IndexedTable) {
        for r in [
            row("apple-ii", "apple", 1977),
            row("ibm-pc", "ibm", 1981),
            row("bbc-micro", "acorn", 1981),
            row("zx-spectrum", "sinclair", 1982),
            row("c64", "commodore", 1982),
        ] {
            table
                .insert(&r)
                .unwrap();
        }
    }

    fn keys(rows: &[Vec<Datum>]) -> Vec<String> {
        rows.iter()
            .map(|r| r[0].to_string())
            .collect()
    }

    #[test]
    fn a_lookup_finds_the_rows_with_that_value_in_key_order() {
        let dir = TempDir::new();
        let store = open(&dir);
        let table = IndexedTable::new(&store, schema(), &["year", "maker"]).unwrap();
        fill(&table);
        let found = table
            .lookup("year", &Datum::Integer(1982))
            .unwrap();
        assert_eq!(keys(&found), ["c64", "zx-spectrum"]);
        let found = table
            .between("year", &Datum::Integer(1978), &Datum::Integer(1981))
            .unwrap();
        assert_eq!(keys(&found), ["bbc-micro", "ibm-pc"]);
        assert_eq!(
            keys(
                &table
                    .lookup("maker", &text("ibm"))
                    .unwrap()
            ),
            ["ibm-pc"]
        );
        assert_eq!(
            table
                .verify()
                .unwrap(),
            Disagreements::default()
        );
    }

    #[test]
    fn update_and_delete_keep_the_index_in_step() {
        let dir = TempDir::new();
        let store = open(&dir);
        let table = IndexedTable::new(&store, schema(), &["year"]).unwrap();
        fill(&table);
        assert!(
            table
                .update("c64", "year", Datum::Integer(1990))
                .unwrap()
        );
        assert_eq!(
            keys(
                &table
                    .lookup("year", &Datum::Integer(1982))
                    .unwrap()
            ),
            ["zx-spectrum"]
        );
        assert_eq!(
            keys(
                &table
                    .lookup("year", &Datum::Integer(1990))
                    .unwrap()
            ),
            ["c64"]
        );
        assert!(
            table
                .update("c64", "maker", text("cbm"))
                .unwrap()
        );
        assert!(
            table
                .delete("ibm-pc")
                .unwrap()
        );
        assert!(
            !table
                .delete("ibm-pc")
                .unwrap()
        );
        assert!(
            !table
                .update("ibm-pc", "year", Datum::Integer(1))
                .unwrap()
        );
        assert_eq!(
            keys(
                &table
                    .lookup("year", &Datum::Integer(1981))
                    .unwrap()
            ),
            ["bbc-micro"]
        );
        assert_eq!(
            table
                .verify()
                .unwrap(),
            Disagreements::default()
        );
    }

    #[test]
    fn a_duplicate_insert_changes_nothing() {
        let dir = TempDir::new();
        let store = open(&dir);
        let table = IndexedTable::new(&store, schema(), &["year"]).unwrap();
        fill(&table);
        assert!(matches!(
            table.insert(&row("c64", "atari", 1999)),
            Err(Error::DuplicateKey(_))
        ));
        assert!(
            table
                .lookup("year", &Datum::Integer(1999))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            table
                .verify()
                .unwrap(),
            Disagreements::default()
        );
    }

    #[test]
    fn concurrent_updates_leave_one_entry_per_row() {
        let dir = TempDir::new();
        let store = open(&dir);
        let table = IndexedTable::new(&store, schema(), &["year"]).unwrap();
        fill(&table);
        std::thread::scope(|s| {
            for t in 0..4 {
                let table = &table;
                s.spawn(move || {
                    for i in 0..100 {
                        table
                            .update("c64", "year", Datum::Integer(2000 + t * 1000 + i))
                            .unwrap();
                    }
                });
            }
        });
        assert_eq!(
            table
                .verify()
                .unwrap(),
            Disagreements::default()
        );
    }

    #[test]
    fn an_index_needs_a_sorted_store_and_a_known_field() {
        let dir = TempDir::new();
        let store = Engine::open(&*dir, Options::default()).unwrap();
        assert!(IndexedTable::new(&store, schema(), &["year"]).is_err());
        let dir = TempDir::new();
        let store = open(&dir);
        assert!(IndexedTable::new(&store, schema(), &["price"]).is_err());
        let table = IndexedTable::new(&store, schema(), &["year"]).unwrap();
        assert!(matches!(
            table.lookup("maker", &text("x")),
            Err(Error::NotIndexed(_))
        ));
    }
}
