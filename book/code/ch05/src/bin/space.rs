//! The size of a store on disk: full, after a `DELETE` of half its rows, and
//! after a merge.
//!
//! usage: space <empty-dir>

use std::fs;
use std::path::Path;

use attributes::computers;
use yabiir::{Bitcask, Engine, Options};
use yabiir_sql::table::{self, Datum, Statement};

const ROWS: usize = 100_000;

/// Bytes in data files, and in hint files.
fn on_disk(dir: &Path) -> (u64, u64) {
    let (mut data, mut hint) = (0, 0);
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        let len = entry.metadata().unwrap().len();
        if name.ends_with(".bitcask.data") {
            data += len;
        } else if name.ends_with(".bitcask.hint") {
            hint += len;
        }
    }
    (data, hint)
}

fn report(dir: &Path, store: &Engine, stage: &str) {
    let (data, hint) = on_disk(dir);
    println!("{stage:<14} {:>8} {data:>11} {hint:>11}", store.list_keys().unwrap().len());
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: space <empty-dir>");
    let dir = Path::new(&dir);
    fs::create_dir_all(dir).unwrap();
    let store = Engine::open(dir, Options::default()).unwrap();
    let schema = computers();
    for i in 0..ROWS {
        let value = schema.encode(&[
            Datum::Text("commodore".into()),
            Datum::Integer(1981 + (i % 2) as i64),
        ]);
        store.put(format!("key-{i:06}").as_bytes(), &value, yabiir::now_unix()).unwrap();
    }
    println!("{:<14} {:>8} {:>11} {:>11}", "", "rows", "data bytes", "hint bytes");
    report(dir, &store, "inserted");
    let Statement::Delete(delete) = table::parse("DELETE FROM store WHERE year = 1982").unwrap()
    else {
        unreachable!()
    };
    let plan = table::plan_delete(&delete, &schema, store.key_order()).unwrap();
    table::delete_rows(&plan, &schema, &store).unwrap();
    report(dir, &store, "after DELETE");
    store.merge().unwrap();
    report(dir, &store, "after merge");
}
