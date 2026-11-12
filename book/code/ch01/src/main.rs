//! `select-where DIR KEY --scan|--get`: run `SELECT * WHERE key = KEY` against the store
//! in DIR, opened read-only, and print the result.

use select_where::{Row, select_by_get, select_by_scan};
use yabiir::{Bitcask, Engine, Options};

fn main() -> yabiir::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, dir, key, mode] = args.as_slice() else {
        eprintln!("usage: select-where <store-dir> <key> --scan|--get");
        std::process::exit(2);
    };
    let store = Engine::open(dir, Options::default().read_only())?;

    let rows = match mode.as_str() {
        "--scan" => {
            let (rows, visited) = select_by_scan(&store, key.as_bytes())?;
            print_rows(&rows);
            println!("rows visited: {visited}");
            return Ok(());
        }
        "--get" => select_by_get(&store, key.as_bytes())?,
        _ => {
            eprintln!("mode must be --scan or --get");
            std::process::exit(2);
        }
    };
    print_rows(&rows);
    Ok(())
}

fn print_rows(rows: &[Row]) {
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let width = rows
        .iter()
        .map(|(k, _)| k.len())
        .max()
        .unwrap_or(0)
        .max("key".len());
    println!(" {:<width$} | value", "key");
    println!("-{}-+-------", "-".repeat(width));
    for (key, value) in rows {
        println!(" {:<width$} | {}", text(key), text(value));
    }
    println!("({} row{})", rows.len(), if rows.len() == 1 { "" } else { "s" });
}
