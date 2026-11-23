//! One thread deletes the rows from 1982 while another moves the only row to
//! 1990. In either order the row survives: deleted first it is written again,
//! moved first it no longer matches. Counts the rounds where it's gone.
//!
//! usage: delete-race <empty-dir>

use std::path::Path;
use std::sync::Barrier;
use std::thread;

use attributes::computers;
use yabiir::{Bitcask, Engine, Options};
use yabiir_sql::table::{self, Datum, Statement};

const ROUNDS: usize = 20_000;
const DELETE: &str = "DELETE FROM store WHERE year = 1982";

/// The `DELETE` that looks right: find the rows, then remove them.
fn scan_then_delete(store: &Engine) {
    let schema = computers();
    let matches = store
        .fold(
            |key, value, mut keys: Vec<Vec<u8>>| {
                if schema.decode(value).unwrap()[1] == Datum::Integer(1982) {
                    keys.push(key.to_vec());
                }
                keys
            },
            Vec::new(),
        )
        .unwrap();
    for key in matches {
        store.delete(&key, yabiir::now_unix()).unwrap();
    }
}

fn guarded(store: &Engine) {
    let Statement::Delete(delete) = table::parse(DELETE).unwrap() else {
        unreachable!()
    };
    let schema = computers();
    let plan = table::plan_delete(&delete, &schema, store.key_order()).unwrap();
    table::delete_rows(&plan, &schema, store).unwrap();
}

fn row(year: i64) -> Vec<u8> {
    computers().encode(&[Datum::Text("commodore".into()), Datum::Integer(year)])
}

fn race(dir: &Path, name: &str, delete: fn(&Engine)) {
    std::fs::create_dir_all(dir).unwrap();
    let store = Engine::open(dir, Options::default()).unwrap();
    let (before, after) = (row(1982), row(1990));
    let start = Barrier::new(2);
    let mut lost = 0;
    for _ in 0..ROUNDS {
        store.put(b"c64", &before, yabiir::now_unix()).unwrap();
        thread::scope(|s| {
            s.spawn(|| {
                start.wait();
                delete(&store);
            });
            s.spawn(|| {
                start.wait();
                store.put(b"c64", &after, yabiir::now_unix()).unwrap();
            });
        });
        if store.get(b"c64").unwrap().is_none() {
            lost += 1;
        }
    }
    println!("{name:<18} {ROUNDS:>6} {lost:>10}");
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: delete-race <empty-dir>");
    let dir = Path::new(&dir);
    println!("{:<18} {:>6} {:>10}", "", "rounds", "rows lost");
    race(&dir.join("scan-then-delete"), "scan, then delete", scan_then_delete);
    race(&dir.join("guarded"), "guarded batch", guarded);
}
