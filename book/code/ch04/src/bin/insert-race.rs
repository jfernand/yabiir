//! Eight threads insert the same key at once, a thousand keys in turn: first
//! with a `get` followed by a `put`, then with one batch.
//!
//! usage: insert-race <empty-dir>

use std::sync::Barrier;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use yabiir::{Bitcask, Cond, Engine, Op, Options};

const THREADS: usize = 8;
const KEYS: usize = 1000;

/// The `INSERT` that looks right: if the key isn't there, write it.
fn get_then_put(store: &Engine, key: &[u8], value: &[u8]) -> bool {
    if store.get(key).unwrap().is_some() {
        return false;
    }
    store.put(key, value, yabiir::now_unix()).unwrap();
    true
}

/// The check and the write in one batch.
fn batch(store: &Engine, key: &[u8], value: &[u8]) -> bool {
    let ops = vec![
        Op::Check { key: key.to_vec(), cond: Cond::Absent },
        Op::Put { key: key.to_vec(), value: value.to_vec() },
    ];
    match store.write_batch(ops).wait() {
        Ok(_) => true,
        Err(yabiir::Error::ConditionFailed { .. }) => false,
        Err(e) => panic!("{e}"),
    }
}

fn race(dir: &std::path::Path, name: &str, insert: fn(&Engine, &[u8], &[u8]) -> bool) {
    std::fs::create_dir_all(dir).unwrap();
    let store = Engine::open(dir, Options::default()).unwrap();
    let start = Barrier::new(THREADS);
    let told_ok: Vec<AtomicUsize> = (0..KEYS).map(|_| AtomicUsize::new(0)).collect();
    thread::scope(|s| {
        for t in 0..THREADS {
            let (store, start, told_ok) = (&store, &start, &told_ok);
            s.spawn(move || {
                let value = [t as u8];
                for (k, count) in told_ok.iter().enumerate() {
                    start.wait();
                    if insert(store, format!("key-{k:04}").as_bytes(), &value) {
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    let counts: Vec<usize> = told_ok.iter().map(|c| c.load(Ordering::Relaxed)).collect();
    let successes: usize = counts.iter().sum();
    let lost = successes - store.list_keys().unwrap().len();
    let keys_with_two = counts.iter().filter(|&&n| n > 1).count();
    println!("{name:<13} {successes:>5} {keys_with_two:>13} {lost:>10}");
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: insert-race <empty-dir>");
    let dir = std::path::Path::new(&dir);
    println!("{KEYS} keys, {THREADS} threads");
    println!("{:<13} {:>5} {:>13} {:>10}", "", "ok", "keys twice+", "rows lost");
    race(&dir.join("get-then-put"), "get then put", get_then_put);
    race(&dir.join("batch"), "batch", batch);
}
