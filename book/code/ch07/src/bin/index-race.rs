//! Two threads change the same row's indexed year at once, 5,000 times, and
//! we count the rounds that leave an index entry the row doesn't back:
//! without a version check, then with one.
//!
//! usage: index-race <empty-dir>

use std::path::Path;
use std::sync::Barrier;
use std::thread;

use indexes::{open, row, schema};
use yabiir::{Bitcask, Engine, Op};
use yabiir_sql::index::IndexedTable;
use yabiir_sql::table::Datum;

const ROUNDS: i64 = 5_000;

/// Read the row, then write it and swap its index entry, with no check.
fn unguarded(table: &IndexedTable, store: &Engine, year: i64) {
    let old = table.get("c64").unwrap().unwrap();
    let mut new = old.clone();
    new[2] = Datum::Integer(year);
    let (key, value) = table.encode_row(&new);
    let mut ops = vec![Op::Put { key, value }];
    for (before, after) in table.entries(&old).into_iter().zip(table.entries(&new)) {
        if before != after {
            ops.push(Op::Delete { key: before });
            ops.push(Op::Put { key: after, value: Vec::new() });
        }
    }
    store.write_batch(ops).wait().unwrap();
}

fn guarded(table: &IndexedTable, _: &Engine, year: i64) {
    table.update("c64", "year", Datum::Integer(year)).unwrap();
}

fn race(dir: &Path, name: &str, change: fn(&IndexedTable, &Engine, i64)) {
    let store = open(dir);
    let table = IndexedTable::new(&store, schema(), &["year"]).unwrap();
    table.insert(&row("c64", "commodore", 1982, 595)).unwrap();
    let start = Barrier::new(2);
    let mut bad = 0;
    for round in 0..ROUNDS {
        thread::scope(|s| {
            for t in 0..2 {
                let (table, store, start) = (&table, &store, &start);
                s.spawn(move || {
                    start.wait();
                    change(table, store, 10_000 + round * 2 + t);
                });
            }
        });
        if table.verify().unwrap().dangling > 0 {
            bad += 1;
            // Start the next round from a store that agrees.
            let keys = store.list_keys().unwrap();
            for key in keys {
                store.delete(&key, 1).unwrap();
            }
            table.insert(&row("c64", "commodore", 1982, 595)).unwrap();
        }
    }
    println!("{name:<16} {ROUNDS:>6} {bad:>16}");
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: index-race <empty-dir>");
    let dir = Path::new(&dir);
    println!("{:<16} {:>6} {:>16}", "", "rounds", "stale index rows");
    race(&dir.join("unguarded"), "no version check", unguarded);
    race(&dir.join("guarded"), "version check", guarded);
}
