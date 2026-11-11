//! `SELECT *` over one yabiir store, translated by hand into `Bitcask::open` and `Bitcask::fold`.
//!
//! yabiir has no SQL. The store is one table whose fields we name `key` and `value`, and
//! `SELECT *` is π_{key,value}(σ_true(store)): every row qualifies, and every field is kept.
//! Rows come back in whatever order the keydir holds them.

use yabiir::{Bitcask, Engine, Options};

fn main() -> yabiir::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: select-star <store-dir>");
    let store = Engine::open(&dir, Options::default().read_only())?;

    // σ_true: the condition every row satisfies.
    let condition = |_key: &[u8], _value: &[u8]| true;

    let rows = store.fold(
        |key, value, mut rows: Vec<(Vec<u8>, Vec<u8>)>| {
            if condition(key, value) {
                // π_{key,value}: keep both fields.
                rows.push((key.to_vec(), value.to_vec()));
            }
            rows
        },
        Vec::new(),
    )?;

    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let width = rows
        .iter()
        .map(|(k, _)| k.len())
        .max()
        .unwrap_or(0)
        .max("key".len());
    println!(" {:<width$} | value", "key");
    println!("-{}-+-------", "-".repeat(width));
    for (key, value) in &rows {
        println!(" {:<width$} | {}", text(key), text(value));
    }
    println!("({} rows)", rows.len());
    Ok(())
}
