//! `time-where DIR N`: fill an empty store in DIR with N rows (`computer-0000000000` →
//! a year), reopen it read-only, and time `SELECT * WHERE key = k` both ways.
//!
//! Scan: the median of 5 full folds, each looking for a key near the middle.
//! Get: the median and p99 of 100,000 single `get`s for keys spread over the store.

use std::time::{Duration, Instant};

use select_where::{select_by_get, select_by_scan};
use yabiir::{Bitcask, Engine, Options, now_unix};

fn key(i: usize) -> Vec<u8> {
    format!("computer-{i:010}").into_bytes()
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn main() -> yabiir::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, dir, n] = args.as_slice() else {
        eprintln!("usage: time-where <empty-dir> <rows>");
        std::process::exit(2);
    };
    let n: usize = n.parse().expect("rows must be a number");

    let writer = Engine::open(dir, Options::default())?;
    let ts = now_unix();
    for i in 0..n {
        let year = 1975 + (i % 20);
        writer.put(&key(i), year.to_string().as_bytes(), ts)?;
    }
    writer.close()?;

    let store = Engine::open(dir, Options::default().read_only())?;
    let target = key(n / 2);

    let mut scans = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let (rows, visited) = select_by_scan(&store, &target)?;
        scans.push(start.elapsed());
        assert_eq!((rows.len(), visited), (1, n));
    }
    scans.sort();

    let mut gets = Vec::with_capacity(100_000);
    for j in 0..100_000 {
        let k = key((j * 7_919) % n);
        let start = Instant::now();
        let rows = select_by_get(&store, &k)?;
        gets.push(start.elapsed());
        assert_eq!(rows.len(), 1);
    }
    gets.sort();

    let scan = percentile(&scans, 0.5);
    let get = percentile(&gets, 0.5);
    println!("rows in the store:       {n}");
    println!("scan (fold), median:     {scan:?}  ({n} rows visited)");
    println!("get, median:             {get:?}");
    println!("get, p99:                {:?}", percentile(&gets, 0.99));
    println!(
        "scan / get (medians):    {:.0}x",
        scan.as_secs_f64() / get.as_secs_f64()
    );
    Ok(())
}
