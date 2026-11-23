//! Removes four of five rows, then cuts the log at every byte the removal
//! wrote and reopens it, as a crash would leave it: first with one delete per
//! row, then with one batch.
//!
//! usage: delete-crash <empty-dir>

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use yabiir::{Bitcask, Engine, Op, Options};

const KEYS: [&str; 5] = ["apple-ii", "ibm-pc", "bbc-micro", "zx-spectrum", "c64"];

fn data_file(dir: &Path) -> PathBuf {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".bitcask.data"))
        .expect("one data file")
}

fn one_at_a_time(store: &Engine, keys: &[&str]) {
    for key in keys {
        store.delete(key.as_bytes(), yabiir::now_unix()).unwrap();
    }
}

fn one_batch(store: &Engine, keys: &[&str]) {
    let ops = keys.iter().map(|k| Op::Delete { key: k.as_bytes().to_vec() }).collect();
    store.write_batch(ops).wait().unwrap();
}

fn cut_everywhere(dir: &Path, name: &str, remove: fn(&Engine, &[&str])) {
    let source = dir.join(name);
    fs::create_dir_all(&source).unwrap();
    let before = {
        let store = Engine::open(&source, Options::default()).unwrap();
        for key in KEYS {
            store.put(key.as_bytes(), b"row", yabiir::now_unix()).unwrap();
        }
        let before = fs::metadata(data_file(&source)).unwrap().len();
        remove(&store, &KEYS[1..]);
        before
    };
    let file = data_file(&source);
    let bytes = fs::read(&file).unwrap();
    let mut rows_left = BTreeSet::new();
    let cuts = before as usize..=bytes.len();
    let n_cuts = cuts.clone().count();
    for cut in cuts {
        let copy = dir.join(format!("{name}-cut"));
        fs::create_dir_all(&copy).unwrap();
        fs::write(copy.join(file.file_name().unwrap()), &bytes[..cut]).unwrap();
        let store = Engine::open(&copy, Options::default().read_only()).unwrap();
        rows_left.insert(store.list_keys().unwrap().len());
        drop(store);
        fs::remove_dir_all(&copy).unwrap();
    }
    let seen: Vec<String> = rows_left.iter().rev().map(ToString::to_string).collect();
    println!("{name:<14} {:>5} {:>5}   {}", bytes.len() - before as usize, n_cuts, seen.join(", "));
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: delete-crash <empty-dir>");
    let dir = Path::new(&dir);
    println!("{:<14} {:>5} {:>5}   rows after reopening", "", "bytes", "cuts");
    cut_everywhere(dir, "one-at-a-time", one_at_a_time);
    cut_everywhere(dir, "one-batch", one_batch);
}
