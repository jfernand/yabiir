//! Runnable, readable demonstration of the whole `Bitcask` API: open, put,
//! get, delete, list_keys, fold, merge, close, reopen, verify persisted.
//! See `docs/bitcask-implementation-plan.md` §9.2.
//!
//! Run with: `cargo run --example basic_usage`

use yabiir::{now_unix, Bitcask, Engine, Options};

fn main() -> yabiir::Result<()> {
    let dir = std::env::temp_dir().join(format!("yabiir-basic-usage-example-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // start from a clean slate each run

    println!("opening datastore at {}", dir.display());
    let db = Engine::open(&dir, Options::default())?;

    db.put(b"language", b"rust", now_unix())?;
    db.put(b"paper", b"bitcask", now_unix())?;
    db.put(b"year", b"2010", now_unix())?;
    println!("wrote 3 keys");

    let language = db.get(b"language")?;
    println!(
        "language = {:?}",
        language.map(|v| String::from_utf8_lossy(&v).into_owned())
    );

    db.delete(b"year", now_unix())?;
    assert_eq!(db.get(b"year")?, None);
    println!("deleted \"year\"");

    let mut keys = db.list_keys()?;
    keys.sort();
    let readable_keys: Vec<_> = keys.iter().map(|k| String::from_utf8_lossy(k)).collect();
    println!("live keys: {readable_keys:?}");

    // Sum the byte length of every live value — a stand-in for whatever
    // aggregate a real caller might want without loading everything into a
    // Vec first.
    let total_value_bytes = db.fold(|_key, value, acc| acc + value.len(), 0usize)?;
    println!("total live value bytes: {total_value_bytes}");

    // Overwrite "language" a couple more times, so merge has something
    // superseded to actually compact away.
    db.put(b"language", b"rust (updated)", now_unix())?;
    db.put(b"language", b"rust (updated again)", now_unix())?;
    db.merge()?;
    println!("merged");

    db.close()?;
    println!("closed");

    // Reopen fresh — recovery rebuilds the keydir from what's on disk, so
    // everything written above should still be there.
    let reopened = Engine::open(&dir, Options::default())?;
    let mut keys_after_reopen = reopened.list_keys()?;
    keys_after_reopen.sort();
    assert_eq!(keys, keys_after_reopen);
    assert_eq!(
        reopened.get(b"language")?,
        Some(b"rust (updated again)".to_vec())
    );
    println!(
        "reopened: {} keys persisted correctly, including the merged value",
        keys_after_reopen.len()
    );

    reopened.close()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
