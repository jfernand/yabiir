//! What an index costs on every insert, and what it saves on a lookup.
//!
//! usage: index-times <empty-dir>

use std::path::Path;
use std::time::{Duration, Instant};

use indexes::{open, row, schema};
use yabiir::Bitcask;
use yabiir_sql::index::IndexedTable;
use yabiir_sql::table::Datum;

const ROWS: usize = 200_000;
const RUNS: usize = 5;
const MAKERS: [&str; 5] = ["apple", "ibm", "acorn", "sinclair", "commodore"];

fn the_row(i: usize) -> Vec<Datum> {
    row(
        &format!("key-{i:07}"),
        MAKERS[i % MAKERS.len()],
        1970 + (i % 30) as i64,
        100 + (i % 900) as i64,
    )
}

fn data_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".bitcask.data"))
        .map(|e| e.metadata().unwrap().len())
        .sum()
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: index-times <empty-dir>");
    let dir = Path::new(&dir);
    println!("{ROWS} rows; each time is the median of {RUNS} runs");
    println!("{:<8} {:>12} {:>12}", "indices", "insert (ns)", "bytes/row");
    let fields = ["year", "maker", "price"];
    for n in 0..=fields.len() {
        let mut times = Vec::new();
        let mut bytes = 0;
        for run in 0..RUNS {
            let d = dir.join(format!("insert-{n}-{run}"));
            let store = open(&d);
            let table = IndexedTable::new(&store, schema(), &fields[..n]).unwrap();
            let start = Instant::now();
            for i in 0..ROWS {
                table.insert(&the_row(i)).unwrap();
            }
            times.push(start.elapsed());
            bytes = data_bytes(&d);
            drop(table);
            drop(store);
            std::fs::remove_dir_all(&d).unwrap();
        }
        println!(
            "{n:<8} {:>12} {:>12}",
            median(times).as_nanos() / ROWS as u128,
            bytes / ROWS as u64
        );
    }
    println!();

    let d = dir.join("lookup");
    let store = open(&d);
    let table = IndexedTable::new(&store, schema(), &["year"]).unwrap();
    for i in 0..ROWS {
        table.insert(&the_row(i)).unwrap();
    }
    let year = Datum::Integer(1982);
    let wanted = schema();
    let mut scans = Vec::new();
    let mut matched = 0usize;
    for _ in 0..RUNS {
        let start = Instant::now();
        matched = store
            .fold(
                |key, value, n| {
                    // Rows are the keys of table 1; index entries have no value.
                    if value.is_empty() || key[8] != 1 {
                        return n;
                    }
                    n + usize::from(wanted.decode(value).unwrap()[1] == year)
                },
                0,
            )
            .unwrap();
        scans.push(start.elapsed());
    }
    let mut lookups = Vec::new();
    let mut found = 0;
    for _ in 0..RUNS {
        let start = Instant::now();
        found = table.lookup("year", &year).unwrap().len();
        lookups.push(start.elapsed());
    }
    assert_eq!(matched, found);
    println!("WHERE year = 1982: {found} rows");
    println!("  full scan  {:>10.2?}", median(scans));
    println!("  index      {:>10.2?}", median(lookups));
}
