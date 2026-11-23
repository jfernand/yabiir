//! Eight threads each add 1 to the same row's year, 2,000 times: first by
//! reading the row and writing it back, then with `UPDATE`.
//!
//! usage: update-race <empty-dir>

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use attributes::computers;
use yabiir::{Bitcask, Engine, Options};
use yabiir_sql::table::{self, Datum, Statement, UpdatePlan};

const THREADS: usize = 8;
const EACH: usize = 2_000;
const START: i64 = 1982;

/// The update that looks right: read the row, change it, write it back.
fn read_then_put(store: &Engine, _: &UpdatePlan) -> usize {
    let schema = computers();
    let bytes = store.get(b"c64").unwrap().unwrap();
    let mut row = schema.decode(&bytes).unwrap();
    let Datum::Integer(year) = row[1] else { unreachable!() };
    row[1] = Datum::Integer(year + 1);
    store.put(b"c64", &schema.encode(&row), yabiir::now_unix()).unwrap();
    0
}

fn update(store: &Engine, plan: &UpdatePlan) -> usize {
    table::update_rows(plan, &computers(), store).unwrap().retries
}

fn race(dir: &Path, name: &str, add_one: fn(&Engine, &UpdatePlan) -> usize) {
    std::fs::create_dir_all(dir).unwrap();
    let store = Engine::open(dir, Options::default()).unwrap();
    let schema = computers();
    let row = [Datum::Text("commodore".into()), Datum::Integer(START)];
    store.put(b"c64", &schema.encode(&row), yabiir::now_unix()).unwrap();
    let Statement::Update(u) =
        table::parse("UPDATE store SET year = year + 1 WHERE key = 'c64'").unwrap()
    else {
        unreachable!()
    };
    let plan = table::plan_update(&u, &schema, store.key_order()).unwrap();
    let retries = AtomicUsize::new(0);
    thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                for _ in 0..EACH {
                    retries.fetch_add(add_one(&store, &plan), Ordering::Relaxed);
                }
            });
        }
    });
    let row = schema.decode(&store.get(b"c64").unwrap().unwrap()).unwrap();
    let Datum::Integer(year) = row[1] else { unreachable!() };
    let added = year - START;
    let expected = (THREADS * EACH) as i64;
    println!(
        "{name:<15} {expected:>8} {added:>7} {:>6} {:>8}",
        expected - added,
        retries.load(Ordering::Relaxed)
    );
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: update-race <empty-dir>");
    let dir = Path::new(&dir);
    println!("{THREADS} threads, {EACH} increments each");
    println!("{:<15} {:>8} {:>7} {:>6} {:>8}", "", "expected", "added", "lost", "retries");
    race(&dir.join("read-then-put"), "read, then put", read_then_put);
    race(&dir.join("update"), "UPDATE", update);
}
