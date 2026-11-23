//! The same store with a hashed and a sorted keydir: time to load it, to get
//! every key in shuffled order, and to reopen it. The median of three runs.
//!
//! usage: keydir-times <empty-dir>

use std::path::Path;
use std::time::Instant;

use yabiir::{Bitcask, Engine, KeyOrder, Options};

const KEYS: u64 = 1_000_000;

fn key(i: u64) -> String {
    format!("key-{i:07}")
}

/// A permutation of 0..KEYS: multiplying by a number coprime to KEYS.
fn shuffled(i: u64) -> u64 {
    i.wrapping_mul(611_953) % KEYS
}

fn once(dir: &Path, key_order: KeyOrder) -> [f64; 3] {
    std::fs::create_dir_all(dir).unwrap();
    let options = Options { key_order, ..Options::default() };
    let store = Engine::open(dir, options.clone()).unwrap();
    let t = Instant::now();
    for i in 0..KEYS {
        store.put(key(shuffled(i)).as_bytes(), b"v", 1).unwrap();
    }
    let put = t.elapsed().as_nanos() as f64 / KEYS as f64;
    let keys: Vec<String> = (0..KEYS).map(|i| key(shuffled(i.wrapping_mul(7)))).collect();
    let t = Instant::now();
    for k in &keys {
        std::hint::black_box(store.get(k.as_bytes()).unwrap());
    }
    let get = t.elapsed().as_nanos() as f64 / KEYS as f64;
    drop(store);
    let t = Instant::now();
    let store = Engine::open(dir, options).unwrap();
    let open = t.elapsed().as_secs_f64() * 1000.0;
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
    [put, get, open]
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: keydir-times <empty-dir>");
    let dir = Path::new(&dir);
    println!("{KEYS} keys, median of 3 runs");
    println!("{:<8} {:>10} {:>10} {:>10}", "keydir", "put (ns)", "get (ns)", "open (ms)");
    for (name, order) in [("hashed", KeyOrder::Hashed), ("sorted", KeyOrder::Sorted)] {
        let runs: Vec<[f64; 3]> = (0..3).map(|_| once(&dir.join(name), order)).collect();
        let median = |c: usize| {
            let mut v: Vec<f64> = runs.iter().map(|r| r[c]).collect();
            v.sort_by(f64::total_cmp);
            v[1]
        };
        println!("{name:<8} {:>10.0} {:>10.0} {:>10.0}", median(0), median(1), median(2));
    }
}
