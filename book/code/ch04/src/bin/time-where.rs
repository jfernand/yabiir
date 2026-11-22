//! The same table with a million rows: a condition on the key, and one on an attribute.
//!
//! usage: time-where <empty-dir>

use std::time::Instant;

use attributes::computers;
use yabiir::{Bitcask, Engine, Options};
use yabiir_sql::table::{self, Datum, Statement};

const ROWS: usize = 1_000_000;
const RUNS: usize = 5;

fn main() {
    let dir = std::env::args().nth(1).expect("usage: time-where <empty-dir>");
    let schema = computers();
    let store = Engine::open(&dir, Options::default()).unwrap();
    let makers = ["apple", "ibm", "acorn", "sinclair", "commodore"];
    for i in 0..ROWS {
        let row = [
            Datum::Text(makers[i % makers.len()].to_string()),
            Datum::Integer(1970 + (i % 30) as i64),
        ];
        store
            .put(format!("key-{i:07}").as_bytes(), &schema.encode(&row), yabiir::now_unix())
            .unwrap();
    }
    println!("{ROWS} rows; each time is the median of {RUNS} runs");
    for text in ["SELECT * WHERE key = 'key-0500000'", "SELECT * WHERE year = 1982"] {
        let Ok(Statement::Select(select)) = table::parse(text) else {
            panic!("{text}");
        };
        let plan = table::plan(&select, &schema, store.key_order()).unwrap();
        let mut times = Vec::new();
        let mut last = None;
        for _ in 0..RUNS {
            let start = Instant::now();
            last = Some(table::execute(&plan, &schema, &store).unwrap());
            times.push(start.elapsed());
        }
        times.sort();
        let rows = last.unwrap();
        println!(
            "{text}: {} rows returned, {} visited, {:?}",
            rows.rows.len(),
            rows.visited,
            times[RUNS / 2]
        );
    }
}
