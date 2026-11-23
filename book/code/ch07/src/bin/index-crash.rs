//! Inserts a row with two indices, then cuts the log at every byte of that
//! insert and reopens it, counting the cuts where rows and index disagree:
//! written as one put per key, and as one batch.
//!
//! usage: index-crash <empty-dir>

use std::fs;
use std::path::{Path, PathBuf};

use indexes::{open, row, schema};
use yabiir::Bitcask;
use yabiir_sql::index::IndexedTable;
use yabiir_sql::table::Datum;

const FIELDS: [&str; 2] = ["year", "maker"];

fn data_file(dir: &Path) -> PathBuf {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".bitcask.data"))
        .expect("one data file")
}

fn one_put_per_key(table: &IndexedTable, store: &yabiir::Engine, r: &[Datum]) {
    let (key, value) = table.encode_row(r);
    store.put(&key, &value, 1).unwrap();
    for entry in table.entries(r) {
        store.put(&entry, b"", 1).unwrap();
    }
}

fn one_batch(table: &IndexedTable, _: &yabiir::Engine, r: &[Datum]) {
    table.insert(r).unwrap();
}

fn cut_everywhere(dir: &Path, name: &str, insert: fn(&IndexedTable, &yabiir::Engine, &[Datum])) {
    let source = dir.join(name);
    let before = {
        let store = open(&source);
        let table = IndexedTable::new(&store, schema(), &FIELDS).unwrap();
        table.insert(&row("zx-spectrum", "sinclair", 1982, 125)).unwrap();
        let before = fs::metadata(data_file(&source)).unwrap().len() as usize;
        insert(&table, &store, &row("c64", "commodore", 1982, 595));
        before
    };
    let file = data_file(&source);
    let bytes = fs::read(&file).unwrap();
    let (mut bad, mut cuts) = (0, 0);
    for cut in before..=bytes.len() {
        let copy = dir.join(format!("{name}-cut"));
        fs::create_dir_all(&copy).unwrap();
        fs::write(copy.join(file.file_name().unwrap()), &bytes[..cut]).unwrap();
        {
            let store = open(&copy);
            let table = IndexedTable::new(&store, schema(), &FIELDS).unwrap();
            let d = table.verify().unwrap();
            cuts += 1;
            bad += usize::from(d.missing + d.dangling > 0);
        }
        fs::remove_dir_all(&copy).unwrap();
    }
    println!("{name:<16} {cuts:>5} {bad:>13}");
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: index-crash <empty-dir>");
    let dir = Path::new(&dir);
    println!("{:<16} {:>5} {:>13}", "", "cuts", "disagreements");
    cut_everywhere(dir, "one put per key", one_put_per_key);
    cut_everywhere(dir, "one batch", one_batch);
}
