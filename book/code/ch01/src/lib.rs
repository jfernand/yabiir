//! `SELECT * WHERE key = k`, translated by hand into yabiir's API two ways: as the
//! Chapter 0 fold with a real condition (σ over every row), and as one `get`.

use yabiir::{Bitcask, Engine};

pub type Row = (Vec<u8>, Vec<u8>);

/// σ_{key = k}, the general way: visit every row and keep the ones whose key is `k`.
/// Returns the rows kept and how many rows were visited.
pub fn select_by_scan(store: &Engine, k: &[u8]) -> yabiir::Result<(Vec<Row>, usize)> {
    let condition = |key: &[u8], _value: &[u8]| key == k; // σ_{key = k}

    store.fold(
        |key, value, (mut rows, visited): (Vec<Row>, usize)| {
            if condition(key, value) {
                rows.push((key.to_vec(), value.to_vec())); // π_{key, value}
            }
            (rows, visited + 1)
        },
        (Vec::new(), 0),
    )
}

/// σ_{key = k}, using that keys are unique: at most one row can match, and `get`
/// finds it directly.
pub fn select_by_get(store: &Engine, k: &[u8]) -> yabiir::Result<Vec<Row>> {
    Ok(store
        .get(k)?
        .map(|value| (k.to_vec(), value))
        .into_iter()
        .collect())
}
