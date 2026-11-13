//! `SELECT key`, `SELECT value` and `SELECT DISTINCT value` over one yabiir store, translated by
//! hand into `Bitcask::open` and `Bitcask::fold`.
//!
//! yabiir has no SQL. `SELECT f` keeps one field of every row, as SQL does: a value that several
//! rows share comes out once per row. `--distinct` keeps the first of each, which is Codd's π_f:
//! a relation is a set, so its projection has no repeats.

use std::collections::HashSet;

use yabiir::{Bitcask, Engine, Options};

fn main() -> yabiir::Result<()> {
    let usage = "usage: select-project <store-dir> key|value [--distinct]";
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .expect(usage);
    let field = args
        .next()
        .expect(usage);
    let distinct = match args
        .next()
        .as_deref()
    {
        None => false,
        Some("--distinct") => true,
        Some(_) => panic!("{usage}"),
    };
    let keep_key = match field.as_str() {
        "key" => true,
        "value" => false,
        _ => panic!("{usage}"),
    };
    let store = Engine::open(&dir, Options::default().read_only())?;

    // σ_true: the condition every row satisfies.
    let condition = |_key: &[u8], _value: &[u8]| true;

    let (rows, _seen) = store.fold(
        |key, value, (mut rows, mut seen): (Vec<Vec<u8>>, HashSet<Vec<u8>>)| {
            if condition(key, value) {
                // π_field: keep one of the two fields.
                let kept = if keep_key { key } else { value }.to_vec();
                // DISTINCT: drop a row equal to one already kept.
                if !distinct || seen.insert(kept.clone()) {
                    rows.push(kept);
                }
            }
            (rows, seen)
        },
        (Vec::new(), HashSet::new()),
    )?;

    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let width = rows
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .max(field.len());
    println!(" {field}");
    println!("-{}-", "-".repeat(width));
    for row in &rows {
        println!(" {}", text(row));
    }
    println!("({} rows)", rows.len());
    Ok(())
}
