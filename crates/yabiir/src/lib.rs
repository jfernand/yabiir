//! yabiir: yet another Bitcask implementation, in Rust.
//!
//! A Bitcask store is a directory of append-only data files and an in-memory
//! index, the keydir, from each key to where its newest value is on disk.
//! Open one with [`Engine::open`](Bitcask::open) and use it through the
//! [`Bitcask`] trait:
//!
//! ```
//! use yabiir::{Bitcask, Engine, Options, now_unix};
//!
//! # let dir = std::env::temp_dir().join(format!("yabiir-doctest-{}", std::process::id()));
//! let store = Engine::open(&dir, Options::default())?;
//! store.put(b"name", b"yabiir", now_unix())?;
//! assert_eq!(store.get(b"name")?, Some(b"yabiir".to_vec()));
//! store.delete(b"name", now_unix())?;
//! assert_eq!(store.get(b"name")?, None);
//! store.close()?;
//! # std::fs::remove_dir_all(&dir).unwrap();
//! # Ok::<(), yabiir::Error>(())
//! ```
//!
//! How the pieces fit, bottom up:
//!
//! - `format`: the bytes of a data-file entry and of a hint-file record.
//! - `datafile`: the files; appending to the active one, reading any of them.
//! - `keydir`: the in-memory index.
//! - `writer`: the single writer, which appends and rotates.
//! - `commit`: group commit, for `sync_on_put`.
//! - `recovery`: rebuilding the keydir at open.
//! - `merge`: compaction.
//! - `engine`: [`Engine`], which puts them behind the [`Bitcask`] API.

mod api;
mod commit;
mod engine;
mod error;
mod lock;
mod log;
mod merge;
mod metrics;
mod recovery;
mod sync;
#[cfg(test)]
mod testutil;
mod writer;

// Internal building blocks, not part of the supported API: public only so
// that `benches/` (a separate crate) can measure them directly.
#[doc(hidden)]
pub mod datafile;
#[doc(hidden)]
pub mod format;
#[doc(hidden)]
pub mod keydir;

pub use api::{Bitcask, KeyOrder, Options};
pub use engine::Engine;
pub use error::{Error, Result};
pub use metrics::Metrics;
use std::time::{SystemTime, UNIX_EPOCH};

/// The current time as seconds since the Unix epoch, the timestamp `put` and
/// `delete` take.
pub fn now_unix() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the system clock is set before 1970")
        .as_secs() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_unix_returns_a_plausible_current_timestamp() {
        // Loosely bounded, not exact: catches a stub returning 0/1 (or any
        // other constant) without being sensitive to test execution speed.
        // 1_700_000_000 is 2023-11-14; comfortably in the past of any real
        // test run without hardcoding "now".
        let t = now_unix();
        assert!(
            t > 1_700_000_000,
            "now_unix() returned implausibly small {t}"
        );
    }
}
