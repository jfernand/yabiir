//! Changes a row's key, then cuts the log at every byte the change wrote and
//! reopens it: as two writes in each order, and as one batch.
//!
//! usage: move-crash <empty-dir>

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use yabiir::{Bitcask, Engine, Op, Options};

fn data_file(dir: &Path) -> PathBuf {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".bitcask.data"))
        .expect("one data file")
}

fn delete_then_put(store: &Engine) {
    store.delete(b"c64", 1).unwrap();
    store.put(b"c128", b"row", 1).unwrap();
}

fn put_then_delete(store: &Engine) {
    store.put(b"c128", b"row", 1).unwrap();
    store.delete(b"c64", 1).unwrap();
}

fn one_batch(store: &Engine) {
    let ops = vec![
        Op::Delete { key: b"c64".to_vec() },
        Op::Put { key: b"c128".to_vec(), value: b"row".to_vec() },
    ];
    store.write_batch(ops).wait().unwrap();
}

fn cut_everywhere(dir: &Path, name: &str, change: fn(&Engine)) {
    let source = dir.join(name);
    fs::create_dir_all(&source).unwrap();
    let before = {
        let store = Engine::open(&source, Options::default()).unwrap();
        store.put(b"c64", b"row", 1).unwrap();
        let before = fs::metadata(data_file(&source)).unwrap().len() as usize;
        change(&store);
        before
    };
    let file = data_file(&source);
    let bytes = fs::read(&file).unwrap();
    let mut states = BTreeSet::new();
    for cut in before..=bytes.len() {
        let copy = dir.join(format!("{name}-cut"));
        fs::create_dir_all(&copy).unwrap();
        fs::write(copy.join(file.file_name().unwrap()), &bytes[..cut]).unwrap();
        let store = Engine::open(&copy, Options::default().read_only()).unwrap();
        let mut keys: Vec<String> = store
            .list_keys()
            .unwrap()
            .iter()
            .map(|k| String::from_utf8_lossy(k).into_owned())
            .collect();
        keys.sort();
        states.insert(if keys.is_empty() { "(no row)".to_string() } else { keys.join(" and ") });
        drop(store);
        fs::remove_dir_all(&copy).unwrap();
    }
    let seen: Vec<String> = states.into_iter().collect();
    println!("{name:<16} {}", seen.join(", "));
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: move-crash <empty-dir>");
    let dir = Path::new(&dir);
    println!("{:<16} what a crash can leave", "");
    cut_everywhere(dir, "delete, put", delete_then_put);
    cut_everywhere(dir, "put, delete", put_then_delete);
    cut_everywhere(dir, "one batch", one_batch);
}
